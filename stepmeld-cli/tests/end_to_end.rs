//! The README's walkthrough from the shell (docs/ORCHESTRATOR-WORKER.md):
//! the definitions in `examples/hello` (add, then multiply the sum), a
//! Worker whose program `hello-program` is a shell script standing in
//! for the example's, `stepmeld work` running it, `stepmeld tick`
//! orchestrating, the Outcomes read back. Nothing but the command
//! touches the engine.

use std::path::Path;
use std::process::{Child, Command};

fn command(dir: &Path, worker_store: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_stepmeld"));
    c.current_dir(dir).args(["--store", "state.sqlite", "--worker-store", worker_store, "--as", "person:max"]);
    c
}

fn stepmeld(dir: &Path, args: &[&str]) -> (bool, String) {
    stepmeld_as(dir, "worker.sqlite", args)
}

fn stepmeld_as(dir: &Path, worker_store: &str, args: &[&str]) -> (bool, String) {
    let out = command(dir, worker_store).args(args).output().unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

/// A Worker running in the background, stopped when dropped.
struct Working(Child);

impl Drop for Working {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn work(dir: &Path, worker_store: &str) -> Working {
    // published before the watch starts, so a tick right after sees it
    let (ok, out) = stepmeld_as(dir, worker_store, &["work"]);
    assert!(ok, "{out}");
    Working(command(dir, worker_store).args(["work", "--watch", "--every", "1"]).stdout(std::process::Stdio::null()).spawn().unwrap())
}

/// About ten seconds: the Workers and the Orchestrator each wait a
/// second between rounds, as a person running them would.
#[test]
fn hello_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // the program: does the work of stepdef_add and stepdef_multiply, as examples/hello's hello-program does
    std::fs::write(
        d.join("verbs.sh"),
        r#"#!/bin/sh
run="$1"
set -- $(python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); i=r["inputs"]; print(r["definition"]["name"], i.get("a", i.get("value")), i.get("b", i.get("factor")))' "$run/request.json")
verb=$1; x=$2; y=$3
if [ "$verb" = stepdef_add ]; then r=$((x + y)); out=sum; sign=+; else r=$((x * y)); out=product; sign=x; fi
echo '{"phase":"'"$verb"'","done":0,"total":3,"at":"now"}' > "$run/progress.json"
echo "$verb $x and $y"
echo '{"state":"succeeded","outputs":{"'"$out"'":'"$r"'},"summary":{"headline":"'"$x $sign $y = $r"'"}}' > "$run/outcome.json"
"#,
    )
    .unwrap();
    let verbs = d.join("verbs.sh").display().to_string();
    let hello = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/hello");
    for file in ["stepdef_add.json", "stepdef_multiply.json", "workflowdef_hello.json"] {
        std::fs::copy(hello.join(file), d.join(file)).unwrap();
    }

    // a recipe nesting another may be named before it in one add
    std::fs::write(
        d.join("outer.json"),
        serde_json::json!({"contract": "stepmeld/workflow-definition.v1", "name": "hello-twice", "version": 1, "steps": [{"name": "once", "step": {"name": "workflowdef_hello", "version": 1}}]}).to_string(),
    )
    .unwrap();
    let (ok, out) = stepmeld(d, &["library", "add", "outer.json", "stepdef_add.json", "stepdef_multiply.json", "workflowdef_hello.json"]);
    assert!(ok && out.contains("added StepDefinition stepdef_add v1") && out.contains("added WorkflowDefinition hello-twice v1"), "{out}");
    let (ok, out) = stepmeld(d, &["library", "add", "outer.json"]);
    assert!(ok, "adding again is fine: {out}");
    std::fs::write(
        d.join("orphan.json"),
        serde_json::json!({"contract": "stepmeld/workflow-definition.v1", "name": "orphan", "version": 1, "steps": [{"name": "x", "step": {"name": "nope", "version": 1}}]}).to_string(),
    )
    .unwrap();
    let (ok, out) = stepmeld(d, &["library", "add", "orphan.json"]);
    assert!(!ok && out.contains("not in the library"), "{out}");
    let (ok, out) = stepmeld(d, &["library"]);
    assert!(
        ok && out.contains("StepDefinition      stepdef_add v1") && out.contains("(program hello-program)") && out.contains("WorkflowDefinition  workflowdef_hello v1"),
        "{out}"
    );

    // this machine's Worker: named, and its program registered in its own store
    let (ok, out) = stepmeld(d, &["work"]);
    assert!(!ok && out.contains("no programs yet"), "{out}");
    let (ok, out) = stepmeld(d, &["worker", "set", "--name", "local"]);
    assert!(ok && out.contains("worker   local"), "{out}");
    let (ok, out) = stepmeld(d, &["worker", "program", "set", "hello-program", "--", "sh", &verbs]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["worker"]);
    assert!(
        ok && out.contains("program  hello-program") && out.contains("verbs.sh") && out.contains("offers   stepdef_add v1, stepdef_multiply v1"),
        "{out}"
    );

    let (ok, out) = stepmeld(d, &["create", "workflowdef_hello", "--id", "wf-1"]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["show", "wf-1"]);
    assert!(ok && out.contains("needs input a"), "{out}");
    for (step, input, value) in [("step_add", "a", "2"), ("step_add", "b", "3"), ("step_multiply", "factor", "4")] {
        let (ok, out) = stepmeld(d, &["set", "wf-1", "--step", step, "--input", input, "--value", value]);
        assert!(ok, "{out}");
    }
    // step_multiply's value comes from step_add's sum: a person cannot set it
    let (ok, out) = stepmeld(d, &["set", "wf-1", "--step", "step_multiply", "--input", "value", "--value", "9"]);
    assert!(!ok && out.contains("fed by a binding"), "{out}");
    // no Worker has published itself yet: the tick says so, and stops
    let (ok, out) = stepmeld(d, &["tick", "--watch", "--every", "1"]);
    assert!(ok && out.contains("waiting") && out.contains("no Worker offers it"), "{out}");
    // once one has, a root Workflow still runs nothing until its go
    let _local = work(d, "worker.sqlite");
    let (ok, out) = stepmeld(d, &["show", "wf-1"]);
    assert!(ok && out.contains("waiting for the workflow to be started"), "{out}");
    let (ok, out) = stepmeld(d, &["start", "wf-1"]);
    assert!(ok && out.contains("go given to wf-1"), "{out}");
    let (ok, out) = stepmeld(d, &["tick", "--watch", "--every", "1"]);
    assert!(ok && out.contains("succeeded"), "{out}");
    let (ok, out) = stepmeld(d, &["show", "wf-1"]);
    assert!(ok && out.contains("2 + 3 = 5") && out.contains("sum            5") && out.contains("1  (default)") && out.contains("run 1 on local"), "{out}");
    assert!(out.contains("value          5") && out.contains("product        20") && out.contains("5 x 4 = 20"), "the sum flowed into multiply: {out}");
    assert!(
        out.contains("step_add         (stepdef_add v1)") && out.contains("step_multiply    (stepdef_multiply v1)"),
        "a Step names its StepDefinition: {out}"
    );
    let (ok, out) = stepmeld(d, &["log", "wf-1", "--step", "step_multiply"]);
    assert!(ok && out.contains("stepdef_multiply 5 and 4"), "{out}");
    let (ok, out) = stepmeld(d, &["history", "wf-1"]);
    assert!(ok && out.contains("run-ended") && out.contains("policy start-automatically"), "{out}");

    // a second Worker whose work happens in the cloud: two offer the verb,
    // and a confirm-before-cloud policy holds the step for a person
    let (ok, out) = stepmeld_as(d, "cloud-worker.sqlite", &["worker", "set", "--name", "batch", "--cloud", "true"]);
    assert!(ok && out.contains("cloud    yes"), "{out}");
    let (ok, out) = stepmeld_as(d, "cloud-worker.sqlite", &["worker", "program", "set", "hello-program", "--", "sh", &verbs]);
    assert!(ok, "{out}");
    let _cloud = work(d, "cloud-worker.sqlite");
    let (ok, out) = stepmeld(d, &["create", "workflowdef_hello", "--id", "wf-2"]);
    assert!(ok, "{out}");
    for (step, input, value) in [("step_add", "a", "40"), ("step_add", "b", "2"), ("step_multiply", "factor", "10")] {
        let (ok, out) = stepmeld(d, &["set", "wf-2", "--step", step, "--input", input, "--value", value]);
        assert!(ok, "{out}");
    }
    let (ok, out) = stepmeld(d, &["start", "wf-2"]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["tick"]);
    assert!(ok && out.contains("several Workers offer it"), "{out}");
    let (ok, out) = stepmeld(d, &["place", "wf-2", "--step", "step_add", "--worker", "nobody"]);
    assert!(!ok && out.contains("no Worker \"nobody\""), "{out}");
    // both Workers have hello-program, so step_multiply is placed by hand too
    let (ok, out) = stepmeld(d, &["place", "wf-2", "--step", "step_multiply", "--worker", "local"]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["policy", "wf-2", "--step", "step_add", "--to", r#"{"placement":{"performer":"batch","confirm":["cloud"]}}"#]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["tick"]);
    assert!(ok && out.contains("waiting for the cloud placement to be confirmed"), "{out}");
    let (ok, out) = stepmeld(d, &["confirm", "wf-2", "--step", "step_add"]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["tick", "--watch", "--every", "1"]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["show", "wf-2"]);
    assert!(
        ok && out.contains("run 1 on batch by policy start-automatically: succeeded: 40 + 2 = 42") && out.contains("run 1 on local by policy start-automatically: succeeded: 42 x 10 = 420"),
        "{out}"
    );

    // a policy change is a command like any other, and a bad one is refused
    let (ok, out) = stepmeld(d, &["policy", "wf-1", "--step", "step_add", "--to", r#"{"placement":{"confirm":["cloud"]}}"#]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["history", "wf-1"]);
    assert!(ok && out.contains("policy-set"), "{out}");
    let (ok, out) = stepmeld(d, &["policy", "wf-1", "--step", "step_add", "--to", r#"{"start":"sometimes"}"#]);
    assert!(!ok && out.contains("not a policy"), "{out}");
    // a refusal is one line and exit 1
    let (ok, out) = stepmeld(d, &["set", "wf-1", "--step", "step_add", "--input", "nope", "--value", "1"]);
    assert!(!ok && out.trim().lines().count() == 1 && out.contains("no input"), "{out}");
    // a finished Workflow and its children are forgotten; a live one is not
    let (ok, out) = stepmeld(d, &["remove", "wf-1"]);
    assert!(ok && out.contains("removed wf-1"), "{out}");
    let (ok, out) = stepmeld(d, &["show", "wf-1"]);
    assert!(!ok && out.contains("no workflow"), "{out}");
    let (ok, out) = stepmeld(d, &["remove", "wf-1"]);
    assert!(!ok && out.contains("no workflow"), "{out}");
}
