//! Orchestrators and Workers (docs/ORCHESTRATOR-WORKER.md), driven
//! through the memory store: an Orchestrator places orders, a Worker
//! runs them through its mechanism and reports, and any Orchestrator
//! reads the reports.

use std::collections::BTreeMap;
use std::sync::Arc;
use stepmeld_core::definition::*;
use stepmeld_core::driver::Driver;
use stepmeld_core::engine::Command;
use stepmeld_core::orders::{Clock, OrderBook, OrderPerformer, Worker};
use stepmeld_core::performer::MemoryPerformer;
use stepmeld_core::store::{MemoryStore, StateStore};
use stepmeld_core::workflow::*;
use stepmeld_core::Value;

fn now(n: u32) -> String {
    format!("2026-10-17T00:{:02}:00Z", n)
}

fn clock() -> Clock {
    Arc::new(|| now(0))
}

fn person() -> Actor {
    Actor::person("ann")
}

/// A one-step library, `add-two`: the walkthrough's first step alone.
fn library(store: &MemoryStore) {
    let mut add = StepDefinition::new("add", 1);
    add.program = Some("add-program".into());
    for n in ["a", "b"] {
        add.inputs.push(InputDefinition {
            name: n.into(),
            tag: "integer".into(),
            required: true,
            label: None,
        });
    }
    add.outputs.push(OutputDefinition {
        name: "sum".into(),
        tag: "integer".into(),
        options: None,
        label: None,
    });
    store.put_step(&add).unwrap();
    let mut w = WorkflowDefinition::new("add-two", 1);
    w.steps = vec![StepEntry {
        name: "add".into(),
        step: Ref::new("add", 1),
        label: None,
    }];
    store.put_workflow_definition(&w).unwrap();
}

/// A Worker on `host` with the program `add-program`, whose mechanism
/// is a memory Performer that performs `add`.
fn worker(store: &Arc<MemoryStore>, name: &str, host: &str, cloud: bool) -> (Worker, Arc<MemoryPerformer>) {
    let mechanism = Arc::new(MemoryPerformer::new(PerformerFace {
        pool: false,
        name: format!("{name}-programs"),
        locality: Locality::ThisMachine,
        verbs: vec![Ref::new("add", 1)],
    }));
    (Worker::new(name, host, cloud, vec!["add-program".into()], store.clone(), mechanism.clone()), mechanism)
}

/// An Orchestrator on `host`: a driver with one Performer per published Worker.
fn orchestrator(store: &Arc<MemoryStore>, host: &str) -> Driver {
    let mut d = Driver::new(store.clone(), &format!("orchestrator@{host}")).with_nesting();
    for p in OrderPerformer::for_workers(store.clone(), &store.library().unwrap(), host, clock()).unwrap() {
        d = d.with_performer(Arc::new(p));
    }
    d
}

fn begin(d: &Driver, id: &str) {
    d.create(&Ref::new("add-two", 1), id, None, &person(), &now(0)).unwrap();
    for (input, v) in [("a", 2), ("b", 3)] {
        d.command(
            id,
            &Command::Set {
                step: "add".into(),
                input: Some(input.into()),
                parameter: None,
                value: Some(Value::from(v)),
            },
            &person(),
            &now(0),
        )
        .unwrap();
    }
    d.command(id, &Command::StartWorkflow, &person(), &now(0)).unwrap();
}

fn status(store: &MemoryStore, id: &str) -> StepStatus {
    let lib = store.library().unwrap();
    let wf = store.get(id).unwrap().unwrap().0;
    wf.step_status(lib.workflow(&wf.definition).unwrap(), "add")
}

#[test]
fn an_order_is_run_by_its_worker_and_reported_back() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, mechanism) = worker(&store, "desk", "desk-host", false);
    // nobody has published: the step waits for a Worker that offers it
    let before = orchestrator(&store, "desk-host");
    begin(&before, "hello-1");
    before.tick_all(&now(1), &now(9)).unwrap();
    let wf = store.get("hello-1").unwrap().unwrap().0;
    let lib = store.library().unwrap();
    let faces: Vec<PerformerFace> = before.performers.iter().map(|p| p.describe()).collect();
    assert_eq!(wf.gate(lib.workflow(&wf.definition).unwrap(), "add", &faces), Gate::Closed(StepStatus::Waiting(Waiting::Performer)));
    assert!(wf.steps["add"].runs.is_empty());

    // the Worker publishes itself; an Orchestrator built now places the Run on it
    w.tick(&now(1)).unwrap();
    let o = orchestrator(&store, "desk-host");
    o.tick_all(&now(2), &now(9)).unwrap();
    let wf = store.get("hello-1").unwrap().unwrap().0;
    let run = wf.steps["add"].latest().unwrap().clone();
    assert_eq!((run.performer.as_str(), run.locality), ("desk", Some(Locality::ThisMachine)), "the placement names the Worker");
    let order = store.order(&run.id).unwrap().expect("an order was placed");
    assert_eq!((order.worker.as_deref(), order.request.inputs["a"].clone()), (Some("desk"), Value::from(2)));
    assert!(mechanism.started().is_empty(), "nothing runs until the Worker takes it");
    assert_eq!(status(&store, "hello-1"), StepStatus::Running, "placed and not yet taken is running");

    // the Worker takes it and reports progress; then the outcome
    let moved = w.tick(&now(3)).unwrap();
    assert_eq!(moved.iter().map(|m| (m.order.clone(), m.word.clone())).collect::<Vec<_>>(), [(run.id.clone(), "running".to_string())]);
    let handle = mechanism.handle_of(&run.id).unwrap();
    mechanism.progress(
        &handle,
        Progress {
            phase: Some("adding".into()),
            done: Some(1),
            total: Some(3),
            unit: Some("beats".into()),
            at: now(4),
        },
    );
    w.tick(&now(4)).unwrap();
    o.tick_all(&now(4), &now(9)).unwrap();
    let wf = store.get("hello-1").unwrap().unwrap().0;
    assert_eq!(wf.steps["add"].latest().unwrap().progress.as_ref().unwrap().done, Some(1));
    let mut done = Outcome::succeeded(BTreeMap::from([("sum".to_string(), Value::from(5))]));
    done.summary = Some(Summary { headline: "2 + 3 = 5".into(), details: None });
    mechanism.finish(&handle, done);
    let moved = w.tick(&now(5)).unwrap();
    assert_eq!((moved[0].word.as_str(), moved[0].detail.as_str()), ("succeeded", "2 + 3 = 5"), "the Worker says how it ended");
    o.tick_all(&now(5), &now(9)).unwrap();
    let wf = store.get("hello-1").unwrap().unwrap().0;
    assert_eq!(status(&store, "hello-1"), StepStatus::Succeeded);
    assert_eq!(wf.steps["add"].outputs["sum"].value, Some(Value::from(5)));
    assert!(store.open_orders("desk").unwrap().is_empty());
    for doc in [serde_json::to_value(store.order(&run.id).unwrap().unwrap()).unwrap(), serde_json::to_value(&store.workers().unwrap()[0]).unwrap()] {
        let contract = doc["contract"].as_str().unwrap().to_string();
        stepmeld_core::contracts::validate(&doc, &contract).unwrap();
    }
}

/// The Orchestrator that placed a Run goes away; another, built later
/// on another machine, reads the Worker's report and finishes the
/// Workflow. The Run never stalled on the first one.
#[test]
fn any_orchestrator_reads_the_report() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, mechanism) = worker(&store, "bench", "bench-host", false);
    w.tick(&now(0)).unwrap();
    let first = orchestrator(&store, "laptop");
    begin(&first, "hello-2");
    first.tick_all(&now(1), &now(9)).unwrap();
    let run = store.get("hello-2").unwrap().unwrap().0.steps["add"].latest().unwrap().clone();
    assert_eq!(run.locality, Some(Locality::LocalNetwork), "seen from the laptop, the bench is on the local network");
    drop(first);

    w.tick(&now(2)).unwrap();
    mechanism.finish(&mechanism.handle_of(&run.id).unwrap(), Outcome::succeeded(BTreeMap::from([("sum".to_string(), Value::from(5))])));
    w.tick(&now(3)).unwrap();
    let second = orchestrator(&store, "desk");
    second.tick_all(&now(4), &now(9)).unwrap();
    assert_eq!(status(&store, "hello-2"), StepStatus::Succeeded);
}

#[test]
fn a_cancel_travels_by_the_order() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, mechanism) = worker(&store, "desk", "h", false);
    w.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "h");
    begin(&o, "hello-3");
    o.tick_all(&now(1), &now(9)).unwrap();
    w.tick(&now(2)).unwrap();
    let run = store.get("hello-3").unwrap().unwrap().0.steps["add"].latest().unwrap().clone();
    o.command(
        "hello-3",
        &Command::Cancel {
            step: Some("add".into()),
            reason: Some("changed my mind".into()),
        },
        &person(),
        &now(3),
    )
    .unwrap();
    o.tick_all(&now(3), &now(9)).unwrap();
    assert_eq!(store.order(&run.id).unwrap().unwrap().cancel.unwrap().reason, "asked by person:ann");
    w.tick(&now(4)).unwrap();
    let handle = mechanism.handle_of(&run.id).unwrap();
    assert!(mechanism.run(&handle).unwrap().canceled.is_some(), "the Worker passed the cancel to its mechanism");
    o.tick_all(&now(5), &now(9)).unwrap();
    assert_eq!(status(&store, "hello-3"), StepStatus::Canceled);
}

#[test]
fn a_cancel_before_the_worker_takes_the_order_never_starts_it() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, mechanism) = worker(&store, "desk", "h", false);
    w.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "h");
    begin(&o, "hello-4");
    o.tick_all(&now(1), &now(9)).unwrap();
    o.command("hello-4", &Command::Cancel { step: Some("add".into()), reason: None }, &person(), &now(2)).unwrap();
    o.tick_all(&now(2), &now(9)).unwrap();
    w.tick(&now(3)).unwrap();
    o.tick_all(&now(4), &now(9)).unwrap();
    assert!(mechanism.started().is_empty());
    assert_eq!(status(&store, "hello-4"), StepStatus::Canceled);
}

#[test]
fn a_mechanism_that_refuses_ends_the_run_in_its_words() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, mechanism) = worker(&store, "desk", "h", false);
    mechanism.set_refusal(Some("no program for add on this machine"));
    w.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "h");
    begin(&o, "hello-5");
    o.tick_all(&now(1), &now(9)).unwrap();
    w.tick(&now(2)).unwrap();
    o.tick_all(&now(3), &now(9)).unwrap();
    let wf = store.get("hello-5").unwrap().unwrap().0;
    let outcome = wf.steps["add"].latest().unwrap().outcome.clone().unwrap();
    assert_eq!((outcome.class.as_deref(), outcome.reason.as_deref()), (Some("refused"), Some("no program for add on this machine")));
}

/// A cloud Worker is `cloud` from everywhere, so a confirm-before-cloud
/// policy holds the step for a person, and two Workers offering the
/// verb make the placement somebody's to choose.
#[test]
fn locality_comes_from_the_worker() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (desk, _) = worker(&store, "desk", "h", false);
    let (cloud, _) = worker(&store, "queue", "elsewhere", true);
    desk.tick(&now(0)).unwrap();
    cloud.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "h");
    let faces: BTreeMap<String, Locality> = o.performers.iter().map(|p| (p.describe().name, p.describe().locality)).collect();
    assert_eq!(faces.get("queue"), Some(&Locality::Cloud));
    assert_eq!(faces.get("desk"), Some(&Locality::ThisMachine));
    begin(&o, "hello-6");
    o.tick_all(&now(1), &now(9)).unwrap();
    assert_eq!(status(&store, "hello-6"), StepStatus::Waiting(Waiting::Placement));
    o.command(
        "hello-6",
        &Command::SetPolicy {
            step: "add".into(),
            policy: Policy {
                placement: PlacementPolicy {
                    performer: Some("queue".into()),
                    confirm: vec![Locality::Cloud],
                },
                ..Default::default()
            },
        },
        &person(),
        &now(2),
    )
    .unwrap();
    o.tick_all(&now(3), &now(9)).unwrap();
    assert_eq!(status(&store, "hello-6"), StepStatus::Waiting(Waiting::Confirmation { locality: Locality::Cloud }));
}

/// A clock the test moves.
fn settable(at: u32) -> (Clock, Arc<std::sync::Mutex<String>>) {
    let t = Arc::new(std::sync::Mutex::new(now(at)));
    let r = t.clone();
    (Arc::new(move || r.lock().unwrap().clone()), t)
}

fn orchestrator_with(store: &Arc<MemoryStore>, host: &str, clock: Clock) -> Driver {
    let mut d = Driver::new(store.clone(), &format!("orchestrator@{host}")).with_nesting();
    for p in OrderPerformer::for_workers(store.clone(), &store.library().unwrap(), host, clock).unwrap() {
        d = d.with_performer(Arc::new(p));
    }
    d
}

fn latest(store: &MemoryStore, id: &str) -> Run {
    store.get(id).unwrap().unwrap().0.steps["add"].latest().unwrap().clone()
}

/// The Worker claims the order and goes away for good. Once its claim
/// lapses, the next Orchestrator to look ends the Run lost: the work
/// was on that machine, and nobody else can find it.
#[test]
fn a_worker_that_stops_renewing_leaves_its_run_lost() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, _) = worker(&store, "bench", "bench-host", false);
    let w = w.with_lease(120, 60);
    w.tick(&now(0)).unwrap();
    let (clock, t) = settable(1);
    let o = orchestrator_with(&store, "desk", clock.clone());
    begin(&o, "gone-1");
    o.tick_all(&now(1), &now(9)).unwrap();
    w.tick(&now(1)).unwrap();
    let run = latest(&store, "gone-1");
    let claim = store.order(&run.id).unwrap().unwrap().claim.unwrap();
    assert_eq!((claim.worker.as_str(), claim.until.as_str()), ("bench", "2026-10-17T00:03:00Z"), "claimed for the lease");
    drop(w);

    *t.lock().unwrap() = now(3);
    o.tick_all(&now(3), &now(9)).unwrap();
    assert_eq!(status(&store, "gone-1"), StepStatus::Running, "not before the claim lapses");
    *t.lock().unwrap() = now(4);
    o.tick_all(&now(4), &now(9)).unwrap();
    let outcome = latest(&store, "gone-1").outcome.unwrap();
    assert_eq!((outcome.state, outcome.class.as_deref()), (RunState::Lost, Some("lost")));
    assert!(outcome.reason.unwrap().contains("bench stopped renewing"), "the reason names the Worker");
    assert!(store.open_orders("bench").unwrap().is_empty(), "the order is ended too");
}

/// A Worker restarted within its lease finds its own claim, renews it
/// and carries the Run on: nothing is lost by a restart.
#[test]
fn a_worker_back_within_its_lease_carries_on() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, mechanism) = worker(&store, "bench", "h", false);
    w.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "h");
    begin(&o, "back-1");
    o.tick_all(&now(1), &now(9)).unwrap();
    w.tick(&now(1)).unwrap();
    let run = latest(&store, "back-1");
    let before = store.order(&run.id).unwrap().unwrap().claim.unwrap();
    drop(w);

    let again = Worker::new("bench", "h", false, vec!["add-program".into()], store.clone(), mechanism.clone());
    again.tick(&now(3)).unwrap();
    let after = store.order(&run.id).unwrap().unwrap().claim.unwrap();
    assert_eq!((after.at.as_str(), after.handle.clone()), (before.at.as_str(), before.handle.clone()), "the same claim");
    assert!(after.until > before.until, "renewed");
    assert_eq!(mechanism.started().len(), 1, "the work was not started twice");
    mechanism.finish(&mechanism.handle_of(&run.id).unwrap(), Outcome::succeeded(BTreeMap::from([("sum".to_string(), Value::from(5))])));
    again.tick(&now(4)).unwrap();
    o.tick_all(&now(5), &now(9)).unwrap();
    assert_eq!(status(&store, "back-1"), StepStatus::Succeeded);
}

/// A Worker back after its Run was ended lost no longer holds the
/// order, and stops the work it had left running.
#[test]
fn a_worker_back_too_late_stops_its_work() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, mechanism) = worker(&store, "bench", "h", false);
    let w = w.with_lease(120, 60);
    w.tick(&now(0)).unwrap();
    let (clock, t) = settable(1);
    let o = orchestrator_with(&store, "h", clock);
    begin(&o, "late-1");
    o.tick_all(&now(1), &now(9)).unwrap();
    w.tick(&now(1)).unwrap();
    let run = latest(&store, "late-1");
    *t.lock().unwrap() = now(5);
    o.tick_all(&now(5), &now(9)).unwrap();
    assert_eq!(latest(&store, "late-1").outcome.unwrap().state, RunState::Lost);

    let moved = w.tick(&now(6)).unwrap();
    assert!(moved.is_empty());
    let handle = mechanism.handle_of(&run.id).unwrap();
    assert!(mechanism.run(&handle).unwrap().canceled.is_some(), "its local work is canceled");
    assert!(store.workers().unwrap()[0].holds.is_empty(), "and it says it holds nothing");
}

/// Unheard for three beats, a Worker is away: nothing new is placed on
/// it, and the Step waits for one that is here.
#[test]
fn an_away_worker_is_not_placed_on() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, _) = worker(&store, "bench", "h", false);
    w.tick(&now(0)).unwrap();
    let (clock, _) = settable(4);
    let o = orchestrator_with(&store, "h", clock);
    assert!(o.performers.iter().find(|p| p.describe().name == "bench").unwrap().describe().verbs.is_empty(), "an away Worker offers nothing");
    begin(&o, "away-1");
    o.tick_all(&now(4), &now(9)).unwrap();
    assert!(store.get("away-1").unwrap().unwrap().0.steps["add"].runs.is_empty());
    let wf = store.get("away-1").unwrap().unwrap().0;
    let lib = store.library().unwrap();
    let faces: Vec<PerformerFace> = o.performers.iter().map(|p| p.describe()).collect();
    assert_eq!(wf.gate(lib.workflow(&wf.definition).unwrap(), "add", &faces), Gate::Closed(StepStatus::Waiting(Waiting::Performer)));
    assert!(!store.workers().unwrap()[0].away(&now(3)), "three beats unheard is not yet away");
}

/// With one slot, a Worker takes one order at a time: the second waits
/// unclaimed until the first ends.
#[test]
fn a_worker_takes_as_many_orders_as_it_has_slots() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, mechanism) = worker(&store, "bench", "h", false);
    w.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "h");
    begin(&o, "slot-1");
    begin(&o, "slot-2");
    o.tick_all(&now(1), &now(9)).unwrap();
    let moved = w.tick(&now(1)).unwrap();
    assert_eq!(moved.len(), 1, "one order taken");
    let (first, second) = (latest(&store, "slot-1"), latest(&store, "slot-2"));
    assert!(store.order(&first.id).unwrap().unwrap().claim.is_some());
    assert!(store.order(&second.id).unwrap().unwrap().claim.is_none(), "the second waits for the slot");
    assert_eq!(store.workers().unwrap()[0].holds, std::slice::from_ref(&first.id));
    mechanism.finish(&mechanism.handle_of(&first.id).unwrap(), Outcome::succeeded(BTreeMap::from([("sum".to_string(), Value::from(5))])));
    w.tick(&now(2)).unwrap();
    assert!(store.order(&second.id).unwrap().unwrap().claim.is_none(), "a slot frees once the round has reported");
    w.tick(&now(3)).unwrap();
    assert!(store.order(&second.id).unwrap().unwrap().claim.is_some());
    assert_eq!(mechanism.started().len(), 2);
}

/// A cancel on an order nobody took ends it at once, without waiting
/// for a Worker that may never come back.
#[test]
fn a_cancel_on_an_order_nobody_took_ends_it_at_once() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, _) = worker(&store, "bench", "h", false);
    w.tick(&now(0)).unwrap();
    drop(w);
    let o = orchestrator(&store, "h");
    begin(&o, "never-1");
    o.tick_all(&now(1), &now(9)).unwrap();
    o.command("never-1", &Command::Cancel { step: Some("add".into()), reason: None }, &person(), &now(2)).unwrap();
    o.tick_all(&now(2), &now(9)).unwrap();
    assert_eq!(status(&store, "never-1"), StepStatus::Canceled);
}

/// `add`, requiring `cuda` and needing two GPUs.
fn require_cuda(store: &MemoryStore) {
    let mut add = store.library().unwrap().step(&Ref::new("add", 1)).unwrap();
    add.requires = vec!["cuda".into()];
    add.needs = BTreeMap::from([("gpus".to_string(), Value::from(2))]);
    store.put_step(&add).unwrap();
}

/// A Worker offers a StepDefinition only when it has every word the
/// StepDefinition requires; what the work needs reaches its Performer.
#[test]
fn a_worker_offers_what_it_has_the_capabilities_for() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    require_cuda(&store);
    let (plain, _) = worker(&store, "plain", "h", false);
    plain.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "h");
    begin(&o, "cap-1");
    o.tick_all(&now(1), &now(9)).unwrap();
    let wf = store.get("cap-1").unwrap().unwrap().0;
    let faces: Vec<PerformerFace> = o.performers.iter().map(|p| p.describe()).collect();
    let lib = store.library().unwrap();
    assert_eq!(wf.gate(lib.workflow(&wf.definition).unwrap(), "add", &faces), Gate::Closed(StepStatus::Waiting(Waiting::Performer)), "no Worker offers cuda");

    let (gpu, mechanism) = worker(&store, "gpu", "h", false);
    let gpu = gpu.with_capabilities(vec!["cuda".into(), "docker".into()]);
    gpu.tick(&now(1)).unwrap();
    let o = orchestrator(&store, "h");
    o.tick_all(&now(2), &now(9)).unwrap();
    assert_eq!(latest(&store, "cap-1").performer, "gpu");
    gpu.tick(&now(2)).unwrap();
    assert_eq!(mechanism.started()[0].needs.get("gpus"), Some(&Value::from(2)), "needs reach the Performer");
}

/// Placed on the pool `any`, the order is open: the first Worker free
/// to take it does, and the other leaves it alone.
#[test]
fn any_worker_takes_an_open_order() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (a, ma) = worker(&store, "bench-a", "a", false);
    let (b, mb) = worker(&store, "bench-b", "b", false);
    a.tick(&now(0)).unwrap();
    b.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "desk");
    let pool = o.performers.iter().map(|p| p.describe()).find(|f| f.name == "any").unwrap();
    assert!(pool.pool && pool.verbs == [Ref::new("add", 1)] && pool.locality == Locality::LocalNetwork);
    begin(&o, "any-1");
    o.tick_all(&now(1), &now(9)).unwrap();
    assert_eq!(status(&store, "any-1"), StepStatus::Waiting(Waiting::Placement), "two Workers, and the pool is never chosen on its own");
    o.command(
        "any-1",
        &Command::Place {
            step: "add".into(),
            performer: "any".into(),
            locality: Locality::LocalNetwork,
        },
        &person(),
        &now(1),
    )
    .unwrap();
    o.tick_all(&now(1), &now(9)).unwrap();
    let run = latest(&store, "any-1");
    let order = store.order(&run.id).unwrap().unwrap();
    assert_eq!((order.worker.clone(), order.open.clone().map(|o| o.cloud)), (None, Some(false)), "an open order");

    b.tick(&now(2)).unwrap();
    a.tick(&now(2)).unwrap();
    assert_eq!(store.order(&run.id).unwrap().unwrap().claim.unwrap().worker, "bench-b", "the first to look takes it");
    assert!(ma.started().is_empty());
    mb.finish(&mb.handle_of(&run.id).unwrap(), Outcome::succeeded(BTreeMap::from([("sum".to_string(), Value::from(5))])));
    b.tick(&now(3)).unwrap();
    o.tick_all(&now(4), &now(9)).unwrap();
    assert_eq!(status(&store, "any-1"), StepStatus::Succeeded);
}

/// A cloud Worker's orders are open to every cloud Worker. When the one
/// that launched the work stops renewing, another takes the claim over
/// and goes on observing the same job: nothing is lost.
#[test]
fn a_cloud_job_is_taken_over_when_its_worker_goes_away() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let cloud = Arc::new(
        MemoryPerformer::new(PerformerFace {
            name: "batch".into(),
            locality: Locality::Cloud,
            verbs: vec![Ref::new("add", 1)],
            pool: false,
        })
        .with_portable_handles(),
    );
    let launcher = |name: &str, host: &str| Worker::new(name, host, true, vec!["add-program".into()], store.clone(), cloud.clone()).with_lease(120, 60);
    let first = launcher("queue-1", "laptop");
    first.tick(&now(0)).unwrap();
    let (clock, t) = settable(1);
    let o = orchestrator_with(&store, "laptop", clock.clone());
    begin(&o, "cloud-1");
    o.command(
        "cloud-1",
        &Command::Place {
            step: "add".into(),
            performer: "queue-1".into(),
            locality: Locality::Cloud,
        },
        &person(),
        &now(1),
    )
    .unwrap();
    o.tick_all(&now(1), &now(9)).unwrap();
    let run = latest(&store, "cloud-1");
    assert!(store.order(&run.id).unwrap().unwrap().open.unwrap().cloud, "a cloud order is open");
    first.tick(&now(1)).unwrap();
    let handle = store.order(&run.id).unwrap().unwrap().claim.unwrap().handle.unwrap();
    drop(first);

    let second = launcher("queue-2", "desk");
    second.tick(&now(2)).unwrap();
    assert_eq!(store.order(&run.id).unwrap().unwrap().claim.unwrap().worker, "queue-1", "not before the claim lapses");
    *t.lock().unwrap() = now(4);
    let o = orchestrator_with(&store, "laptop", clock);
    o.tick_all(&now(4), &now(9)).unwrap();
    assert_eq!(status(&store, "cloud-1"), StepStatus::Running, "a portable claim that lapsed is not lost");
    second.tick(&now(4)).unwrap();
    let claim = store.order(&run.id).unwrap().unwrap().claim.unwrap();
    assert_eq!((claim.worker.as_str(), claim.handle.as_deref()), ("queue-2", Some(handle.as_str())), "taken over, same job");
    assert_eq!(cloud.started().len(), 1, "the job was not launched again");
    cloud.finish(&handle, Outcome::succeeded(BTreeMap::from([("sum".to_string(), Value::from(5))])));
    second.tick(&now(5)).unwrap();
    o.tick_all(&now(5), &now(9)).unwrap();
    assert_eq!(status(&store, "cloud-1"), StepStatus::Succeeded);
}
