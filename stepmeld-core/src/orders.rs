//! Orchestrators and Workers (docs/ORCHESTRATOR-WORKER.md,
//! docs/PLACEMENT.md): an Orchestrator reaches a Worker by placing an
//! order in the shared store; the Worker claims it under a lease, runs
//! it through its own Performer and reports on it. A Worker never
//! writes a Workflow.
//!
//! - [`OrderBook`]: the shared store's orders and published Workers,
//!   with [`crate::store::MemoryStore`] implementing it for tests and a
//!   conformance suite beside the trait.
//! - [`OrderPerformer`]: the Orchestrator's Performer for one published
//!   Worker: `start` places an order, `observe` reads its report, and a
//!   claim whose lease lapsed on a local handle ends the Run lost.
//! - [`Worker`]: the loop a Worker runs: claim its orders, renew its
//!   claims, run each through a Performer, report, and publish itself
//!   as its heartbeat.

use crate::definition::{Locality, Ref, StepDefinition};
use crate::engine::{Observation, StartRequest};
use crate::library::Library;
use crate::performer::Performer;
use crate::workflow::{Outcome, PerformerFace, Progress};
use crate::Error;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub const ORDER: &str = "stepmeld/order.v1";
pub const WORKER: &str = "stepmeld/worker.v1";

/// How long a claim holds unless renewed, and how often it is renewed
/// (from the dispatch these orders replace; docs/PLACEMENT.md §5).
pub const LEASE_S: u64 = 300;
pub const RENEW_S: u64 = 60;
/// How often a Worker says it is alive, at the least; it is away after
/// [`AWAY_AFTER`] beats unheard (docs/PLACEMENT.md §6).
pub const BEAT_S: u64 = 60;
pub const AWAY_AFTER: u64 = 3;
/// The pool of every Worker that is not a cloud Worker: "next
/// available". No Worker may take this name.
pub const ANY: &str = "any";

fn one() -> u32 {
    1
}

fn beat() -> u64 {
    BEAT_S
}

/// A Worker as every Orchestrator sees it: published by the Worker,
/// replaced each time it publishes. Publishing is its heartbeat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerFace {
    pub contract: String,
    pub name: String,
    /// The machine it runs on, as the machine names itself.
    pub host: String,
    /// Its work happens in the cloud (it launches work there).
    #[serde(default)]
    pub cloud: bool,
    /// The programs it has a command for, by name. It offers every
    /// StepDefinition that names one of them.
    pub programs: Vec<String>,
    /// The words it offers to what a StepDefinition requires. Opaque,
    /// compared for equality.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// How many orders it runs at once.
    #[serde(default = "one")]
    pub slots: u32,
    /// The orders it holds a claim on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holds: Vec<String>,
    /// The longest it goes between publishing, in seconds.
    #[serde(default = "beat")]
    pub beat_s: u64,
    /// When it last published itself.
    pub seen: String,
}

impl WorkerFace {
    /// Unheard for [`AWAY_AFTER`] beats at `now`. A `seen` or `now`
    /// spelled some other way is never away.
    pub fn away(&self, now: &str) -> bool {
        match (crate::secs(&self.seen), crate::secs(now)) {
            (Some(seen), Some(now)) => now > seen + AWAY_AFTER * self.beat_s.max(1),
            _ => false,
        }
    }

    /// Whether it may take `step`'s work: it has the program, and it
    /// offers every word the StepDefinition requires.
    pub fn offers(&self, step: &StepDefinition) -> bool {
        step.program.as_ref().is_some_and(|p| self.programs.contains(p)) && step.requires.iter().all(|r| self.capabilities.contains(r))
    }

    /// Where it is, seen from an Orchestrator on `here` (a host name):
    /// a cloud Worker is `cloud`, one on `here` `this-machine`, any
    /// other `local-network`.
    pub fn locality(&self, here: &str) -> Locality {
        if self.cloud {
            Locality::Cloud
        } else if self.host == here {
            Locality::ThisMachine
        } else {
            Locality::LocalNetwork
        }
    }

    /// The Performer an Orchestrator on `here` places Runs on for this
    /// Worker, at `now`: it offers every StepDefinition in the Library
    /// the Worker [offers](Self::offers), or nothing while the Worker
    /// is away.
    pub fn performer_face(&self, here: &str, lib: &Library, now: &str) -> PerformerFace {
        let locality = self.locality(here);
        let verbs = if self.away(now) { Vec::new() } else { lib.steps().filter(|s| self.offers(s)).map(|s| s.reference()).collect() };
        PerformerFace {
            name: self.name.clone(),
            locality,
            verbs,
            pool: false,
        }
    }
}

/// A cancel asked for on an order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CancelAsk {
    pub reason: String,
    pub at: String,
}

/// A Worker's hold on an order: taken by a conditional write, kept by
/// renewing it before `until`, and open to another once `until` has
/// passed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub worker: String,
    /// When this Worker took the order.
    pub at: String,
    /// Until when it holds unless renewed.
    pub until: String,
    /// The Performer's handle, once the work has started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Whether the handle means anything on another machine.
    #[serde(default)]
    pub portable: bool,
}

impl Claim {
    pub fn lapsed(&self, now: &str) -> bool {
        self.until.as_str() < now
    }
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

impl Report {
    fn ended(at: &str, outcome: Outcome, log: Option<String>) -> Report {
        Report {
            at: at.into(),
            progress: None,
            outcome: Some(outcome),
            log,
        }
    }
}

/// Who may take an open order: any Worker that has its program,
/// offers every word it requires, and is a cloud Worker or not, as it
/// says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Open {
    #[serde(default)]
    pub cloud: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

/// One Run's work, addressed to one Worker (pinned) or to any Worker
/// that may take it (open). The Orchestrator writes the request and any
/// cancel; the Worker claims it and writes the report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub contract: String,
    /// The Run's id: one order per Run.
    pub id: String,
    /// The Worker it is pinned to; none for an open order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// Who may take it, when it is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<Open>,
    pub placed: String,
    pub request: StartRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<CancelAsk>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<Claim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<Report>,
}

impl Order {
    pub fn ended(&self) -> bool {
        self.report.as_ref().is_some_and(|r| r.outcome.is_some())
    }

    /// Whether a Worker of this name, cloud or not, with these programs
    /// and capabilities, may take it.
    pub fn for_worker(&self, name: &str, cloud: bool, programs: &[String], capabilities: &[String]) -> bool {
        match (&self.worker, &self.open) {
            (Some(w), _) => w == name,
            (None, Some(open)) => open.cloud == cloud && self.request.program.as_ref().is_some_and(|p| programs.contains(p)) && open.requires.iter().all(|r| capabilities.contains(r)),
            (None, None) => false,
        }
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
    /// The orders that have not ended and are pinned to `worker`, and
    /// every open order that has not ended, oldest first. Which open
    /// ones the Worker may take is its to judge ([`Order::for_worker`]).
    fn open_orders(&self, worker: &str) -> Result<Vec<Order>, Error>;
    /// Replace the claim on an order that has not ended, only if its
    /// claim is still `expect` (none, for an order nobody holds):
    /// taking, renewing, recording the handle and taking over are all
    /// this one conditional write. Whether it was made.
    fn claim(&self, id: &str, expect: Option<&Claim>, claim: &Claim) -> Result<bool, Error>;
    /// The claiming Worker's latest word. Refused from any other
    /// Worker, and on an order that has ended: an outcome is final.
    fn report(&self, id: &str, worker: &str, report: &Report) -> Result<(), Error>;
    /// End an order with `report`, which carries an outcome, only if
    /// its claim is still `expect`: a cancel on an order nobody took, a
    /// claim whose lease lapsed. Whether it was ended.
    fn end(&self, id: &str, expect: Option<&Claim>, report: &Report) -> Result<bool, Error>;
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
    /// Open orders, and for cloud Workers too: who may take one. None
    /// pins each order to the Worker the face names.
    open: Option<bool>,
    /// What each StepDefinition requires, for an open order to say.
    requires: BTreeMap<Ref, Vec<String>>,
}

impl OrderPerformer {
    /// Pinned: each order goes to the Worker `face` names.
    pub fn new(book: Arc<dyn OrderBook>, face: PerformerFace, clock: Clock) -> OrderPerformer {
        OrderPerformer {
            book,
            face,
            clock,
            open: None,
            requires: BTreeMap::new(),
        }
    }

    /// Open: each order goes to any Worker, cloud or not as `cloud`
    /// says, that has its program and offers what it requires.
    pub fn open(book: Arc<dyn OrderBook>, face: PerformerFace, clock: Clock, cloud: bool, lib: &Library) -> OrderPerformer {
        OrderPerformer {
            open: Some(cloud),
            requires: lib.steps().map(|s| (s.reference(), s.requires.clone())).collect(),
            ..OrderPerformer::new(book, face, clock)
        }
    }

    /// One Performer per published Worker, as seen from `here`, then
    /// the pool [`ANY`]. A Worker offers what the Library says its
    /// programs and capabilities let it take; one that is away offers
    /// nothing, so nothing new is placed on it, while what it holds is
    /// still observed. A cloud Worker's orders are open to every cloud
    /// Worker, so another can take over its work. The pool offers what
    /// any Worker that is here and not a cloud Worker offers; the first
    /// free to take an order takes it.
    pub fn for_workers(book: Arc<dyn OrderBook>, lib: &Library, here: &str, clock: Clock) -> Result<Vec<OrderPerformer>, Error> {
        let now = clock();
        let workers: Vec<WorkerFace> = book.workers()?.into_iter().filter(|w| w.name != ANY).collect();
        let mut out: Vec<OrderPerformer> = workers
            .iter()
            .map(|w| {
                let face = w.performer_face(here, lib, &now);
                if w.cloud {
                    OrderPerformer::open(book.clone(), face, clock.clone(), true, lib)
                } else {
                    OrderPerformer::new(book.clone(), face, clock.clone())
                }
            })
            .collect();
        let local: Vec<&WorkerFace> = workers.iter().filter(|w| !w.cloud && !w.away(&now)).collect();
        let verbs: Vec<Ref> = lib.steps().filter(|s| local.iter().any(|w| w.offers(s))).map(|s| s.reference()).collect();
        let locality = if local.iter().all(|w| w.host == here) { Locality::ThisMachine } else { Locality::LocalNetwork };
        let pool = PerformerFace {
            name: ANY.into(),
            locality,
            verbs,
            pool: true,
        };
        out.push(OrderPerformer::open(book.clone(), pool, clock, false, lib));
        Ok(out)
    }
}

impl Performer for OrderPerformer {
    fn describe(&self) -> PerformerFace {
        self.face.clone()
    }

    fn start(&self, request: &StartRequest) -> Result<String, String> {
        let (worker, open) = match self.open {
            None => (Some(self.face.name.clone()), None),
            Some(cloud) => (
                None,
                Some(Open {
                    cloud,
                    requires: self.requires.get(&request.definition).cloned().unwrap_or_default(),
                }),
            ),
        };
        let order = Order {
            contract: ORDER.into(),
            id: request.run.clone(),
            worker,
            open,
            placed: (self.clock)(),
            request: request.clone(),
            cancel: None,
            claim: None,
            report: None,
        };
        self.book.place(&order).map_err(|e| format!("the order could not be placed: {e}"))?;
        Ok(order.id)
    }

    fn observe(&self, handle: &str) -> Observation {
        let order = match self.book.order(handle) {
            Err(e) => return Observation::Unreachable { reason: e.to_string() },
            Ok(None) => return Observation::Absent,
            Ok(Some(order)) => order,
        };
        if let Some(Report { outcome: Some(outcome), .. }) = order.report {
            return Observation::Ended { outcome };
        }
        let now = (self.clock)();
        if let Some(claim) = order.claim.as_ref().filter(|c| c.lapsed(&now) && !c.portable) {
            // its Worker stopped renewing, and nobody else can find the
            // work: re-issued, never revived
            let lost = Report::ended(
                &now,
                Outcome::lost(&format!("Worker {} stopped renewing its claim (it held until {})", claim.worker, claim.until)),
                order.report.as_ref().and_then(|r| r.log.clone()),
            );
            return match self.book.end(handle, Some(claim), &lost) {
                Ok(true) => Observation::Ended { outcome: lost.outcome.unwrap() },
                Ok(false) => Observation::Running { progress: None, attention: None },
                Err(e) => Observation::Unreachable { reason: e.to_string() },
            };
        }
        // placed and not yet taken, taken, or waiting to be taken over
        Observation::Running {
            progress: order.report.and_then(|r| r.progress),
            attention: None,
        }
    }

    fn cancel(&self, handle: &str, reason: &str) {
        let now = (self.clock)();
        let _ = self.book.ask_cancel(handle, &CancelAsk { reason: reason.into(), at: now.clone() });
        // an order nobody took ends here, even with its Worker away
        let _ = self.book.end(handle, None, &Report::ended(&now, Outcome::canceled(reason), None));
    }

    fn log(&self, handle: &str) -> Option<String> {
        self.book.order(handle).ok()??.report?.log
    }
}

/// An order that moved in a Worker's round: taken, or ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub order: String,
    /// `running`, or how it ended.
    pub word: String,
    /// The headline it ended with, or why it failed; empty while it runs.
    pub detail: String,
}

impl Moved {
    fn of(order: &str, report: &Report) -> Moved {
        let (word, detail) = match &report.outcome {
            Some(o) => (format!("{:?}", o.state).to_lowercase(), o.summary.as_ref().map(|s| s.headline.clone()).or_else(|| o.reason.clone()).unwrap_or_default()),
            None => ("running".into(), String::new()),
        };
        Moved { order: order.into(), word, detail }
    }
}

/// A Worker: claims the orders addressed to it in the shared store and
/// runs them through its Performer, reporting on each.
pub struct Worker {
    pub name: String,
    pub host: String,
    pub cloud: bool,
    /// The programs its Performer has a command for, by name.
    pub programs: Vec<String>,
    pub capabilities: Vec<String>,
    /// How many orders it runs at once.
    pub slots: u32,
    pub lease_s: u64,
    pub renew_s: u64,
    pub beat_s: u64,
    book: Arc<dyn OrderBook>,
    performer: Arc<dyn Performer>,
    /// When it last published, and what it held then.
    published: Mutex<Option<(String, Vec<String>)>>,
    /// The work it has started, by order: the handle, and whether it is
    /// portable. What leaves the open orders without this Worker ending
    /// it ended elsewhere, and local work is stopped.
    carrying: Mutex<std::collections::BTreeMap<String, (String, bool)>>,
}

impl Worker {
    pub fn new(name: &str, host: &str, cloud: bool, programs: Vec<String>, book: Arc<dyn OrderBook>, performer: Arc<dyn Performer>) -> Worker {
        Worker {
            name: name.into(),
            host: host.into(),
            cloud,
            programs,
            capabilities: Vec::new(),
            slots: 1,
            lease_s: LEASE_S,
            renew_s: RENEW_S,
            beat_s: BEAT_S,
            book,
            performer,
            published: Mutex::new(None),
            carrying: Mutex::new(Default::default()),
        }
    }

    pub fn with_capabilities(mut self, capabilities: Vec<String>) -> Worker {
        self.capabilities = capabilities;
        self
    }

    pub fn with_slots(mut self, slots: u32) -> Worker {
        self.slots = slots.max(1);
        self
    }

    /// Hold claims for `lease_s` seconds, renewing every `renew_s`.
    pub fn with_lease(mut self, lease_s: u64, renew_s: u64) -> Worker {
        self.lease_s = lease_s;
        self.renew_s = renew_s.min(lease_s);
        self
    }

    pub fn with_beat(mut self, beat_s: u64) -> Worker {
        self.beat_s = beat_s.max(1);
        self
    }

    /// The face this Worker publishes.
    pub fn face(&self, now: &str, holds: Vec<String>) -> WorkerFace {
        WorkerFace {
            contract: WORKER.into(),
            name: self.name.clone(),
            host: self.host.clone(),
            cloud: self.cloud,
            programs: self.programs.clone(),
            capabilities: self.capabilities.clone(),
            slots: self.slots,
            holds,
            beat_s: self.beat_s,
            seen: now.into(),
        }
    }

    /// One round: carry every open order one step on (claim it if a
    /// slot is free, renew a claim that is due, start the work, pass on
    /// a cancel, observe, report), then publish this Worker when a beat
    /// is due or what it holds has changed. The orders taken or ended
    /// in this round.
    pub fn tick(&self, now: &str) -> Result<Vec<Moved>, Error> {
        let until = crate::later(now, self.lease_s)?;
        let renew_by = crate::later(now, self.lease_s - self.renew_s)?;
        // what it may take, and whatever it already holds
        let orders: Vec<Order> = self
            .book
            .open_orders(&self.name)?
            .into_iter()
            .filter(|o| o.claim.as_ref().is_some_and(|c| c.worker == self.name) || o.for_worker(&self.name, self.cloud, &self.programs, &self.capabilities))
            .collect();
        let mut held = orders.iter().filter(|o| o.claim.as_ref().is_some_and(|c| c.worker == self.name)).count();
        let mut holds = Vec::new();
        let mut moved = Vec::new();
        for order in &orders {
            let fresh = Claim {
                worker: self.name.clone(),
                at: now.into(),
                until: until.clone(),
                handle: None,
                portable: self.performer.portable(),
            };
            let (claim, taken) = match &order.claim {
                Some(c) if c.worker == self.name => {
                    if c.until < renew_by {
                        let renewed = Claim { until: until.clone(), ..c.clone() };
                        if !self.book.claim(&order.id, Some(c), &renewed)? {
                            continue; // ended or taken over meanwhile
                        }
                        (renewed, false)
                    } else {
                        (c.clone(), false)
                    }
                }
                None => {
                    if let Some(c) = &order.cancel {
                        // canceled before it was ever taken: it never starts
                        let r = Report::ended(now, Outcome::canceled(&c.reason), None);
                        if self.book.end(&order.id, None, &r)? {
                            moved.push(Moved::of(&order.id, &r));
                        }
                        continue;
                    }
                    if held >= self.slots as usize || !self.book.claim(&order.id, None, &fresh)? {
                        continue;
                    }
                    held += 1;
                    (fresh, true)
                }
                // another Worker's claim: taken over only once lapsed,
                // and only when the work can be found from here
                Some(c) => {
                    if !(c.lapsed(now) && c.portable && fresh.portable) || held >= self.slots as usize {
                        continue;
                    }
                    let over = Claim { handle: c.handle.clone(), ..fresh };
                    if !self.book.claim(&order.id, Some(c), &over)? {
                        continue;
                    }
                    held += 1;
                    (over, true)
                }
            };
            let Some((claim, report)) = self.carry(order, claim, now)? else {
                holds.push(order.id.clone());
                continue;
            };
            if let Some(h) = &claim.handle {
                self.carrying.lock().unwrap().insert(order.id.clone(), (h.clone(), claim.portable));
            }
            let unchanged = order.report.as_ref().map(|b| (&b.progress, &b.outcome, &b.log)) == Some((&report.progress, &report.outcome, &report.log));
            if !unchanged {
                if let Err(e) = self.book.report(&order.id, &self.name, &report) {
                    // the order ended elsewhere, or another took it over
                    self.let_go(&order.id, &format!("the order is no longer this Worker's: {e}"));
                    continue;
                }
            }
            if report.outcome.is_none() {
                holds.push(order.id.clone());
            } else {
                self.carrying.lock().unwrap().remove(&order.id);
            }
            if taken || report.outcome.is_some() {
                moved.push(Moved::of(&order.id, &report));
            }
        }
        let gone: Vec<String> = self.carrying.lock().unwrap().keys().filter(|id| !orders.iter().any(|o| &o.id == *id)).cloned().collect();
        for id in gone {
            self.let_go(&id, "the order ended elsewhere (its claim lapsed, or it was canceled)");
        }
        self.beat(now, holds)?;
        Ok(moved)
    }

    /// Stop carrying an order this Worker no longer holds: local work
    /// is canceled; portable work goes on under whoever holds it now.
    fn let_go(&self, id: &str, reason: &str) {
        if let Some((handle, portable)) = self.carrying.lock().unwrap().remove(id) {
            if !portable {
                self.performer.cancel(&handle, reason);
            }
        }
    }

    /// Publish this Worker when a beat is due or its holds changed.
    fn beat(&self, now: &str, holds: Vec<String>) -> Result<(), Error> {
        let mut last = self.published.lock().unwrap();
        let due = match last.as_ref() {
            None => true,
            Some((at, held)) => *held != holds || crate::later(at, self.beat_s).map_or(true, |next| next.as_str() <= now),
        };
        if due {
            self.book.publish_worker(&self.face(now, holds.clone()))?;
            *last = Some((now.into(), holds));
        }
        Ok(())
    }

    /// Start or find the work, pass on a cancel, observe. The claim as
    /// it now stands (with its handle) and the report to write; `None`
    /// when the Performer could not be reached, which changes nothing.
    fn carry(&self, order: &Order, mut claim: Claim, now: &str) -> Result<Option<(Claim, Report)>, Error> {
        let handle = match &claim.handle {
            // a portable handle is found, never started again
            Some(h) if claim.portable => h.clone(),
            // a local one is started again (idempotent): that is how
            // a program that said it is resumable resumes
            _ => match self.performer.start(&order.request) {
                Ok(h) => h,
                Err(words) if words.starts_with("unreachable:") => return Ok(None),
                Err(words) => return Ok(Some((claim, Report::ended(now, Outcome::failed("refused", &words), None)))),
            },
        };
        if claim.handle.as_deref() != Some(handle.as_str()) {
            let with = Claim {
                handle: Some(handle.clone()),
                ..claim.clone()
            };
            if !self.book.claim(&order.id, Some(&claim), &with)? {
                return Ok(None);
            }
            claim = with;
        }
        if let Some(c) = &order.cancel {
            self.performer.cancel(&handle, &c.reason);
        }
        let log = self.performer.log(&handle);
        let report = match self.performer.observe(&handle) {
            Observation::Running { progress, .. } => Report {
                at: now.into(),
                progress,
                outcome: None,
                log,
            },
            Observation::Ended { outcome } => Report::ended(now, outcome, log),
            Observation::Absent => Report::ended(now, Outcome::lost("the Worker's Performer does not know this Run"), log),
            Observation::Unreachable { .. } => return Ok(None),
        };
        Ok(Some((claim, report)))
    }
}

/// The OrderBook contract as tests.
pub mod conformance {
    use super::*;
    use crate::definition::Ref;
    use crate::workflow::RunState;
    use std::collections::BTreeMap;

    fn order(id: &str, worker: &str) -> Order {
        Order {
            contract: ORDER.into(),
            id: id.into(),
            worker: Some(worker.into()),
            open: None,
            placed: "2026-10-17T00:00:00Z".into(),
            request: StartRequest {
                workflow: "wf".into(),
                step: "s".into(),
                run: id.into(),
                definition: Ref::new("stepdef_add", 1),
                program: Some("hello-program".into()),
                needs: BTreeMap::new(),
                inputs: BTreeMap::new(),
                parameters: BTreeMap::new(),
                outputs: Vec::new(),
            },
            cancel: None,
            claim: None,
            report: None,
        }
    }

    fn claim(worker: &str, until: &str) -> Claim {
        Claim {
            worker: worker.into(),
            at: "2026-10-17T00:00:00Z".into(),
            until: until.into(),
            handle: None,
            portable: false,
        }
    }

    pub fn run(book: &dyn OrderBook) {
        let face = WorkerFace {
            contract: WORKER.into(),
            name: "w1".into(),
            host: "h".into(),
            cloud: false,
            programs: vec!["hello-program".into()],
            capabilities: vec!["gpu".into()],
            slots: 2,
            holds: Vec::new(),
            beat_s: 60,
            seen: "2026-10-17T00:00:00Z".into(),
        };
        book.publish_worker(&face).unwrap();
        book.publish_worker(&WorkerFace {
            seen: "2026-10-17T00:00:05Z".into(),
            holds: vec!["o-1".into()],
            ..face.clone()
        })
        .unwrap();
        let workers = book.workers().unwrap();
        assert_eq!(workers.len(), 1, "publishing again replaces");
        assert_eq!(
            (workers[0].seen.as_str(), workers[0].holds.clone(), workers[0].capabilities.clone()),
            ("2026-10-17T00:00:05Z", vec!["o-1".to_string()], vec!["gpu".to_string()])
        );

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

        // a claim is one conditional write: take, renew, take over
        let first = claim("w1", "2026-10-17T00:05:00Z");
        assert!(book.claim("o-1", None, &first).unwrap(), "an order nobody holds is taken");
        assert!(!book.claim("o-1", None, &claim("w2", "2026-10-17T00:05:00Z")).unwrap(), "a held order is not taken from under its holder");
        let renewed = Claim {
            until: "2026-10-17T00:06:00Z".into(),
            handle: Some("h-1".into()),
            ..first.clone()
        };
        assert!(book.claim("o-1", Some(&first), &renewed).unwrap(), "the holder renews");
        assert!(!book.claim("o-1", Some(&first), &claim("w1", "2026-10-17T00:09:00Z")).unwrap(), "only over the claim as it stands");
        assert_eq!(book.order("o-1").unwrap().unwrap().claim, Some(renewed.clone()));

        let running = Report {
            at: "2026-10-17T00:01:00Z".into(),
            progress: None,
            outcome: None,
            log: Some("so far".into()),
        };
        assert!(book.report("o-1", "w2", &running).is_err(), "only the claiming Worker reports");
        assert!(book.report("o-2", "w1", &running).is_err(), "nor on an order nobody holds");
        book.report("o-1", "w1", &running).unwrap();
        assert_eq!(book.order("o-1").unwrap().unwrap().report, Some(running.clone()));
        let done = Report {
            at: "2026-10-17T00:02:00Z".into(),
            outcome: Some(Outcome::succeeded(BTreeMap::new())),
            ..running.clone()
        };
        book.report("o-1", "w1", &done).unwrap();
        assert!(book.report("o-1", "w1", &running).is_err(), "an outcome is final");
        assert!(!book.claim("o-1", Some(&renewed), &claim("w1", "2026-10-17T00:09:00Z")).unwrap(), "an ended order is not claimed");
        assert_eq!(book.open_orders("w1").unwrap().len(), 1, "an ended order is not open");

        // ending only over the claim as it stands
        let canceled = Report {
            at: "2026-10-17T00:03:00Z".into(),
            progress: None,
            outcome: Some(Outcome::canceled("asked")),
            log: None,
        };
        let held = claim("w2", "2026-10-17T00:01:00Z");
        assert!(book.claim("o-3", None, &held).unwrap());
        assert!(!book.end("o-3", None, &canceled).unwrap(), "a held order is not ended as if nobody held it");
        let lost = Report {
            outcome: Some(Outcome::lost("lapsed")),
            ..canceled.clone()
        };
        assert!(book.end("o-3", Some(&held), &lost).unwrap(), "a lapsed claim is ended over");
        assert_eq!(book.order("o-3").unwrap().unwrap().report.unwrap().outcome.unwrap().state, RunState::Lost);
        assert!(!book.end("o-3", Some(&held), &canceled).unwrap(), "an ended order stays as it ended");

        book.ask_cancel("o-2", &CancelAsk { reason: "first".into(), at: "t3".into() }).unwrap();
        book.ask_cancel("o-2", &CancelAsk { reason: "second".into(), at: "t4".into() }).unwrap();
        assert_eq!(book.order("o-2").unwrap().unwrap().cancel.unwrap().reason, "first", "the first ask stands");
        assert!(book.end("o-2", None, &canceled).unwrap(), "an order nobody took is ended");
        assert!(book.open_orders("w1").unwrap().is_empty());
        book.ask_cancel("o-1", &CancelAsk { reason: "late".into(), at: "t5".into() }).unwrap();
        let o1 = book.order("o-1").unwrap().unwrap();
        assert!(o1.cancel.is_none() && o1.report.unwrap().outcome.unwrap().state == RunState::Succeeded, "an ended order is left as it is");
        assert!(book.report("nope", "w1", &running).is_err(), "a report on no order is refused");
        assert!(!book.claim("nope", None, &first).unwrap(), "nor is a claim made on one");

        // an open order is listed for every Worker, which judges it
        let open = Order {
            worker: None,
            open: Some(Open { cloud: false, requires: vec!["gpu".into()] }),
            ..order("o-4", "unused")
        };
        book.place(&open).unwrap();
        for w in ["w1", "w9"] {
            let ids: Vec<String> = book.open_orders(w).unwrap().into_iter().map(|o| o.id).collect();
            assert_eq!(ids, ["o-4"], "listed for {w}");
        }
        let o4 = book.order("o-4").unwrap().unwrap();
        assert_eq!(o4, open, "an open order reads back as placed");
        assert!(o4.for_worker("w1", false, &["hello-program".into()], &["gpu".into()]));
        assert!(!o4.for_worker("w1", false, &["hello-program".into()], &[]), "without the capability");
        assert!(!o4.for_worker("w1", true, &["hello-program".into()], &["gpu".into()]), "a cloud Worker, for a local order");
        assert!(book.claim("o-4", None, &claim("w9", "2026-10-17T00:05:00Z")).unwrap(), "claimed like any other");
    }
}
