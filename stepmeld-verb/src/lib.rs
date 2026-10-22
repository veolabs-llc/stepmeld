//! A verb in Rust: the program side of what `stepmeld-local` speaks
//! (the Python twin is `stepmeld.verb`).
//!
//! ```text
//! <program> [args...] <run-dir>
//!   <run-dir>/request.json    read: the StartRequest
//!   <run-dir>/progress.json   written as the work goes (optional)
//!   <run-dir>/state.json      what a fresh start needs to carry on (optional)
//!   <run-dir>/outcome.json    written once, when the work ends
//!   <run-dir>/cancel.json     read: a cancel asked for, and its deadline
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
    stepmeld_core::iso(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()))
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

    /// A field of a reference Value (`{"file": ..., "sheet": ...}`),
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

    /// The reason a cancel was asked for, once one has been: the verb
    /// stops what it started (a job elsewhere), then calls
    /// [`Run::canceled`], before the deadline in `cancel.json`, after
    /// which the Performer stops the program.
    pub fn cancel_asked(&self) -> Option<String> {
        let asked: Value = serde_json::from_slice(&std::fs::read(self.dir.join("cancel.json")).ok()?).ok()?;
        Some(asked.get("reason").and_then(Value::as_str).unwrap_or("canceled").to_string())
    }

    /// End the Run as canceled, in the verb's own words.
    pub fn canceled(&mut self, reason: &str) -> Result<()> {
        self.end(&Outcome::canceled(reason))
    }

    pub fn ended(&self) -> bool {
        self.ended
    }
}

pub type Verb = fn(&mut Run) -> Result<()>;

/// A line of output as the log keeps it: stamped with the moment it
/// was read, unless it already carries a stamp ([`Run::log`]'s).
pub fn stamped(line: &[u8]) -> Vec<u8> {
    let has = line.len() >= 20
        && line[..20].iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
        && (line.len() == 20 || line[20] == b' ');
    if has {
        return line.to_vec();
    }
    let mut out = now().into_bytes();
    out.push(b' ');
    out.extend_from_slice(line);
    out
}

/// Everything this process writes to stdout and stderr, at the file
/// descriptors (so a C library's logging and every child process are
/// covered too), stamped line by line on its way to where stdout went
/// before (the Run's `log.txt`): a log a reader shows with one clock per
/// line (contracts/run-directory.v1.md). Unix only; elsewhere output
/// goes as written and only [`Run::log`]'s lines are stamped.
#[cfg(unix)]
mod stamper {
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;

    pub struct Stamper {
        out: i32,
        err: i32,
        thread: std::thread::JoinHandle<()>,
    }

    /// One at a time: a `main` called while another runs in this
    /// process (tests run in parallel) writes as it is.
    static ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    pub fn start() -> Option<Stamper> {
        use std::sync::atomic::Ordering;
        if ON.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return None;
        }
        let stamper = open();
        if stamper.is_none() {
            ON.store(false, Ordering::SeqCst);
        }
        stamper
    }

    fn open() -> Option<Stamper> {
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        // SAFETY: plain descriptor calls; each result is checked
        unsafe {
            let mut fds = [0i32; 2];
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                return None;
            }
            let (read, write) = (fds[0], fds[1]);
            let out = libc::dup(1);
            let err = libc::dup(2);
            if out < 0 || err < 0 || libc::dup2(write, 1) < 0 || libc::dup2(write, 2) < 0 {
                libc::close(read);
                libc::close(write);
                return None;
            }
            libc::close(write);
            let mut reader = std::fs::File::from_raw_fd(read);
            let mut sink = std::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(out));
            let thread = std::thread::spawn(move || {
                let mut pending = Vec::new();
                let mut buf = [0u8; 65536];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    pending.extend_from_slice(&buf[..n]);
                    let mut lines = Vec::new();
                    while let Some(at) = pending.iter().position(|c| *c == b'\n') {
                        let line: Vec<u8> = pending.drain(..=at).collect();
                        lines.extend(super::stamped(&line[..line.len() - 1]));
                        lines.push(b'\n');
                    }
                    if !lines.is_empty() {
                        let _ = sink.write_all(&lines);
                    }
                }
                if !pending.is_empty() {
                    let mut last = super::stamped(&pending);
                    last.push(b'\n');
                    let _ = sink.write_all(&last);
                }
            });
            Some(Stamper { out, err, thread })
        }
    }

    impl Stamper {
        /// Put stdout and stderr back and let the last lines through. A
        /// child process still holding the pipe is not waited for long.
        pub fn stop(self) {
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            // SAFETY: the descriptors saved in `start`
            unsafe {
                libc::dup2(self.out, 1);
                libc::dup2(self.err, 2);
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !self.thread.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if self.thread.is_finished() {
                let _ = self.thread.join();
            }
            // SAFETY: as above; the thread is done with `out` or is left
            unsafe {
                libc::close(self.err);
            }
            ON.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// Run the verb the request names on the run directory the last
/// argument names; the exit status for `main`. Every line the program
/// writes while it runs is stamped (unix).
pub fn main(verbs: &[(&str, Verb)], args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("usage: <program> <run-dir>");
        return 2;
    }
    let stamping = stamp_output();
    let code = run_main(verbs, args);
    drop(stamping);
    code
}

/// Stamp every line this process writes to stdout and stderr until
/// the guard is dropped, as [`main`] does for a verb: for a command a
/// Performer runs that is not a verb (one run before the program).
/// Unix only; None elsewhere, or while another guard is held.
pub fn stamp_output() -> Option<Stamping> {
    #[cfg(unix)]
    return stamper::start().map(|s| Stamping(Some(s)));
    #[cfg(not(unix))]
    None
}

/// Output stamped while held: see [`stamp_output`].
pub struct Stamping(#[cfg(unix)] Option<stamper::Stamper>);

impl Drop for Stamping {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(s) = self.0.take() {
            s.stop();
        }
    }
}

fn run_main(verbs: &[(&str, Verb)], args: &[String]) -> i32 {
    let Some(dir) = args.last() else {
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

    #[test]
    fn a_stamp_is_added_once() {
        let line = stamped(b"I20261022 13:02:04.090663 87 image.cc:377] glog");
        assert_eq!(line[19], b'Z');
        assert_eq!(&line[21..], b"I20261022 13:02:04.090663 87 image.cc:377] glog");
        assert_eq!(stamped(&line), line);
        assert_eq!(stamped(b"2026-10-22T13:02:04Z"), b"2026-10-22T13:02:04Z");
        assert_ne!(stamped(b"2026-10-22T13:02:04Zx"), b"2026-10-22T13:02:04Zx");
    }

    /// Run as the Performer runs it: this test binary again, its stdout
    /// and stderr a file. A print, a raw write to fd 2 (a C library's
    /// logging), a child process and `run.log` land one stamp each.
    #[cfg(unix)]
    #[test]
    fn every_line_the_program_writes_is_stamped_once() {
        const CHILD: &str = "STEPMELD_VERB_STAMP_CHILD";
        if let Ok(dir) = std::env::var(CHILD) {
            fn talk(run: &mut Run) -> Result<()> {
                // the harness's own `test ... ` has no newline yet
                println!();
                println!("== begin");
                println!("a print");
                let glog = b"I20261022 13:02:04.090663 87 image.cc:377] glog\n";
                unsafe { libc::write(2, glog.as_ptr().cast(), glog.len()) };
                std::process::Command::new("sh").args(["-c", "echo a child"]).status()?;
                run.log("said twice\nby run.log");
                print!("no newline at the end");
                run.succeed(BTreeMap::new(), "talked", None)
            }
            main(&[("count-lines", talk)], &[dir]);
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        request(tmp.path(), "count-lines");
        let log = std::fs::File::create(tmp.path().join("log.txt")).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::every_line_the_program_writes_is_stamped_once", "--nocapture", "--test-threads=1"])
            .env(CHILD, tmp.path())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .status()
            .unwrap();
        assert!(status.success());
        let text = std::fs::read_to_string(tmp.path().join("log.txt")).unwrap();
        let lines: Vec<&str> = text.lines().skip_while(|l| !l.ends_with("== begin")).take(7).collect();
        assert!(lines.iter().all(|l| stamped(l.as_bytes()) == l.as_bytes() && stamped(&l.as_bytes()[21..]) != l.as_bytes()[21..]), "{text}");
        let bodies: Vec<&str> = lines.iter().map(|l| &l[21..]).collect();
        assert_eq!(
            bodies,
            ["== begin", "a print", "I20261022 13:02:04.090663 87 image.cc:377] glog", "a child", "said twice", "by run.log", "no newline at the end"],
            "{text}"
        );
    }
    use stepmeld_core::definition::{OutputDefinition, Ref};

    fn request(dir: &Path, verb: &str) {
        let req = StartRequest {
            needs: Default::default(),
            workflow: "wf".into(),
            step: "s".into(),
            run: "wf/s/1".into(),
            definition: Ref::new(verb, 1),
            program: Some("count-program".into()),
            inputs: BTreeMap::from([("source".to_string(), json!({"file": "notes.txt", "sheet": "s1"}))]),
            parameters: BTreeMap::from([("slug".to_string(), json!("x"))]),
            outputs: vec![OutputDefinition {
                name: "counts".into(),
                tag: "counts".into(),
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
        let source = run.input("source")?;
        let file = run.field(source, "document reference", "file")?.to_string();
        run.progress(Some("counting"), Some(1), Some(2), Some("passes"));
        run.save_state(json!({"job": "j-1"}))?;
        run.succeed(BTreeMap::from([("counts".to_string(), json!({"file": file, "counts": "c-1"}))]), "counted", None)
    }

    fn refuses(run: &mut Run) -> Result<()> {
        run.field(&json!("notes.txt"), "document reference", "file")?;
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

    fn hears_the_cancel(run: &mut Run) -> Result<()> {
        loop {
            if let Some(reason) = run.cancel_asked() {
                return run.canceled(&format!("stopped its job ({reason})"));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    const VERBS: &[(&str, Verb)] = &[("count", ok), ("refuses", refuses), ("forgets", forgets), ("panics", panics), ("owes", owes), ("waits", hears_the_cancel)];

    fn valid(dir: &Path, file: &str, contract: &str) {
        let doc: Value = serde_json::from_slice(&std::fs::read(dir.join(file)).unwrap()).unwrap();
        assert_eq!(stepmeld_core::contracts::validate(&doc, contract), Ok(()), "{file}: {doc}");
    }

    /// What the verb writes is the run directory's contracts, and a
    /// verb told of a cancel ends canceled in its own words.
    #[test]
    fn the_files_hold_to_their_contracts_and_a_cancel_is_heard() {
        let (_, _, dir) = run_verb("count");
        valid(&dir, "request.json", "stepmeld/run-request.v1");
        valid(&dir, "progress.json", "stepmeld/run-progress.v1");
        valid(&dir, "outcome.json", "stepmeld/run-outcome.v1");
        for verb in ["refuses", "panics", "owes"] {
            let (_, _, dir) = run_verb(verb);
            valid(&dir, "outcome.json", "stepmeld/run-outcome.v1");
        }
        let dir = tempfile::tempdir().unwrap().keep();
        request(&dir, "waits");
        std::fs::write(dir.join("cancel.json"), r#"{"reason":"asked by person:ann","at":"2026-10-17T14:05:00Z","deadline":"2026-10-17T14:05:30Z"}"#).unwrap();
        assert_eq!(main(VERBS, &[dir.display().to_string()]), 1);
        let o = outcome(&dir);
        assert_eq!((o.state, o.reason.as_deref()), (stepmeld_core::workflow::RunState::Canceled, Some("stopped its job (asked by person:ann)")));
        valid(&dir, "outcome.json", "stepmeld/run-outcome.v1");
    }

    fn run_verb(verb: &str) -> (i32, Outcome, PathBuf) {
        let dir = tempfile::tempdir().unwrap().keep();
        request(&dir, verb);
        let code = main(VERBS, &[dir.display().to_string()]);
        (code, outcome(&dir), dir)
    }

    #[test]
    fn a_verb_that_succeeds_writes_outputs_progress_and_state() {
        let (code, o, dir) = run_verb("count");
        assert_eq!(code, 0);
        assert_eq!(o.outputs["counts"], json!({"file": "notes.txt", "counts": "c-1"}));
        assert_eq!(o.summary.unwrap().headline, "counted");
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
        assert!(o.reason.unwrap().contains("document reference"));
        let (_, o, _) = run_verb("forgets");
        assert_eq!(o.class.as_deref(), Some("fault"));
        let (_, o, _) = run_verb("panics");
        assert_eq!(o.class.as_deref(), Some("fault"));
        assert!(o.reason.unwrap().contains("boom"));
        let (_, o, _) = run_verb("owes");
        assert!(o.reason.unwrap().contains("counts"));
        let (_, o, _) = run_verb("nope");
        assert_eq!(o.class.as_deref(), Some("refused"));
    }
}
