//! The walkthrough's program, `hello-program`. Both StepDefinitions
//! name it (`"program": "hello-program"`): it adds for `stepdef_add`
//! and multiplies for `stepdef_multiply`, slowly enough to watch.
//!
//! It uses nothing of stepmeld. A Worker runs it as
//! `hello-program <run-dir>`; it reads `request.json` from that folder
//! and writes `progress.json` as it goes and `outcome.json` at the end
//! (the run directory protocol, contracts/run-directory.v1.md).
//! Whatever it prints goes to the Run's log.
//!
//! A tag is a label, not a type: the engine never looks inside a value,
//! so the program checks its own Inputs and refuses, in words, what it
//! cannot use.

use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn main() {
    let dir = std::env::args().next_back().unwrap_or_default();
    std::process::exit(run(Path::new(&dir)));
}

/// Do the work the request names; the exit status.
fn run(dir: &Path) -> i32 {
    let request: Value = match std::fs::read_to_string(dir.join("request.json")).ok().and_then(|t| serde_json::from_str(&t).ok()) {
        Some(r) => r,
        None => {
            eprintln!("cannot read {}/request.json", dir.display());
            return 2;
        }
    };
    let step_definition = request["definition"]["name"].as_str().unwrap_or("");
    let (first, second, output, sign) = match step_definition {
        "stepdef_add" => ("a", "b", "sum", "+"),
        "stepdef_multiply" => ("value", "factor", "product", "×"),
        other => return refuse(dir, &format!("hello-program does no work for {other:?}")),
    };
    let (x, y) = match (request["inputs"][first].as_i64(), request["inputs"][second].as_i64()) {
        (Some(x), Some(y)) => (x, y),
        _ => return refuse(dir, &format!("{first} and {second} must be integers, not {} and {}", request["inputs"][first], request["inputs"][second])),
    };
    let answer = match output {
        "sum" => x.checked_add(y),
        _ => x.checked_mul(y),
    };
    let Some(answer) = answer else { return refuse(dir, &format!("{x} {sign} {y} does not fit in 64 bits")) };

    let beat = request["parameters"]["beat-seconds"].as_f64().unwrap_or(1.0).max(0.0);
    let phase = if output == "sum" { "adding" } else { "multiplying" };
    for i in 1..=3 {
        write(dir, "progress.json", json!({"phase": phase, "done": i, "total": 3, "unit": "beats", "at": now()}));
        println!("thinking about {x} {sign} {y} ({i}/3)");
        std::thread::sleep(Duration::from_secs_f64(beat));
    }
    write(dir, "outcome.json", json!({"state": "succeeded", "outputs": {output: answer}, "summary": {"headline": format!("{x} {sign} {y} = {answer}")}}));
    0
}

/// End the Run as refused: it cannot do this request, in words.
fn refuse(dir: &Path, words: &str) -> i32 {
    write(dir, "outcome.json", json!({"state": "failed", "class": "refused", "reason": words}));
    1
}

fn write(dir: &Path, file: &str, doc: Value) {
    std::fs::write(dir.join(file), doc.to_string()).expect("the run directory is writable");
}

/// The time now, as the protocol spells it: RFC 3339, UTC.
fn now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (day, month) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run directory holding the request a Worker would hand over for
    /// the StepDefinition in `stepdef_<name>.json`, with its Outputs.
    fn run_dir(name: &str, inputs: Value) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("stepdef_{name}.json"));
        let definition: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let request = json!({
            "workflow": "hello-1", "step": format!("step_{name}"), "run": format!("hello-1/step_{name}/1"),
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

    #[test]
    fn it_adds_and_reports_progress() {
        let d = run_dir("add", json!({"a": 2, "b": 3}));
        assert_eq!(run(d.path()), 0);
        let out = file(d.path(), "outcome.json");
        assert_eq!((out["outputs"]["sum"].clone(), out["summary"]["headline"].clone()), (json!(5), json!("2 + 3 = 5")));
        let progress = file(d.path(), "progress.json");
        assert_eq!(progress["done"], 3);
        // what it writes by hand is what the contracts say
        assert_eq!(stepmeld_core::contracts::validate(&progress, "stepmeld/run-progress.v1"), Ok(()));
        assert_eq!(stepmeld_core::contracts::validate(&out, "stepmeld/run-outcome.v1"), Ok(()));
    }

    #[test]
    fn it_multiplies() {
        let d = run_dir("multiply", json!({"value": 5, "factor": 4}));
        assert_eq!(run(d.path()), 0);
        assert_eq!(file(d.path(), "outcome.json")["summary"]["headline"], "5 × 4 = 20");
    }

    #[test]
    fn a_tag_is_not_a_type_so_it_checks() {
        let d = run_dir("add", json!({"a": 2, "b": "three"}));
        assert_eq!(run(d.path()), 1);
        let out = file(d.path(), "outcome.json");
        assert_eq!((out["state"].as_str(), out["class"].as_str()), (Some("failed"), Some("refused")));
        assert!(out["reason"].as_str().unwrap().contains("must be integers"), "{out}");
        assert_eq!(stepmeld_core::contracts::validate(&out, "stepmeld/run-outcome.v1"), Ok(()));
        let d = run_dir("multiply", json!({"value": i64::MAX, "factor": 2}));
        assert_eq!(run(d.path()), 1);
        assert!(file(d.path(), "outcome.json")["reason"].as_str().unwrap().contains("does not fit"));
    }
}
