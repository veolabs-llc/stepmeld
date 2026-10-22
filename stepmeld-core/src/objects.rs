//! A shared store over an object store with conditional writes (S3,
//! say, or Azure's blobs): [`ObjectStore`] keeps every document as an
//! object and makes each condition the store needs a conditional write
//! (docs/PLACEMENT.md §2, §9). A provider's crate implements
//! [`Objects`] for its service; [`MemoryObjects`] is the tests'.
//!
//! Keys, under the store's prefix:
//!
//! | Key | Holds |
//! |---|---|
//! | `definitions.json` | every StepDefinition and WorkflowDefinition |
//! | `workflows/<id>.json` | a Workflow; its version is the object's |
//! | `history/<id>/<first>+<count>.json` | one append of its History |
//! | `leases/<id>.json` | who advances it, until when |
//! | `workers/<name>.json` | a published Worker |
//! | `orders/<id>.json` | an order with its claim, cancel and report |
//! | `open/<id>` | an order that has not ended |
//! | `logs/<id>/<at>` | an order's log, a chunk at each length |

use crate::definition::{Ref, StepDefinition, WorkflowDefinition};
use crate::history::Entry;
use crate::library::Library;
use crate::orders::{CancelAsk, Claim, Order, OrderBook, Report, WorkerFace};
use crate::store::StateStore;
use crate::workflow::Workflow;
use crate::Error;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// The condition on a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond<'a> {
    None,
    /// Only over this version.
    IfMatch(&'a str),
    /// Only if there is no object yet.
    IfNoneMatch,
}

/// An object store with conditional writes: what a provider supplies.
pub trait Objects: Send + Sync {
    fn describe(&self) -> String;
    /// An object and its version; `None` when there is none.
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, Error>;
    /// Write it; the new version, or `None` when the condition did not
    /// hold.
    fn put(&self, key: &str, body: &[u8], cond: Cond) -> Result<Option<String>, Error>;
    /// One that is not there is nothing to do.
    fn delete(&self, key: &str) -> Result<(), Error>;
    /// The keys under `prefix`, sorted.
    fn list(&self, prefix: &str) -> Result<Vec<String>, Error>;
    /// The keys under `prefix` with their versions. By default, each
    /// one read; a service whose listing carries versions says so.
    fn listing(&self, prefix: &str) -> Result<Vec<(String, String)>, Error> {
        let mut out = Vec::new();
        for k in self.list(prefix)? {
            if let Some((_, v)) = self.get(&k)? {
                out.push((k, v));
            }
        }
        Ok(out)
    }
}

/// Each key's body and version, and the last version handed out.
type Held = (BTreeMap<String, (Vec<u8>, String)>, u64);

/// Objects held in memory: the tests'.
#[derive(Default)]
pub struct MemoryObjects {
    inner: Mutex<Held>,
}

impl MemoryObjects {
    pub fn new() -> MemoryObjects {
        MemoryObjects::default()
    }
}

impl Objects for MemoryObjects {
    fn describe(&self) -> String {
        "objects in memory".into()
    }

    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, Error> {
        Ok(self.inner.lock().unwrap().0.get(key).cloned())
    }

    fn put(&self, key: &str, body: &[u8], cond: Cond) -> Result<Option<String>, Error> {
        let mut inner = self.inner.lock().unwrap();
        let held = inner.0.get(key).map(|(_, v)| v.clone());
        let ok = match cond {
            Cond::None => true,
            Cond::IfNoneMatch => held.is_none(),
            Cond::IfMatch(v) => held.as_deref() == Some(v),
        };
        if !ok {
            return Ok(None);
        }
        inner.1 += 1;
        let version = format!("\"{}\"", inner.1);
        inner.0.insert(key.to_string(), (body.to_vec(), version.clone()));
        Ok(Some(version))
    }

    fn delete(&self, key: &str) -> Result<(), Error> {
        self.inner.lock().unwrap().0.remove(key);
        Ok(())
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, Error> {
        Ok(self.inner.lock().unwrap().0.keys().filter(|k| k.starts_with(prefix)).cloned().collect())
    }
}

/// How many times a read-modify-write is tried against others' writes.
const TRIES: usize = 16;

#[derive(Default, Serialize, Deserialize)]
struct Definitions {
    #[serde(default)]
    steps: Vec<StepDefinition>,
    #[serde(default)]
    workflows: Vec<WorkflowDefinition>,
}

#[derive(Serialize, Deserialize)]
struct Lease {
    holder: String,
    until: String,
}

/// The shared store kept as objects.
pub struct ObjectStore<O: Objects> {
    objects: O,
    prefix: String,
}

fn json<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec_pretty(v).unwrap()
}

impl<O: Objects> ObjectStore<O> {
    /// Under `prefix` (`stepmeld/`, say; empty for the whole store).
    pub fn new(objects: O, prefix: &str) -> ObjectStore<O> {
        let prefix = if prefix.is_empty() || prefix.ends_with('/') { prefix.to_string() } else { format!("{prefix}/") };
        ObjectStore { objects, prefix }
    }

    /// The objects it is kept in: for an embedder that writes one of the
    /// documents the table above names whole (a mirror of definitions,
    /// say), under the same conditions.
    pub fn objects(&self) -> &O {
        &self.objects
    }

    fn key(&self, k: &str) -> String {
        format!("{}{k}", self.prefix)
    }

    fn read<T: DeserializeOwned>(&self, k: &str) -> Result<Option<(T, String)>, Error> {
        match self.objects.get(&self.key(k))? {
            None => Ok(None),
            Some((body, v)) => serde_json::from_slice(&body).map(|t| Some((t, v))).map_err(|e| Error::Protocol(format!("{}: {e}", self.key(k)))),
        }
    }

    fn write<T: Serialize>(&self, k: &str, v: &T, cond: Cond) -> Result<Option<String>, Error> {
        self.objects.put(&self.key(k), &json(v), cond)
    }

    /// Read, change, write over what was read, again when another wrote
    /// first. `change` answers what to write, or `None` to write nothing;
    /// the answer is whatever it set aside in `out`.
    fn update<T: Serialize + DeserializeOwned, R>(&self, k: &str, mut change: impl FnMut(Option<T>) -> Result<(Option<T>, R), Error>) -> Result<R, Error> {
        for _ in 0..TRIES {
            let held = self.read::<T>(k)?;
            let (value, version) = match held {
                Some((t, v)) => (Some(t), Some(v)),
                None => (None, None),
            };
            let (next, out) = change(value)?;
            let Some(next) = next else { return Ok(out) };
            let cond = version.as_deref().map_or(Cond::IfNoneMatch, Cond::IfMatch);
            if self.write(k, &next, cond)?.is_some() {
                return Ok(out);
            }
        }
        Err(Error::Refused(format!("{} kept changing underneath; ask again", self.key(k))))
    }

    fn definitions(&self) -> Result<Definitions, Error> {
        Ok(self.read::<Definitions>("definitions.json")?.map(|(d, _)| d).unwrap_or_default())
    }

    fn appends(&self, workflow: &str) -> Result<Vec<(u64, u64, String)>, Error> {
        let prefix = self.key(&format!("history/{workflow}/"));
        let mut out: Vec<(u64, u64, String)> = self
            .objects
            .list(&prefix)?
            .into_iter()
            .filter_map(|k| {
                let name = k.strip_prefix(&prefix)?.strip_suffix(".json")?;
                let (first, count) = name.split_once('+')?;
                Some((first.parse().ok()?, count.parse().ok()?, k.clone()))
            })
            .collect();
        out.sort();
        Ok(out)
    }

    fn order_doc(&self, id: &str) -> String {
        format!("orders/{}.json", id.replace('/', "~"))
    }

    fn open_key(&self, id: &str) -> String {
        format!("open/{}", id.replace('/', "~"))
    }

    fn log_prefix(&self, id: &str) -> String {
        format!("logs/{}/", id.replace('/', "~"))
    }

    /// The log's chunks: where each starts, and its key.
    fn chunks(&self, id: &str) -> Result<Vec<(u64, String)>, Error> {
        let prefix = self.key(&self.log_prefix(id));
        let mut out: Vec<(u64, String)> = self.objects.list(&prefix)?.into_iter().filter_map(|k| Some((k.strip_prefix(&prefix)?.parse().ok()?, k.clone()))).collect();
        out.sort();
        Ok(out)
    }

    /// Change an order that has not ended, over the order as it stands.
    /// Whether it was changed; `change` says whether to.
    fn change_order(&self, id: &str, mut change: impl FnMut(&mut Order) -> bool) -> Result<bool, Error> {
        let k = self.order_doc(id);
        let changed = self.update::<Order, bool>(&k, |held| match held {
            Some(mut o) if !o.ended() => {
                let changed = change(&mut o);
                Ok((changed.then_some(o), changed))
            }
            _ => Ok((None, false)),
        })?;
        if changed {
            if let Some((o, _)) = self.read::<Order>(&k)? {
                if o.ended() {
                    self.objects.delete(&self.key(&self.open_key(id)))?;
                }
            }
        }
        Ok(changed)
    }
}

impl<O: Objects> StateStore for ObjectStore<O> {
    fn describe(&self) -> String {
        format!("{} under {:?}", self.objects.describe(), self.prefix)
    }

    fn put_step(&self, def: &StepDefinition) -> Result<(), Error> {
        def.check()?;
        self.update::<Definitions, ()>("definitions.json", |held| {
            let mut d = held.unwrap_or_default();
            d.steps.retain(|s| s.reference() != def.reference());
            d.steps.push(def.clone());
            Ok((Some(d), ()))
        })
    }

    fn put_workflow_definition(&self, def: &WorkflowDefinition) -> Result<(), Error> {
        self.library()?.check_workflow(def)?;
        self.update::<Definitions, ()>("definitions.json", |held| {
            let mut d = held.unwrap_or_default();
            d.workflows.retain(|w| w.reference() != def.reference());
            d.workflows.push(def.clone());
            Ok((Some(d), ()))
        })
    }

    fn remove_definition(&self, reference: &Ref) -> Result<(), Error> {
        self.update::<Definitions, ()>("definitions.json", |held| {
            let Some(mut d) = held else { return Ok((None, ())) };
            let before = d.steps.len() + d.workflows.len();
            d.steps.retain(|s| &s.reference() != reference);
            d.workflows.retain(|w| &w.reference() != reference);
            Ok(((d.steps.len() + d.workflows.len() != before).then_some(d), ()))
        })
    }

    fn definition_refs(&self) -> Result<Vec<Ref>, Error> {
        let d = self.definitions()?;
        Ok(d.steps.iter().map(|s| s.reference()).chain(d.workflows.iter().map(|w| w.reference())).collect())
    }

    fn library(&self) -> Result<Library, Error> {
        let d = self.definitions()?;
        Library::from_parts(d.steps, d.workflows)
    }

    fn definitions(&self) -> Result<(Vec<StepDefinition>, Vec<WorkflowDefinition>), Error> {
        let d = ObjectStore::definitions(self)?;
        Ok((d.steps, d.workflows))
    }

    fn get(&self, id: &str) -> Result<Option<(Workflow, String)>, Error> {
        self.read(&format!("workflows/{id}.json"))
    }

    fn put(&self, wf: &Workflow, expected: Option<&str>) -> Result<String, Error> {
        let cond = expected.map_or(Cond::IfNoneMatch, Cond::IfMatch);
        match self.write(&format!("workflows/{}.json", wf.id), wf, cond)? {
            Some(v) => Ok(v),
            None if expected.is_none() => Err(Error::Refused(format!("workflow {} already exists", wf.id))),
            None => Err(Error::Refused(format!("workflow {} changed underneath", wf.id))),
        }
    }

    fn list(&self) -> Result<Vec<String>, Error> {
        let prefix = self.key("workflows/");
        Ok(self.objects.list(&prefix)?.into_iter().filter_map(|k| k.strip_prefix(&prefix)?.strip_suffix(".json").map(str::to_string)).collect())
    }

    fn listing(&self) -> Result<BTreeMap<String, String>, Error> {
        let prefix = self.key("workflows/");
        Ok(self.objects.listing(&prefix)?.into_iter().filter_map(|(k, v)| Some((k.strip_prefix(&prefix)?.strip_suffix(".json")?.to_string(), v))).collect())
    }

    fn remove(&self, id: &str) -> Result<(), Error> {
        // its Runs' ids are `<id>/<step>/<n>`: the orders under that
        // prefix, each checked by the Workflow its request names
        let prefix = self.key(&self.order_doc(&format!("{id}/")));
        let prefix = prefix.strip_suffix(".json").unwrap_or(&prefix).to_string();
        let mut orders = Vec::new();
        for k in self.objects.list(&prefix)? {
            let Some((body, _)) = self.objects.get(&k)? else { continue };
            let order: Order = serde_json::from_slice(&body).map_err(|e| Error::Protocol(format!("{k}: {e}")))?;
            if order.request.workflow != id {
                continue;
            }
            if !order.ended() {
                return Err(crate::orders::live_order(id, &order.id));
            }
            orders.push((k, order.id));
        }
        for (k, order) in orders {
            for (_, chunk) in self.chunks(&order)? {
                self.objects.delete(&chunk)?;
            }
            self.objects.delete(&self.key(&self.open_key(&order)))?;
            self.objects.delete(&k)?;
        }
        self.objects.delete(&self.key(&format!("workflows/{id}.json")))?;
        for (_, _, k) in self.appends(id)? {
            self.objects.delete(&k)?;
        }
        self.objects.delete(&self.key(&format!("leases/{id}.json")))
    }

    fn append(&self, entries: &[Entry]) -> Result<(), Error> {
        let mut by: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
        for e in entries {
            by.entry(e.workflow.as_str()).or_default().push(e);
        }
        for (workflow, entries) in by {
            let mut written = false;
            for _ in 0..TRIES {
                let first = self.appends(workflow)?.last().map_or(1, |(f, c, _)| f + c);
                let numbered: Vec<Entry> = entries
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let mut e = (*e).clone();
                        e.seq = first + i as u64;
                        e
                    })
                    .collect();
                if self.write(&format!("history/{workflow}/{first:012}+{}.json", numbered.len()), &numbered, Cond::IfNoneMatch)?.is_some() {
                    written = true;
                    break;
                }
            }
            if !written {
                return Err(Error::Refused(format!("the history of {workflow} kept growing underneath; ask again")));
            }
        }
        Ok(())
    }

    fn history(&self, workflow: &str) -> Result<Vec<Entry>, Error> {
        let mut out = Vec::new();
        for (_, _, k) in self.appends(workflow)? {
            if let Some((body, _)) = self.objects.get(&k)? {
                out.extend(serde_json::from_slice::<Vec<Entry>>(&body).map_err(|e| Error::Protocol(format!("{k}: {e}")))?);
            }
        }
        Ok(out)
    }

    fn lease(&self, workflow: &str, holder: &str, now: &str, until: &str) -> Result<bool, Error> {
        let k = format!("leases/{workflow}.json");
        let mine = Lease { holder: holder.into(), until: until.into() };
        let cond_version = match self.read::<Lease>(&k)? {
            None => None,
            Some((held, v)) => {
                if held.holder != holder && !held.holder.is_empty() && held.until.as_str() >= now {
                    return Ok(false);
                }
                Some(v)
            }
        };
        let cond = cond_version.as_deref().map_or(Cond::IfNoneMatch, Cond::IfMatch);
        Ok(self.write(&k, &mine, cond)?.is_some())
    }

    fn release(&self, workflow: &str, holder: &str) -> Result<(), Error> {
        let k = format!("leases/{workflow}.json");
        if let Some((held, v)) = self.read::<Lease>(&k)? {
            if held.holder == holder {
                // lost to another meanwhile: theirs now, nothing to do
                let _ = self.write(&k, &Lease { holder: String::new(), until: String::new() }, Cond::IfMatch(&v))?;
            }
        }
        Ok(())
    }
}

impl<O: Objects> OrderBook for ObjectStore<O> {
    fn publish_worker(&self, face: &WorkerFace) -> Result<(), Error> {
        self.write(&format!("workers/{}.json", face.name), face, Cond::None).map(|_| ())
    }

    fn workers(&self) -> Result<Vec<WorkerFace>, Error> {
        let mut out = Vec::new();
        for k in self.objects.list(&self.key("workers/"))? {
            if let Some((body, _)) = self.objects.get(&k)? {
                out.push(serde_json::from_slice(&body).map_err(|e| Error::Protocol(format!("{k}: {e}")))?);
            }
        }
        Ok(out)
    }

    fn place(&self, order: &Order) -> Result<(), Error> {
        let bare = Order {
            cancel: None,
            claim: None,
            report: None,
            ..order.clone()
        };
        if self.write(&self.order_doc(&order.id), &bare, Cond::IfNoneMatch)?.is_some() {
            self.objects.put(&self.key(&self.open_key(&order.id)), order.worker.as_deref().unwrap_or("").as_bytes(), Cond::None)?;
        }
        Ok(())
    }

    fn order(&self, id: &str) -> Result<Option<Order>, Error> {
        Ok(self.read::<Order>(&self.order_doc(id))?.map(|(o, _)| o))
    }

    fn open_orders(&self, worker: &str) -> Result<Vec<Order>, Error> {
        let mut out = Vec::new();
        for k in self.objects.list(&self.key("open/"))? {
            let Some((addressed, _)) = self.objects.get(&k)? else { continue };
            let addressed = String::from_utf8_lossy(&addressed);
            if !addressed.is_empty() && addressed != worker {
                continue;
            }
            let id = k.rsplit('/').next().unwrap_or_default().replace('~', "/");
            if let Some(o) = self.order(&id)?.filter(|o| !o.ended()) {
                out.push(o);
            }
        }
        out.sort_by(|a, b| (&a.placed, &a.id).cmp(&(&b.placed, &b.id)));
        Ok(out)
    }

    fn claim(&self, id: &str, expect: Option<&Claim>, claim: &Claim) -> Result<bool, Error> {
        self.change_order(id, |o| {
            if o.claim.as_ref() != expect {
                return false;
            }
            o.claim = Some(claim.clone());
            true
        })
    }

    fn report(&self, id: &str, worker: &str, report: &Report) -> Result<(), Error> {
        let Some(held) = self.order(id)? else { return Err(Error::Refused(format!("no order {id:?}"))) };
        if held.ended() {
            return Err(Error::Refused(format!("order {id} has ended")));
        }
        let changed = self.change_order(id, |o| {
            if o.claim.as_ref().map(|c| c.worker.as_str()) != Some(worker) {
                return false;
            }
            o.report = Some(report.clone());
            true
        })?;
        if changed {
            Ok(())
        } else if self.order(id)?.is_some_and(|o| o.ended()) {
            Err(Error::Refused(format!("order {id} has ended")))
        } else {
            Err(Error::Refused(format!("order {id} is not claimed by {worker}")))
        }
    }

    fn end(&self, id: &str, expect: Option<&Claim>, report: &Report) -> Result<bool, Error> {
        if report.outcome.is_none() {
            return Err(Error::Refused(format!("ending order {id} needs an outcome")));
        }
        self.change_order(id, |o| {
            if o.claim.as_ref() != expect {
                return false;
            }
            o.report = Some(report.clone());
            true
        })
    }

    fn ask_cancel(&self, id: &str, ask: &CancelAsk) -> Result<(), Error> {
        self.change_order(id, |o| {
            if o.cancel.is_some() {
                return false;
            }
            o.cancel = Some(ask.clone());
            true
        })
        .map(|_| ())
    }

    fn log(&self, id: &str) -> Result<String, Error> {
        let mut out = String::new();
        for (_, k) in self.chunks(id)? {
            if let Some((body, _)) = self.objects.get(&k)? {
                out.push_str(&String::from_utf8_lossy(&body));
            }
        }
        Ok(out)
    }

    fn append_log(&self, id: &str, worker: &str, at: u64, text: &str) -> Result<bool, Error> {
        let Some(o) = self.order(id)? else { return Ok(false) };
        if o.ended() || o.claim.as_ref().map(|c| c.worker.as_str()) != Some(worker) {
            return Ok(false);
        }
        let len = match self.chunks(id)?.last() {
            None => 0,
            Some((start, k)) => start + self.objects.get(k)?.map_or(0, |(b, _)| b.len() as u64),
        };
        if len != at {
            return Ok(false);
        }
        Ok(self.objects.put(&self.key(&format!("{}{at:012}", self.log_prefix(id))), text.as_bytes(), Cond::IfNoneMatch)?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_object_store_conforms() {
        crate::store::conformance::run(&ObjectStore::new(MemoryObjects::new(), "stepmeld"));
    }

    #[test]
    fn the_object_store_keeps_orders_as_the_book_says() {
        crate::orders::conformance::run(&ObjectStore::new(MemoryObjects::new(), "stepmeld/"));
        crate::orders::conformance::removing(&ObjectStore::new(MemoryObjects::new(), "stepmeld/"));
    }

    #[test]
    fn memory_objects_hold_their_conditions() {
        let o = MemoryObjects::new();
        let v1 = o.put("a", b"1", Cond::IfNoneMatch).unwrap().unwrap();
        assert!(o.put("a", b"2", Cond::IfNoneMatch).unwrap().is_none());
        let v2 = o.put("a", b"2", Cond::IfMatch(&v1)).unwrap().unwrap();
        assert!(o.put("a", b"3", Cond::IfMatch(&v1)).unwrap().is_none());
        assert_eq!(o.get("a").unwrap().unwrap(), (b"2".to_vec(), v2));
        o.put("b/c", b"", Cond::None).unwrap();
        assert_eq!(o.list("b/").unwrap(), ["b/c"]);
        o.delete("a").unwrap();
        o.delete("a").unwrap();
        assert!(o.get("a").unwrap().is_none());
    }
}
