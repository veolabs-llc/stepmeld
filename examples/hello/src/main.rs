//! The walkthrough's verb: add two integers, slowly enough to watch.
//!
//! A tag is a label, not a type: the engine never looks inside a
//! Value, so the verb checks its own Inputs and refuses, in words, what
//! it cannot add.
//!
//! Run by a Worker as `hello-add <run-dir>`: it reads the request from
//! the run directory and writes progress and the outcome there, through
//! `stepmeld-verb` (contracts/run-directory.v1.md).

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;
use stepmeld_verb::{refused, Result, Run};

fn add(run: &mut Run) -> Result<()> {
    let (a, b) = (run.input("a")?.clone(), run.input("b")?.clone());
    let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) else {
        return Err(refused(format!("a and b must be integers, not {a} and {b}")));
    };
    let sum = x.checked_add(y).ok_or_else(|| refused(format!("{x} + {y} does not fit in 64 bits")))?;
    let beat = run.parameter("beat-seconds").and_then(Value::as_f64).unwrap_or(1.0).max(0.0);
    for i in 1..=3 {
        run.progress(Some("adding"), Some(i), Some(3), Some("beats"));
        run.log(&format!("thinking about {x} + {y} ({i}/3)"));
        std::thread::sleep(Duration::from_secs_f64(beat));
    }
    run.succeed(BTreeMap::from([("sum".to_string(), json!(sum))]), &format!("{x} + {y} = {sum}"), None)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(stepmeld_verb::main(&[("add", add)], &args));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A run directory holding the request a Worker would hand over,
    /// with the verb's own Outputs from `add.json`.
    fn run_dir(a: Value, b: Value) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let definition: Value = serde_json::from_str(&std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("add.json")).unwrap()).unwrap();
        let request = json!({
            "workflow": "hello-1", "step": "add", "run": "hello-1/add/1",
            "definition": {"name": "add", "version": 1},
            "inputs": {"a": a, "b": b},
            "parameters": {"beat-seconds": 0},
            "outputs": definition["outputs"],
        });
        std::fs::write(dir.path().join("request.json"), request.to_string()).unwrap();
        dir
    }

    fn file(dir: &Path, name: &str) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join(name)).unwrap()).unwrap()
    }

    fn perform(dir: &Path) -> i32 {
        stepmeld_verb::main(&[("add", add)], &[dir.display().to_string()])
    }

    #[test]
    fn it_adds_and_reports_progress() {
        let d = run_dir(json!(2), json!(3));
        assert_eq!(perform(d.path()), 0);
        let out = file(d.path(), "outcome.json");
        assert_eq!((out["outputs"]["sum"].clone(), out["summary"]["headline"].clone()), (json!(5), json!("2 + 3 = 5")));
        assert_eq!(file(d.path(), "progress.json")["done"], 3);
    }

    #[test]
    fn a_tag_is_not_a_type_so_the_verb_checks() {
        let d = run_dir(json!(2), json!("three"));
        assert_eq!(perform(d.path()), 1);
        let out = file(d.path(), "outcome.json");
        assert_eq!(out["class"], "refused");
        assert!(out["reason"].as_str().unwrap().contains("must be integers"), "{out}");
        let d = run_dir(json!(i64::MAX), json!(1));
        perform(d.path());
        assert!(file(d.path(), "outcome.json")["reason"].as_str().unwrap().contains("does not fit"));
    }
}
