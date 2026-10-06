//! The vocabulary walks of docs/DESIGN.md §7, as tests: the one-step
//! example, then A to D. Each drives the engine through the Driver over
//! the memory store and memory performers.

use std::collections::BTreeMap;
use std::sync::Arc;
use stepmeld_core::definition::*;
use stepmeld_core::driver::Driver;
use stepmeld_core::engine::Command;
use stepmeld_core::performer::{MemoryPerformer, Performer};
use stepmeld_core::store::{MemoryStore, StateStore};
use stepmeld_core::workflow::*;
use stepmeld_core::{Error, Value};

fn now(n: u32) -> String {
    format!("2026-10-06T00:{:02}:00Z", n)
}

fn max() -> Actor {
    Actor::person("max")
}

fn verb(name: &str, inputs: &[(&str, &str)], params: &[(&str, &str, Option<Value>)], outputs: &[(&str, &str)]) -> StepDefinition {
    let mut d = StepDefinition::new(name, 1);
    d.inputs = inputs
        .iter()
        .map(|(n, t)| InputDefinition {
            name: n.to_string(),
            tag: t.to_string(),
            required: true,
            label: None,
        })
        .collect();
    d.parameters = params
        .iter()
        .map(|(n, t, dflt)| ParameterDefinition {
            name: n.to_string(),
            tag: t.to_string(),
            required: false,
            default: dflt.clone(),
            label: None,
        })
        .collect();
    d.outputs = outputs
        .iter()
        .map(|(n, t)| OutputDefinition {
            name: n.to_string(),
            tag: t.to_string(),
            options: None,
            label: None,
        })
        .collect();
    d
}

fn person_verb(name: &str, inputs: &[(&str, &str)], outputs: &[(&str, &str)], decision: Option<(&str, &[&str])>) -> StepDefinition {
    let mut d = verb(name, inputs, &[], outputs);
    d.done_by = DoneBy::Person;
    if let Some((n, opts)) = decision {
        d.outputs.push(OutputDefinition {
            name: n.into(),
            tag: DECISION.into(),
            options: Some(opts.iter().map(|s| s.to_string()).collect()),
            label: None,
        });
    }
    d
}

fn entry(name: &str, def: &str) -> StepEntry {
    StepEntry {
        name: name.into(),
        step: Ref::new(def, 1),
        label: None,
    }
}

fn bind(from: &str, to: &str) -> Binding {
    Binding {
        from: PortRef(from.into()),
        to: PortRef(to.into()),
        guard: None,
    }
}

fn performer(name: &str, locality: Locality, verbs: &[&str]) -> Arc<MemoryPerformer> {
    Arc::new(MemoryPerformer::new(PerformerFace {
        name: name.into(),
        locality,
        verbs: verbs.iter().map(|v| Ref::new(v, 1)).collect(),
    }))
}

fn set(step: &str, input: &str, value: Value) -> Command {
    Command::Set {
        step: step.into(),
        input: Some(input.into()),
        parameter: None,
        value: Some(value),
    }
}

fn param(step: &str, parameter: &str, value: Value) -> Command {
    Command::Set {
        step: step.into(),
        input: None,
        parameter: Some(parameter.into()),
        value: Some(value),
    }
}

fn start(step: &str) -> Command {
    Command::Start { step: step.into() }
}

fn complete(step: &str, outputs: &[(&str, Value)]) -> Command {
    Command::Complete {
        step: step.into(),
        outputs: outputs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
    }
}

fn outs(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

struct Rig {
    store: Arc<MemoryStore>,
    driver: Driver,
    id: String,
    clock: u32,
}

impl Rig {
    fn new(store: Arc<MemoryStore>, performers: Vec<Arc<MemoryPerformer>>, definition: &str, id: &str) -> Rig {
        let mut driver = Driver::new(store.clone(), "test").with_nesting();
        for p in performers {
            driver = driver.with_performer(p);
        }
        driver.create(&Ref::new(definition, 1), id, None, &max(), &now(0)).unwrap();
        Rig { store, driver, id: id.into(), clock: 1 }
    }

    fn tick(&mut self) -> Workflow {
        self.clock += 1;
        let until = now(self.clock + 5);
        self.driver.tick_all(&now(self.clock), &until).unwrap();
        self.store.get(&self.id).unwrap().unwrap().0
    }

    fn command(&mut self, cmd: Command) -> Result<Workflow, Error> {
        self.clock += 1;
        self.driver.command(&self.id, &cmd, &max(), &now(self.clock))
    }

    /// The document as stored, held to its contract every time.
    fn wf(&self) -> Workflow {
        let wf = self.store.get(&self.id).unwrap().unwrap().0;
        let doc = serde_json::to_value(&wf).unwrap();
        stepmeld_core::contracts::validate(&doc, "stepmeld/workflow.v1").unwrap_or_else(|e| panic!("workflow {} departs from its contract: {e}", wf.id));
        for e in self.store.history(&self.id).unwrap() {
            stepmeld_core::contracts::validate(&serde_json::to_value(&e).unwrap(), "stepmeld/history.v1").unwrap_or_else(|err| panic!("history entry departs from its contract: {err}"));
        }
        wf
    }

    /// Write the golden fixtures from this rig's store when asked to
    /// (`STEPMELD_WRITE_FIXTURES=1`): every definition, this Workflow,
    /// its History.
    fn write_fixtures(&self, tag: &str) {
        if std::env::var("STEPMELD_WRITE_FIXTURES").is_err() {
            return;
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../contracts/fixtures");
        let write = |dir: &str, name: &str, doc: &Value| {
            let d = root.join(dir);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(format!("{name}.json")), format!("{}\n", serde_json::to_string_pretty(doc).unwrap())).unwrap();
        };
        let lib = self.store.library().unwrap();
        for s in lib.steps() {
            write("step-definition.v1", &s.name, &serde_json::to_value(s).unwrap());
        }
        for w in lib.workflows() {
            write("workflow-definition.v1", &w.name, &serde_json::to_value(w).unwrap());
        }
        write("workflow.v1", tag, &serde_json::to_value(self.wf()).unwrap());
        for e in self.store.history(&self.id).unwrap().iter().take(3) {
            write("history.v1", &format!("{tag}-{}", e.seq), &serde_json::to_value(e).unwrap());
        }
    }

    fn def(&self) -> WorkflowDefinition {
        self.store.library().unwrap().workflow(&self.wf().definition).unwrap().clone()
    }

    fn status(&self, step: &str) -> StepStatus {
        self.wf().step_status(&self.def(), step)
    }

    fn workflow_status(&self) -> WorkflowStatus {
        self.wf().status(&self.def())
    }

    fn handle(&self, p: &MemoryPerformer, step: &str) -> String {
        let run = self.wf().step(step).unwrap().latest().unwrap().id.clone();
        p.handle_of(&run).unwrap_or_else(|| panic!("{step} was started on {}", p.describe().name))
    }

    fn history_kinds(&self) -> Vec<String> {
        self.store
            .history(&self.id)
            .unwrap()
            .iter()
            .map(|e| format!("{}{}", e.kind, e.step.as_ref().map(|s| format!("@{s}")).unwrap_or_default()))
            .collect()
    }
}

// ---------------------------------------------------------------------
// The one-step example: Detect OmniTargets.

fn detect_library(store: &MemoryStore) {
    store
        .put_step(&verb("detect-targets", &[("images", "file-list")], &[("min-size", "integer", Some(Value::from(12)))], &[("targets", "target-table")]))
        .unwrap();
    // the verb and the recipe may share a label, never a name: verbs are one namespace
    let mut w = WorkflowDefinition::new("find-targets", 1);
    w.label = Some("Detect OmniTargets".into());
    w.steps = vec![entry("detect", "detect-targets")];
    store.put_workflow_definition(&w).unwrap();
}

#[test]
fn one_step_workflow_runs_when_its_input_is_given() {
    let store = Arc::new(MemoryStore::new());
    detect_library(&store);
    let local = performer("local", Locality::ThisMachine, &["detect-targets"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "find-targets", "wf-1");

    // created: the step waits for the one input nobody feeds
    assert_eq!(rig.status("detect"), StepStatus::Waiting(Waiting::Input { name: "images".into() }));
    assert_eq!(rig.workflow_status(), WorkflowStatus::Waiting);
    rig.tick();
    assert!(local.started().is_empty(), "nothing starts with its gate closed");

    // the file list arrives; the gate opens; a tick mints and starts a run
    rig.command(set("detect", "images", serde_json::json!(["a.jpg", "b.jpg"]))).unwrap();
    assert_eq!(rig.status("detect"), StepStatus::Ready);
    let wf = rig.tick();
    assert_eq!(wf.step_status(&rig.def(), "detect"), StepStatus::Running);
    let req = &local.started()[0];
    assert_eq!(req.parameters["min-size"], Value::from(12), "the default is in effect");
    assert_eq!(req.inputs["images"], serde_json::json!(["a.jpg", "b.jpg"]));
    assert_eq!(wf.step("detect").unwrap().latest().unwrap().started.by, Actor::policy("start-automatically", "detect"));

    // progress is read, then the outcome; the output is bound
    let h = rig.handle(&local, "detect");
    local.progress(
        &h,
        Progress {
            phase: Some("detecting".into()),
            done: Some(1),
            total: Some(2),
            unit: Some("images".into()),
            at: now(3),
        },
    );
    let wf = rig.tick();
    assert_eq!(wf.step("detect").unwrap().latest().unwrap().progress.as_ref().unwrap().done, Some(1));
    let mut outcome = Outcome::succeeded(outs(&[("targets", serde_json::json!({"rows": 86}))]));
    outcome.summary = Some(Summary {
        headline: "86 targets in 2 images".into(),
        details: None,
    });
    local.finish(&h, outcome);
    let wf = rig.tick();
    assert_eq!(wf.step_status(&rig.def(), "detect"), StepStatus::Succeeded);
    assert_eq!(wf.step("detect").unwrap().outputs["targets"].value, Some(serde_json::json!({"rows": 86})));
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);
    assert_eq!(rig.history_kinds(), ["created", "set@detect", "run-minted@detect", "run-started@detect", "progress@detect", "run-ended@detect"]);

    // a set on a succeeded step makes it stale; nothing restarts on its own
    rig.command(param("detect", "min-size", Value::from(8))).unwrap();
    assert_eq!(rig.status("detect"), StepStatus::Stale);
    rig.tick();
    assert_eq!(local.started().len(), 1);
    rig.command(start("detect")).unwrap();
    rig.tick();
    assert_eq!(local.started().len(), 2, "a start retakes a stale step");
    assert_eq!(local.started()[1].parameters["min-size"], Value::from(8));
    assert_eq!(rig.wf().step("detect").unwrap().runs.len(), 2, "runs are append-only");
    rig.write_fixtures("find-targets");
}

#[test]
fn a_bound_input_cannot_be_set_and_a_running_step_cannot_be_started() {
    let store = Arc::new(MemoryStore::new());
    detect_library(&store);
    let local = performer("local", Locality::ThisMachine, &["detect-targets"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "find-targets", "wf-2");
    rig.command(set("detect", "images", serde_json::json!(["a.jpg"]))).unwrap();
    rig.tick();
    assert!(matches!(rig.command(start("detect")), Err(Error::Refused(w)) if w.contains("running")));
    assert!(matches!(rig.command(set("detect", "nope", Value::Null)), Err(Error::Refused(w)) if w.contains("running")));
}

#[test]
fn a_refusal_is_a_failed_run_that_never_ran() {
    let store = Arc::new(MemoryStore::new());
    detect_library(&store);
    let local = performer("local", Locality::ThisMachine, &["detect-targets"]);
    local.set_refusal(Some("no such file a.jpg"));
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "find-targets", "wf-3");
    rig.command(set("detect", "images", serde_json::json!(["a.jpg"]))).unwrap();
    let wf = rig.tick();
    let run = wf.step("detect").unwrap().latest().unwrap();
    assert_eq!(run.outcome.as_ref().unwrap().class.as_deref(), Some("refused"));
    assert_eq!(rig.status("detect"), StepStatus::Failed);
    assert_eq!(rig.workflow_status(), WorkflowStatus::Failed);
}

#[test]
fn an_unreachable_performer_changes_nothing() {
    let store = Arc::new(MemoryStore::new());
    detect_library(&store);
    let local = performer("local", Locality::ThisMachine, &["detect-targets"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "find-targets", "wf-4");
    rig.command(set("detect", "images", serde_json::json!(["a.jpg"]))).unwrap();
    rig.tick();
    let before = rig.wf();
    local.set_unreachable(Some("network"));
    rig.tick();
    assert_eq!(rig.wf(), before);
    local.set_unreachable(None);
    local.forget(&rig.handle(&local, "detect"));
    let wf = rig.tick();
    assert_eq!(wf.step("detect").unwrap().latest().unwrap().outcome.as_ref().unwrap().state, RunState::Lost, "absent is lost");
}

#[test]
fn a_success_without_the_declared_outputs_is_a_fault() {
    let store = Arc::new(MemoryStore::new());
    detect_library(&store);
    let local = performer("local", Locality::ThisMachine, &["detect-targets"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "find-targets", "wf-5");
    rig.command(set("detect", "images", serde_json::json!(["a.jpg"]))).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "detect"), Outcome::succeeded(BTreeMap::new()));
    let wf = rig.tick();
    let o = wf.step("detect").unwrap().latest().unwrap().outcome.clone().unwrap();
    assert_eq!((o.state, o.class.as_deref()), (RunState::Failed, Some("fault")));
}

// ---------------------------------------------------------------------
// A. The photogrammetry layer.

fn photogrammetry_library(store: &MemoryStore) {
    store.put_step(&person_verb("choose-photos", &[], &[("photos", "photo-set")], None)).unwrap();
    store
        .put_step(&verb("solve", &[("photos", "photo-set")], &[("matcher", "string", Some("exhaustive".into()))], &[("model", "sparse-model")]))
        .unwrap();
    store.put_step(&person_verb("review-solve", &[("model", "sparse-model")], &[], Some(("decision", &["accept", "refine"])))).unwrap();
    store.put_step(&verb("refine", &[("model", "sparse-model")], &[], &[("model", "sparse-model")])).unwrap();
    let mut dense = verb("dense", &[("model", "sparse-model"), ("photos", "photo-set")], &[("image-size", "integer", Some(4096.into()))], &[("cloud", "point-cloud")]);
    dense.policy.placement.confirm = vec![Locality::Cloud];
    dense.policy.on_failure = OnFailure::Retake { max: 1, classes: vec!["lost".into()] };
    store.put_step(&dense).unwrap();
    store.put_step(&verb("publish", &[("cloud", "point-cloud")], &[], &[("layer", "layer")])).unwrap();
    let mut w = WorkflowDefinition::new("photogrammetry-layer", 1);
    w.steps = vec![
        entry("photos", "choose-photos"),
        entry("solve", "solve"),
        entry("review", "review-solve"),
        entry("refine", "refine"),
        entry("dense", "dense"),
        entry("publish", "publish"),
    ];
    w.bindings = vec![
        bind("photos.photos", "solve.photos"),
        bind("photos.photos", "dense.photos"),
        bind("solve.model", "review.model"),
        Binding {
            from: PortRef("solve.model".into()),
            to: PortRef("refine.model".into()),
            guard: Some(Guard::Decision {
                step: "review".into(),
                output: "decision".into(),
                is: "refine".into(),
            }),
        },
        bind("refine.model", "dense.model"),
        bind("solve.model", "dense.model"),
        bind("dense.cloud", "publish.cloud"),
    ];
    store.put_workflow_definition(&w).unwrap();
}

#[test]
fn scenario_a_photogrammetry_layer() {
    let store = Arc::new(MemoryStore::new());
    photogrammetry_library(&store);
    let local = performer("local", Locality::ThisMachine, &["solve", "refine", "publish"]);
    let cloud = performer("batch", Locality::Cloud, &["dense"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone(), cloud.clone()], "photogrammetry-layer", "layer-1");

    // 1. created: photos waits on a person, the rest is not ready
    assert_eq!(rig.status("photos"), StepStatus::Waiting(Waiting::Completion));
    assert_eq!(rig.status("solve"), StepStatus::NotReady);
    assert_eq!(rig.status("refine"), StepStatus::NotReady);

    // 2. photos completed: solve starts on this machine
    rig.command(complete("photos", &[("photos", "ps-1".into())])).unwrap();
    assert!(matches!(rig.command(start("photos")), Err(Error::Refused(_))), "a person's step is completed, not started");
    rig.tick();
    assert_eq!(rig.status("solve"), StepStatus::Running);
    assert_eq!(local.started()[0].inputs["photos"], Value::from("ps-1"));
    assert_eq!(rig.wf().step("solve").unwrap().latest().unwrap().locality, Some(Locality::ThisMachine));

    // 3. solve succeeds; review waits on a person; refine and dense cannot tell yet
    local.finish(&rig.handle(&local, "solve"), Outcome::succeeded(outs(&[("model", "model-1".into())])));
    rig.tick();
    assert_eq!(rig.status("solve"), StepStatus::Succeeded);
    assert_eq!(rig.status("review"), StepStatus::Waiting(Waiting::Completion));
    assert_eq!(rig.status("refine"), StepStatus::NotReady);
    assert_eq!(rig.status("dense"), StepStatus::NotReady, "dense's model may yet come from refine");

    // 4. the decision is refine: the guard holds, refine runs
    rig.command(complete("review", &[("decision", "refine".into())])).unwrap();
    rig.tick();
    assert_eq!(rig.status("refine"), StepStatus::Running);
    local.finish(&rig.handle(&local, "refine"), Outcome::succeeded(outs(&[("model", "model-1r".into())])));
    rig.tick();

    // 5. dense takes refine's model, and waits for the cloud to be confirmed
    assert_eq!(rig.wf().step("dense").unwrap().inputs["model"].value, Some("model-1r".into()));
    assert_eq!(rig.status("dense"), StepStatus::Waiting(Waiting::Confirmation { locality: Locality::Cloud }));
    rig.command(Command::Confirm { step: "dense".into() }).unwrap();
    rig.tick();
    assert_eq!(rig.status("dense"), StepStatus::Running);

    // 6. run 1 lost: the policy retakes once
    cloud.forget(&rig.handle(&cloud, "dense"));
    rig.tick();
    let dense = rig.wf().step("dense").unwrap().clone();
    assert_eq!(dense.runs.len(), 2);
    assert_eq!(dense.runs[0].outcome.as_ref().unwrap().state, RunState::Lost);
    assert_eq!(dense.runs[1].started.by, Actor::policy("retake", "dense"));
    assert!(dense.runs[1].live());

    // 7. run 2 fails out of memory: the policy does not cover it
    cloud.finish(&rig.handle(&cloud, "dense"), Outcome::failed("out-of-memory", "CUDA out of memory at 4096"));
    rig.tick();
    assert_eq!(rig.status("dense"), StepStatus::Failed);
    assert_eq!(rig.workflow_status(), WorkflowStatus::Failed);

    // 8. Max sets the image size and starts run 3; a person's start confirms the placement
    rig.command(param("dense", "image-size", 3200.into())).unwrap();
    rig.command(start("dense")).unwrap();
    rig.tick();
    let dense = rig.wf().step("dense").unwrap().clone();
    assert_eq!(dense.runs.len(), 3);
    assert_eq!(dense.runs[2].parameters["image-size"], Value::from(3200));
    assert_eq!(dense.runs[2].started.by, max());
    cloud.finish(&rig.handle(&cloud, "dense"), Outcome::succeeded(outs(&[("cloud", "cloud-1".into())])));
    rig.tick();

    // 9. publish runs on its own and the workflow succeeds
    assert_eq!(rig.status("publish"), StepStatus::Running);
    local.finish(&rig.handle(&local, "publish"), Outcome::succeeded(outs(&[("layer", "layer-1".into())])));
    rig.tick();
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);
    assert_eq!(rig.wf().step("dense").unwrap().runs.len(), 3, "runs 1 and 2 stay in the record");
    rig.write_fixtures("photogrammetry-layer");
}

#[test]
fn scenario_a_with_accept_skips_refine() {
    let store = Arc::new(MemoryStore::new());
    photogrammetry_library(&store);
    let local = performer("local", Locality::ThisMachine, &["solve", "refine", "dense", "publish"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "photogrammetry-layer", "layer-2");
    rig.command(complete("photos", &[("photos", "ps-1".into())])).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "solve"), Outcome::succeeded(outs(&[("model", "model-1".into())])));
    rig.tick();
    rig.command(complete("review", &[("decision", "accept".into())])).unwrap();
    rig.tick();
    assert_eq!(rig.status("refine"), StepStatus::Skipped);
    assert_eq!(rig.status("dense"), StepStatus::Running, "dense takes solve's model, on this machine with nothing to confirm");
    assert_eq!(rig.wf().step("dense").unwrap().inputs["model"].value, Some("model-1".into()));
    assert!(matches!(rig.command(complete("review", &[("decision", "maybe".into())])), Err(Error::Refused(w)) if w.contains("not one of")));
}

// ---------------------------------------------------------------------
// B. Rerunning upstream: stale.

#[test]
fn scenario_b_rerunning_upstream_makes_downstream_stale() {
    let store = Arc::new(MemoryStore::new());
    photogrammetry_library(&store);
    let local = performer("local", Locality::ThisMachine, &["solve", "refine", "dense", "publish"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "photogrammetry-layer", "layer-3");
    let publish_policy = Policy {
        on_stale: OnStale::Rerun,
        ..Default::default()
    };
    rig.command(Command::SetPolicy {
        step: "publish".into(),
        policy: publish_policy,
    })
    .unwrap();
    rig.command(complete("photos", &[("photos", "ps-1".into())])).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "solve"), Outcome::succeeded(outs(&[("model", "model-1".into())])));
    rig.tick();
    rig.command(complete("review", &[("decision", "accept".into())])).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "dense"), Outcome::succeeded(outs(&[("cloud", "cloud-1".into())])));
    rig.tick();
    local.finish(&rig.handle(&local, "publish"), Outcome::succeeded(outs(&[("layer", "layer-1".into())])));
    rig.tick();
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);

    // 1. a parameter changes: solve is stale, nothing downstream yet
    rig.command(param("solve", "matcher", "sequential".into())).unwrap();
    assert_eq!(rig.status("solve"), StepStatus::Stale);
    assert_eq!(rig.status("dense"), StepStatus::Succeeded);
    assert_eq!(rig.workflow_status(), WorkflowStatus::Stale);
    rig.tick();
    assert_eq!(local.started().len(), 3, "a stale step does not start on its own");

    // 2. retake solve: while it runs, dense still holds the old model
    rig.command(start("solve")).unwrap();
    rig.tick();
    assert_eq!(rig.status("solve"), StepStatus::Running);
    assert_eq!(rig.wf().step("dense").unwrap().inputs["model"].value, Some("model-1".into()));
    local.finish(&rig.handle(&local, "solve"), Outcome::succeeded(outs(&[("model", "model-2".into())])));
    rig.tick();

    // 3. review is stale (a person must look again); dense is stale and waits
    assert_eq!(rig.status("solve"), StepStatus::Succeeded);
    assert_eq!(rig.status("review"), StepStatus::Stale);
    assert_eq!(rig.status("dense"), StepStatus::Stale);
    rig.command(complete("review", &[("decision", "accept".into())])).unwrap();
    assert_eq!(rig.status("review"), StepStatus::Succeeded);
    assert_eq!(rig.status("refine"), StepStatus::Skipped);
    rig.tick();
    assert_eq!(rig.status("dense"), StepStatus::Stale, "on stale: wait");

    // 4. dense retaken; 5. publish reruns on its own
    rig.command(start("dense")).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "dense"), Outcome::succeeded(outs(&[("cloud", "cloud-2".into())])));
    rig.tick();
    assert_eq!(rig.status("publish"), StepStatus::Running, "on stale: rerun");
    assert_eq!(rig.wf().step("publish").unwrap().latest().unwrap().started.by, Actor::policy("rerun-on-stale", "publish"));
    local.finish(&rig.handle(&local, "publish"), Outcome::succeeded(outs(&[("layer", "layer-2".into())])));
    rig.tick();
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);
    let runs = |s: &str| rig.wf().step(s).unwrap().runs.len();
    assert_eq!((runs("solve"), runs("review"), runs("dense"), runs("publish")), (2, 2, 2, 2));

    // a failed retake: the step shows failed, downstream keeps the last good outputs
    rig.command(param("dense", "image-size", 1600.into())).unwrap();
    rig.command(start("dense")).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "dense"), Outcome::failed("failed", "disk full"));
    rig.tick();
    assert_eq!(rig.status("dense"), StepStatus::Failed);
    assert_eq!(rig.wf().step("publish").unwrap().inputs["cloud"].value, Some("cloud-2".into()));
    assert_eq!(rig.status("publish"), StepStatus::Succeeded);
}

// ---------------------------------------------------------------------
// C. Publish as a nested workflow.

#[test]
fn scenario_c_publish_as_a_nested_workflow() {
    let store = Arc::new(MemoryStore::new());
    store.put_step(&verb("dense", &[("photos", "photo-set")], &[], &[("cloud", "point-cloud")])).unwrap();
    store.put_step(&verb("chunk", &[("cloud", "point-cloud")], &[], &[("chunkset", "chunk-set")])).unwrap();
    store.put_step(&verb("add-to-scene", &[("chunkset", "chunk-set")], &[], &[("layer", "layer")])).unwrap();
    store.put_step(&verb("warm", &[("chunkset", "chunk-set")], &[], &[])).unwrap();
    let mut child = WorkflowDefinition::new("publish-layer", 1);
    child.steps = vec![entry("chunk", "chunk"), entry("layers", "add-to-scene"), entry("warm", "warm")];
    child.bindings = vec![bind("chunk.chunkset", "layers.chunkset"), bind("chunk.chunkset", "warm.chunkset")];
    store.put_workflow_definition(&child).unwrap();
    let mut parent = WorkflowDefinition::new("layer", 1);
    parent.steps = vec![entry("dense", "dense"), entry("publish", "publish-layer")];
    parent.bindings = vec![bind("dense.cloud", "publish.chunk.cloud")];
    store.put_workflow_definition(&parent).unwrap();

    // the recipe's derived face
    let face = store.library().unwrap().step(&Ref::new("publish-layer", 1)).unwrap();
    assert_eq!(face.inputs.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["chunk.cloud"]);
    assert_eq!(face.outputs.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(), ["chunk.chunkset", "layers.layer"]);

    let local = performer("local", Locality::ThisMachine, &["dense", "chunk", "add-to-scene", "warm"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "layer", "parent-1");
    rig.command(set("dense", "photos", "ps-1".into())).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "dense"), Outcome::succeeded(outs(&[("cloud", "cloud-1".into())])));
    rig.tick();

    // the child exists, fed by the parent, with chunk running
    let child_id = rig.wf().step("publish").unwrap().latest().unwrap().handle.clone().unwrap();
    let (child_wf, _) = store.get(&child_id).unwrap().unwrap();
    assert_eq!(
        child_wf.created.by,
        Actor::Run {
            workflow: "parent-1".into(),
            step: "publish".into(),
            run: "parent-1/publish/1".into()
        }
    );
    assert_eq!(child_wf.step("chunk").unwrap().inputs["cloud"].value, Some("cloud-1".into()));
    assert_eq!(child_wf.step_status(&child, "chunk"), StepStatus::Running);
    assert_eq!(rig.status("publish"), StepStatus::Running);
    let progress = rig.wf().step("publish").unwrap().latest().unwrap().progress.clone().unwrap();
    assert_eq!((progress.done, progress.total), (Some(0), Some(3)));

    // chunk done: layers and warm run together; one fails, and the parent sees attention, not failure
    let chunk_run = child_wf.step("chunk").unwrap().latest().unwrap().id.clone();
    local.finish(&local.handle_of(&chunk_run).unwrap(), Outcome::succeeded(outs(&[("chunkset", "cs-1".into())])));
    rig.tick();
    let (child_wf, _) = store.get(&child_id).unwrap().unwrap();
    assert_eq!(child_wf.step_status(&child, "layers"), StepStatus::Running);
    assert_eq!(child_wf.step_status(&child, "warm"), StepStatus::Running);
    let layers_run = child_wf.step("layers").unwrap().latest().unwrap().id.clone();
    let warm_run = child_wf.step("warm").unwrap().latest().unwrap().id.clone();
    local.finish(&local.handle_of(&warm_run).unwrap(), Outcome::succeeded(BTreeMap::new()));
    local.finish(&local.handle_of(&layers_run).unwrap(), Outcome::failed("failed", "scene locked"));
    rig.tick();
    assert_eq!(rig.status("publish"), StepStatus::Running);
    let attention = rig.wf().step("publish").unwrap().latest().unwrap().attention.clone().unwrap();
    assert!(attention.contains("layers is failed"), "{attention}");

    // fixed inside the child: the parent proceeds when the child does
    let mut child_driver = Driver::new(store.clone(), "fixer").with_performer(local.clone());
    child_driver.command(&child_id, &start("layers"), &max(), &now(50)).unwrap();
    child_driver = child_driver.with_nesting();
    let _ = &mut child_driver;
    rig.tick();
    let (child_wf, _) = store.get(&child_id).unwrap().unwrap();
    let layers_run = child_wf.step("layers").unwrap().latest().unwrap().id.clone();
    local.finish(&local.handle_of(&layers_run).unwrap(), Outcome::succeeded(outs(&[("layer", "layer-1".into())])));
    rig.tick();
    assert_eq!(rig.status("publish"), StepStatus::Succeeded);
    assert_eq!(rig.wf().step("publish").unwrap().outputs["layers.layer"].value, Some("layer-1".into()));
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);
}

// ---------------------------------------------------------------------
// D. The same verb twice, joined.

#[test]
fn scenario_d_the_same_verb_twice() {
    let store = Arc::new(MemoryStore::new());
    store.put_step(&person_verb("choose-photos", &[], &[("photos", "photo-set")], None)).unwrap();
    store
        .put_step(&verb("detect-targets", &[("images", "photo-set")], &[("min-size", "integer", Some(12.into()))], &[("targets", "target-table")]))
        .unwrap();
    let mut compare = verb(
        "compare-targets",
        &[("left", "target-table"), ("right", "target-table")],
        &[("tolerance", "number", Some(0.005.into()))],
        &[("report", "table")],
    );
    compare.outputs.push(OutputDefinition {
        name: "decision".into(),
        tag: DECISION.into(),
        options: Some(vec!["stable".into(), "moved".into()]),
        label: None,
    });
    store.put_step(&compare).unwrap();
    store.put_step(&person_verb("notify-surveyor", &[("report", "table")], &[], None)).unwrap();
    let mut w = WorkflowDefinition::new("target-drift", 1);
    w.steps = vec![
        entry("early", "choose-photos"),
        entry("late", "choose-photos"),
        entry("detect-early", "detect-targets"),
        entry("detect-late", "detect-targets"),
        entry("compare", "compare-targets"),
        entry("flag", "notify-surveyor"),
    ];
    w.bindings = vec![
        bind("early.photos", "detect-early.images"),
        bind("late.photos", "detect-late.images"),
        bind("detect-early.targets", "compare.left"),
        bind("detect-late.targets", "compare.right"),
        Binding {
            from: PortRef("compare.report".into()),
            to: PortRef("flag.report".into()),
            guard: Some(Guard::Decision {
                step: "compare".into(),
                output: "decision".into(),
                is: "moved".into(),
            }),
        },
    ];
    store.put_workflow_definition(&w).unwrap();
    let local = performer("local", Locality::ThisMachine, &["detect-targets", "compare-targets"]);
    let fleet = performer("ryzenbox", Locality::LocalNetwork, &["detect-targets"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone(), fleet.clone()], "target-drift", "drift-1");

    // one side first: detect-early starts; nothing waits on "the previous step"
    rig.command(complete("early", &[("photos", "ps-early".into())])).unwrap();
    rig.tick();
    assert_eq!(rig.status("detect-early"), StepStatus::Running);
    assert_eq!(rig.status("detect-late"), StepStatus::NotReady);
    assert_eq!(rig.status("compare"), StepStatus::NotReady);

    // the other side, placed on the fleet with its own parameter
    rig.command(complete("late", &[("photos", "ps-late".into())])).unwrap();
    rig.command(param("detect-late", "min-size", 6.into())).unwrap();
    rig.command(Command::Place {
        step: "detect-late".into(),
        performer: "ryzenbox".into(),
        locality: Locality::LocalNetwork,
    })
    .unwrap();
    rig.tick();
    assert_eq!(rig.status("detect-late"), StepStatus::Running);
    assert_eq!(fleet.started()[0].parameters["min-size"], Value::from(6));
    assert_eq!(local.started()[0].parameters["min-size"], Value::from(12));

    // one side fails and is retaken alone
    fleet.finish(&rig.handle(&fleet, "detect-late"), Outcome::failed("failed", "unreadable file"));
    local.finish(&rig.handle(&local, "detect-early"), Outcome::succeeded(outs(&[("targets", "t-early".into())])));
    rig.tick();
    assert_eq!(rig.status("detect-late"), StepStatus::Failed);
    assert_eq!(rig.status("compare"), StepStatus::NotReady);
    rig.command(complete("late", &[("photos", "ps-late-fixed".into())])).unwrap();
    assert_eq!(rig.status("detect-late"), StepStatus::Failed, "a failed step with a new input still waits for a start");
    rig.command(start("detect-late")).unwrap();
    rig.tick();
    fleet.finish(&rig.handle(&fleet, "detect-late"), Outcome::succeeded(outs(&[("targets", "t-late".into())])));
    rig.tick();
    assert_eq!(local.started().len(), 2, "detect-early was untouched; compare started");

    // the join and the branch
    assert_eq!(rig.status("compare"), StepStatus::Running);
    local.finish(&rig.handle(&local, "compare"), Outcome::succeeded(outs(&[("report", "r-1".into()), ("decision", "moved".into())])));
    rig.tick();
    assert_eq!(rig.status("flag"), StepStatus::Waiting(Waiting::Completion));
    assert_eq!(rig.workflow_status(), WorkflowStatus::Waiting);
    rig.command(complete("flag", &[])).unwrap();
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);
}

#[test]
fn a_cancel_stops_the_live_run_and_the_workflow() {
    let store = Arc::new(MemoryStore::new());
    detect_library(&store);
    let local = performer("local", Locality::ThisMachine, &["detect-targets"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "find-targets", "wf-6");
    rig.command(set("detect", "images", serde_json::json!(["a.jpg"]))).unwrap();
    rig.tick();
    rig.command(Command::Cancel {
        step: None,
        reason: Some("changed my mind".into()),
    })
    .unwrap();
    let wf = rig.tick();
    let run = wf.step("detect").unwrap().latest().unwrap();
    assert_eq!(run.outcome.as_ref().unwrap().state, RunState::Canceled);
    assert_eq!(rig.workflow_status(), WorkflowStatus::Canceled);
    assert!(local.run(&rig.handle(&local, "detect")).unwrap().canceled.is_some());
}

#[test]
fn definitions_are_checked() {
    let store = MemoryStore::new();
    detect_library(&store);
    let mut w = WorkflowDefinition::new("bad", 1);
    w.steps = vec![entry("a", "detect-targets"), entry("b", "detect-targets")];
    w.bindings = vec![bind("a.targets", "b.images")];
    assert!(matches!(store.put_workflow_definition(&w), Err(Error::Invalid(m)) if m.contains("tags differ")), "{:?}", store.put_workflow_definition(&w));
    w.bindings = vec![bind("a.nope", "b.images")];
    assert!(matches!(store.put_workflow_definition(&w), Err(Error::Invalid(m)) if m.contains("no output")));
    w.steps = vec![entry("a", "detect-targets"), entry("a", "detect-targets")];
    w.bindings = vec![];
    assert!(matches!(store.put_workflow_definition(&w), Err(Error::Invalid(m)) if m.contains("twice")));
    let mut d = StepDefinition::new("x", 1);
    d.outputs.push(OutputDefinition {
        name: "d".into(),
        tag: "string".into(),
        options: Some(vec!["a".into()]),
        label: None,
    });
    assert!(matches!(store.put_step(&d), Err(Error::Invalid(m)) if m.contains("decision")));
}
