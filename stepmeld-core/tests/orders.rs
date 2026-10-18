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
    assert_eq!((order.worker.as_str(), order.request.inputs["a"].clone()), ("desk", Value::from(2)));
    assert!(mechanism.started().is_empty(), "nothing runs until the Worker takes it");
    assert_eq!(status(&store, "hello-1"), StepStatus::Running, "placed and not yet taken is running");

    // the Worker takes it and reports progress; then the outcome
    let moved = w.tick(&now(3)).unwrap();
    assert_eq!(moved, [(run.id.clone(), "running".to_string())]);
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
    mechanism.finish(&handle, Outcome::succeeded(BTreeMap::from([("sum".to_string(), Value::from(5))])));
    w.tick(&now(5)).unwrap();
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
