//! The one-step example from the shell: a verb that is a shell script,
//! a recipe, a Workflow, a tick, an Outcome read back. Nothing but the
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
fn detect_targets_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // the verb: counts the files it was given and writes a table
    std::fs::write(
        d.join("detect.sh"),
        r#"#!/bin/sh
run="$1"
n=$(python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(len(r["inputs"]["images"]))' "$run/request.json")
echo '{"phase":"detecting","done":0,"total":'"$n"',"at":"now"}' > "$run/progress.json"
echo "looked at $n images"
echo '{"state":"succeeded","outputs":{"targets":{"rows":[[1,10.5,20.25],[2,30.0,40.0]]}},"summary":{"headline":"2 targets in '"$n"' images"}}' > "$run/outcome.json"
"#,
    )
    .unwrap();
    std::fs::write(
        d.join("performers.json"),
        serde_json::json!({"root": d.join("runs"), "programs": [{"verb": {"name": "detect-targets", "version": 1}, "command": ["sh", d.join("detect.sh")]}]}).to_string(),
    )
    .unwrap();
    std::fs::write(d.join("verb.json"), serde_json::json!({"contract": "stepmeld/step-definition.v1", "name": "detect-targets", "version": 1, "label": "Detect OmniTargets", "inputs": [{"name": "images", "tag": "file-list"}], "parameters": [{"name": "min-size", "tag": "integer", "default": 12}], "outputs": [{"name": "targets", "tag": "target-table"}]}).to_string()).unwrap();
    std::fs::write(
        d.join("recipe.json"),
        serde_json::json!({"contract": "stepmeld/workflow-definition.v1", "name": "find-targets", "version": 1, "label": "Detect OmniTargets", "steps": [{"name": "detect", "step": {"name": "detect-targets", "version": 1}}]}).to_string(),
    )
    .unwrap();

    let (ok, out) = stepmeld(d, &["add", "verb.json", "recipe.json"]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["library"]);
    assert!(ok && out.contains("verb    detect-targets v1") && out.contains("recipe  find-targets v1"), "{out}");
    let (ok, out) = stepmeld(d, &["create", "find-targets", "--id", "wf-1"]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["show", "wf-1"]);
    assert!(ok && out.contains("needs input images"), "{out}");
    let (ok, out) = stepmeld(d, &["set", "wf-1", "detect", "--input", "images", "--value", r#"["a.jpg","b.jpg","c.jpg"]"#]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["tick", "--watch", "--every", "1"]);
    assert!(ok && out.contains("succeeded"), "{out}");
    let (ok, out) = stepmeld(d, &["show", "wf-1"]);
    assert!(
        ok && out.contains("2 targets in 3 images") && out.contains(r#"{"rows":[[1,10.5,20.25],[2,30.0,40.0]]}"#) && out.contains("12  (default)"),
        "{out}"
    );
    let (ok, out) = stepmeld(d, &["log", "wf-1", "detect"]);
    assert!(ok && out.contains("looked at 3 images"), "{out}");
    let (ok, out) = stepmeld(d, &["history", "wf-1"]);
    assert!(ok && out.contains("run-ended") && out.contains("policy start-automatically"), "{out}");
    // a policy change is a command like any other, and a bad one is refused
    let (ok, out) = stepmeld(d, &["policy", "wf-1", "detect", "--to", r#"{"placement":{"confirm":["cloud"]}}"#]);
    assert!(ok, "{out}");
    let (ok, out) = stepmeld(d, &["history", "wf-1"]);
    assert!(ok && out.contains("policy-set"), "{out}");
    let (ok, out) = stepmeld(d, &["policy", "wf-1", "detect", "--to", r#"{"start":"sometimes"}"#]);
    assert!(!ok && out.contains("not a policy"), "{out}");
    // a refusal is one line and exit 1
    let (ok, out) = stepmeld(d, &["set", "wf-1", "detect", "--input", "nope", "--value", "1"]);
    assert!(!ok && out.trim().lines().count() == 1 && out.contains("no input"), "{out}");
}
