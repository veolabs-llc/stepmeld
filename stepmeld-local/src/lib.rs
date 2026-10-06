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

pub struct LocalPerformer {
    name: String,
    root: PathBuf,
    programs: Vec<Program>,
    children: Mutex<BTreeMap<String, std::process::Child>>,
}

impl LocalPerformer {
    /// Runs live under `root`, one directory each.
    pub fn new(name: &str, root: &Path, programs: Vec<Program>) -> LocalPerformer {
        LocalPerformer {
            name: name.to_string(),
            root: root.to_path_buf(),
            programs,
            children: Mutex::new(BTreeMap::new()),
        }
    }

    fn dir(&self, handle: &str) -> PathBuf {
        self.root.join(handle)
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
            locality: Locality::ThisMachine,
            verbs: self.programs.iter().map(|p| p.verb.clone()).collect(),
        }
    }

    fn start(&self, request: &StartRequest) -> Result<String, String> {
        let handle = Self::handle_for(request);
        let dir = self.dir(&handle);
        if dir.join("request.json").exists() {
            return Ok(handle);
        }
        let program = self.programs.iter().find(|p| p.verb == request.definition).ok_or_else(|| format!("{} does not perform {}", self.name, request.definition))?;
        let (exe, args) = program.command.split_first().ok_or_else(|| format!("{} has no program for {}", self.name, request.definition))?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
        std::fs::write(dir.join("request.json"), serde_json::to_string_pretty(request).unwrap()).map_err(|e| format!("cannot write the request: {e}"))?;
        let log = std::fs::File::create(dir.join("log.txt")).map_err(|e| format!("cannot open the log: {e}"))?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let child = Command::new(exe).args(args).arg(&dir).stdin(Stdio::null()).stdout(Stdio::from(log)).stderr(Stdio::from(err)).spawn().map_err(|e| {
            let _ = std::fs::remove_dir_all(&dir);
            format!("cannot run {exe}: {e}")
        })?;
        std::fs::write(dir.join("pid"), child.id().to_string()).map_err(|e| e.to_string())?;
        self.children.lock().unwrap().insert(handle.clone(), child);
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
                // ended without an outcome: a 0 exit with nothing to
                // deliver is a success; anything else failed
                let declared: Vec<String> = Self::read::<StartRequest>(&dir.join("request.json")).map(|r| r.outputs.into_iter().map(|o| o.name).collect()).unwrap_or_default();
                let outcome = match status {
                    Some(s) if s.success() && declared.is_empty() => Outcome::succeeded(BTreeMap::new()),
                    Some(s) if s.success() => Outcome::failed("fault", &format!("the program exited 0 without writing outcome.json; it owed {declared:?}")),
                    Some(s) => Outcome::failed("exit", &format!("the program ended with {s}: {}", Self::last_line(&dir.join("log.txt")).unwrap_or_else(|| "(no output)".into()))),
                    None => Outcome::lost("the process is gone and wrote no outcome"),
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
