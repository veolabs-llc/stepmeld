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
    w.steps = vec![StepEntry::new("add", Ref::new("add", 1))];
    store.put_workflow_definition(&w).unwrap();
}

/// A Worker on `host` with the program `add-program`, whose mechanism
/// is a memory Performer that performs `add`.
fn worker(store: &Arc<MemoryStore>, name: &str, host: &str, cloud: bool) -> (Worker, Arc<MemoryPerformer>) {
    let mechanism = Arc::new(MemoryPerformer::new(PerformerFace {
        pool: false,
        nests: false,
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
    let wf = store.get(id).unwrap().unwrap().0;
    wf.step_status("add")
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
    let faces: Vec<PerformerFace> = before.performers.iter().map(|p| p.describe()).collect();
    assert_eq!(wf.gate("add", &faces), Gate::Closed(StepStatus::Waiting(Waiting::Performer)));
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

/// The cloud is `cloud` from everywhere, so a confirm-before-cloud
/// policy holds the step for a person, and a Worker here and the cloud
/// both offering the verb make the placement somebody's to choose.
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
    assert_eq!((faces.get("cloud"), faces.get("queue")), (Some(&Locality::Cloud), None), "the cloud Worker is reached as the pool `cloud`");
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
                    performer: Some("cloud".into()),
                    confirm: vec![Locality::Cloud],
                    locality: None,
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
    let faces: Vec<PerformerFace> = o.performers.iter().map(|p| p.describe()).collect();
    assert_eq!(wf.gate("add", &faces), Gate::Closed(StepStatus::Waiting(Waiting::Performer)));
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

/// A Performer's own slots hold within the Worker's: with room for
/// eight orders, one container at a time still waits its turn.
#[test]
fn a_program_s_limit_holds_within_the_worker_s_slots() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, mechanism) = worker(&store, "bench", "h", false);
    let w = w.with_slots(8).with_limit(vec!["add-program".into()], 1);
    w.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "h");
    begin(&o, "limit-1");
    begin(&o, "limit-2");
    o.tick_all(&now(1), &now(9)).unwrap();
    assert_eq!(w.tick(&now(1)).unwrap().len(), 1, "one taken, with seven slots to spare");
    let (first, second) = (latest(&store, "limit-1"), latest(&store, "limit-2"));
    assert!(store.order(&second.id).unwrap().unwrap().claim.is_none(), "the second waits for the program's slot");
    w.tick(&now(2)).unwrap();
    assert!(store.order(&second.id).unwrap().unwrap().claim.is_none(), "held, not just taken in one round");
    mechanism.finish(&mechanism.handle_of(&first.id).unwrap(), Outcome::succeeded(BTreeMap::from([("sum".to_string(), Value::from(5))])));
    w.tick(&now(3)).unwrap();
    w.tick(&now(4)).unwrap();
    assert!(store.order(&second.id).unwrap().unwrap().claim.is_some());
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
    assert_eq!(wf.gate("add", &faces), Gate::Closed(StepStatus::Waiting(Waiting::Performer)), "no Worker offers cuda");

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

/// The cloud's Workers are one placement, the pool `cloud`: a cloud
/// Worker's own name is not one. The Step goes to the pool unasked
/// when nothing else offers its verb, as to a lone Worker.
fn cloud_mechanism() -> Arc<MemoryPerformer> {
    Arc::new(
        MemoryPerformer::new(PerformerFace {
            name: "batch".into(),
            locality: Locality::Cloud,
            verbs: vec![Ref::new("add", 1)],
            pool: false,
            nests: false,
        })
        .with_portable_handles(),
    )
}

/// A cloud Worker's orders are open to every cloud Worker. When the one
/// that launched the work stops renewing, another takes the claim over
/// and goes on observing the same job: nothing is lost.
#[test]
fn a_cloud_job_is_taken_over_when_its_worker_goes_away() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let cloud = cloud_mechanism();
    let launcher = |name: &str, host: &str| Worker::new(name, host, true, vec!["add-program".into()], store.clone(), cloud.clone()).with_lease(120, 60);
    let first = launcher("queue-1", "laptop");
    first.tick(&now(0)).unwrap();
    let (clock, t) = settable(1);
    let o = orchestrator_with(&store, "laptop", clock.clone());
    let faces: Vec<PerformerFace> = o.performers.iter().map(|p| p.describe()).collect();
    assert!(faces.iter().all(|f| f.name != "queue-1"), "a cloud Worker is not a placement by name: {faces:?}");
    let pool = faces.iter().find(|f| f.name == "cloud").expect("the pool `cloud`");
    assert!(pool.pool && pool.locality == Locality::Cloud && pool.verbs == [Ref::new("add", 1)]);
    begin(&o, "cloud-1");
    o.tick_all(&now(1), &now(9)).unwrap();
    let run = latest(&store, "cloud-1");
    assert_eq!((run.performer.as_str(), run.locality), ("cloud", Some(Locality::Cloud)), "the pool alone offers the verb, so it is the placement");
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

/// With a Worker here and the cloud both offering a verb, a person
/// chooses; chosen, the pool `cloud` places one open cloud order, which
/// a cloud Worker takes and the Worker here leaves alone. Two cloud
/// Workers are still that one placement.
#[test]
fn the_cloud_is_one_placement_beside_the_workers_here() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (bench, mb) = worker(&store, "bench", "desk", false);
    let cloud = cloud_mechanism();
    let launcher = |name: &str, host: &str| Worker::new(name, host, true, vec!["add-program".into()], store.clone(), cloud.clone());
    let (q1, q2) = (launcher("queue-1", "desk"), launcher("queue-2", "laptop"));
    for w in [&bench, &q1, &q2] {
        w.tick(&now(0)).unwrap();
    }
    let o = orchestrator(&store, "desk");
    let names: Vec<String> = o.performers.iter().map(|p| p.describe().name).collect();
    assert_eq!(names, ["nested", "bench", "any", "cloud"], "one pool for both cloud Workers, after the Workers here and `any`");
    begin(&o, "cloud-2");
    o.tick_all(&now(1), &now(9)).unwrap();
    assert_eq!(status(&store, "cloud-2"), StepStatus::Waiting(Waiting::Placement), "a Worker here and the cloud: a person chooses");
    o.command(
        "cloud-2",
        &Command::Place {
            step: "add".into(),
            performer: "cloud".into(),
            locality: Locality::Cloud,
        },
        &person(),
        &now(1),
    )
    .unwrap();
    o.tick_all(&now(1), &now(9)).unwrap();
    let run = latest(&store, "cloud-2");
    let order = store.order(&run.id).unwrap().unwrap();
    assert_eq!((order.worker.clone(), order.open.clone().map(|o| o.cloud)), (None, Some(true)), "one open cloud order");
    bench.tick(&now(2)).unwrap();
    assert!(store.order(&run.id).unwrap().unwrap().claim.is_none() && mb.started().is_empty(), "the Worker here leaves a cloud order alone");
    q2.tick(&now(2)).unwrap();
    q1.tick(&now(2)).unwrap();
    assert_eq!(store.order(&run.id).unwrap().unwrap().claim.unwrap().worker, "queue-2", "the first cloud Worker to look launches it");
    assert_eq!(cloud.started().len(), 1);
    let handle = cloud.handle_of(&run.id).unwrap();
    cloud.finish(&handle, Outcome::succeeded(BTreeMap::from([("sum".to_string(), Value::from(5))])));
    q2.tick(&now(3)).unwrap();
    o.tick_all(&now(4), &now(9)).unwrap();
    assert_eq!(status(&store, "cloud-2"), StepStatus::Succeeded);
}

/// A Step placed on a cloud Worker by name, as every placement on the
/// cloud was before the cloud became one placement, is placed on the
/// cloud: the record says where a person put it, and the cloud's one way
/// in is its pool. The Run is the pool's. A Step placed on a machine
/// that is gone still waits for a person, and a reader given the
/// Performers sees what the Gate sees.
#[test]
fn a_cloud_worker_placed_by_name_is_the_cloud() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (bench, _) = worker(&store, "bench", "desk", false);
    let cloud = cloud_mechanism();
    let q1 = Worker::new("queue-1", "desk", true, vec!["add-program".into()], store.clone(), cloud.clone());
    for w in [&bench, &q1] {
        w.tick(&now(0)).unwrap();
    }
    let o = orchestrator(&store, "desk");
    let faces: Vec<PerformerFace> = o.performers.iter().map(|p| p.describe()).collect();
    let place = |id: &str, performer: &str, locality: Locality| {
        begin(&o, id);
        o.command(
            id,
            &Command::Place {
                step: "add".into(),
                performer: performer.into(),
                locality,
            },
            &person(),
            &now(1),
        )
        .unwrap();
    };
    place("old-cloud", "queue-1", Locality::Cloud);
    place("gone-box", "gone-box", Locality::ThisMachine);
    let before = store.get("gone-box").unwrap().unwrap().0;
    assert_eq!(before.step_status("add"), StepStatus::Ready, "without the Performers a reader cannot tell");
    assert_eq!(before.statuses_among(&faces)["add"], StepStatus::Waiting(Waiting::Performer), "with them it sees what the Gate sees");
    o.tick_all(&now(2), &now(9)).unwrap();
    let run = latest(&store, "old-cloud");
    assert_eq!((run.performer.as_str(), run.locality), ("cloud", Some(Locality::Cloud)), "the Run is the pool's");
    assert!(store.order(&run.id).unwrap().unwrap().open.unwrap().cloud, "one open cloud order");
    q1.tick(&now(3)).unwrap();
    assert_eq!(cloud.started().len(), 1, "a cloud Worker launched it");
    let gone = store.get("gone-box").unwrap().unwrap().0;
    assert!(gone.steps["add"].latest().is_none(), "a Step on a machine that is gone is not moved unasked");
    assert_eq!(gone.statuses_among(&faces)["add"], StepStatus::Waiting(Waiting::Performer));
}

/// A Run's log is kept beside its order and grows by appending, so
/// any Orchestrator reads it, and a Worker restarted mid-Run carries on
/// from what the store holds without writing a line twice.
#[test]
fn the_log_grows_in_the_store() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, mechanism) = worker(&store, "bench", "h", false);
    w.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "h");
    begin(&o, "log-1");
    o.tick_all(&now(1), &now(9)).unwrap();
    w.tick(&now(1)).unwrap();
    let run = latest(&store, "log-1");
    let handle = mechanism.handle_of(&run.id).unwrap();
    mechanism.print(&handle, "adding 2 + 3\n");
    w.tick(&now(2)).unwrap();
    assert_eq!(store.log(&run.id).unwrap(), "adding 2 + 3\n");
    drop(w);
    let again = Worker::new("bench", "h", false, vec!["add-program".into()], store.clone(), mechanism.clone());
    mechanism.print(&handle, "sum is 5\n");
    again.tick(&now(3)).unwrap();
    assert_eq!(store.log(&run.id).unwrap(), "adding 2 + 3\nsum is 5\n", "carried on from what the store holds");
    let reader = orchestrator(&store, "elsewhere");
    let p = reader.performers.iter().find(|p| p.describe().name == "bench").unwrap();
    assert_eq!(p.log(&run.id).as_deref(), Some("adding 2 + 3\nsum is 5\n"), "any Orchestrator reads it");
}

/// An Orchestrator ticking only what moved: a Workflow waiting for a
/// person is left alone until a Command changes it, one with a Run live
/// is ticked every time, and a Worker coming wakes every one.
#[test]
fn an_orchestrator_ticks_only_what_moved() {
    use stepmeld_core::driver::Seen;
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, mechanism) = worker(&store, "bench", "h", false);
    w.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "h");
    begin(&o, "busy");
    o.create(&Ref::new("add-two", 1), "idle", None, &person(), &now(0)).unwrap();
    o.command("idle", &Command::StartWorkflow, &person(), &now(0)).unwrap();
    let mut seen = Seen::default();
    o.tick_moved(&now(1), &now(9), &mut seen).unwrap();
    assert_eq!(seen.ticked, ["busy", "idle"], "the first time, every one");
    w.tick(&now(1)).unwrap();
    let statuses = o.tick_moved(&now(2), &now(9), &mut seen).unwrap();
    assert_eq!(seen.ticked, ["busy"], "the idle one waits for its inputs; only the busy one is ticked");
    assert_eq!(statuses["idle"], WorkflowStatus::Waiting);

    o.command(
        "idle",
        &Command::Set {
            step: "add".into(),
            input: Some("a".into()),
            parameter: None,
            value: Some(Value::from(1)),
        },
        &person(),
        &now(3),
    )
    .unwrap();
    o.tick_moved(&now(3), &now(9), &mut seen).unwrap();
    assert_eq!(seen.ticked, ["busy", "idle"], "a Command moved it");
    o.tick_moved(&now(4), &now(9), &mut seen).unwrap();
    assert_eq!(seen.ticked, ["busy"]);

    let (other, _) = worker(&store, "bench-2", "h", false);
    other.tick(&now(4)).unwrap();
    let o = orchestrator(&store, "h");
    o.tick_moved(&now(5), &now(9), &mut seen).unwrap();
    assert_eq!(seen.ticked, ["busy", "idle"], "a Worker came: every one may move");

    let run = store.get("busy").unwrap().unwrap().0.steps["add"].latest().unwrap().clone();
    mechanism.finish(&mechanism.handle_of(&run.id).unwrap(), Outcome::succeeded(BTreeMap::from([("sum".to_string(), Value::from(5))])));
    w.tick(&now(6)).unwrap();
    let statuses = o.tick_moved(&now(6), &now(9), &mut seen).unwrap();
    assert_eq!(statuses["busy"], WorkflowStatus::Succeeded);
    o.tick_moved(&now(7), &now(9), &mut seen).unwrap();
    assert_eq!(seen.ticked, ["busy"], "what the last tick wrote is looked at once more");
    o.tick_moved(&now(8), &now(9), &mut seen).unwrap();
    assert!(seen.ticked.is_empty(), "nothing left to move");
}

/// A Worker whose machine holds `gib` of memory, sized by a deployment
/// that says `add` needs ten GiB per unit of `a`.
fn sized_worker(store: &Arc<MemoryStore>, name: &str, gib: f64) -> (Worker, Arc<MemoryPerformer>) {
    let mechanism = Arc::new(
        MemoryPerformer::new(PerformerFace {
            pool: false,
            nests: false,
            name: name.into(),
            locality: Locality::ThisMachine,
            verbs: vec![Ref::new("add", 1)],
        })
        .with_memory_gib(gib),
    );
    let sizing: stepmeld_core::orders::Sizing = Arc::new(|r| {
        let a = r.inputs.get("a").and_then(Value::as_f64).ok_or("no `a` to size it by")?;
        Ok(BTreeMap::from([("memory_gib".to_string(), Value::from(a * 10.0))]))
    });
    let w = Worker::new(name, name, false, vec!["add-program".into()], store.clone(), mechanism.clone()).with_sizing(sizing);
    (w, mechanism)
}

#[test]
fn a_worker_hands_its_performer_the_needs_the_deployment_computes() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, mechanism) = sized_worker(&store, "big", 64.0);
    w.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "big");
    begin(&o, "sized-1");
    o.tick_all(&now(1), &now(9)).unwrap();
    w.tick(&now(2)).unwrap();
    let started = mechanism.started();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].needs.get("memory_gib"), Some(&Value::from(20.0)), "a = 2, at ten GiB each");
    let order = store.order(&started[0].run).unwrap().unwrap();
    assert!(order.request.needs.is_empty(), "the order keeps the request as placed");
}

#[test]
fn work_too_large_for_the_machine_it_is_pinned_to_is_refused_before_it_starts() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (w, mechanism) = sized_worker(&store, "small", 16.0);
    w.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "small");
    begin(&o, "sized-2");
    o.tick_all(&now(1), &now(9)).unwrap();
    w.tick(&now(2)).unwrap();
    o.tick_all(&now(3), &now(9)).unwrap();
    assert!(mechanism.started().is_empty(), "nothing started");
    let outcome = latest(&store, "sized-2").outcome.unwrap();
    assert_eq!(outcome.class.as_deref(), Some("refused"));
    assert_eq!(outcome.reason.as_deref(), Some("needs 20 GiB of memory; small has 16 (small cannot hold it)"));
}

#[test]
fn an_open_order_waits_for_a_worker_that_holds_it() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let (small, ms) = sized_worker(&store, "small", 16.0);
    let (big, mb) = sized_worker(&store, "big", 64.0);
    small.tick(&now(0)).unwrap();
    big.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "desk");
    begin(&o, "sized-3");
    o.command(
        "sized-3",
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
    let run = latest(&store, "sized-3");
    small.tick(&now(2)).unwrap();
    let order = store.order(&run.id).unwrap().unwrap();
    assert!(order.claim.is_none() && !order.ended(), "the small machine leaves it, and it waits");
    assert!(ms.started().is_empty());
    big.tick(&now(3)).unwrap();
    assert_eq!(store.order(&run.id).unwrap().unwrap().claim.unwrap().worker, "big");
    assert_eq!(mb.started()[0].needs.get("memory_gib"), Some(&Value::from(20.0)));
}

#[test]
fn needs_that_cannot_be_computed_refuse_the_order_in_words() {
    let store = Arc::new(MemoryStore::new());
    library(&store);
    let mechanism = Arc::new(MemoryPerformer::new(PerformerFace {
        pool: false,
        nests: false,
        name: "desk".into(),
        locality: Locality::ThisMachine,
        verbs: vec![Ref::new("add", 1)],
    }));
    let sizing: stepmeld_core::orders::Sizing = Arc::new(|_| Err("`cameras` is not in Input model".into()));
    let w = Worker::new("desk", "desk", false, vec!["add-program".into()], store.clone(), mechanism.clone()).with_sizing(sizing);
    w.tick(&now(0)).unwrap();
    let o = orchestrator(&store, "desk");
    begin(&o, "sized-4");
    o.tick_all(&now(1), &now(9)).unwrap();
    w.tick(&now(2)).unwrap();
    assert!(mechanism.started().is_empty());
    let outcome = store.order(&latest(&store, "sized-4").id).unwrap().unwrap().report.unwrap().outcome.unwrap();
    assert_eq!(outcome.reason.as_deref(), Some("its needs could not be computed: `cameras` is not in Input model"));
}
