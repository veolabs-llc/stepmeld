//! A verb in Rust: the program side of what `stepmeld-local` speaks
//! (the Python twin is `stepmeld.verb`).
//!
//! ```text
//! <program> [args...] <run-dir>
//!   <run-dir>/request.json    read: the StartRequest
//!   <run-dir>/progress.json   written as the work goes (optional)
//!   <run-dir>/state.json      what a fresh start needs to carry on (optional)
//!   <run-dir>/outcome.json    written once, when the work ends
//! ```
//!
//! [`main`] reads the request, picks the verb by the definition's name,
//! runs it, and makes sure an outcome is written whatever happens: a
//! [`Refused`] becomes class `refused`, any other error class `failed`
//! with its words, a panic class `fault`, a verb that returns without
//! saying anything class `fault` too. Files are written atomically
//! (the Performer polls them).

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use stepmeld_core::engine::StartRequest;
use stepmeld_core::workflow::{Outcome, Progress, Summary};

/// The verb cannot do this request, in words a person reads. Nothing
/// ran.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

pub fn refused(words: impl Into<String>) -> Box<dyn std::error::Error> {
    Box::new(Refused(words.into()))
}

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn write_atomic(path: &Path, doc: &Value) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(doc).unwrap_or_default())?;
    std::fs::rename(tmp, path)
}

/// One Run of a verb, as the Performer handed it over.
pub struct Run {
    pub dir: PathBuf,
    pub request: StartRequest,
    /// What the last life saved; empty on a first start.
    pub state: Value,
    /// How many times the Performer started this program again.
    pub resumed: u32,
    ended: bool,
}

impl Run {
    pub fn open(dir: &Path) -> Result<Run> {
        let request: StartRequest = serde_json::from_slice(&std::fs::read(dir.join("request.json"))?)?;
        let state = std::fs::read(dir.join("state.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!({}));
        let resumed = std::fs::read_to_string(dir.join("resumes")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        Ok(Run {
            dir: dir.to_path_buf(),
            request,
            state,
            resumed,
            ended: false,
        })
    }

    pub fn verb(&self) -> &str {
        &self.request.definition.name
    }

    /// An Input's Value; refused when it was not given.
    pub fn input(&self, name: &str) -> Result<&Value> {
        self.request.inputs.get(name).ok_or_else(|| refused(format!("input {name:?} was not given")))
    }

    pub fn parameter(&self, name: &str) -> Option<&Value> {
        self.request.parameters.get(name)
    }

    /// A field of a reference Value (`{"project": ..., "run": ...}`),
    /// refused in words when it is not there.
    pub fn field<'a>(&self, value: &'a Value, what: &str, name: &str) -> Result<&'a str> {
        value
            .get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| refused(format!("a {what} is {{\"{name}\": ...}}, not {}", serde_json::to_string(value).unwrap_or_default().chars().take(80).collect::<String>())))
    }

    /// One line of the Run's log, stamped with the moment (UTC, the
    /// one spelling every stepmeld record uses; a reader shows local
    /// time). A message of several lines is several stamped lines.
    pub fn log(&self, line: &str) {
        let stamp = now();
        let mut lines = line.lines().peekable();
        if lines.peek().is_none() {
            println!("{stamp} ");
        }
        for l in lines {
            println!("{stamp} {l}");
        }
    }

    pub fn progress(&self, phase: Option<&str>, done: Option<u64>, total: Option<u64>, unit: Option<&str>) {
        let p = Progress {
            phase: phase.map(str::to_string),
            done,
            total,
            unit: unit.map(str::to_string),
            at: now(),
        };
        let _ = write_atomic(&self.dir.join("progress.json"), &serde_json::to_value(p).unwrap_or(Value::Null));
    }

    /// Record what a fresh start would need, and tell the Performer the
    /// program may be started again if it dies.
    pub fn save_state(&mut self, state: Value) -> Result<()> {
        write_atomic(&self.dir.join("state.json"), &state)?;
        std::fs::write(self.dir.join("resumable"), "")?;
        self.state = state;
        Ok(())
    }

    pub fn succeed(&mut self, outputs: BTreeMap<String, Value>, headline: &str, details: Option<Value>) -> Result<()> {
        let owed: Vec<&str> = self.request.outputs.iter().map(|o| o.name.as_str()).filter(|n| !outputs.contains_key(*n)).collect();
        if !owed.is_empty() {
            return Err(format!("the verb owes outputs {owed:?} and did not produce them").into());
        }
        let mut outcome = Outcome::succeeded(outputs);
        outcome.summary = Some(Summary { headline: headline.to_string(), details });
        self.end(&outcome)
    }

    pub fn fail(&mut self, class: &str, reason: &str) -> Result<()> {
        let mut outcome = Outcome::failed(class, reason);
        outcome.summary = Some(Summary { headline: reason.to_string(), details: None });
        self.end(&outcome)
    }

    fn end(&mut self, outcome: &Outcome) -> Result<()> {
        if self.ended {
            return Ok(());
        }
        write_atomic(&self.dir.join("outcome.json"), &serde_json::to_value(outcome)?)?;
        self.ended = true;
        Ok(())
    }

    pub fn ended(&self) -> bool {
        self.ended
    }
}

pub type Verb = fn(&mut Run) -> Result<()>;

/// Run the verb the request names on the run directory the last
/// argument names; the exit status for `main`.
pub fn main(verbs: &[(&str, Verb)], args: &[String]) -> i32 {
    let Some(dir) = args.last() else {
        eprintln!("usage: <program> <run-dir>");
        return 2;
    };
    let mut run = match Run::open(Path::new(dir)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot read {dir}/request.json: {e}");
            return 2;
        }
    };
    let Some((_, verb)) = verbs.iter().find(|(name, _)| *name == run.verb()) else {
        let have: Vec<&str> = verbs.iter().map(|(n, _)| *n).collect();
        let _ = run.fail("refused", &format!("this program has no verb {:?}; it has {have:?}", run.verb()));
        return 1;
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| verb(&mut run)));
    match result {
        Ok(Ok(())) if run.ended() => {}
        Ok(Ok(())) => {
            let _ = run.fail("fault", &format!("verb {:?} returned without saying how it ended", run.verb()));
        }
        Ok(Err(e)) => {
            let class = if e.downcast_ref::<Refused>().is_some() { "refused" } else { "failed" };
            let _ = run.fail(class, &e.to_string());
        }
        Err(panic) => {
            let words = panic.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| panic.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into());
            let _ = run.fail("fault", &format!("the verb panicked: {words}"));
        }
    }
    let outcome: Option<Outcome> = std::fs::read(run.dir.join("outcome.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    match outcome {
        Some(o) if o.state == stepmeld_core::workflow::RunState::Succeeded => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stepmeld_core::definition::{OutputDefinition, Ref};

    fn request(dir: &Path, verb: &str) {
        let req = StartRequest {
            workflow: "wf".into(),
            step: "s".into(),
            run: "wf/s/1".into(),
            definition: Ref::new(verb, 1),
            inputs: BTreeMap::from([("cloud".to_string(), json!({"project": "p", "run": "r"}))]),
            parameters: BTreeMap::from([("slug".to_string(), json!("x"))]),
            outputs: vec![OutputDefinition {
                name: "chunkset".into(),
                tag: "chunk-set".into(),
                options: None,
                label: None,
            }],
        };
        std::fs::write(dir.join("request.json"), serde_json::to_string(&req).unwrap()).unwrap();
    }

    fn outcome(dir: &Path) -> Outcome {
        serde_json::from_slice(&std::fs::read(dir.join("outcome.json")).unwrap()).unwrap()
    }

    fn ok(run: &mut Run) -> Result<()> {
        let cloud = run.input("cloud")?;
        let project = run.field(cloud, "point cloud", "project")?.to_string();
        run.progress(Some("chunking"), Some(1), Some(2), Some("passes"));
        run.save_state(json!({"job": "j-1"}))?;
        run.succeed(BTreeMap::from([("chunkset".to_string(), json!({"project": project, "chunkset": "cs-1"}))]), "chunked", None)
    }

    fn refuses(run: &mut Run) -> Result<()> {
        run.field(&json!("ps-1"), "photo set", "project")?;
        Ok(())
    }

    fn forgets(_run: &mut Run) -> Result<()> {
        Ok(())
    }

    fn panics(_run: &mut Run) -> Result<()> {
        panic!("boom")
    }

    fn owes(run: &mut Run) -> Result<()> {
        run.succeed(BTreeMap::new(), "nothing", None)
    }

    const VERBS: &[(&str, Verb)] = &[("chunk", ok), ("refuses", refuses), ("forgets", forgets), ("panics", panics), ("owes", owes)];

    fn run_verb(verb: &str) -> (i32, Outcome, PathBuf) {
        let dir = tempfile::tempdir().unwrap().keep();
        request(&dir, verb);
        let code = main(VERBS, &[dir.display().to_string()]);
        (code, outcome(&dir), dir)
    }

    #[test]
    fn a_verb_that_succeeds_writes_outputs_progress_and_state() {
        let (code, o, dir) = run_verb("chunk");
        assert_eq!(code, 0);
        assert_eq!(o.outputs["chunkset"], json!({"project": "p", "chunkset": "cs-1"}));
        assert_eq!(o.summary.unwrap().headline, "chunked");
        let p: Progress = serde_json::from_slice(&std::fs::read(dir.join("progress.json")).unwrap()).unwrap();
        assert_eq!((p.done, p.total), (Some(1), Some(2)));
        assert!(dir.join("resumable").exists());
        let again = Run::open(&dir).unwrap();
        assert_eq!(again.state, json!({"job": "j-1"}));
    }

    #[test]
    fn refusals_faults_and_owed_outputs_are_outcomes() {
        let (code, o, _) = run_verb("refuses");
        assert_eq!((code, o.class.as_deref()), (1, Some("refused")));
        assert!(o.reason.unwrap().contains("photo set"));
        let (_, o, _) = run_verb("forgets");
        assert_eq!(o.class.as_deref(), Some("fault"));
        let (_, o, _) = run_verb("panics");
        assert_eq!(o.class.as_deref(), Some("fault"));
        assert!(o.reason.unwrap().contains("boom"));
        let (_, o, _) = run_verb("owes");
        assert!(o.reason.unwrap().contains("chunkset"));
        let (_, o, _) = run_verb("nope");
        assert_eq!(o.class.as_deref(), Some("refused"));
    }
}
