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
        driver.command(id, &Command::StartWorkflow, &max(), &now(1)).unwrap();
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
// The one-step example: add two integers (the first step of the README's walkthrough).

fn add_library(store: &MemoryStore) {
    let mut add = verb("add", &[("a", "integer"), ("b", "integer")], &[("beat-seconds", "number", Some(Value::from(1)))], &[("sum", "integer")]);
    add.label = Some("Add two integers".into());
    store.put_step(&add).unwrap();
    // the verb and the recipe may share a label, never a name: verbs are one namespace
    let mut w = WorkflowDefinition::new("add-two", 1);
    w.label = Some("Add two integers".into());
    w.steps = vec![entry("add", "add")];
    store.put_workflow_definition(&w).unwrap();
}

#[test]
fn one_step_workflow_runs_when_its_inputs_are_given() {
    let store = Arc::new(MemoryStore::new());
    add_library(&store);
    let local = performer("local", Locality::ThisMachine, &["add"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "add-two", "wf-1");

    // created: the step waits for the one input nobody feeds
    assert_eq!(rig.status("add"), StepStatus::Waiting(Waiting::Input { name: "a".into() }));
    assert_eq!(rig.workflow_status(), WorkflowStatus::Waiting);
    rig.tick();
    assert!(local.started().is_empty(), "nothing starts with its gate closed");

    // the two numbers arrive; the gate opens; a tick mints and starts a run
    rig.command(set("add", "a", 2.into())).unwrap();
    assert_eq!(rig.status("add"), StepStatus::Waiting(Waiting::Input { name: "b".into() }));
    rig.command(set("add", "b", 3.into())).unwrap();
    assert_eq!(rig.status("add"), StepStatus::Ready);
    let wf = rig.tick();
    assert_eq!(wf.step_status(&rig.def(), "add"), StepStatus::Running);
    let req = &local.started()[0];
    assert_eq!(req.parameters["beat-seconds"], Value::from(1), "the default is in effect");
    assert_eq!((&req.inputs["a"], &req.inputs["b"]), (&Value::from(2), &Value::from(3)));
    assert_eq!(wf.step("add").unwrap().latest().unwrap().started.by, Actor::policy("start-automatically", "add"));

    // progress is read, then the outcome; the output is bound
    let h = rig.handle(&local, "add");
    local.progress(
        &h,
        Progress {
            phase: Some("adding".into()),
            done: Some(1),
            total: Some(3),
            unit: Some("beats".into()),
            at: now(3),
        },
    );
    let wf = rig.tick();
    assert_eq!(wf.step("add").unwrap().latest().unwrap().progress.as_ref().unwrap().done, Some(1));
    let mut outcome = Outcome::succeeded(outs(&[("sum", 5.into())]));
    outcome.summary = Some(Summary { headline: "2 + 3 = 5".into(), details: None });
    local.finish(&h, outcome);
    let wf = rig.tick();
    assert_eq!(wf.step_status(&rig.def(), "add"), StepStatus::Succeeded);
    assert_eq!(wf.step("add").unwrap().outputs["sum"].value, Some(Value::from(5)));
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);
    assert_eq!(rig.history_kinds(), ["created", "started", "set@add", "set@add", "run-minted@add", "run-started@add", "progress@add", "run-ended@add"]);

    // a set on a succeeded step makes it stale; nothing restarts on its own
    rig.command(param("add", "beat-seconds", Value::from(0))).unwrap();
    assert_eq!(rig.status("add"), StepStatus::Stale);
    rig.tick();
    assert_eq!(local.started().len(), 1);
    rig.command(start("add")).unwrap();
    rig.tick();
    assert_eq!(local.started().len(), 2, "a start retakes a stale step");
    assert_eq!(local.started()[1].parameters["beat-seconds"], Value::from(0));
    assert_eq!(rig.wf().step("add").unwrap().runs.len(), 2, "runs are append-only");
    rig.write_fixtures("add-two");
}

#[test]
fn a_bound_input_cannot_be_set_and_a_running_step_cannot_be_started() {
    let store = Arc::new(MemoryStore::new());
    add_library(&store);
    let local = performer("local", Locality::ThisMachine, &["add"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "add-two", "wf-2");
    rig.command(set("add", "a", 2.into())).unwrap();
    rig.command(set("add", "b", 3.into())).unwrap();
    rig.tick();
    assert!(matches!(rig.command(start("add")), Err(Error::Refused(w)) if w.contains("running")));
    assert!(matches!(rig.command(set("add", "nope", Value::Null)), Err(Error::Refused(w)) if w.contains("running")));
}

#[test]
fn a_refusal_is_a_failed_run_that_never_ran() {
    let store = Arc::new(MemoryStore::new());
    add_library(&store);
    let local = performer("local", Locality::ThisMachine, &["add"]);
    local.set_refusal(Some("no such file notes.txt"));
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "add-two", "wf-3");
    rig.command(set("add", "a", 2.into())).unwrap();
    rig.command(set("add", "b", 3.into())).unwrap();
    let wf = rig.tick();
    let run = wf.step("add").unwrap().latest().unwrap();
    assert_eq!(run.outcome.as_ref().unwrap().class.as_deref(), Some("refused"));
    assert_eq!(rig.status("add"), StepStatus::Failed);
    assert_eq!(rig.workflow_status(), WorkflowStatus::Failed);
}

#[test]
fn an_unreachable_performer_changes_nothing() {
    let store = Arc::new(MemoryStore::new());
    add_library(&store);
    let local = performer("local", Locality::ThisMachine, &["add"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "add-two", "wf-4");
    rig.command(set("add", "a", 2.into())).unwrap();
    rig.command(set("add", "b", 3.into())).unwrap();
    rig.tick();
    let before = rig.wf();
    local.set_unreachable(Some("network"));
    rig.tick();
    assert_eq!(rig.wf(), before);
    local.set_unreachable(None);
    local.forget(&rig.handle(&local, "add"));
    let wf = rig.tick();
    assert_eq!(wf.step("add").unwrap().latest().unwrap().outcome.as_ref().unwrap().state, RunState::Lost, "absent is lost");
}

#[test]
fn a_success_without_the_declared_outputs_is_a_fault() {
    let store = Arc::new(MemoryStore::new());
    add_library(&store);
    let local = performer("local", Locality::ThisMachine, &["add"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "add-two", "wf-5");
    rig.command(set("add", "a", 2.into())).unwrap();
    rig.command(set("add", "b", 3.into())).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "add"), Outcome::succeeded(BTreeMap::new()));
    let wf = rig.tick();
    let o = wf.step("add").unwrap().latest().unwrap().outcome.clone().unwrap();
    assert_eq!((o.state, o.class.as_deref()), (RunState::Failed, Some("fault")));
}

// ---------------------------------------------------------------------
// A. A report: notes, a draft, a review, a revision, typesetting, publishing.

fn report_library(store: &MemoryStore) {
    store.put_step(&person_verb("gather-notes", &[], &[("notes", "notes")], None)).unwrap();
    store.put_step(&verb("draft", &[("notes", "notes")], &[("style", "string", Some("plain".into()))], &[("text", "text")])).unwrap();
    store.put_step(&person_verb("review-draft", &[("text", "text")], &[], Some(("decision", &["accept", "revise"])))).unwrap();
    store.put_step(&verb("revise", &[("text", "text")], &[], &[("text", "text")])).unwrap();
    let mut typeset = verb("typeset", &[("text", "text"), ("notes", "notes")], &[("dpi", "integer", Some(600.into()))], &[("document", "document")]);
    typeset.policy.placement.confirm = vec![Locality::Cloud];
    typeset.policy.on_failure = OnFailure::Retake { max: 1, classes: vec!["lost".into()] };
    store.put_step(&typeset).unwrap();
    store.put_step(&verb("publish", &[("document", "document")], &[], &[("release", "release")])).unwrap();
    let mut w = WorkflowDefinition::new("report", 1);
    w.steps = vec![
        entry("notes", "gather-notes"),
        entry("draft", "draft"),
        entry("review", "review-draft"),
        entry("revise", "revise"),
        entry("typeset", "typeset"),
        entry("publish", "publish"),
    ];
    w.bindings = vec![
        bind("notes.notes", "draft.notes"),
        bind("notes.notes", "typeset.notes"),
        bind("draft.text", "review.text"),
        Binding {
            from: PortRef("draft.text".into()),
            to: PortRef("revise.text".into()),
            guard: Some(Guard::Decision {
                step: "review".into(),
                output: "decision".into(),
                is: "revise".into(),
            }),
        },
        bind("revise.text", "typeset.text"),
        bind("draft.text", "typeset.text"),
        bind("typeset.document", "publish.document"),
    ];
    store.put_workflow_definition(&w).unwrap();
}

#[test]
fn scenario_a_report() {
    let store = Arc::new(MemoryStore::new());
    report_library(&store);
    let local = performer("local", Locality::ThisMachine, &["draft", "revise", "publish"]);
    let cloud = performer("batch", Locality::Cloud, &["typeset"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone(), cloud.clone()], "report", "report-1");

    // 1. created: notes waits on a person, the rest is not ready
    assert_eq!(rig.status("notes"), StepStatus::Waiting(Waiting::Completion));
    assert_eq!(rig.status("draft"), StepStatus::NotReady);
    assert_eq!(rig.status("revise"), StepStatus::NotReady);

    // 2. notes completed: draft starts on this machine
    rig.command(complete("notes", &[("notes", "notes-1".into())])).unwrap();
    assert!(matches!(rig.command(start("notes")), Err(Error::Refused(_))), "a person's step is completed, not started");
    rig.tick();
    assert_eq!(rig.status("draft"), StepStatus::Running);
    assert_eq!(local.started()[0].inputs["notes"], Value::from("notes-1"));
    assert_eq!(rig.wf().step("draft").unwrap().latest().unwrap().locality, Some(Locality::ThisMachine));

    // 3. draft succeeds; review waits on a person; revise and typeset cannot tell yet
    local.finish(&rig.handle(&local, "draft"), Outcome::succeeded(outs(&[("text", "text-1".into())])));
    rig.tick();
    assert_eq!(rig.status("draft"), StepStatus::Succeeded);
    assert_eq!(rig.status("review"), StepStatus::Waiting(Waiting::Completion));
    assert_eq!(rig.status("revise"), StepStatus::NotReady);
    assert_eq!(rig.status("typeset"), StepStatus::NotReady, "typeset's text may yet come from revise");

    // 4. the decision is revise: the guard holds, revise runs
    rig.command(complete("review", &[("decision", "revise".into())])).unwrap();
    rig.tick();
    assert_eq!(rig.status("revise"), StepStatus::Running);
    local.finish(&rig.handle(&local, "revise"), Outcome::succeeded(outs(&[("text", "text-1r".into())])));
    rig.tick();

    // 5. typeset takes revise's text, and waits for the cloud to be confirmed
    assert_eq!(rig.wf().step("typeset").unwrap().inputs["text"].value, Some("text-1r".into()));
    assert_eq!(rig.status("typeset"), StepStatus::Waiting(Waiting::Confirmation { locality: Locality::Cloud }));
    rig.command(Command::Confirm { step: "typeset".into() }).unwrap();
    rig.tick();
    assert_eq!(rig.status("typeset"), StepStatus::Running);

    // 6. run 1 lost: the policy retakes once
    cloud.forget(&rig.handle(&cloud, "typeset"));
    rig.tick();
    let typeset = rig.wf().step("typeset").unwrap().clone();
    assert_eq!(typeset.runs.len(), 2);
    assert_eq!(typeset.runs[0].outcome.as_ref().unwrap().state, RunState::Lost);
    assert_eq!(typeset.runs[1].started.by, Actor::policy("retake", "typeset"));
    assert!(typeset.runs[1].live());

    // 7. run 2 fails out of memory: the policy does not cover it
    cloud.finish(&rig.handle(&cloud, "typeset"), Outcome::failed("out-of-memory", "out of memory at 600 dpi"));
    rig.tick();
    assert_eq!(rig.status("typeset"), StepStatus::Failed);
    assert_eq!(rig.workflow_status(), WorkflowStatus::Failed);

    // 8. a person sets the dpi and starts run 3; a person's start confirms the placement
    rig.command(param("typeset", "dpi", 300.into())).unwrap();
    rig.command(start("typeset")).unwrap();
    rig.tick();
    let typeset = rig.wf().step("typeset").unwrap().clone();
    assert_eq!(typeset.runs.len(), 3);
    assert_eq!(typeset.runs[2].parameters["dpi"], Value::from(300));
    assert_eq!(typeset.runs[2].started.by, max());
    cloud.finish(&rig.handle(&cloud, "typeset"), Outcome::succeeded(outs(&[("document", "doc-1".into())])));
    rig.tick();

    // 9. publish runs on its own and the workflow succeeds
    assert_eq!(rig.status("publish"), StepStatus::Running);
    local.finish(&rig.handle(&local, "publish"), Outcome::succeeded(outs(&[("release", "release-1".into())])));
    rig.tick();
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);
    assert_eq!(rig.wf().step("typeset").unwrap().runs.len(), 3, "runs 1 and 2 stay in the record");
    rig.write_fixtures("report");
}

#[test]
fn scenario_a_with_accept_skips_revise() {
    let store = Arc::new(MemoryStore::new());
    report_library(&store);
    let local = performer("local", Locality::ThisMachine, &["draft", "revise", "typeset", "publish"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "report", "report-2");
    rig.command(complete("notes", &[("notes", "notes-1".into())])).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "draft"), Outcome::succeeded(outs(&[("text", "text-1".into())])));
    rig.tick();
    rig.command(complete("review", &[("decision", "accept".into())])).unwrap();
    rig.tick();
    assert_eq!(rig.status("revise"), StepStatus::Skipped);
    assert_eq!(rig.status("typeset"), StepStatus::Running, "typeset takes draft's text, on this machine with nothing to confirm");
    assert_eq!(rig.wf().step("typeset").unwrap().inputs["text"].value, Some("text-1".into()));
    assert!(matches!(rig.command(complete("review", &[("decision", "maybe".into())])), Err(Error::Refused(w)) if w.contains("not one of")));
}

// ---------------------------------------------------------------------
// B. Rerunning upstream: stale.

#[test]
fn scenario_b_rerunning_upstream_makes_downstream_stale() {
    let store = Arc::new(MemoryStore::new());
    report_library(&store);
    let local = performer("local", Locality::ThisMachine, &["draft", "revise", "typeset", "publish"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "report", "report-3");
    let publish_policy = Policy {
        on_stale: OnStale::Rerun,
        ..Default::default()
    };
    rig.command(Command::SetPolicy {
        step: "publish".into(),
        policy: publish_policy,
    })
    .unwrap();
    rig.command(complete("notes", &[("notes", "notes-1".into())])).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "draft"), Outcome::succeeded(outs(&[("text", "text-1".into())])));
    rig.tick();
    rig.command(complete("review", &[("decision", "accept".into())])).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "typeset"), Outcome::succeeded(outs(&[("document", "doc-1".into())])));
    rig.tick();
    local.finish(&rig.handle(&local, "publish"), Outcome::succeeded(outs(&[("release", "release-1".into())])));
    rig.tick();
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);

    // 1. a parameter changes: draft is stale, nothing downstream yet
    rig.command(param("draft", "style", "formal".into())).unwrap();
    assert_eq!(rig.status("draft"), StepStatus::Stale);
    assert_eq!(rig.status("typeset"), StepStatus::Succeeded);
    assert_eq!(rig.workflow_status(), WorkflowStatus::Stale);
    rig.tick();
    assert_eq!(local.started().len(), 3, "a stale step does not start on its own");

    // 2. retake draft: while it runs, typeset still holds the old text
    rig.command(start("draft")).unwrap();
    rig.tick();
    assert_eq!(rig.status("draft"), StepStatus::Running);
    assert_eq!(rig.wf().step("typeset").unwrap().inputs["text"].value, Some("text-1".into()));
    local.finish(&rig.handle(&local, "draft"), Outcome::succeeded(outs(&[("text", "text-2".into())])));
    rig.tick();

    // 3. review is stale (a person must look again); typeset is stale and waits
    assert_eq!(rig.status("draft"), StepStatus::Succeeded);
    assert_eq!(rig.status("review"), StepStatus::Stale);
    assert_eq!(rig.status("typeset"), StepStatus::Stale);
    rig.command(complete("review", &[("decision", "accept".into())])).unwrap();
    assert_eq!(rig.status("review"), StepStatus::Succeeded);
    assert_eq!(rig.status("revise"), StepStatus::Skipped);
    rig.tick();
    assert_eq!(rig.status("typeset"), StepStatus::Stale, "on stale: wait");

    // 4. typeset retaken; 5. publish reruns on its own
    rig.command(start("typeset")).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "typeset"), Outcome::succeeded(outs(&[("document", "doc-2".into())])));
    rig.tick();
    assert_eq!(rig.status("publish"), StepStatus::Running, "on stale: rerun");
    assert_eq!(rig.wf().step("publish").unwrap().latest().unwrap().started.by, Actor::policy("rerun-on-stale", "publish"));
    local.finish(&rig.handle(&local, "publish"), Outcome::succeeded(outs(&[("release", "release-2".into())])));
    rig.tick();
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);
    let runs = |s: &str| rig.wf().step(s).unwrap().runs.len();
    assert_eq!((runs("draft"), runs("review"), runs("typeset"), runs("publish")), (2, 2, 2, 2));

    // a failed retake: the step shows failed, downstream keeps the last good outputs
    rig.command(param("typeset", "dpi", 150.into())).unwrap();
    rig.command(start("typeset")).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "typeset"), Outcome::failed("failed", "disk full"));
    rig.tick();
    assert_eq!(rig.status("typeset"), StepStatus::Failed);
    assert_eq!(rig.wf().step("publish").unwrap().inputs["document"].value, Some("doc-2".into()));
    assert_eq!(rig.status("publish"), StepStatus::Succeeded);
}

// ---------------------------------------------------------------------
// C. Publish as a nested workflow.

#[test]
fn scenario_c_publish_as_a_nested_workflow() {
    let store = Arc::new(MemoryStore::new());
    store.put_step(&verb("typeset", &[("notes", "notes")], &[], &[("document", "document")])).unwrap();
    store.put_step(&verb("package", &[("document", "document")], &[], &[("bundle", "bundle")])).unwrap();
    store.put_step(&verb("upload", &[("bundle", "bundle")], &[], &[("release", "release")])).unwrap();
    store.put_step(&verb("announce", &[("bundle", "bundle")], &[], &[])).unwrap();
    let mut child = WorkflowDefinition::new("publish-document", 1);
    child.steps = vec![entry("package", "package"), entry("upload", "upload"), entry("announce", "announce")];
    child.bindings = vec![bind("package.bundle", "upload.bundle"), bind("package.bundle", "announce.bundle")];
    store.put_workflow_definition(&child).unwrap();
    let mut parent = WorkflowDefinition::new("release", 1);
    parent.steps = vec![entry("typeset", "typeset"), entry("publish", "publish-document")];
    parent.bindings = vec![bind("typeset.document", "publish.package.document")];
    store.put_workflow_definition(&parent).unwrap();

    // the recipe's derived face
    let face = store.library().unwrap().step(&Ref::new("publish-document", 1)).unwrap();
    assert_eq!(face.inputs.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["package.document"]);
    assert_eq!(face.outputs.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(), ["package.bundle", "upload.release"]);

    let local = performer("local", Locality::ThisMachine, &["typeset", "package", "upload", "announce"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "release", "parent-1");
    rig.command(set("typeset", "notes", "notes-1".into())).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "typeset"), Outcome::succeeded(outs(&[("document", "doc-1".into())])));
    rig.tick();

    // the child exists, fed by the parent, with package running
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
    assert_eq!(child_wf.step("package").unwrap().inputs["document"].value, Some("doc-1".into()));
    assert_eq!(child_wf.step_status(&child, "package"), StepStatus::Running);
    assert_eq!(rig.status("publish"), StepStatus::Running);
    let progress = rig.wf().step("publish").unwrap().latest().unwrap().progress.clone().unwrap();
    assert_eq!((progress.done, progress.total), (Some(0), Some(3)));

    // package done: upload and announce run together; one fails, and the parent sees attention, not failure
    let package_run = child_wf.step("package").unwrap().latest().unwrap().id.clone();
    local.finish(&local.handle_of(&package_run).unwrap(), Outcome::succeeded(outs(&[("bundle", "bundle-1".into())])));
    rig.tick();
    let (child_wf, _) = store.get(&child_id).unwrap().unwrap();
    assert_eq!(child_wf.step_status(&child, "upload"), StepStatus::Running);
    assert_eq!(child_wf.step_status(&child, "announce"), StepStatus::Running);
    let upload_run = child_wf.step("upload").unwrap().latest().unwrap().id.clone();
    let announce_run = child_wf.step("announce").unwrap().latest().unwrap().id.clone();
    local.finish(&local.handle_of(&announce_run).unwrap(), Outcome::succeeded(BTreeMap::new()));
    local.finish(&local.handle_of(&upload_run).unwrap(), Outcome::failed("failed", "server busy"));
    rig.tick();
    assert_eq!(rig.status("publish"), StepStatus::Running);
    let attention = rig.wf().step("publish").unwrap().latest().unwrap().attention.clone().unwrap();
    assert!(attention.contains("upload is failed"), "{attention}");

    // fixed inside the child: the parent proceeds when the child does
    let mut child_driver = Driver::new(store.clone(), "fixer").with_performer(local.clone());
    child_driver.command(&child_id, &start("upload"), &max(), &now(50)).unwrap();
    child_driver = child_driver.with_nesting();
    let _ = &mut child_driver;
    rig.tick();
    let (child_wf, _) = store.get(&child_id).unwrap().unwrap();
    let upload_run = child_wf.step("upload").unwrap().latest().unwrap().id.clone();
    local.finish(&local.handle_of(&upload_run).unwrap(), Outcome::succeeded(outs(&[("release", "release-1".into())])));
    rig.tick();
    assert_eq!(rig.status("publish"), StepStatus::Succeeded);
    assert_eq!(rig.wf().step("publish").unwrap().outputs["upload.release"].value, Some("release-1".into()));
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);
}

// ---------------------------------------------------------------------
// D. The same verb twice, joined.

#[test]
fn scenario_d_the_same_verb_twice() {
    let store = Arc::new(MemoryStore::new());
    store.put_step(&person_verb("take-sample", &[], &[("sample", "sample")], None)).unwrap();
    store.put_step(&verb("measure", &[("sample", "sample")], &[("resolution", "integer", Some(12.into()))], &[("reading", "reading")])).unwrap();
    let mut compare = verb("compare-readings", &[("left", "reading"), ("right", "reading")], &[("tolerance", "number", Some(0.005.into()))], &[("report", "table")]);
    compare.outputs.push(OutputDefinition {
        name: "decision".into(),
        tag: DECISION.into(),
        options: Some(vec!["stable".into(), "moved".into()]),
        label: None,
    });
    store.put_step(&compare).unwrap();
    store.put_step(&person_verb("notify-owner", &[("report", "table")], &[], None)).unwrap();
    let mut w = WorkflowDefinition::new("reading-drift", 1);
    w.steps = vec![
        entry("early", "take-sample"),
        entry("late", "take-sample"),
        entry("measure-early", "measure"),
        entry("measure-late", "measure"),
        entry("compare", "compare-readings"),
        entry("flag", "notify-owner"),
    ];
    w.bindings = vec![
        bind("early.sample", "measure-early.sample"),
        bind("late.sample", "measure-late.sample"),
        bind("measure-early.reading", "compare.left"),
        bind("measure-late.reading", "compare.right"),
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
    let local = performer("local", Locality::ThisMachine, &["measure", "compare-readings"]);
    let workstation = performer("workstation", Locality::LocalNetwork, &["measure"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone(), workstation.clone()], "reading-drift", "drift-1");

    // one side first: measure-early starts; nothing waits on "the previous step"
    rig.command(complete("early", &[("sample", "s-early".into())])).unwrap();
    rig.tick();
    // two Performers offer the verb and nobody chose: it waits to be placed
    assert_eq!(rig.status("measure-early"), StepStatus::Waiting(Waiting::Placement));
    assert!(local.started().is_empty() && workstation.started().is_empty());
    rig.command(Command::Place {
        step: "measure-early".into(),
        performer: "local".into(),
        locality: Locality::ThisMachine,
    })
    .unwrap();
    rig.tick();
    assert_eq!(rig.status("measure-early"), StepStatus::Running);
    assert_eq!(rig.status("measure-late"), StepStatus::NotReady);
    assert_eq!(rig.status("compare"), StepStatus::NotReady);

    // the other side, placed on the workstation with its own parameter
    rig.command(complete("late", &[("sample", "s-late".into())])).unwrap();
    rig.command(param("measure-late", "resolution", 6.into())).unwrap();
    rig.command(Command::Place {
        step: "measure-late".into(),
        performer: "workstation".into(),
        locality: Locality::LocalNetwork,
    })
    .unwrap();
    rig.tick();
    assert_eq!(rig.status("measure-late"), StepStatus::Running);
    assert_eq!(workstation.started()[0].parameters["resolution"], Value::from(6));
    assert_eq!(local.started()[0].parameters["resolution"], Value::from(12));

    // one side fails and is retaken alone
    workstation.finish(&rig.handle(&workstation, "measure-late"), Outcome::failed("failed", "unreadable file"));
    local.finish(&rig.handle(&local, "measure-early"), Outcome::succeeded(outs(&[("reading", "r-early".into())])));
    rig.tick();
    assert_eq!(rig.status("measure-late"), StepStatus::Failed);
    assert_eq!(rig.status("compare"), StepStatus::NotReady);
    rig.command(complete("late", &[("sample", "s-late-fixed".into())])).unwrap();
    assert_eq!(rig.status("measure-late"), StepStatus::Failed, "a failed step with a new input still waits for a start");
    rig.command(start("measure-late")).unwrap();
    rig.tick();
    workstation.finish(&rig.handle(&workstation, "measure-late"), Outcome::succeeded(outs(&[("reading", "r-late".into())])));
    rig.tick();
    assert_eq!(local.started().len(), 2, "measure-early was untouched; compare started");

    // the join and the branch
    assert_eq!(rig.status("compare"), StepStatus::Running);
    local.finish(&rig.handle(&local, "compare"), Outcome::succeeded(outs(&[("report", "r-1".into()), ("decision", "moved".into())])));
    rig.tick();
    assert_eq!(rig.status("flag"), StepStatus::Waiting(Waiting::Completion));
    assert_eq!(rig.workflow_status(), WorkflowStatus::Waiting);
    rig.command(complete("flag", &[])).unwrap();
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);
}

/// Two drivers on one store, each with its own Performer: a Run placed
/// on the other's is minted and left alone here, started and ended
/// there, and seen here as it ends. Neither fails what is not its own.
#[test]
fn drivers_that_share_a_store_each_run_their_own() {
    let store = Arc::new(MemoryStore::new());
    add_library(&store);
    let desk = performer("desk", Locality::ThisMachine, &["add"]);
    let bench = performer("bench", Locality::LocalNetwork, &["add"]);
    let here = Driver::new(store.clone(), "here").with_nesting().with_performer(desk.clone()).with_elsewhere(vec![bench.describe()]);
    let there = Driver::new(store.clone(), "there").with_nesting().with_performer(bench.clone()).with_elsewhere(vec![desk.describe()]);
    here.create(&Ref::new("add-two", 1), "wf-s", None, &max(), &now(0)).unwrap();
    here.command("wf-s", &Command::StartWorkflow, &max(), &now(0)).unwrap();
    here.command("wf-s", &set("add", "a", 2.into()), &max(), &now(1)).unwrap();
    here.command("wf-s", &set("add", "b", 3.into()), &max(), &now(1)).unwrap();
    let status = |_: &Driver| {
        let lib = store.library().unwrap();
        let wf = store.get("wf-s").unwrap().unwrap().0;
        wf.step_status(lib.workflow(&wf.definition).unwrap(), "add")
    };
    // both offer it: nobody starts anything until it is placed
    here.tick_all(&now(2), &now(9)).unwrap();
    there.tick_all(&now(2), &now(9)).unwrap();
    assert_eq!(status(&here), StepStatus::Waiting(Waiting::Placement));
    // placed on the other driver's Performer, from this one
    here.command(
        "wf-s",
        &Command::Place {
            step: "add".into(),
            performer: "bench".into(),
            locality: Locality::LocalNetwork,
        },
        &max(),
        &now(3),
    )
    .unwrap();
    here.tick_all(&now(4), &now(9)).unwrap();
    here.tick_all(&now(5), &now(9)).unwrap();
    let wf = store.get("wf-s").unwrap().unwrap().0;
    let run = wf.steps["add"].latest().unwrap();
    assert!(run.live() && run.handle.is_none() && run.performer == "bench", "minted here, not started, not failed: {run:?}");
    assert!(desk.started().is_empty());
    // its own driver starts it, and this one watches without touching
    there.tick_all(&now(6), &now(9)).unwrap();
    assert_eq!(bench.started().len(), 1);
    here.tick_all(&now(7), &now(9)).unwrap();
    assert_eq!(status(&here), StepStatus::Running);
    let handle = bench.handle_of(&store.get("wf-s").unwrap().unwrap().0.steps["add"].latest().unwrap().id).unwrap();
    bench.finish(&handle, Outcome::succeeded(outs(&[("sum", 5.into())])));
    here.tick_all(&now(8), &now(9)).unwrap();
    assert_eq!(status(&here), StepStatus::Running, "only its own driver hears it end");
    there.tick_all(&now(8), &now(9)).unwrap();
    assert_eq!(status(&here), StepStatus::Succeeded);
    // a Performer no driver of the store is known to have is still a refusal
    let alone = Driver::new(store.clone(), "alone").with_nesting();
    alone.create(&Ref::new("add-two", 1), "wf-t", None, &max(), &now(0)).unwrap();
    alone.command("wf-t", &Command::StartWorkflow, &max(), &now(0)).unwrap();
    alone.command("wf-t", &set("add", "a", 2.into()), &max(), &now(1)).unwrap();
    alone.command("wf-t", &set("add", "b", 3.into()), &max(), &now(1)).unwrap();
    alone.tick_all(&now(2), &now(9)).unwrap();
    let lib = store.library().unwrap();
    let wf = store.get("wf-t").unwrap().unwrap().0;
    assert!(wf.steps["add"].runs.is_empty(), "nobody offers it: nothing is minted");
    assert_eq!(wf.gate(lib.workflow(&wf.definition).unwrap(), "add", &alone_faces(&alone)), Gate::Closed(StepStatus::Waiting(Waiting::Performer)));
}

fn alone_faces(d: &Driver) -> Vec<PerformerFace> {
    d.performers.iter().map(|p| p.describe()).collect()
}

/// A root Workflow runs nothing until it is started, however ready its
/// Steps are (Max, 2026-10-13); a nested one has its go from creation.
#[test]
fn a_root_workflow_waits_for_its_start_and_a_nested_one_does_not() {
    let store = Arc::new(MemoryStore::new());
    add_library(&store);
    let local = performer("local", Locality::ThisMachine, &["add"]);
    let driver = Driver::new(store.clone(), "test").with_nesting().with_performer(local.clone());
    driver.create(&Ref::new("add-two", 1), "wf-r", None, &max(), &now(0)).unwrap();
    driver.command("wf-r", &set("add", "a", 2.into()), &max(), &now(1)).unwrap();
    driver.command("wf-r", &set("add", "b", 3.into()), &max(), &now(1)).unwrap();
    driver.tick_all(&now(2), &now(9)).unwrap();
    let lib = store.library().unwrap();
    let wf = store.get("wf-r").unwrap().unwrap().0;
    let def = lib.workflow(&wf.definition).unwrap().clone();
    assert_eq!(wf.step_status(&def, "add"), StepStatus::Waiting(Waiting::Start));
    assert_eq!(wf.status(&def), WorkflowStatus::Waiting);
    assert!(local.started().is_empty(), "nothing is minted before the go");
    stepmeld_core::contracts::validate(&serde_json::to_value(&wf).unwrap(), "stepmeld/workflow.v1").unwrap();
    driver.command("wf-r", &Command::StartWorkflow, &max(), &now(3)).unwrap();
    assert!(matches!(driver.command("wf-r", &Command::StartWorkflow, &max(), &now(3)), Err(Error::Refused(w)) if w.contains("already started")));
    driver.tick_all(&now(4), &now(9)).unwrap();
    assert_eq!(local.started().len(), 1);
    let kinds: Vec<String> = store.history("wf-r").unwrap().iter().map(|e| e.kind.clone()).collect();
    assert_eq!(&kinds[..2], ["created", "set"]);
    assert!(kinds.contains(&"started".to_string()));
    for e in store.history("wf-r").unwrap() {
        stepmeld_core::contracts::validate(&serde_json::to_value(&e).unwrap(), "stepmeld/history.v1").unwrap();
    }
    // a nested child: the parent's Run is its go
    let mut outer = WorkflowDefinition::new("outer", 1);
    outer.steps = vec![entry("inner", "add-two")];
    store.put_workflow_definition(&outer).unwrap();
    driver.create(&Ref::new("outer", 1), "wf-o", None, &max(), &now(5)).unwrap();
    driver.command("wf-o", &set("inner", "add.a", 4.into()), &max(), &now(6)).unwrap();
    driver.command("wf-o", &set("inner", "add.b", 5.into()), &max(), &now(6)).unwrap();
    driver.command("wf-o", &Command::StartWorkflow, &max(), &now(7)).unwrap();
    driver.tick_all(&now(8), &now(20)).unwrap();
    let child = store.list().unwrap().into_iter().find(|id| id.starts_with("wf-o~")).expect("a child was created");
    assert!(store.get(&child).unwrap().unwrap().0.started.is_some(), "the child has its go from creation");
    assert_eq!(local.started().len(), 2, "and ran");
}

#[test]
fn a_cancel_stops_the_live_run_and_the_workflow() {
    let store = Arc::new(MemoryStore::new());
    add_library(&store);
    let local = performer("local", Locality::ThisMachine, &["add"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "add-two", "wf-6");
    rig.command(set("add", "a", 2.into())).unwrap();
    rig.command(set("add", "b", 3.into())).unwrap();
    rig.tick();
    rig.command(Command::Cancel {
        step: None,
        reason: Some("changed my mind".into()),
    })
    .unwrap();
    let wf = rig.tick();
    let run = wf.step("add").unwrap().latest().unwrap();
    assert_eq!(run.outcome.as_ref().unwrap().state, RunState::Canceled);
    assert_eq!(rig.workflow_status(), WorkflowStatus::Canceled);
    assert!(local.run(&rig.handle(&local, "add")).unwrap().canceled.is_some());
}

// ---------------------------------------------------------------------
// E. Skipping a Step nobody will ever feed.

/// A -> B -> C: B takes A's document and a glossary nobody feeds; C's
/// document comes from B, else from A (`fallback`), or from B alone.
fn skip_library(store: &MemoryStore, fallback: bool) {
    store.put_step(&verb("typeset", &[("notes", "notes")], &[], &[("document", "document")])).unwrap();
    store.put_step(&verb("translate", &[("document", "document"), ("glossary", "glossary")], &[], &[("document", "document")])).unwrap();
    store.put_step(&verb("publish", &[("document", "document")], &[], &[("release", "release")])).unwrap();
    let mut w = WorkflowDefinition::new("translated-report", 1);
    w.steps = vec![entry("typeset", "typeset"), entry("translate", "translate"), entry("publish", "publish")];
    w.bindings = vec![bind("typeset.document", "translate.document"), bind("translate.document", "publish.document")];
    if fallback {
        w.bindings.push(bind("typeset.document", "publish.document"));
    }
    store.put_workflow_definition(&w).unwrap();
}

fn skip(step: &str) -> Command {
    Command::Skip { step: step.into() }
}

/// draft -> outline (an outline out) -> typeset, whose `outline` Input
/// is OPTIONAL and bound from outline: typeset follows the outline when
/// there is one, and goes without when outline is skipped.
fn optional_bound_library(store: &MemoryStore) {
    store.put_step(&verb("draft", &[("notes", "notes")], &[], &[("text", "text")])).unwrap();
    store.put_step(&verb("outline", &[("text", "text")], &[], &[("outline", "outline")])).unwrap();
    let mut typeset = verb("typeset", &[("text", "text"), ("outline", "outline")], &[], &[("document", "document")]);
    typeset.inputs[1].required = false;
    store.put_step(&typeset).unwrap();
    let mut w = WorkflowDefinition::new("outlined", 1);
    w.steps = vec![entry("draft", "draft"), entry("outline", "outline"), entry("typeset", "typeset")];
    w.bindings = vec![bind("draft.text", "outline.text"), bind("draft.text", "typeset.text"), bind("outline.outline", "typeset.outline")];
    store.put_workflow_definition(&w).unwrap();
}

/// An optional Input fed by a Binding waits for its source like a
/// required one (before, a step started the moment its required Input
/// arrived and went stale when the optional one did). Skipping the source
/// lets the step run without it.
#[test]
fn scenario_f_an_optional_bound_input_waits_for_its_source() {
    let store = Arc::new(MemoryStore::new());
    optional_bound_library(&store);
    let local = performer("local", Locality::ThisMachine, &["draft", "outline", "typeset"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "outlined", "opt-1");
    rig.command(set("draft", "notes", "notes-1".into())).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "draft"), Outcome::succeeded(outs(&[("text", "text-1".into())])));
    rig.tick();
    // the text is there, the outline may still come: typeset waits
    assert_eq!(rig.status("outline"), StepStatus::Running);
    assert_eq!(rig.status("typeset"), StepStatus::NotReady, "an optional bound Input waits for its source");
    local.finish(&rig.handle(&local, "outline"), Outcome::succeeded(outs(&[("outline", "outline-1".into())])));
    rig.tick();
    assert_eq!(rig.status("typeset"), StepStatus::Running);
    let typeset_start = local.started().into_iter().find(|r| r.step == "typeset").unwrap();
    assert_eq!(typeset_start.inputs["outline"], Value::from("outline-1"));
    local.finish(&rig.handle(&local, "typeset"), Outcome::succeeded(outs(&[("document", "doc-1".into())])));
    rig.tick();
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);

    // the same recipe with the outline skipped: typeset runs without one
    let store = Arc::new(MemoryStore::new());
    optional_bound_library(&store);
    let local = performer("local", Locality::ThisMachine, &["draft", "outline", "typeset"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "outlined", "opt-2");
    rig.command(set("draft", "notes", "notes-1".into())).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "draft"), Outcome::succeeded(outs(&[("text", "text-1".into())])));
    rig.command(skip("outline")).unwrap();
    rig.tick();
    assert_eq!(rig.status("typeset"), StepStatus::Running);
    let typeset_start = local.started().into_iter().find(|r| r.step == "typeset").unwrap();
    assert!(!typeset_start.inputs.contains_key("outline"), "skipped: the optional Input is simply absent");
}

#[test]
fn scenario_e_a_skipped_step_lets_the_next_binding_carry() {
    let store = Arc::new(MemoryStore::new());
    skip_library(&store, true);
    let local = performer("local", Locality::ThisMachine, &["typeset", "translate", "publish"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "translated-report", "skip-1");

    // typeset runs; translate then waits on the glossary nobody feeds, and
    // publish cannot tell yet which binding will carry: the workflow
    // would wait forever
    rig.command(set("typeset", "notes", "notes-1".into())).unwrap();
    rig.tick();
    assert!(matches!(rig.command(skip("typeset")), Err(Error::Refused(w)) if w.contains("running")), "a running step is not skipped");
    local.finish(&rig.handle(&local, "typeset"), Outcome::succeeded(outs(&[("document", "doc-1".into())])));
    rig.tick();
    assert!(matches!(rig.command(skip("typeset")), Err(Error::Refused(w)) if w.contains("succeeded")), "a succeeded step is not skipped");
    assert_eq!(rig.status("translate"), StepStatus::Waiting(Waiting::Input { name: "glossary".into() }));
    assert_eq!(rig.status("publish"), StepStatus::NotReady);
    assert_eq!(rig.workflow_status(), WorkflowStatus::Waiting);

    // skip translate: it is final, and publish takes typeset's document
    rig.command(skip("translate")).unwrap();
    assert_eq!(rig.status("translate"), StepStatus::Skipped);
    assert!(matches!(rig.command(start("translate")), Err(Error::Refused(w)) if w.contains("final")));
    assert!(matches!(rig.command(skip("translate")), Err(Error::Refused(w)) if w.contains("already")));
    assert_eq!(rig.wf().step("publish").unwrap().inputs["document"].value, Some("doc-1".into()));
    rig.tick();
    assert_eq!(rig.status("publish"), StepStatus::Running);
    assert_eq!(local.started().iter().find(|r| r.step == "publish").unwrap().inputs["document"], Value::from("doc-1"));
    local.finish(&rig.handle(&local, "publish"), Outcome::succeeded(outs(&[("release", "release-1".into())])));
    rig.tick();
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);
    assert_eq!(rig.wf().step("translate").unwrap().skipped.as_ref().unwrap().by, max());
    assert_eq!(
        rig.history_kinds(),
        [
            "created",
            "started",
            "set@typeset",
            "run-minted@typeset",
            "run-started@typeset",
            "run-ended@typeset",
            "skipped@translate",
            "run-minted@publish",
            "run-started@publish",
            "run-ended@publish"
        ]
    );
}

#[test]
fn scenario_e_a_skipped_step_skips_what_only_it_feeds() {
    let store = Arc::new(MemoryStore::new());
    skip_library(&store, false);
    let local = performer("local", Locality::ThisMachine, &["typeset", "translate", "publish"]);
    let mut rig = Rig::new(store.clone(), vec![local.clone()], "translated-report", "skip-2");
    rig.command(set("typeset", "notes", "notes-1".into())).unwrap();
    rig.tick();
    local.finish(&rig.handle(&local, "typeset"), Outcome::succeeded(outs(&[("document", "doc-1".into())])));
    rig.tick();
    rig.command(skip("translate")).unwrap();
    assert_eq!(rig.status("translate"), StepStatus::Skipped);
    assert_eq!(rig.status("publish"), StepStatus::Skipped, "its only binding will never carry");
    assert_eq!(rig.workflow_status(), WorkflowStatus::Succeeded);
    rig.tick();
    assert_eq!(local.started().len(), 1, "nothing else starts");
}

#[test]
fn definitions_are_checked() {
    let store = MemoryStore::new();
    add_library(&store);
    let mut w = WorkflowDefinition::new("bad", 1);
    store.put_step(&verb("describe", &[], &[], &[("words", "text")])).unwrap();
    w.steps = vec![entry("a", "describe"), entry("b", "add")];
    w.bindings = vec![bind("a.words", "b.a")];
    assert!(matches!(store.put_workflow_definition(&w), Err(Error::Invalid(m)) if m.contains("tags differ")), "{:?}", store.put_workflow_definition(&w));
    w.bindings = vec![bind("a.nope", "b.a")];
    assert!(matches!(store.put_workflow_definition(&w), Err(Error::Invalid(m)) if m.contains("no output")));
    w.steps = vec![entry("a", "add"), entry("a", "add")];
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
