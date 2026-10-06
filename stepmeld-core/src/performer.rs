//! The Performer seam: what does a Step's work. Every call is short;
//! the Performer is asked repeatedly.

use crate::engine::{Observation, StartRequest};
use crate::workflow::{Outcome, PerformerFace, Progress};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// What a Run would cost, asking nothing and changing nothing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Estimate {
    pub seconds: Option<f64>,
    pub cost: Option<String>,
    pub note: Option<String>,
}

pub trait Performer: Send + Sync {
    fn describe(&self) -> PerformerFace;
    /// Accept and return a handle, or refuse in words before doing
    /// anything. Asked twice for the same Run id, returns the same
    /// handle.
    fn start(&self, request: &StartRequest) -> Result<String, String>;
    fn observe(&self, handle: &str) -> Observation;
    /// Canceling work that already ended is not an error.
    fn cancel(&self, handle: &str, reason: &str);
    fn log(&self, _handle: &str) -> Option<String> {
        None
    }
    fn estimate(&self, _request: &StartRequest) -> Option<Estimate> {
        None
    }
}

/// A Performer held in memory: the tests'. It accepts every Run of the
/// verbs it offers and ends them when told to.
pub struct MemoryPerformer {
    face: PerformerFace,
    inner: Mutex<MemoryInner>,
}

#[derive(Default)]
struct MemoryInner {
    /// run id -> handle
    handles: BTreeMap<String, String>,
    runs: BTreeMap<String, MemoryRun>,
    unreachable: Option<String>,
    refuse: Option<String>,
    next: u64,
}

#[derive(Debug, Clone)]
pub struct MemoryRun {
    pub request: StartRequest,
    pub progress: Option<Progress>,
    pub attention: Option<String>,
    pub outcome: Option<Outcome>,
    pub canceled: Option<String>,
    pub log: String,
}

impl MemoryPerformer {
    pub fn new(face: PerformerFace) -> MemoryPerformer {
        MemoryPerformer {
            face,
            inner: Mutex::new(MemoryInner::default()),
        }
    }

    /// The Runs it was asked to start, in order.
    pub fn started(&self) -> Vec<StartRequest> {
        let inner = self.inner.lock().unwrap();
        inner.handles.values().filter_map(|h| inner.runs.get(h)).map(|r| r.request.clone()).collect()
    }

    pub fn handle_of(&self, run: &str) -> Option<String> {
        self.inner.lock().unwrap().handles.get(run).cloned()
    }

    pub fn run(&self, handle: &str) -> Option<MemoryRun> {
        self.inner.lock().unwrap().runs.get(handle).cloned()
    }

    pub fn progress(&self, handle: &str, progress: Progress) {
        if let Some(r) = self.inner.lock().unwrap().runs.get_mut(handle) {
            r.progress = Some(progress);
        }
    }

    pub fn attention(&self, handle: &str, why: Option<&str>) {
        if let Some(r) = self.inner.lock().unwrap().runs.get_mut(handle) {
            r.attention = why.map(str::to_string);
        }
    }

    /// End a Run.
    pub fn finish(&self, handle: &str, outcome: Outcome) {
        if let Some(r) = self.inner.lock().unwrap().runs.get_mut(handle) {
            r.outcome = Some(outcome);
        }
    }

    /// Forget a Run: observing it answers absent.
    pub fn forget(&self, handle: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.runs.remove(handle);
        inner.handles.retain(|_, h| h != handle);
    }

    /// Answer unreachable to everything until told otherwise.
    pub fn set_unreachable(&self, why: Option<&str>) {
        self.inner.lock().unwrap().unreachable = why.map(str::to_string);
    }

    /// Refuse every start with these words until told otherwise.
    pub fn set_refusal(&self, why: Option<&str>) {
        self.inner.lock().unwrap().refuse = why.map(str::to_string);
    }
}

impl Performer for MemoryPerformer {
    fn describe(&self) -> PerformerFace {
        self.face.clone()
    }

    fn start(&self, request: &StartRequest) -> Result<String, String> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(why) = &inner.unreachable {
            return Err(format!("unreachable: {why}"));
        }
        if let Some(h) = inner.handles.get(&request.run) {
            return Ok(h.clone());
        }
        if let Some(why) = &inner.refuse {
            return Err(why.clone());
        }
        if !self.face.verbs.contains(&request.definition) {
            return Err(format!("{} does not perform {}", self.face.name, request.definition));
        }
        inner.next += 1;
        let handle = format!("{}-{}", self.face.name, inner.next);
        inner.handles.insert(request.run.clone(), handle.clone());
        inner.runs.insert(
            handle.clone(),
            MemoryRun {
                request: request.clone(),
                progress: None,
                attention: None,
                outcome: None,
                canceled: None,
                log: String::new(),
            },
        );
        Ok(handle)
    }

    fn observe(&self, handle: &str) -> Observation {
        let inner = self.inner.lock().unwrap();
        if let Some(why) = &inner.unreachable {
            return Observation::Unreachable { reason: why.clone() };
        }
        match inner.runs.get(handle) {
            None => Observation::Absent,
            Some(r) => match &r.outcome {
                Some(o) => Observation::Ended { outcome: o.clone() },
                None => Observation::Running {
                    progress: r.progress.clone(),
                    attention: r.attention.clone(),
                },
            },
        }
    }

    fn cancel(&self, handle: &str, reason: &str) {
        if let Some(r) = self.inner.lock().unwrap().runs.get_mut(handle) {
            r.canceled = Some(reason.to_string());
            if r.outcome.is_none() {
                r.outcome = Some(Outcome::canceled(reason));
            }
        }
    }

    fn log(&self, handle: &str) -> Option<String> {
        self.inner.lock().unwrap().runs.get(handle).map(|r| r.log.clone())
    }
}

/// The Performer contract as tests. An implementation's test module
/// calls this with a way to end a Run (`finish`) and a request it will
/// accept.
pub mod conformance {
    use super::*;
    use crate::workflow::RunState;

    pub fn run(p: &dyn Performer, request: &StartRequest, finish: &dyn Fn(&str)) {
        let face = p.describe();
        assert!(!face.name.is_empty(), "a performer has a name");
        assert!(face.verbs.contains(&request.definition), "the performer offers the request's verb");
        assert!(matches!(p.observe("no-such-handle"), Observation::Absent | Observation::Unreachable { .. }), "an unknown handle is absent, never a run");
        let h1 = p.start(request).expect("the request is accepted");
        let h2 = p.start(request).expect("asked again, still accepted");
        assert_eq!(h1, h2, "the same run gets the same handle");
        assert!(matches!(p.observe(&h1), Observation::Running { .. }), "a started run is running");
        finish(&h1);
        match p.observe(&h1) {
            Observation::Ended { outcome } => assert!(matches!(outcome.state, RunState::Succeeded | RunState::Failed | RunState::Canceled | RunState::Lost)),
            other => panic!("after finishing, the run has ended: {other:?}"),
        }
        p.cancel(&h1, "conformance");
        assert!(matches!(p.observe(&h1), Observation::Ended { .. }), "canceling an ended run changes nothing");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::{Locality, Ref};

    #[test]
    fn the_memory_performer_conforms() {
        let p = MemoryPerformer::new(PerformerFace {
            name: "mem".into(),
            locality: Locality::ThisMachine,
            verbs: vec![Ref::new("verb", 1)],
        });
        let request = StartRequest {
            workflow: "wf".into(),
            step: "s".into(),
            run: "wf/s/1".into(),
            definition: Ref::new("verb", 1),
            inputs: BTreeMap::new(),
            parameters: BTreeMap::new(),
            outputs: Vec::new(),
        };
        conformance::run(&p, &request, &|h| p.finish(h, Outcome::succeeded(BTreeMap::new())));
    }
}
