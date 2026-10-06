//! A [`Performer`] for this machine: each verb is a program. The
//! protocol between the Performer and the program is files in a
//! directory, so a program in any language can be a verb:
//!
//! ```text
//! <program> [args...] <run-dir>
//!   <run-dir>/request.json    the StartRequest, written before the start
//!   <run-dir>/progress.json   written by the program as it goes (optional)
//!   <run-dir>/outcome.json    written by the program when it ends: an Outcome
//!   <run-dir>/log.txt         its stdout and stderr
//!   <run-dir>/pid             the process id, written by the Performer
//! ```
//!
//! A program that exits without writing `outcome.json` has failed
//! (class `exit`, reason: its exit status and the last line of its
//! log), or succeeded with no Outputs when it exits 0 and the verb
//! declares none. What the Performer knows of its Runs is on disk, so
//! a new process finds the Runs an earlier one started.
//!
//! **Resuming.** A program whose work goes on elsewhere (a launcher
//! following a job on a cloud queue) writes what it would need to pick
//! the work back up to `<run-dir>/state.json` and touches
//! `<run-dir>/resumable`. If such a program is gone without an outcome
//! (killed, a reboot), the Performer starts it again on the same run
//! directory instead of calling the Run lost, up to [`RESUMES`] times;
//! the program reads its state and carries on. A program that never
//! said it was resumable is lost, as before.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use stepmeld_core::definition::{Locality, Ref};
use stepmeld_core::engine::{Observation, StartRequest};
use stepmeld_core::performer::Performer;
use stepmeld_core::workflow::{Outcome, PerformerFace, Progress};

/// How a verb is run.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Program {
    pub verb: Ref,
    /// The program and its leading arguments; the run directory is
    /// appended.
    pub command: Vec<String>,
}

/// How many times a resumable program is started again before its Run
/// is lost.
pub const RESUMES: u32 = 3;

pub struct LocalPerformer {
    name: String,
    /// What this Performer answers to "where": the programs run here,
    /// but a program may be a launcher whose work happens elsewhere
    /// (a job on a cloud queue), and then the Performer says so.
    locality: Locality,
    root: PathBuf,
    programs: Vec<Program>,
    children: Mutex<BTreeMap<String, std::process::Child>>,
}

impl LocalPerformer {
    /// Runs live under `root`, one directory each.
    pub fn new(name: &str, root: &Path, programs: Vec<Program>) -> LocalPerformer {
        LocalPerformer {
            name: name.to_string(),
            locality: Locality::ThisMachine,
            root: root.to_path_buf(),
            programs,
            children: Mutex::new(BTreeMap::new()),
        }
    }

    /// Say where the work happens, where that is not this machine: a
    /// program may be a launcher for a job on a cloud queue.
    pub fn at(mut self, locality: Locality) -> LocalPerformer {
        self.locality = locality;
        self
    }

    fn dir(&self, handle: &str) -> PathBuf {
        self.root.join(handle)
    }

    fn program_for(&self, verb: &Ref) -> Result<Program, String> {
        self.programs.iter().find(|p| &p.verb == verb).cloned().ok_or_else(|| format!("{} does not perform {verb}", self.name))
    }

    /// Start the program on a run directory: its log appended (a
    /// resume continues the same log), its pid written, the child kept.
    fn spawn(&self, handle: &str, dir: &Path, program: &Program, resume: bool) -> Result<(), String> {
        let (exe, args) = program.command.split_first().ok_or_else(|| format!("{} has no program for {}", self.name, program.verb))?;
        let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log.txt")).map_err(|e| format!("cannot open the log: {e}"))?;
        if resume {
            use std::io::Write;
            let _ = writeln!(&log, "== resumed by {} ({})", self.name, Self::read::<u32>(&dir.join("resumes")).unwrap_or(0));
        }
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let child = Command::new(exe)
            .args(args)
            .arg(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| format!("cannot run {exe}: {e}"))?;
        std::fs::write(dir.join("pid"), child.id().to_string()).map_err(|e| e.to_string())?;
        self.children.lock().unwrap().insert(handle.to_string(), child);
        Ok(())
    }

    /// A program that said it could resume, gone without an outcome:
    /// start it again on the same directory, counting; Some(why) when
    /// it will not be.
    fn resume(&self, handle: &str, dir: &Path) -> Result<(), String> {
        if !dir.join("resumable").exists() {
            return Err("the program never said it could resume".into());
        }
        let resumes = Self::read::<u32>(&dir.join("resumes")).unwrap_or(0);
        if resumes >= RESUMES {
            return Err(format!("resumed {resumes} times already"));
        }
        let request = Self::read::<StartRequest>(&dir.join("request.json")).ok_or("the request cannot be read")?;
        let program = self.program_for(&request.definition)?;
        std::fs::write(dir.join("resumes"), (resumes + 1).to_string()).map_err(|e| e.to_string())?;
        self.spawn(handle, dir, &program, true)
    }

    fn handle_for(request: &StartRequest) -> String {
        request.run.replace(['/', '\\', ':'], "_")
    }

    fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }

    fn last_line(path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()?.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string)
    }

    /// Whether the process a Run's `pid` file names is alive.
    fn alive(dir: &Path) -> bool {
        let Some(pid) = std::fs::read_to_string(dir.join("pid")).ok().and_then(|p| p.trim().parse::<u32>().ok()) else {
            return false;
        };
        Command::new("kill").args(["-0", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
    }
}

impl Performer for LocalPerformer {
    fn describe(&self) -> PerformerFace {
        PerformerFace {
            name: self.name.clone(),
            locality: self.locality,
            verbs: self.programs.iter().map(|p| p.verb.clone()).collect(),
        }
    }

    fn start(&self, request: &StartRequest) -> Result<String, String> {
        let handle = Self::handle_for(request);
        let dir = self.dir(&handle);
        if dir.join("request.json").exists() {
            return Ok(handle);
        }
        let program = self.program_for(&request.definition)?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
        std::fs::write(dir.join("request.json"), serde_json::to_string_pretty(request).unwrap()).map_err(|e| format!("cannot write the request: {e}"))?;
        if let Err(e) = self.spawn(&handle, &dir, &program, false) {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        Ok(handle)
    }

    fn observe(&self, handle: &str) -> Observation {
        let dir = self.dir(handle);
        if !dir.join("request.json").exists() {
            return Observation::Absent;
        }
        if let Some(outcome) = Self::read::<Outcome>(&dir.join("outcome.json")) {
            return Observation::Ended { outcome };
        }
        // our own child: reap it; another process's: ask the pid
        let exited = {
            let mut children = self.children.lock().unwrap();
            match children.get_mut(handle).map(|c| c.try_wait()) {
                Some(Ok(Some(status))) => {
                    children.remove(handle);
                    Some(Some(status))
                }
                Some(Ok(None)) => None,
                Some(Err(_)) => Some(None),
                None => {
                    if Self::alive(&dir) {
                        None
                    } else {
                        Some(None)
                    }
                }
            }
        };
        match exited {
            None => Observation::Running {
                progress: Self::read::<Progress>(&dir.join("progress.json")),
                attention: None,
            },
            Some(status) => {
                // gone without an outcome: a program that can resume is
                // started again; otherwise a 0 exit with nothing to
                // deliver is a success, anything else failed
                let declared: Vec<String> = Self::read::<StartRequest>(&dir.join("request.json")).map(|r| r.outputs.into_iter().map(|o| o.name).collect()).unwrap_or_default();
                let clean_success = status.is_some_and(|s| s.success()) && declared.is_empty();
                let resumed = if clean_success { Err(String::new()) } else { self.resume(handle, &dir) };
                if resumed.is_ok() {
                    return Observation::Running {
                        progress: Self::read::<Progress>(&dir.join("progress.json")),
                        attention: None,
                    };
                }
                let not_resumed = resumed.unwrap_err();
                let gave_up = if dir.join("resumable").exists() { format!(" ({not_resumed})") } else { String::new() };
                let outcome = match status {
                    Some(s) if s.success() && declared.is_empty() => Outcome::succeeded(BTreeMap::new()),
                    Some(s) if s.success() => Outcome::failed("fault", &format!("the program exited 0 without writing outcome.json; it owed {declared:?}")),
                    Some(s) => Outcome::failed("exit", &format!("the program ended with {s}{gave_up}: {}", Self::last_line(&dir.join("log.txt")).unwrap_or_else(|| "(no output)".into()))),
                    None => Outcome::lost(&format!("the process is gone and wrote no outcome ({not_resumed})")),
                };
                let _ = std::fs::write(dir.join("outcome.json"), serde_json::to_string_pretty(&outcome).unwrap());
                Observation::Ended { outcome }
            }
        }
    }

    fn cancel(&self, handle: &str, reason: &str) {
        let dir = self.dir(handle);
        if dir.join("outcome.json").exists() {
            return;
        }
        if let Some(mut child) = self.children.lock().unwrap().remove(handle) {
            let _ = child.kill();
            let _ = child.wait();
        } else if let Some(pid) = std::fs::read_to_string(dir.join("pid")).ok().and_then(|p| p.trim().parse::<u32>().ok()) {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
        let _ = std::fs::write(dir.join("outcome.json"), serde_json::to_string_pretty(&Outcome::canceled(reason)).unwrap());
    }

    fn log(&self, handle: &str) -> Option<String> {
        std::fs::read_to_string(self.dir(handle).join("log.txt")).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stepmeld_core::definition::OutputDefinition;

    fn request(run: &str, outputs: &[&str]) -> StartRequest {
        StartRequest {
            workflow: "wf".into(),
            step: "s".into(),
            run: run.into(),
            definition: Ref::new("echo-verb", 1),
            inputs: BTreeMap::new(),
            parameters: BTreeMap::new(),
            outputs: outputs
                .iter()
                .map(|o| OutputDefinition {
                    name: o.to_string(),
                    tag: "text".into(),
                    options: None,
                    label: None,
                })
                .collect(),
        }
    }

    fn performer(root: &Path, script: &str) -> LocalPerformer {
        let path = root.join("verb.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        LocalPerformer::new(
            "local",
            &root.join("runs"),
            vec![Program {
                verb: Ref::new("echo-verb", 1),
                command: vec!["sh".into(), path.display().to_string()],
            }],
        )
    }

    fn settle(p: &LocalPerformer, handle: &str) -> Observation {
        for _ in 0..200 {
            match p.observe(handle) {
                Observation::Running { .. } => std::thread::sleep(std::time::Duration::from_millis(20)),
                other => return other,
            }
        }
        panic!("the run did not end");
    }

    #[test]
    fn conforms_and_reads_the_outcome_the_program_writes() {
        let dir = tempfile::tempdir().unwrap();
        let p = performer(dir.path(), r#"echo '{"state":"succeeded","outputs":{"out":"hello"},"summary":{"headline":"said hello"}}' > "$1/outcome.json""#);
        let req = request("wf/s/1", &["out"]);
        stepmeld_core::performer::conformance::run(&p, &req, &|h| {
            settle(&p, h);
        });
        let h = p.start(&request("wf/s/2", &["out"])).unwrap();
        match settle(&p, &h) {
            Observation::Ended { outcome } => assert_eq!(outcome.outputs["out"], "hello"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_program_that_exits_badly_has_failed_with_its_last_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = performer(dir.path(), "echo starting; echo 'no such file' >&2; exit 3");
        let h = p.start(&request("wf/s/1", &["out"])).unwrap();
        match settle(&p, &h) {
            Observation::Ended { outcome } => {
                assert_eq!(outcome.class.as_deref(), Some("exit"));
                assert!(outcome.reason.as_deref().unwrap().contains("no such file"), "{outcome:?}");
            }
            other => panic!("{other:?}"),
        }
        assert!(p.log(&h).unwrap().contains("starting"));
    }

    #[test]
    fn progress_is_read_while_it_runs_and_a_cancel_ends_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = performer(dir.path(), r#"echo '{"phase":"working","done":1,"total":4,"at":"t"}' > "$1/progress.json"; sleep 30"#);
        let h = p.start(&request("wf/s/1", &[])).unwrap();
        let mut seen = None;
        for _ in 0..100 {
            if let Observation::Running { progress: Some(pr), .. } = p.observe(&h) {
                seen = Some(pr);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(seen.unwrap().done, Some(1));
        p.cancel(&h, "enough");
        assert!(matches!(p.observe(&h), Observation::Ended { outcome } if outcome.class.as_deref() == Some("canceled")));
    }

    /// A launcher for work elsewhere says so, and the gate's confirm
    /// policies see it (2026-10-06: a cloud performer that answered
    /// this-machine submitted a Batch job past a confirm-before-cloud
    /// policy).
    #[test]
    fn a_performer_answers_where_its_work_happens() {
        let dir = tempfile::tempdir().unwrap();
        let p = performer(dir.path(), "true");
        assert_eq!(p.describe().locality, Locality::ThisMachine);
        let p = performer(dir.path(), "true").at(Locality::Cloud);
        assert_eq!(p.describe().locality, Locality::Cloud);
    }

    /// A launcher that recorded its state is started again when it is
    /// gone without an outcome, on the same directory, and carries on
    /// from that state; one that never said so is lost.
    #[test]
    fn a_resumable_program_is_started_again_and_resumes_from_its_state() {
        let dir = tempfile::tempdir().unwrap();
        // first life: save state, say resumable, die; second life: resume from the state
        let p = performer(
            dir.path(),
            r#"if [ -f "$1/state.json" ]; then echo "resuming job $(cat "$1/state.json")"; echo '{"state":"succeeded","summary":{"headline":"resumed"}}' > "$1/outcome.json"; exit 0; fi
echo '{"job":"j-9"}' > "$1/state.json"; touch "$1/resumable"; echo "submitted j-9"; exit 1"#,
        );
        let h = p.start(&request("wf/s/1", &[])).unwrap();
        match settle(&p, &h) {
            Observation::Ended { outcome } => assert_eq!(outcome.summary.unwrap().headline, "resumed"),
            other => panic!("{other:?}"),
        }
        let log = p.log(&h).unwrap();
        assert!(log.contains("submitted j-9") && log.contains("== resumed by local (1)") && log.contains("resuming job"), "{log}");
        assert_eq!(std::fs::read_to_string(dir.path().join("runs").join(&h).join("resumes")).unwrap(), "1");
    }

    #[test]
    fn a_program_that_keeps_dying_fails_after_the_last_resume() {
        let dir = tempfile::tempdir().unwrap();
        let p = performer(dir.path(), r#"touch "$1/resumable"; kill -9 $$"#);
        let h = p.start(&request("wf/s/1", &[])).unwrap();
        match settle(&p, &h) {
            Observation::Ended { outcome } => {
                // our own child, so how it died is known: failed by exit, not lost
                assert_eq!((outcome.state, outcome.class.as_deref()), (stepmeld_core::workflow::RunState::Failed, Some("exit")));
                assert!(outcome.reason.as_ref().unwrap().contains(&format!("resumed {RESUMES} times")), "{outcome:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_new_performer_finds_the_runs_an_earlier_one_started() {
        let dir = tempfile::tempdir().unwrap();
        let p = performer(dir.path(), r#"echo '{"state":"succeeded"}' > "$1/outcome.json""#);
        let h = p.start(&request("wf/s/1", &[])).unwrap();
        settle(&p, &h);
        let again = performer(dir.path(), "true");
        assert!(matches!(again.observe(&h), Observation::Ended { .. }));
        assert_eq!(again.start(&request("wf/s/1", &[])).unwrap(), h, "the same run gets the same handle");
    }
}
