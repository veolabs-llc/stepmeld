//! The loop at the edge: read, plan, act on Performers, apply, write.
//! Everything the core does not do.

use crate::definition::{Ref, WorkflowDefinition};
use crate::engine::{self, Command, Effect, Observation, StartRequest};
use crate::history::Entry;
use crate::library::Library;
use crate::performer::Performer;
use crate::store::StateStore;
use crate::workflow::{Actor, PerformerFace, Workflow, WorkflowStatus};
use crate::Error;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct Driver {
    pub store: Arc<dyn StateStore>,
    pub performers: Vec<Arc<dyn Performer>>,
    /// Who holds leases.
    pub holder: String,
}

impl Driver {
    pub fn new(store: Arc<dyn StateStore>, holder: &str) -> Driver {
        Driver {
            store,
            performers: Vec::new(),
            holder: holder.to_string(),
        }
    }

    pub fn with_performer(mut self, p: Arc<dyn Performer>) -> Driver {
        self.performers.push(p);
        self
    }

    /// The nested-workflow Performer over this driver's store: every
    /// recipe in the Library becomes a verb.
    pub fn with_nesting(self) -> Driver {
        let nested = Arc::new(Nested { store: self.store.clone() });
        self.with_performer(nested)
    }

    fn faces(&self) -> Vec<PerformerFace> {
        self.performers.iter().map(|p| p.describe()).collect()
    }

    fn performer(&self, name: &str) -> Option<&Arc<dyn Performer>> {
        self.performers.iter().find(|p| p.describe().name == name)
    }

    fn definition(&self, lib: &Library, wf: &Workflow) -> Result<WorkflowDefinition, Error> {
        lib.workflow(&wf.definition).cloned().ok_or_else(|| Error::Invalid(format!("{} is not in the library", wf.definition)))
    }

    pub fn create(&self, definition: &Ref, id: &str, label: Option<&str>, by: &Actor, now: &str) -> Result<Workflow, Error> {
        let lib = self.store.library()?;
        let def = lib.workflow(definition).ok_or_else(|| Error::Refused(format!("{definition} is not in the library")))?;
        let (wf, entries) = engine::create(&lib, def, id, label, by, now)?;
        self.store.put(&wf, None)?;
        self.store.append(&entries)?;
        Ok(wf)
    }

    /// Apply a Command and write the Workflow back over the version
    /// read; a lost race is refused, to be asked again.
    pub fn command(&self, id: &str, cmd: &Command, by: &Actor, now: &str) -> Result<Workflow, Error> {
        let lib = self.store.library()?;
        let (mut wf, version) = self.store.get(id)?.ok_or_else(|| Error::Refused(format!("no workflow {id:?}")))?;
        let def = self.definition(&lib, &wf)?;
        let entries = engine::apply(&lib, &def, &mut wf, cmd, by, now)?;
        self.store.put(&wf, Some(&version))?;
        self.store.append(&entries)?;
        Ok(wf)
    }

    /// Forget a Workflow and the children its Runs made (`<id>~…`): what
    /// a person asks once it is over. One with a Run still live, or
    /// held under another driver's lease, is refused in words; cancel
    /// it first, and tick. The Runs' files on the Performers' side stay
    /// where they are: the engine never owned them.
    pub fn remove(&self, id: &str, now: &str, lease_until: &str) -> Result<Vec<String>, Error> {
        let lib = self.store.library()?;
        let (wf, _) = self.store.get(id)?.ok_or_else(|| Error::Refused(format!("no workflow {id:?}")))?;
        let def = self.definition(&lib, &wf)?;
        if matches!(wf.status(&def), WorkflowStatus::Running) {
            return Err(Error::Refused(format!("{id} has a Run still live: cancel it first, and tick")));
        }
        if !self.store.lease(id, &self.holder, now, lease_until)? {
            return Err(Error::Refused(format!("{id} is held by another driver right now; ask again")));
        }
        let prefix = format!("{id}~");
        let mut gone: Vec<String> = self.store.list()?.into_iter().filter(|c| c.starts_with(&prefix)).collect();
        for child in &gone {
            self.store.remove(child)?;
        }
        self.store.remove(id)?;
        gone.push(id.to_string());
        Ok(gone)
    }

    /// Advance one Workflow as far as it goes now: mint and start what
    /// is ready, observe what runs, apply what came back, until nothing
    /// more happens. Under the lease; skipped when another holds it.
    pub fn tick(&self, id: &str, now: &str, lease_until: &str) -> Result<Option<Workflow>, Error> {
        if !self.store.lease(id, &self.holder, now, lease_until)? {
            return Ok(None);
        }
        let result = self.tick_held(id, now);
        self.store.release(id, &self.holder)?;
        result.map(Some)
    }

    fn tick_held(&self, id: &str, now: &str) -> Result<Workflow, Error> {
        let lib = self.store.library()?;
        let faces = self.faces();
        let mut rounds = 0;
        loop {
            let (mut wf, version) = self.store.get(id)?.ok_or_else(|| Error::Refused(format!("no workflow {id:?}")))?;
            let def = self.definition(&lib, &wf)?;
            let effects = engine::plan(&def, &wf, &faces);
            let mut entries: Vec<Entry> = engine::note_placements(&def, &mut wf, &faces, now);
            let mut moved = false;
            for effect in effects {
                match effect {
                    Effect::Mint { step } => {
                        let (request, minted) = engine::mint(&lib, &def, &mut wf, &step, &faces, now)?;
                        entries.extend(minted);
                        entries.extend(self.start(&def, &mut wf, &request, now)?);
                        moved = true;
                    }
                    Effect::Start { step, run } => {
                        let request = engine::request_for(&lib, &wf, &step, &run)?;
                        entries.extend(self.start(&def, &mut wf, &request, now)?);
                        moved = true;
                    }
                    Effect::Observe { step, run, performer, handle } => {
                        let obs = match self.performer(&performer) {
                            Some(p) => p.observe(&handle),
                            None => Observation::Unreachable {
                                reason: format!("no performer {performer:?} in this driver"),
                            },
                        };
                        let got = engine::observed(&def, &mut wf, &step, &run, &obs, now)?;
                        moved |= !got.is_empty();
                        entries.extend(got);
                    }
                    Effect::Cancel { step, run, performer, handle, reason } => {
                        if let Some(p) = self.performer(&performer) {
                            p.cancel(&handle, &reason);
                            let obs = p.observe(&handle);
                            let got = engine::observed(&def, &mut wf, &step, &run, &obs, now)?;
                            moved |= !got.is_empty();
                            entries.extend(got);
                        }
                    }
                }
            }
            if !entries.is_empty() {
                self.store.put(&wf, Some(&version))?;
                self.store.append(&entries)?;
            }
            rounds += 1;
            if !moved || rounds > 64 {
                return Ok(wf);
            }
        }
    }

    fn start(&self, def: &WorkflowDefinition, wf: &mut Workflow, request: &StartRequest, now: &str) -> Result<Vec<Entry>, Error> {
        let performer = wf.step(&request.step)?.run(&request.run).map(|r| r.performer.clone()).ok_or_else(|| Error::Refused(format!("no run {}", request.run)))?;
        let Some(p) = self.performer(&performer) else {
            return engine::refused_start(def, wf, &request.step, &request.run, &format!("no performer {performer:?} in this driver"), now);
        };
        match p.start(request) {
            Ok(handle) => engine::started(wf, &request.step, &request.run, &handle, now),
            Err(words) if words.starts_with("unreachable:") => Ok(Vec::new()),
            Err(words) => engine::refused_start(def, wf, &request.step, &request.run, &words, now),
        }
    }

    /// Every Workflow, once, children before parents so a parent sees
    /// what its child did this tick; a child created along the way is
    /// ticked too, so a parent and its new child advance in one call.
    pub fn tick_all(&self, now: &str, lease_until: &str) -> Result<BTreeMap<String, WorkflowStatus>, Error> {
        let lib = self.store.library()?;
        let mut out = BTreeMap::new();
        loop {
            let mut fresh: Vec<String> = self.store.list()?.into_iter().filter(|id| !out.contains_key(id)).collect();
            fresh.sort_by_key(|id| std::cmp::Reverse(id.matches('~').count()));
            if fresh.is_empty() {
                return Ok(out);
            }
            for id in fresh {
                match self.tick(&id, now, lease_until)? {
                    Some(wf) => {
                        let def = self.definition(&lib, &wf)?;
                        out.insert(id, wf.status(&def));
                    }
                    None => {
                        out.insert(id, WorkflowStatus::Running);
                    }
                }
            }
        }
    }
}

/// The nested-workflow Performer: `start` creates a child Workflow from
/// the recipe the verb names, `observe` reads the child's status, and
/// the child's Outputs map back by path. The child is advanced by
/// whoever ticks the store's Workflows.
pub struct Nested {
    pub store: Arc<dyn StateStore>,
}

impl Nested {
    fn child_id(request: &StartRequest) -> String {
        let tail: Vec<&str> = request.run.rsplit('/').take(2).collect();
        format!("{}~{}-{}", request.workflow, tail[1], tail[0])
    }
}

impl Performer for Nested {
    fn describe(&self) -> PerformerFace {
        let verbs = self.store.library().map(|l| l.workflows().map(|w| w.reference()).collect()).unwrap_or_default();
        PerformerFace {
            name: "nested".into(),
            locality: crate::definition::Locality::ThisMachine,
            verbs,
        }
    }

    fn start(&self, request: &StartRequest) -> Result<String, String> {
        let id = Self::child_id(request);
        if self.store.get(&id).map_err(|e| format!("unreachable: {e}"))?.is_some() {
            return Ok(id);
        }
        let lib = self.store.library().map_err(|e| format!("unreachable: {e}"))?;
        let def = lib.workflow(&request.definition).ok_or_else(|| format!("{} is not a recipe in the library", request.definition))?;
        let by = Actor::Run {
            workflow: request.workflow.clone(),
            step: request.step.clone(),
            run: request.run.clone(),
        };
        let now = "";
        let (mut wf, mut entries) = engine::create(&lib, def, &id, None, &by, now).map_err(|e| e.to_string())?;
        for (path, value) in request.inputs.iter().chain(request.parameters.iter()) {
            let Some((step, port)) = path.split_once('.') else { continue };
            let is_input = wf.steps.get(step).is_some_and(|s| s.inputs.contains_key(port));
            let cmd = if is_input {
                Command::Set {
                    step: step.into(),
                    input: Some(port.into()),
                    parameter: None,
                    value: Some(value.clone()),
                }
            } else {
                Command::Set {
                    step: step.into(),
                    input: None,
                    parameter: Some(port.into()),
                    value: Some(value.clone()),
                }
            };
            entries.extend(engine::apply(&lib, def, &mut wf, &cmd, &by, now).map_err(|e| e.to_string())?);
        }
        self.store.put(&wf, None).map_err(|e| e.to_string())?;
        self.store.append(&entries).map_err(|e| e.to_string())?;
        Ok(id)
    }

    fn observe(&self, handle: &str) -> Observation {
        let lib = match self.store.library() {
            Ok(l) => l,
            Err(e) => return Observation::Unreachable { reason: e.to_string() },
        };
        let (wf, _) = match self.store.get(handle) {
            Ok(Some(w)) => w,
            Ok(None) => return Observation::Absent,
            Err(e) => return Observation::Unreachable { reason: e.to_string() },
        };
        let Some(def) = lib.workflow(&wf.definition) else {
            return Observation::Unreachable {
                reason: format!("{} is not in the library", wf.definition),
            };
        };
        let statuses = wf.statuses(def);
        let done = statuses.values().filter(|s| s.is_done()).count();
        match wf.status(def) {
            WorkflowStatus::Succeeded => {
                let mut outputs = BTreeMap::new();
                for (name, step) in &wf.steps {
                    for (port, o) in &step.outputs {
                        if let Some(v) = &o.value {
                            outputs.insert(format!("{name}.{port}"), v.clone());
                        }
                    }
                }
                let mut outcome = crate::workflow::Outcome::succeeded(outputs);
                outcome.summary = Some(crate::workflow::Summary {
                    headline: format!("{} steps succeeded in workflow {}", done, wf.id),
                    details: serde_json::to_value(&statuses).ok(),
                });
                Observation::Ended { outcome }
            }
            WorkflowStatus::Canceled => Observation::Ended {
                outcome: crate::workflow::Outcome::canceled(&format!("workflow {} was canceled", wf.id)),
            },
            status => {
                let attention = matches!(status, WorkflowStatus::Waiting | WorkflowStatus::Failed | WorkflowStatus::Stale).then(|| {
                    let who: Vec<String> = statuses
                        .iter()
                        .filter(|(_, s)| !matches!(s, crate::workflow::StepStatus::NotReady | crate::workflow::StepStatus::Ready | crate::workflow::StepStatus::Running) && !s.is_done())
                        .map(|(n, s)| format!("{n} is {}", s.word()))
                        .collect();
                    format!("inside workflow {}: {}", wf.id, who.join(", "))
                });
                Observation::Running {
                    progress: Some(crate::workflow::Progress {
                        phase: None,
                        done: Some(done as u64),
                        total: Some(statuses.len() as u64),
                        unit: Some("steps".into()),
                        at: String::new(),
                    }),
                    attention,
                }
            }
        }
    }

    fn cancel(&self, handle: &str, reason: &str) {
        let Ok(lib) = self.store.library() else { return };
        let Ok(Some((mut wf, version))) = self.store.get(handle) else { return };
        let Some(def) = lib.workflow(&wf.definition).cloned() else { return };
        let by = Actor::policy("cancel-from-parent", "");
        if let Ok(entries) = engine::apply(
            &lib,
            &def,
            &mut wf,
            &Command::Cancel {
                step: None,
                reason: Some(reason.to_string()),
            },
            &by,
            "",
        ) {
            let _ = self.store.put(&wf, Some(&version));
            let _ = self.store.append(&entries);
        }
    }
}
