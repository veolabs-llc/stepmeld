//! The StateStore seam: the little persistence the engine needs.
//! "Could not be asked" is [`Error::Protocol`]... no: it is a store's
//! own failure, [`Error::Refused`] is a condition that did not hold.

use crate::definition::{Ref, StepDefinition, WorkflowDefinition};
use crate::history::Entry;
use crate::library::Library;
use crate::workflow::Workflow;
use crate::Error;
use std::collections::BTreeMap;
use std::sync::Mutex;

pub trait StateStore: Send + Sync {
    fn describe(&self) -> String;
    // ---- the Library ----
    fn put_step(&self, def: &StepDefinition) -> Result<(), Error>;
    fn put_workflow_definition(&self, def: &WorkflowDefinition) -> Result<(), Error>;
    /// Everything defined, as a Library.
    fn library(&self) -> Result<Library, Error>;
    // ---- Workflows ----
    /// A Workflow and the version it was read at.
    fn get(&self, id: &str) -> Result<Option<(Workflow, String)>, Error>;
    /// Write over `expected` (None: create once). A condition that did
    /// not hold is [`Error::Refused`] with "changed underneath" or
    /// "already exists" in it. The version now held.
    fn put(&self, wf: &Workflow, expected: Option<&str>) -> Result<String, Error>;
    fn list(&self) -> Result<Vec<String>, Error>;
    // ---- History ----
    /// Append, assigning each entry the next `seq` of its workflow.
    fn append(&self, entries: &[Entry]) -> Result<(), Error>;
    fn history(&self, workflow: &str) -> Result<Vec<Entry>, Error>;
    // ---- the lease: one driver advances a Workflow at a time ----
    /// Take the lease when free, lapsed (`until` < `now`) or already
    /// this holder's; whether it was taken.
    fn lease(&self, workflow: &str, holder: &str, now: &str, until: &str) -> Result<bool, Error>;
    fn release(&self, workflow: &str, holder: &str) -> Result<(), Error>;
}

/// A store held in memory: the tests'.
#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<MemoryInner>,
}

#[derive(Default)]
struct MemoryInner {
    steps: BTreeMap<Ref, StepDefinition>,
    workflows_defs: BTreeMap<Ref, WorkflowDefinition>,
    workflows: BTreeMap<String, (Workflow, u64)>,
    history: BTreeMap<String, Vec<Entry>>,
    leases: BTreeMap<String, (String, String)>,
}

impl MemoryStore {
    pub fn new() -> MemoryStore {
        MemoryStore::default()
    }
}

impl StateStore for MemoryStore {
    fn describe(&self) -> String {
        "memory".into()
    }

    fn put_step(&self, def: &StepDefinition) -> Result<(), Error> {
        def.check()?;
        self.inner.lock().unwrap().steps.insert(def.reference(), def.clone());
        Ok(())
    }

    fn put_workflow_definition(&self, def: &WorkflowDefinition) -> Result<(), Error> {
        self.library()?.check_workflow(def)?;
        self.inner.lock().unwrap().workflows_defs.insert(def.reference(), def.clone());
        Ok(())
    }

    fn library(&self) -> Result<Library, Error> {
        let inner = self.inner.lock().unwrap();
        Library::from_parts(inner.steps.values().cloned(), inner.workflows_defs.values().cloned())
    }

    fn get(&self, id: &str) -> Result<Option<(Workflow, String)>, Error> {
        Ok(self.inner.lock().unwrap().workflows.get(id).map(|(w, v)| (w.clone(), v.to_string())))
    }

    fn put(&self, wf: &Workflow, expected: Option<&str>) -> Result<String, Error> {
        let mut inner = self.inner.lock().unwrap();
        let next = match (inner.workflows.get(&wf.id), expected) {
            (Some(_), None) => return Err(Error::Refused(format!("workflow {} already exists", wf.id))),
            (None, Some(_)) => return Err(Error::Refused(format!("workflow {} changed underneath: it is gone", wf.id))),
            (None, None) => 1,
            (Some((_, v)), Some(e)) if v.to_string() == e => v + 1,
            (Some(_), Some(_)) => return Err(Error::Refused(format!("workflow {} changed underneath", wf.id))),
        };
        inner.workflows.insert(wf.id.clone(), (wf.clone(), next));
        Ok(next.to_string())
    }

    fn list(&self) -> Result<Vec<String>, Error> {
        Ok(self.inner.lock().unwrap().workflows.keys().cloned().collect())
    }

    fn append(&self, entries: &[Entry]) -> Result<(), Error> {
        let mut inner = self.inner.lock().unwrap();
        for e in entries {
            let log = inner.history.entry(e.workflow.clone()).or_default();
            let mut e = e.clone();
            e.seq = log.len() as u64 + 1;
            log.push(e);
        }
        Ok(())
    }

    fn history(&self, workflow: &str) -> Result<Vec<Entry>, Error> {
        Ok(self.inner.lock().unwrap().history.get(workflow).cloned().unwrap_or_default())
    }

    fn lease(&self, workflow: &str, holder: &str, now: &str, until: &str) -> Result<bool, Error> {
        let mut inner = self.inner.lock().unwrap();
        let free = match inner.leases.get(workflow) {
            None => true,
            Some((h, _)) if h == holder => true,
            Some((_, u)) => u.as_str() < now,
        };
        if free {
            inner.leases.insert(workflow.to_string(), (holder.to_string(), until.to_string()));
        }
        Ok(free)
    }

    fn release(&self, workflow: &str, holder: &str) -> Result<(), Error> {
        let mut inner = self.inner.lock().unwrap();
        if inner.leases.get(workflow).is_some_and(|(h, _)| h == holder) {
            inner.leases.remove(workflow);
        }
        Ok(())
    }
}

/// The StateStore contract as tests.
pub mod conformance {
    use super::*;
    use crate::workflow::{Actor, Stamp};

    fn workflow(id: &str) -> Workflow {
        Workflow {
            contract: crate::workflow::WORKFLOW.into(),
            id: id.into(),
            definition: Ref::new("r", 1),
            label: None,
            created: Stamp {
                at: "2026-10-06T00:00:00Z".into(),
                by: Actor::person("t"),
            },
            steps: BTreeMap::new(),
            canceled: None,
        }
    }

    pub fn run(store: &dyn StateStore) {
        let id = "conformance-wf";
        assert!(store.get(id).unwrap().is_none(), "absent is None");
        let v1 = store.put(&workflow(id), None).unwrap();
        assert!(matches!(store.put(&workflow(id), None), Err(Error::Refused(w)) if w.contains("already exists")), "create once");
        let (_, read) = store.get(id).unwrap().unwrap();
        assert_eq!(read, v1, "get hands back the version put");
        let v2 = store.put(&workflow(id), Some(&v1)).unwrap();
        assert_ne!(v1, v2);
        assert!(matches!(store.put(&workflow(id), Some(&v1)), Err(Error::Refused(w)) if w.contains("changed underneath")), "a stale version is refused");
        assert!(store.list().unwrap().contains(&id.to_string()));
        let by = Actor::person("t");
        store.append(&[Entry::new("2026-10-06T00:00:01Z", &by, id, "a"), Entry::new("2026-10-06T00:00:02Z", &by, id, "b")]).unwrap();
        store.append(&[Entry::new("2026-10-06T00:00:03Z", &by, id, "c")]).unwrap();
        let seqs: Vec<u64> = store.history(id).unwrap().iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3], "seq counts up per workflow");
        assert!(store.lease(id, "a", "2026-10-06T00:00:00Z", "2026-10-06T00:01:00Z").unwrap(), "a free lease is taken");
        assert!(!store.lease(id, "b", "2026-10-06T00:00:30Z", "2026-10-06T00:01:30Z").unwrap(), "a held lease is not");
        assert!(store.lease(id, "a", "2026-10-06T00:00:30Z", "2026-10-06T00:01:30Z").unwrap(), "the holder renews");
        assert!(store.lease(id, "b", "2026-10-06T00:02:00Z", "2026-10-06T00:03:00Z").unwrap(), "a lapsed lease is taken");
        store.release(id, "a").unwrap();
        assert!(!store.lease(id, "a", "2026-10-06T00:02:10Z", "2026-10-06T00:03:00Z").unwrap(), "releasing another's lease does nothing");
        store.release(id, "b").unwrap();
        assert!(store.lease(id, "a", "2026-10-06T00:02:20Z", "2026-10-06T00:03:00Z").unwrap(), "a released lease is free");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_memory_store_conforms() {
        super::conformance::run(&super::MemoryStore::new());
    }
}
