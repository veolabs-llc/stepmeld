//! The loop at the edge: read, plan, act on Performers, apply, write.
//! Everything the core does not do.

use crate::definition::{Ref, WorkflowDefinition};
use crate::engine::{self, Command, Effect, Observation, StartRequest};
use crate::history::Entry;
use crate::performer::Performer;
use crate::store::StateStore;
use crate::workflow::{Actor, PerformerFace, Workflow, WorkflowStatus};
use crate::Error;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct Driver {
    pub store: Arc<dyn StateStore>,
    pub performers: Vec<Arc<dyn Performer>>,
    /// Performers other drivers of this store serve, by their faces:
    /// what this driver may place a Run on and must then leave alone.
    /// Names are one namespace across the drivers of a store.
    pub elsewhere: Vec<PerformerFace>,
    /// Who holds leases.
    pub holder: String,
}

impl Driver {
    pub fn new(store: Arc<dyn StateStore>, holder: &str) -> Driver {
        Driver {
            store,
            performers: Vec::new(),
            elsewhere: Vec::new(),
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

    /// Tell the driver of the Performers other drivers of its store
    /// serve. Several drivers may tick one store, each with its own
    /// Performers: a Gate and a placement are judged over all of them,
    /// and a Run is started, observed and canceled only by a driver
    /// that has its Performer. A face named as one of this driver's own
    /// is ignored.
    pub fn with_elsewhere(mut self, faces: Vec<PerformerFace>) -> Driver {
        self.elsewhere = faces;
        self
    }

    /// Every Performer a Step may be placed on: this driver's, then the
    /// ones elsewhere.
    fn faces(&self) -> Vec<PerformerFace> {
        let mut faces: Vec<PerformerFace> = self.performers.iter().map(|p| p.describe()).collect();
        for face in &self.elsewhere {
            if !faces.iter().any(|f| f.name == face.name) {
                faces.push(face.clone());
            }
        }
        faces
    }

    /// Whether another driver serves the Performer of this name.
    fn is_elsewhere(&self, name: &str) -> bool {
        self.performer(name).is_none() && self.elsewhere.iter().any(|f| f.name == name)
    }

    fn performer(&self, name: &str) -> Option<&Arc<dyn Performer>> {
        self.performers.iter().find(|p| p.describe().name == name)
    }

    /// The Workflow's own WorkflowDefinition, as the engine takes it.
    fn definition(&self, wf: &Workflow) -> WorkflowDefinition {
        wf.workflow_definition.clone()
    }

    /// Bring a Workflow made before each carried its own
    /// WorkflowDefinition to this shape ([`Workflow::adopt`]), written
    /// over the version read and noted in its History. One already in
    /// this shape is refused in words.
    pub fn adopt(&self, id: &str, by: &Actor, now: &str) -> Result<Workflow, Error> {
        let lib = self.store.library()?;
        let (doc, version) = self.store.document(id)?.ok_or_else(|| Error::Refused(format!("no workflow {id:?}")))?;
        let wf = Workflow::adopt(&doc, &lib)?;
        self.store.put(&wf, Some(&version))?;
        self.store.append(&[Entry::new(now, by, id, "adopted").detail(serde_json::json!({"definition": wf.from}))])?;
        Ok(wf)
    }

    /// A root Workflow, created without its go: no Step runs until
    /// `Command::StartWorkflow`.
    pub fn create(&self, definition: &Ref, id: &str, label: Option<&str>, by: &Actor, now: &str) -> Result<Workflow, Error> {
        let lib = self.store.library()?;
        let def = lib.workflow(definition).ok_or_else(|| Error::Refused(format!("{definition} is not in the library")))?;
        let (wf, entries) = engine::create(&lib, def, id, label, by, now, false)?;
        self.store.put(&wf, None)?;
        self.store.append(&entries)?;
        Ok(wf)
    }

    /// Apply a Command and write the Workflow back over the version
    /// read; a lost race is refused, to be asked again.
    pub fn command(&self, id: &str, cmd: &Command, by: &Actor, now: &str) -> Result<Workflow, Error> {
        let lib = self.store.library()?;
        let (mut wf, version) = self.store.get(id)?.ok_or_else(|| Error::Refused(format!("no workflow {id:?}")))?;
        let def = self.definition(&wf);
        let entries = engine::apply(&lib, &def, &mut wf, cmd, by, now)?;
        self.store.put(&wf, Some(&version))?;
        self.store.append(&entries)?;
        Ok(wf)
    }

    /// Forget a Workflow and the children its Runs made (`<id>~…`): what
    /// a person asks once it is over. One with a Run still live, or
    /// held under another driver's lease, is refused in words; cancel
    /// it first, and tick. The orders its Runs placed and their logs go
    /// with it (the store's [`StateStore::remove`]); the Runs' files on
    /// the Performers' side stay where they are: the engine never owned
    /// them.
    pub fn remove(&self, id: &str, now: &str, lease_until: &str) -> Result<Vec<String>, Error> {
        let (wf, _) = self.store.get(id)?.ok_or_else(|| Error::Refused(format!("no workflow {id:?}")))?;
        let def = self.definition(&wf);
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
            let def = self.definition(&wf);
            let effects = engine::plan(&def, &wf, &faces);
            let mut entries: Vec<Entry> = engine::note_placements(&def, &mut wf, &faces, now);
            let noted = engine::note_offers(&def, &mut wf, &faces);
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
                        // a start that changed nothing (its Performer is
                        // another driver's, or unreachable) is not a move
                        let started = self.start(&def, &mut wf, &request, now)?;
                        moved |= !started.is_empty();
                        entries.extend(started);
                    }
                    Effect::Observe { step, run, performer, handle } => {
                        if self.is_elsewhere(&performer) {
                            continue; // its driver observes it
                        }
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
            if !entries.is_empty() || noted {
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
        if self.is_elsewhere(&performer) {
            return Ok(Vec::new()); // minted here, started by the driver that has it
        }
        let Some(p) = self.performer(&performer) else {
            return engine::refused_start(def, wf, &request.step, &request.run, &format!("no performer {performer:?} in this driver"), now);
        };
        match p.start(request) {
            Ok(handle) => engine::started(wf, &request.step, &request.run, &handle, now),
            Err(words) if words.starts_with("unreachable:") => Ok(Vec::new()),
            Err(words) => engine::refused_start(def, wf, &request.step, &request.run, &words, now),
        }
    }

    /// Every Workflow that may have moved since `seen` was last brought
    /// up to date, once, children before parents: one whose version
    /// changed since the last listing (a Command, a Worker's report
    /// applied elsewhere, or this Orchestrator's own last tick), one
    /// with a Run live or a Step ready, and every one when the Workers
    /// on offer changed (a Worker came, went away, or came back). A
    /// Workflow that is waiting for a person, or finished, costs one
    /// listing. Every Workflow's status, as last seen (docs/PLACEMENT.md
    /// §10, phase 5).
    pub fn tick_moved(&self, now: &str, lease_until: &str, seen: &mut Seen) -> Result<BTreeMap<String, WorkflowStatus>, Error> {
        let offers: Vec<(String, Vec<Ref>)> = self.faces().into_iter().map(|f| (f.name, f.verbs)).collect();
        if offers != seen.offers {
            seen.settled.clear();
            seen.offers = offers;
        }
        seen.ticked.clear();
        let listing = self.store.listing()?;
        seen.statuses.retain(|id, _| listing.contains_key(id));
        seen.versions.retain(|id, _| listing.contains_key(id));
        seen.settled.retain(|id| listing.contains_key(id));
        let mut ids: Vec<&String> = listing.keys().collect();
        ids.sort_by_key(|id| std::cmp::Reverse(id.matches('~').count()));
        for id in ids {
            let unchanged = seen.versions.get(id) == listing.get(id);
            if unchanged && seen.settled.contains(id) {
                continue;
            }
            seen.ticked.push(id.clone());
            let Some(wf) = self.tick(id, now, lease_until)? else {
                seen.statuses.insert(id.clone(), WorkflowStatus::Running);
                continue; // another Orchestrator holds it
            };
            let status = wf.status(&self.definition(&wf));
            // the listing's word for it, not a read's: a store may list
            // a different stamp (a modified time) than it reads (an
            // ETag). What this tick wrote is ticked once more next time.
            if let Some(v) = listing.get(id) {
                seen.versions.insert(id.clone(), v.clone());
            }
            if matches!(status, WorkflowStatus::Running | WorkflowStatus::Ready) {
                seen.settled.remove(id);
            } else {
                seen.settled.insert(id.clone());
            }
            seen.statuses.insert(id.clone(), status);
        }
        Ok(seen.statuses.clone())
    }

    /// Every Workflow, once, children before parents so a parent sees
    /// what its child did this tick; a child created along the way is
    /// ticked too, so a parent and its new child advance in one call.
    pub fn tick_all(&self, now: &str, lease_until: &str) -> Result<BTreeMap<String, WorkflowStatus>, Error> {
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
                        let def = self.definition(&wf);
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

/// What an Orchestrator saw on its last [`Driver::tick_moved`]: each
/// Workflow's version and status, which of them have nothing to observe
/// until they change, and the Workers on offer.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    versions: BTreeMap<String, String>,
    statuses: BTreeMap<String, WorkflowStatus>,
    settled: std::collections::BTreeSet<String>,
    offers: Vec<(String, Vec<Ref>)>,
    /// The Workflows the last call ticked.
    pub ticked: Vec<String>,
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
            pool: false,
            nests: true,
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
        // the parent's own copy of the nested definition, so the child
        // is what the parent says, whatever the Library holds now
        let (parent, _) = self.store.get(&request.workflow).map_err(|e| format!("unreachable: {e}"))?.ok_or_else(|| format!("no workflow {:?}", request.workflow))?;
        let def = parent
            .workflow_definition
            .entry(&request.step)
            .and_then(|e| e.definition.clone())
            .ok_or_else(|| format!("step {:?} of {} carries no WorkflowDefinition to make a child of", request.step, request.workflow))?;
        let def = &def;
        let by = Actor::Run {
            workflow: request.workflow.clone(),
            step: request.step.clone(),
            run: request.run.clone(),
        };
        let now = "";
        // a nested Workflow is its parent's Run: it has its go already
        let (mut wf, mut entries) = engine::create(&lib, def, &id, None, &by, now, true).map_err(|e| e.to_string())?;
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
        let (wf, _) = match self.store.get(handle) {
            Ok(Some(w)) => w,
            Ok(None) => return Observation::Absent,
            Err(e) => return Observation::Unreachable { reason: e.to_string() },
        };
        let def = &wf.workflow_definition;
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
        let def = wf.workflow_definition.clone();
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
