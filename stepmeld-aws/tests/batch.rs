//! The Batch Performer against stand-in jobs and folders: placement on
//! the size ladder, a job's life as the Performer sees it, the wrapper
//! running a program on a copy of the folder, and a cloud Worker taking
//! an order through to its outcome.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use stepmeld_aws::batch::{wrapped, BatchPerformer, BatchProgram, JobState, Jobs, MemoryJobs, RUN_ENV};
use stepmeld_core::definition::Ref;
use stepmeld_core::engine::{Observation, StartRequest};
use stepmeld_core::objects::{Cond, MemoryObjects, Objects};
use stepmeld_core::performer::Performer;
use stepmeld_core::workflow::{Outcome, RunState};

fn program() -> BatchProgram {
    BatchProgram {
        definition: "typeset-def".into(),
        queues: BTreeMap::from([("on-demand".to_string(), "od-queue".to_string()), ("spot".to_string(), "spot-queue".to_string())]),
        capacity: "on-demand".into(),
        ladder: vec![(28, 4), (56, 8), (120, 16)],
        env: BTreeMap::new(),
    }
}

fn request(run: &str, needs: serde_json::Value) -> StartRequest {
    StartRequest {
        workflow: "wf".into(),
        step: "typeset".into(),
        run: run.into(),
        definition: Ref::new("typeset", 1),
        program: Some("typeset-program".into()),
        needs: serde_json::from_value(needs).unwrap(),
        inputs: BTreeMap::new(),
        parameters: BTreeMap::new(),
        outputs: Vec::new(),
    }
}

fn performer(jobs: &Arc<MemoryJobs>, folders: &Arc<MemoryObjects>) -> BatchPerformer {
    BatchPerformer::new(
        "queue",
        jobs.clone(),
        folders.clone(),
        "s3://runs-bucket/stepmeld/runs/",
        "stepmeld/runs/",
        BTreeMap::from([("typeset-program".to_string(), program())]),
    )
    .with_grace(Duration::ZERO)
}

#[test]
fn a_need_is_placed_on_the_smallest_size_that_holds_it() {
    let p = program();
    let needs = |v: serde_json::Value| serde_json::from_value::<BTreeMap<String, serde_json::Value>>(v).unwrap();
    assert_eq!(p.place(&needs(serde_json::json!({}))).unwrap(), ("od-queue".to_string(), None));
    assert_eq!(p.place(&needs(serde_json::json!({"memory_gib": 30}))).unwrap(), ("od-queue".to_string(), Some((56, 8))));
    assert_eq!(p.place(&needs(serde_json::json!({"memory_gib": 20, "vcpus": 10, "capacity": "spot"}))).unwrap(), ("spot-queue".to_string(), Some((120, 16))));
    assert!(p.place(&needs(serde_json::json!({"memory_gib": 500}))).unwrap_err().contains("more than the largest"));
    assert!(p.place(&needs(serde_json::json!({"capacity": "reserved"}))).unwrap_err().contains("no reserved queue"));
}

#[test]
fn the_batch_performer_conforms() {
    let (jobs, folders) = (Arc::new(MemoryJobs::new()), Arc::new(MemoryObjects::new()));
    let p = performer(&jobs, &folders);
    stepmeld_core::performer::conformance::run(&p, &request("wf/typeset/1", serde_json::json!({})), &|_| {
        let (id, _) = jobs.submitted().pop().unwrap();
        jobs.set(&id, JobState::Succeeded);
    });
    assert!(p.portable(), "a Batch job's handle is good on any machine");
}

#[test]
fn a_job_is_told_its_folder_and_observed_by_its_state_and_its_folder() {
    let (jobs, folders) = (Arc::new(MemoryJobs::new()), Arc::new(MemoryObjects::new()));
    let p = performer(&jobs, &folders);
    let h = p.start(&request("wf/typeset/1", serde_json::json!({"memory_gib": 40}))).unwrap();
    let (id, spec) = jobs.submitted().pop().unwrap();
    assert_eq!((spec.queue.as_str(), spec.definition.as_str(), spec.size), ("od-queue", "typeset-def", Some((56, 8))));
    assert_eq!(spec.env, [(RUN_ENV.to_string(), format!("s3://runs-bucket/stepmeld/runs/{h}/"))]);
    assert!(folders.get(&format!("stepmeld/runs/{h}/request.json")).unwrap().is_some(), "the request is in the folder");

    folders
        .put(&format!("stepmeld/runs/{h}/progress.json"), br#"{"phase":"typesetting","done":2,"total":5,"at":"2026-10-20T00:00:00Z"}"#, Cond::None)
        .unwrap();
    jobs.set(&id, JobState::Running);
    match p.observe(&h) {
        Observation::Running { progress: Some(pr), .. } => assert_eq!(pr.done, Some(2)),
        other => panic!("{other:?}"),
    }
    // another machine's Worker, taking over, sees the same
    let other = performer(&jobs, &folders);
    assert!(matches!(other.observe(&h), Observation::Running { .. }));
    folders.put(&format!("stepmeld/runs/{h}/log.txt"), b"set 5 pages\n", Cond::None).unwrap();
    assert_eq!(other.log(&h).as_deref(), Some("set 5 pages\n"));
    jobs.set(
        &id,
        JobState::Failed {
            reason: "Essential container in task exited".into(),
            exit_code: Some(3),
        },
    );
    match other.observe(&h) {
        Observation::Ended { outcome } => assert!(outcome.state == RunState::Failed && outcome.reason.unwrap().contains("exit 3")),
        other => panic!("{other:?}"),
    }
}

/// A program's `env` goes with the submit, beside where its folder is.
#[test]
fn a_program_s_env_is_sent_with_the_submit() {
    let (jobs, folders) = (Arc::new(MemoryJobs::new()), Arc::new(MemoryObjects::new()));
    let mut typeset = program();
    typeset.env = BTreeMap::from([("TYPESET_PAPER".to_string(), "a4".to_string())]);
    let p = BatchPerformer::new("queue", jobs.clone(), folders, "s3://b/runs/", "runs/", BTreeMap::from([("typeset-program".to_string(), typeset)]));
    let h = p.start(&request("wf/typeset/1", serde_json::json!({}))).unwrap();
    let (_, spec) = jobs.submitted().pop().unwrap();
    assert_eq!(spec.env, [("TYPESET_PAPER".to_string(), "a4".to_string()), (RUN_ENV.to_string(), format!("s3://b/runs/{h}/"))]);
}

#[test]
fn a_cancel_is_asked_in_the_folder_then_the_job_is_terminated() {
    let (jobs, folders) = (Arc::new(MemoryJobs::new()), Arc::new(MemoryObjects::new()));
    let p = performer(&jobs, &folders);
    let h = p.start(&request("wf/typeset/2", serde_json::json!({}))).unwrap();
    let (id, _) = jobs.submitted().pop().unwrap();
    jobs.set(&id, JobState::Running);
    p.cancel(&h, "not needed");
    assert!(folders.get(&format!("stepmeld/runs/{h}/cancel.json")).unwrap().is_some(), "asked in the folder first");
    match p.observe(&h) {
        Observation::Ended { outcome } => assert_eq!(outcome.state, RunState::Canceled),
        other => panic!("{other:?}"),
    }
    assert!(matches!(jobs.state(&id).unwrap(), Some(JobState::Canceled { .. })), "and terminated at the deadline");
}

#[test]
fn a_job_batch_forgets_is_lost_after_a_few_looks() {
    let (jobs, folders) = (Arc::new(MemoryJobs::new()), Arc::new(MemoryObjects::new()));
    let p = performer(&jobs, &folders);
    let h = p.start(&request("wf/typeset/3", serde_json::json!({}))).unwrap();
    let (id, _) = jobs.submitted().pop().unwrap();
    jobs.forget(&id);
    for _ in 0..4 {
        assert!(matches!(p.observe(&h), Observation::Running { .. }), "not at once: a new job may not be listed yet");
    }
    match p.observe(&h) {
        Observation::Ended { outcome } => assert_eq!(outcome.state, RunState::Lost),
        other => panic!("{other:?}"),
    }
}

/// The wrapper: the request comes down, the program runs on the local
/// copy, and its progress, log and outcome go up.
#[test]
fn the_wrapper_runs_the_program_on_a_copy_of_the_folder() {
    let folders = MemoryObjects::new();
    folders
        .put("runs/r1/request.json", serde_json::to_vec(&request("wf/typeset/9", serde_json::json!({}))).unwrap().as_slice(), Cond::None)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("verb.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nrun=\"$1\"\ngrep -q typeset \"$run/request.json\" && echo 'read the request'\necho '{\"phase\":\"x\",\"at\":\"now\"}' > \"$run/progress.json\"\necho '{\"state\":\"succeeded\",\"outputs\":{}}' > \"$run/outcome.json\"\n",
    )
    .unwrap();
    let code = wrapped(&folders, "runs/r1", &["sh".into(), script.display().to_string()], &dir.path().join("local"), Duration::from_millis(20)).unwrap();
    assert_eq!(code, 0);
    let get = |f: &str| String::from_utf8(folders.get(&format!("runs/r1/{f}")).unwrap().unwrap().0).unwrap();
    assert!(get("log.txt").contains("read the request"));
    assert!(get("progress.json").contains("\"x\""));
    let outcome: Outcome = serde_json::from_str(&get("outcome.json")).unwrap();
    assert_eq!(outcome.state, RunState::Succeeded);
    assert!(wrapped(&folders, "runs/none", &["true".into()], &dir.path().join("l2"), Duration::from_millis(20)).is_err(), "no request, no run");
}

/// Two cloud Workers whose programs run as Batch jobs. The first
/// launches the job and goes away; once its claim lapses the second
/// takes the order over and observes the same job, which the wrapper
/// runs to its outcome. The Workflow finishes; the job ran once.
#[test]
fn a_cloud_worker_takes_over_a_batch_job_and_sees_it_through() {
    use stepmeld_core::definition::*;
    use stepmeld_core::driver::Driver;
    use stepmeld_core::engine::Command;
    use stepmeld_core::orders::{Clock, OrderBook, OrderPerformer, Worker};
    use stepmeld_core::store::{MemoryStore, StateStore};
    use stepmeld_core::workflow::{Actor, StepStatus};

    let store = Arc::new(MemoryStore::new());
    let mut typeset = StepDefinition::new("typeset", 1);
    typeset.program = Some("typeset-program".into());
    typeset.needs = BTreeMap::from([("memory_gib".to_string(), serde_json::json!(8))]);
    typeset.outputs.push(OutputDefinition {
        name: "pages".into(),
        tag: "n".into(),
        options: None,
        label: None,
    });
    store.put_step(&typeset).unwrap();
    let mut wd = WorkflowDefinition::new("book", 1);
    wd.steps.push(StepEntry {
        name: "typeset".into(),
        step: Ref::new("typeset", 1),
        label: None,
    });
    store.put_workflow_definition(&wd).unwrap();

    let (jobs, folders) = (Arc::new(MemoryJobs::new()), Arc::new(MemoryObjects::new()));
    let launcher = |name: &str| Worker::new(name, &format!("{name}-host"), true, vec!["typeset-program".into()], store.clone(), Arc::new(performer(&jobs, &folders))).with_lease(120, 60);
    let at = |m: u32| format!("2026-10-20T00:{m:02}:00Z");
    let clock_at = Arc::new(std::sync::Mutex::new(at(1)));
    let clock: Clock = {
        let c = clock_at.clone();
        Arc::new(move || c.lock().unwrap().clone())
    };
    let orchestrate = |m: u32| {
        *clock_at.lock().unwrap() = at(m);
        let mut d = Driver::new(store.clone(), "laptop");
        for p in OrderPerformer::for_workers(store.clone(), &store.library().unwrap(), "laptop", clock.clone()).unwrap() {
            d = d.with_performer(Arc::new(p));
        }
        d.tick_all(&at(m), &at(m + 1)).unwrap();
        d
    };

    let first = launcher("queue-1");
    first.tick(&at(0)).unwrap();
    let d = orchestrate(1);
    d.create(&Ref::new("book", 1), "book-1", None, &Actor::person("ann"), &at(1)).unwrap();
    d.command("book-1", &Command::StartWorkflow, &Actor::person("ann"), &at(1)).unwrap();
    orchestrate(1);
    first.tick(&at(1)).unwrap();
    let (job, spec) = jobs.submitted().pop().expect("the first Worker launched the job");
    assert_eq!(spec.size, Some((28, 4)), "sized by the Step's needs");
    drop(first);

    // the job runs, wrapped, while nobody holds the order
    jobs.set(&job, JobState::Running);
    let prefix = spec.env[0].1.trim_start_matches("s3://runs-bucket/").to_string();
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("verb.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho typesetting\necho '{\"state\":\"succeeded\",\"outputs\":{\"pages\":5},\"summary\":{\"headline\":\"5 pages\"}}' > \"$1/outcome.json\"\n",
    )
    .unwrap();
    assert_eq!(
        wrapped(folders.as_ref(), &prefix, &["sh".into(), script.display().to_string()], &dir.path().join("local"), Duration::from_millis(10)).unwrap(),
        0
    );
    jobs.set(&job, JobState::Succeeded);

    let second = launcher("queue-2");
    second.tick(&at(2)).unwrap();
    orchestrate(2);
    let lib = store.library().unwrap();
    let status = |s: &MemoryStore| {
        let wf = s.get("book-1").unwrap().unwrap().0;
        wf.step_status(lib.workflow(&wf.definition).unwrap(), "typeset")
    };
    assert_eq!(status(&store), StepStatus::Running, "not before the first one's claim lapses");
    orchestrate(4);
    assert_eq!(status(&store), StepStatus::Running, "a lapsed claim on a Batch job is not lost");
    second.tick(&at(4)).unwrap();
    orchestrate(5);
    assert_eq!(status(&store), StepStatus::Succeeded);
    let wf = store.get("book-1").unwrap().unwrap().0;
    assert_eq!(wf.steps["typeset"].outputs["pages"].value, Some(serde_json::json!(5)));
    let run = wf.steps["typeset"].latest().unwrap();
    assert_eq!(store.order(&run.id).unwrap().unwrap().claim.unwrap().worker, "queue-2", "taken over");
    assert_eq!(jobs.submitted().len(), 1, "the job ran once");
    assert!(store.log(&run.id).unwrap().contains("typesetting"), "its log reached the store");
}
