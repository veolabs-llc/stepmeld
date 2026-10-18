//! The walkthrough's program, `hello-program`: it does the work of two
//! StepDefinitions, `stepdef_add` (a + b) and `stepdef_multiply`
//! (value × factor), each slowly enough to watch. Both StepDefinitions
//! name it (`"program": "hello-program"`), and a Worker registers the
//! command for it once.
//!
//! A tag is a label, not a type: the engine never looks inside a
//! Value, so each StepDefinition's work checks its own Inputs and
//! refuses, in words, what it cannot use.
//!
//! Run by a Worker as `hello-program <run-dir>`: it reads the request
//! from the run directory, does the work of the StepDefinition the
//! request names, and writes progress and the outcome there, through
//! `stepmeld-verb` (contracts/run-directory.v1.md).

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;
use stepmeld_verb::{refused, Result, Run};

/// Two integer Inputs, or a refusal that says what came instead.
fn integers(run: &Run, first: &str, second: &str) -> Result<(i64, i64)> {
    let (x, y) = (run.input(first)?.clone(), run.input(second)?.clone());
    match (x.as_i64(), y.as_i64()) {
        (Some(x), Some(y)) => Ok((x, y)),
        _ => Err(refused(format!("{first} and {second} must be integers, not {x} and {y}"))),
    }
}

/// Three beats of progress, `beat-seconds` apart.
fn beats(run: &Run, phase: &str, what: &str) {
    let beat = run.parameter("beat-seconds").and_then(Value::as_f64).unwrap_or(1.0).max(0.0);
    for i in 1..=3 {
        run.progress(Some(phase), Some(i), Some(3), Some("beats"));
        run.log(&format!("thinking about {what} ({i}/3)"));
        std::thread::sleep(Duration::from_secs_f64(beat));
    }
}

fn add(run: &mut Run) -> Result<()> {
    let (a, b) = integers(run, "a", "b")?;
    let sum = a.checked_add(b).ok_or_else(|| refused(format!("{a} + {b} does not fit in 64 bits")))?;
    beats(run, "adding", &format!("{a} + {b}"));
    run.succeed(BTreeMap::from([("sum".to_string(), json!(sum))]), &format!("{a} + {b} = {sum}"), None)
}

fn multiply(run: &mut Run) -> Result<()> {
    let (value, factor) = integers(run, "value", "factor")?;
    let product = value.checked_mul(factor).ok_or_else(|| refused(format!("{value} × {factor} does not fit in 64 bits")))?;
    beats(run, "multiplying", &format!("{value} × {factor}"));
    run.succeed(BTreeMap::from([("product".to_string(), json!(product))]), &format!("{value} × {factor} = {product}"), None)
}

/// The work this program does, by StepDefinition.
const STEP_DEFINITIONS: &[(&str, stepmeld_verb::Verb)] = &[("stepdef_add", add), ("stepdef_multiply", multiply)];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(stepmeld_verb::main(STEP_DEFINITIONS, &args));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A run directory holding the request a Worker would hand over for
    /// the StepDefinition in `stepdef_<step>.json`, with its Outputs.
    fn run_dir(step: &str, inputs: Value) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("stepdef_{step}.json"));
        let definition: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let request = json!({
            "workflow": "hello-1", "step": step, "run": format!("hello-1/{step}/1"),
            "definition": {"name": definition["name"], "version": 1},
            "program": definition["program"],
            "inputs": inputs,
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
        stepmeld_verb::main(STEP_DEFINITIONS, &[dir.display().to_string()])
    }

    #[test]
    fn add_adds_and_reports_progress() {
        let d = run_dir("add", json!({"a": 2, "b": 3}));
        assert_eq!(perform(d.path()), 0);
        let out = file(d.path(), "outcome.json");
        assert_eq!((out["outputs"]["sum"].clone(), out["summary"]["headline"].clone()), (json!(5), json!("2 + 3 = 5")));
        assert_eq!(file(d.path(), "progress.json")["done"], 3);
    }

    #[test]
    fn multiply_multiplies() {
        let d = run_dir("multiply", json!({"value": 5, "factor": 4}));
        assert_eq!(perform(d.path()), 0);
        let out = file(d.path(), "outcome.json");
        assert_eq!((out["outputs"]["product"].clone(), out["summary"]["headline"].clone()), (json!(20), json!("5 × 4 = 20")));
    }

    #[test]
    fn a_tag_is_not_a_type_so_each_step_checks() {
        let d = run_dir("add", json!({"a": 2, "b": "three"}));
        assert_eq!(perform(d.path()), 1);
        let out = file(d.path(), "outcome.json");
        assert_eq!(out["class"], "refused");
        assert!(out["reason"].as_str().unwrap().contains("must be integers"), "{out}");
        let d = run_dir("multiply", json!({"value": i64::MAX, "factor": 2}));
        perform(d.path());
        assert!(file(d.path(), "outcome.json")["reason"].as_str().unwrap().contains("does not fit"));
    }
}
