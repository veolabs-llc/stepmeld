//! The object store under two Orchestrators and a Worker at once
//! (docs/PLACEMENT.md §2): every Workflow finishes, and nothing is
//! minted, started or ordered twice. The same walk as the SQLite
//! store's, over objects with conditional writes.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use stepmeld_core::definition::*;
use stepmeld_core::driver::Driver;
use stepmeld_core::engine::Command;
use stepmeld_core::objects::{MemoryObjects, ObjectStore};
use stepmeld_core::orders::{wall_clock, OrderBook, OrderPerformer, Worker};
use stepmeld_core::performer::MemoryPerformer;
use stepmeld_core::store::StateStore;
use stepmeld_core::workflow::{Actor, Outcome, PerformerFace, WorkflowStatus};

fn now() -> String {
    wall_clock()()
}

fn until() -> String {
    stepmeld_core::later(&now(), 60).unwrap()
}

#[test]
fn two_orchestrators_share_an_object_store_without_doing_anything_twice() {
    let store = Arc::new(ObjectStore::new(MemoryObjects::new(), "stepmeld/"));
    let mut count = StepDefinition::new("count", 1);
    count.program = Some("count-program".into());
    count.inputs.push(InputDefinition {
        name: "x".into(),
        tag: "n".into(),
        required: false,
        label: None,
    });
    count.outputs.push(OutputDefinition {
        name: "n".into(),
        tag: "n".into(),
        options: None,
        label: None,
    });
    store.put_step(&count).unwrap();
    let mut two = WorkflowDefinition::new("two", 1);
    for name in ["first", "second"] {
        two.steps.push(StepEntry {
            name: name.into(),
            step: Ref::new("count", 1),
            label: None,
        });
    }
    two.bindings.push(Binding {
        from: PortRef::new("first", "n"),
        to: PortRef::new("second", "x"),
        guard: None,
    });
    store.put_workflow_definition(&two).unwrap();
    let ids: Vec<String> = (0..4).map(|i| format!("wf-{i}")).collect();
    let setup = Driver::new(store.clone(), "setup");
    for id in &ids {
        setup.create(&Ref::new("two", 1), id, None, &Actor::person("t"), &now()).unwrap();
        setup.command(id, &Command::StartWorkflow, &Actor::person("t"), &now()).unwrap();
    }

    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let (store, stop) = (store.clone(), stop.clone());
        std::thread::spawn(move || {
            let mechanism = Arc::new(MemoryPerformer::new(PerformerFace {
                name: "m".into(),
                locality: Locality::ThisMachine,
                verbs: vec![Ref::new("count", 1)],
                pool: false,
            }));
            let w = Worker::new("bench", "h", false, vec!["count-program".into()], store, mechanism.clone()).with_slots(8);
            while !stop.load(Ordering::Relaxed) {
                w.tick(&now()).unwrap();
                for req in mechanism.started() {
                    let h = mechanism.handle_of(&req.run).unwrap();
                    if mechanism.run(&h).unwrap().outcome.is_none() {
                        mechanism.finish(&h, Outcome::succeeded(BTreeMap::from([("n".to_string(), serde_json::json!(1))])));
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    let orchestrators: Vec<_> = ["o-1", "o-2"]
        .into_iter()
        .map(|holder| {
            let (store, ids) = (store.clone(), ids.clone());
            std::thread::spawn(move || {
                for _ in 0..5000 {
                    let mut d = Driver::new(store.clone(), holder);
                    for p in OrderPerformer::for_workers(store.clone(), &store.library().unwrap(), "h", wall_clock()).unwrap() {
                        d = d.with_performer(Arc::new(p));
                    }
                    let statuses = d.tick_all(&now(), &until()).unwrap();
                    if ids.iter().all(|id| statuses.get(id) == Some(&WorkflowStatus::Succeeded)) {
                        return;
                    }
                }
                panic!("{holder} gave up before every Workflow finished");
            })
        })
        .collect();
    for o in orchestrators {
        o.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    worker.join().unwrap();
    for id in &ids {
        let (wf, _) = store.get(id).unwrap().unwrap();
        for (name, step) in &wf.steps {
            assert_eq!(step.runs.len(), 1, "{id}/{name} has one Run");
            assert!(store.order(&step.runs[0].id).unwrap().unwrap().ended());
        }
        let kinds: Vec<String> = store.history(id).unwrap().into_iter().map(|e| e.kind).collect();
        for kind in ["run-minted", "run-started"] {
            assert_eq!(kinds.iter().filter(|k| *k == kind).count(), 2, "{id}: {kind} once per Step: {kinds:?}");
        }
    }
    assert!(store.open_orders("bench").unwrap().is_empty(), "no order left open");
}
