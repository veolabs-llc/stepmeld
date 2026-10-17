//! The README's walkthrough from the shell: the definitions in
//! `examples/hello`, a verb that is a shell script standing in for its
//! `add.py`, a Workflow, a tick, an Outcome read back. Nothing but the
//! command touches the engine.

use std::path::Path;
use std::process::Command;

fn stepmeld(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_stepmeld"))
        .current_dir(dir)
        .args(["--store", "state.sqlite", "--performers", "performers.json", "--as", "person:max"])
        .args(args)
        .output()
        .unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn hello_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // the verb: adds its two inputs, as examples/hello/add.py does
    std::fs::write(
        d.join("add.sh"),
        r#"#!/bin/sh
run="$1"
set -- $(python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(r["inputs"]["a"], r["inputs"]["b"])' "$run/request.json")
echo '{"phase":"adding","done":0,"total":3,"at":"now"}' > "$run/progress.json"
echo "adding $1 and $2"
echo '{"state":"succeeded","outputs":{"sum":'"$(($1 + $2))"'},"summary":{"headline":"'"$1 + $2 = $(($1 + $2))"'"}}' > "$run/outcome.json"
"#,
    )
    .unwrap();
    std::fs::write(
        d.join("performers.json"),
        serde_json::json!({"root": d.join("runs"), "programs": [{"verb": {"name": "add", "version": 1}, "command": ["sh", d.join("add.sh")]}]}).to_string(),
    )
    .unwrap();
    let hello = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/hello");
    std::fs::copy(hello.join("add.json"), d.join("verb.json")).unwrap();
    std::fs::copy(hello.join("hello.json"), d.join("recipe.json")).unwrap();

    // a recipe nesting another may be named before it in one add
    std::fs::write(
        d.join("outer.json"),
        serde_json::json!({"contract": "stepmeld/workflow-definition.v1", "name": "hello-twice", "version": 1, "steps": [{"name": "once", "step": {"name": "hello", "version": 1}}]}).to_string(),
    )
    .unwrap();
    let (ok, out) = stepmeld(d, &["add", "outer.json", "verb.json", "recipe.json"]);
    assert!(ok && out.contains("added recipe hello-twice v1"), "{out}");
    let (ok, out) = stepmeld(d, &["add", "outer.json"]);
    assert!(ok, "adding again is fine: {out}");
    std::fs::write(
        d.join("orphan.json"),
        serde_json::json!({"contract": "stepmeld/workflow-definition.v1", "name": "orphan", "version": 1, "steps": [{"name": "x", "step": {"name": "nope", "version": 1}}]}).to_string(),
    )
    .unwrap();
    let (ok, out) = stepmeld(d, &["add", "orphan.json"]);
    assert!(!ok && out.contains("not in the library"), "{out}");
    let (ok, out) = stepmeld(d, &["library"]);
    assert!(ok && out.contains("verb    add v1") && out.contains("recipe  hello v1"), "{out}");
    let (ok, out) = stepmeld(d, &["create", "hello", "--id", "wf-1"]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["show", "wf-1"]);
    assert!(ok && out.contains("needs input a"), "{out}");
    for (input, value) in [("a", "2"), ("b", "3")] {
        let (ok, out) = stepmeld(d, &["set", "wf-1", "add", "--input", input, "--value", value]);
        assert!(ok, "{out}");
    }
    // a root Workflow runs nothing until its go
    let (ok, out) = stepmeld(d, &["show", "wf-1"]);
    assert!(ok && out.contains("waiting for the workflow to be started"), "{out}");
    let (ok, out) = stepmeld(d, &["tick"]);
    assert!(ok && !out.contains("succeeded"), "{out}");
    let (ok, out) = stepmeld(d, &["start", "wf-1"]);
    assert!(ok && out.contains("go given to wf-1"), "{out}");
    let (ok, out) = stepmeld(d, &["tick", "--watch", "--every", "1"]);
    assert!(ok && out.contains("succeeded"), "{out}");
    let (ok, out) = stepmeld(d, &["show", "wf-1"]);
    assert!(ok && out.contains("2 + 3 = 5") && out.contains("sum            5") && out.contains("1  (default)"), "{out}");
    let (ok, out) = stepmeld(d, &["log", "wf-1", "add"]);
    assert!(ok && out.contains("adding 2 and 3"), "{out}");
    let (ok, out) = stepmeld(d, &["history", "wf-1"]);
    assert!(ok && out.contains("run-ended") && out.contains("policy start-automatically"), "{out}");
    // a second performer that launches work in the cloud: the file says so,
    // and a confirm-before-cloud policy holds a step for a person
    std::fs::write(
        d.join("performers.json"),
        serde_json::json!({"performers": [
            {"name": "local", "root": d.join("runs"), "programs": [{"verb": {"name": "add", "version": 1}, "command": ["sh", d.join("add.sh")]}]},
            {"name": "batch", "locality": "cloud", "root": d.join("runs"), "programs": [{"verb": {"name": "add", "version": 1}, "command": ["sh", d.join("add.sh")]}]}
        ]})
        .to_string(),
    )
    .unwrap();
    let (ok, out) = stepmeld(d, &["create", "hello", "--id", "wf-2"]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["policy", "wf-2", "add", "--to", r#"{"placement":{"performer":"batch","confirm":["cloud"]}}"#]);
    assert!(ok, "{out}");
    for (input, value) in [("a", "40"), ("b", "2")] {
        let (ok, out) = stepmeld(d, &["set", "wf-2", "add", "--input", input, "--value", value]);
        assert!(ok, "{out}");
    }
    let (ok, out) = stepmeld(d, &["start", "wf-2"]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["tick"]);
    assert!(ok && out.contains("wf-2") && out.contains("waiting"), "{out}");
    let (ok, out) = stepmeld(d, &["show", "wf-2"]);
    assert!(ok && out.contains("waiting for the cloud placement to be confirmed"), "{out}");
    let (ok, out) = stepmeld(d, &["confirm", "wf-2", "add"]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["tick", "--watch", "--every", "1"]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["show", "wf-2"]);
    assert!(ok && out.contains("run 1 on batch by policy start-automatically: succeeded"), "{out}");

    // a policy change is a command like any other, and a bad one is refused
    let (ok, out) = stepmeld(d, &["policy", "wf-1", "add", "--to", r#"{"placement":{"confirm":["cloud"]}}"#]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["history", "wf-1"]);
    assert!(ok && out.contains("policy-set"), "{out}");
    let (ok, out) = stepmeld(d, &["policy", "wf-1", "add", "--to", r#"{"start":"sometimes"}"#]);
    assert!(!ok && out.contains("not a policy"), "{out}");
    // a refusal is one line and exit 1
    let (ok, out) = stepmeld(d, &["set", "wf-1", "add", "--input", "nope", "--value", "1"]);
    assert!(!ok && out.trim().lines().count() == 1 && out.contains("no input"), "{out}");
    // a finished Workflow and its children are forgotten; a live one is not
    let (ok, out) = stepmeld(d, &["remove", "wf-1"]);
    assert!(ok && out.contains("removed wf-1"), "{out}");
    let (ok, out) = stepmeld(d, &["show", "wf-1"]);
    assert!(!ok && out.contains("no workflow"), "{out}");
    let (ok, out) = stepmeld(d, &["remove", "wf-1"]);
    assert!(!ok && out.contains("no workflow"), "{out}");
}
