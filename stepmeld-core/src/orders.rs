//! Orchestrators and Workers (docs/ORCHESTRATOR-WORKER.md): an
//! Orchestrator reaches a Worker by placing an order in the shared
//! store; the Worker runs it through its own mechanism and reports on
//! it. A Worker never writes a Workflow.
//!
//! - [`OrderBook`]: the shared store's orders and published Workers,
//!   with [`crate::store::MemoryStore`] implementing it for tests and a
//!   conformance suite beside the trait.
//! - [`OrderPerformer`]: the Orchestrator's Performer for one published
//!   Worker: `start` places an order, `observe` reads its report.
//! - [`Worker`]: the loop a Worker runs: publish itself, take its open
//!   orders, run each through a mechanism (any [`Performer`]), report.

use crate::definition::{Locality, Ref};
use crate::engine::{Observation, StartRequest};
use crate::performer::Performer;
use crate::workflow::{Outcome, PerformerFace, Progress};
use crate::Error;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const ORDER: &str = "stepmeld/order.v1";
pub const WORKER: &str = "stepmeld/worker.v1";

/// A Worker as every Orchestrator sees it: published by the Worker,
/// replaced each time it publishes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerFace {
    pub contract: String,
    pub name: String,
    /// The machine it runs on, as the machine names itself.
    pub host: String,
    /// Its work happens in the cloud (it launches work there).
    #[serde(default)]
    pub cloud: bool,
    pub verbs: Vec<Ref>,
    /// When it last published itself.
    pub seen: String,
}

impl WorkerFace {
    /// The Performer an Orchestrator on `here` (a host name) places
    /// Runs on for this Worker. Locality comes from the Worker: a cloud
    /// Worker is `cloud`, one on the Orchestrator's own host
    /// `this-machine`, any other `local-network`.
    pub fn performer_face(&self, here: &str) -> PerformerFace {
        let locality = if self.cloud {
            Locality::Cloud
        } else if self.host == here {
            Locality::ThisMachine
        } else {
            Locality::LocalNetwork
        };
        PerformerFace {
            name: self.name.clone(),
            locality,
            verbs: self.verbs.clone(),
        }
    }
}

/// A cancel asked for on an order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CancelAsk {
    pub reason: String,
    pub at: String,
}

/// A Worker's latest word on an order: progress while it runs, the
/// outcome once it ends (final), and the log so far.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

/// One Run's work, addressed to one Worker. The Orchestrator writes
/// the request and any cancel; the Worker writes the report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub contract: String,
    /// The Run's id: one order per Run.
    pub id: String,
    pub worker: String,
    pub placed: String,
    pub request: StartRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<CancelAsk>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<Report>,
}

impl Order {
    pub fn ended(&self) -> bool {
        self.report.as_ref().is_some_and(|r| r.outcome.is_some())
    }
}

/// The shared store's side of orders and Workers.
pub trait OrderBook: Send + Sync {
    /// Publish a Worker, replacing what it published before.
    fn publish_worker(&self, face: &WorkerFace) -> Result<(), Error>;
    fn workers(&self) -> Result<Vec<WorkerFace>, Error>;
    /// Place an order once; placing the same id again changes nothing.
    fn place(&self, order: &Order) -> Result<(), Error>;
    fn order(&self, id: &str) -> Result<Option<Order>, Error>;
    /// A Worker's orders that have not ended, oldest first.
    fn open_orders(&self, worker: &str) -> Result<Vec<Order>, Error>;
    /// The Worker's latest word. A report on an order that has ended is
    /// refused: an outcome is final.
    fn report(&self, id: &str, report: &Report) -> Result<(), Error>;
    /// Ask for a cancel; the first ask stands, and an ended order is
    /// left as it is.
    fn ask_cancel(&self, id: &str, ask: &CancelAsk) -> Result<(), Error>;
}

/// The time, as stepmeld spells it, for the edge pieces that need one.
pub type Clock = Arc<dyn Fn() -> String + Send + Sync>;

/// The wall clock.
pub fn wall_clock() -> Clock {
    Arc::new(|| crate::iso(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())))
}

/// The Orchestrator's Performer for one Worker: a Run is an order
/// addressed to it, and what the Worker reports is the observation.
pub struct OrderPerformer {
    book: Arc<dyn OrderBook>,
    face: PerformerFace,
    clock: Clock,
}

impl OrderPerformer {
    pub fn new(book: Arc<dyn OrderBook>, face: PerformerFace, clock: Clock) -> OrderPerformer {
        OrderPerformer { book, face, clock }
    }

    /// One Performer per published Worker, as seen from `here`.
    pub fn for_workers(book: Arc<dyn OrderBook>, here: &str, clock: Clock) -> Result<Vec<OrderPerformer>, Error> {
        Ok(book.workers()?.iter().map(|w| OrderPerformer::new(book.clone(), w.performer_face(here), clock.clone())).collect())
    }
}

impl Performer for OrderPerformer {
    fn describe(&self) -> PerformerFace {
        self.face.clone()
    }

    fn start(&self, request: &StartRequest) -> Result<String, String> {
        let order = Order {
            contract: ORDER.into(),
            id: request.run.clone(),
            worker: self.face.name.clone(),
            placed: (self.clock)(),
            request: request.clone(),
            cancel: None,
            report: None,
        };
        self.book.place(&order).map_err(|e| format!("the order could not be placed: {e}"))?;
        Ok(order.id)
    }

    fn observe(&self, handle: &str) -> Observation {
        match self.book.order(handle) {
            Err(e) => Observation::Unreachable { reason: e.to_string() },
            Ok(None) => Observation::Absent,
            Ok(Some(order)) => match order.report {
                Some(Report { outcome: Some(outcome), .. }) => Observation::Ended { outcome },
                Some(Report { progress, .. }) => Observation::Running { progress, attention: None },
                // placed, not yet taken by the Worker
                None => Observation::Running { progress: None, attention: None },
            },
        }
    }

    fn cancel(&self, handle: &str, reason: &str) {
        let _ = self.book.ask_cancel(handle, &CancelAsk { reason: reason.into(), at: (self.clock)() });
    }

    fn log(&self, handle: &str) -> Option<String> {
        self.book.order(handle).ok()??.report?.log
    }
}

/// A Worker: takes the orders addressed to it from the shared store and
/// runs them through its mechanism, reporting on each.
pub struct Worker {
    pub name: String,
    pub host: String,
    pub cloud: bool,
    book: Arc<dyn OrderBook>,
    mechanism: Arc<dyn Performer>,
}

impl Worker {
    pub fn new(name: &str, host: &str, cloud: bool, book: Arc<dyn OrderBook>, mechanism: Arc<dyn Performer>) -> Worker {
        Worker {
            name: name.into(),
            host: host.into(),
            cloud,
            book,
            mechanism,
        }
    }

    /// The face this Worker publishes: the verbs its mechanism offers.
    pub fn face(&self, now: &str) -> WorkerFace {
        WorkerFace {
            contract: WORKER.into(),
            name: self.name.clone(),
            host: self.host.clone(),
            cloud: self.cloud,
            verbs: self.mechanism.describe().verbs,
            seen: now.into(),
        }
    }

    /// Publish this Worker, then carry every open order one step on:
    /// start it (the mechanism's start is idempotent), pass on a cancel,
    /// observe, report. The orders that changed, as (id, word).
    pub fn tick(&self, now: &str) -> Result<Vec<(String, String)>, Error> {
        self.book.publish_worker(&self.face(now))?;
        let mut moved = Vec::new();
        for order in self.book.open_orders(&self.name)? {
            let before = order.report.clone();
            let report = self.carry(&order, now);
            let Some(report) = report else { continue };
            let word = match &report.outcome {
                Some(o) => format!("{:?}", o.state).to_lowercase(),
                None => "running".into(),
            };
            let changed = before.as_ref().map(|b| (&b.progress, &b.outcome, &b.log)) != Some((&report.progress, &report.outcome, &report.log));
            if changed {
                self.book.report(&order.id, &report)?;
            }
            if before.is_none() || report.outcome.is_some() {
                moved.push((order.id.clone(), word));
            }
        }
        Ok(moved)
    }

    fn carry(&self, order: &Order, now: &str) -> Option<Report> {
        let ended = |outcome: Outcome, log: Option<String>| {
            Some(Report {
                at: now.into(),
                progress: None,
                outcome: Some(outcome),
                log,
            })
        };
        // canceled before it was ever taken: it never starts
        if let (None, Some(c)) = (&order.report, &order.cancel) {
            return ended(Outcome::canceled(&c.reason), None);
        }
        let handle = match self.mechanism.start(&order.request) {
            Ok(h) => h,
            Err(words) => return ended(Outcome::failed("refused", &words), None),
        };
        if let Some(c) = &order.cancel {
            self.mechanism.cancel(&handle, &c.reason);
        }
        let log = self.mechanism.log(&handle);
        match self.mechanism.observe(&handle) {
            Observation::Running { progress, .. } => Some(Report {
                at: now.into(),
                progress,
                outcome: None,
                log,
            }),
            Observation::Ended { outcome } => ended(outcome, log),
            Observation::Absent => ended(Outcome::lost("the Worker's mechanism does not know this Run"), log),
            Observation::Unreachable { .. } => None,
        }
    }
}

/// The OrderBook contract as tests.
pub mod conformance {
    use super::*;
    use crate::workflow::RunState;
    use std::collections::BTreeMap;

    fn order(id: &str, worker: &str) -> Order {
        Order {
            contract: ORDER.into(),
            id: id.into(),
            worker: worker.into(),
            placed: "2026-10-17T00:00:00Z".into(),
            request: StartRequest {
                workflow: "wf".into(),
                step: "s".into(),
                run: id.into(),
                definition: Ref::new("add", 1),
                inputs: BTreeMap::new(),
                parameters: BTreeMap::new(),
                outputs: Vec::new(),
            },
            cancel: None,
            report: None,
        }
    }

    pub fn run(book: &dyn OrderBook) {
        let face = WorkerFace {
            contract: WORKER.into(),
            name: "w1".into(),
            host: "h".into(),
            cloud: false,
            verbs: vec![Ref::new("add", 1)],
            seen: "2026-10-17T00:00:00Z".into(),
        };
        book.publish_worker(&face).unwrap();
        book.publish_worker(&WorkerFace {
            seen: "2026-10-17T00:00:05Z".into(),
            ..face.clone()
        })
        .unwrap();
        let workers = book.workers().unwrap();
        assert_eq!(workers.len(), 1, "publishing again replaces");
        assert_eq!(workers[0].seen, "2026-10-17T00:00:05Z");

        assert!(book.order("o-1").unwrap().is_none(), "absent is None");
        book.place(&order("o-1", "w1")).unwrap();
        book.place(&order("o-2", "w1")).unwrap();
        book.place(&order("o-3", "w2")).unwrap();
        book.place(&Order {
            placed: "later".into(),
            ..order("o-1", "w1")
        })
        .unwrap();
        assert_eq!(book.order("o-1").unwrap().unwrap().placed, "2026-10-17T00:00:00Z", "placing again changes nothing");
        let open: Vec<String> = book.open_orders("w1").unwrap().into_iter().map(|o| o.id).collect();
        assert_eq!(open, ["o-1", "o-2"], "a Worker's open orders, oldest first");

        let running = Report {
            at: "t1".into(),
            progress: None,
            outcome: None,
            log: Some("so far".into()),
        };
        book.report("o-1", &running).unwrap();
        assert_eq!(book.order("o-1").unwrap().unwrap().report, Some(running.clone()));
        let done = Report {
            at: "t2".into(),
            outcome: Some(Outcome::succeeded(BTreeMap::new())),
            ..running.clone()
        };
        book.report("o-1", &done).unwrap();
        assert!(book.report("o-1", &running).is_err(), "an outcome is final");
        assert_eq!(book.open_orders("w1").unwrap().len(), 1, "an ended order is not open");

        book.ask_cancel("o-2", &CancelAsk { reason: "first".into(), at: "t3".into() }).unwrap();
        book.ask_cancel("o-2", &CancelAsk { reason: "second".into(), at: "t4".into() }).unwrap();
        assert_eq!(book.order("o-2").unwrap().unwrap().cancel.unwrap().reason, "first", "the first ask stands");
        book.ask_cancel("o-1", &CancelAsk { reason: "late".into(), at: "t5".into() }).unwrap();
        let o1 = book.order("o-1").unwrap().unwrap();
        assert!(o1.cancel.is_none() && o1.report.unwrap().outcome.unwrap().state == RunState::Succeeded, "an ended order is left as it is");
        assert!(book.report("nope", &running).is_err(), "a report on no order is refused");
    }
}
