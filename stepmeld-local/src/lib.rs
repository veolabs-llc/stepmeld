//! A [`Performer`] for this machine, the mechanism behind a Worker: it
//! runs the program a request names (a StepDefinition names its
//! program; the Worker registered a command for each name), spoken to
//! through the run directory protocol (`contracts/run-directory.v1.md`),
//! files in a directory, so a program can be written in any language:
//!
//! ```text
//! <program> [args...] <run-dir>
//!   <run-dir>/request.json    the StartRequest, written before the start
//!   <run-dir>/progress.json   written by the program as it goes (optional)
//!   <run-dir>/outcome.json    written by the program when it ends: an Outcome
//!   <run-dir>/cancel.json     written by the Performer when a cancel is asked
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
//!
//! **Canceling.** A cancel writes `cancel.json` (the reason and a
//! deadline) and nothing more: the program, which may have work
//! elsewhere to stop, has until the deadline ([`CANCEL_GRACE`] unless
//! the Performer is given another) to end on its own, writing its own
//! outcome or none. At the deadline the Performer stops it and writes a
//! canceled outcome. A program gone after a cancel is canceled, never
//! resumed. The Run stays running until then: the engine asks again on
//! every tick, and each ask is short.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use stepmeld_core::definition::Locality;
use stepmeld_core::engine::{Observation, StartRequest};
use stepmeld_core::performer::Performer;
use stepmeld_core::workflow::{Outcome, PerformerFace, Progress};

/// A program this machine has: the name StepDefinitions use for it,
/// and the command that runs it here.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Program {
    pub name: String,
    /// The executable and its leading arguments; the run directory is
    /// appended.
    pub command: Vec<String>,
    /// Variables set for it beyond this process's own: the secrets its
    /// configuration lists, by name (docs/PLACEMENT.md §8).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Run as a container of this image instead of `command`.
    #[serde(default)]
    pub image: Option<Image>,
}

/// A program that runs as a container: `docker run` of `reference`,
/// with the Run's folder mounted at [`RUN_IN_CONTAINER`] and appended
/// to `args`. The program inside speaks the run directory protocol as
/// any other does. The container is named after the Run, so it can be
/// stopped by name.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct Image {
    /// The image, as `docker run` takes it.
    #[serde(rename = "image")]
    pub reference: String,
    /// The program and its leading arguments inside the image; none runs
    /// the image's own entrypoint with the Run's folder alone.
    #[serde(default)]
    pub args: Vec<String>,
    /// GPUs to give it (`--gpus`); none by default.
    #[serde(default)]
    pub gpus: u32,
    /// Shared memory, in GiB (`--shm-size`).
    #[serde(default)]
    pub shm_gib: Option<u32>,
    /// Pull the image before every Run (`--pull always`).
    #[serde(default)]
    pub pull_always: bool,
    /// The kind of sign-in to do first, by name: a registry login, and
    /// credentials for the job (a provider's; docs/PLACEMENT.md §4).
    #[serde(default)]
    pub sign_in: Option<String>,
    /// A command run on this machine before the container, with the
    /// Run's folder appended (making a cache ready for it, say); the
    /// container starts only if it succeeds, and its output is the
    /// Run's log.
    #[serde(default)]
    pub before: Vec<String>,
    /// Folders of this machine mounted in the container, as `docker run
    /// -v` takes them (`/host/path:/in/container:ro`).
    #[serde(default)]
    pub volumes: Vec<String>,
}

/// Where a container finds its Run's folder.
pub const RUN_IN_CONTAINER: &str = "/stepmeld/run";

/// What signing in gives a container: variables (credentials) and any
/// further `docker run` arguments (a mount, say).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SignedIn {
    pub env: BTreeMap<String, String>,
    pub args: Vec<String>,
}

/// Signs a container in before it runs, fresh for each Run (credentials
/// expire): an `Err` refuses the start in its words.
pub type SignIn = std::sync::Arc<dyn Fn(&Image) -> Result<SignedIn, String> + Send + Sync>;

/// The command that runs an image: `docker run` and its arguments, and
/// the variables the `docker` client is given (each passed to the
/// container by name with `-e`, so no value is on a command line).
pub fn docker_run(image: &Image, container: &str, dir: &Path, env: &BTreeMap<String, String>, signed: &SignedIn) -> (Vec<String>, BTreeMap<String, String>) {
    // --quiet: no pull progress, unstamped, in the Run's log; the
    // Performer's own line names the image
    let mut argv: Vec<String> = ["docker", "run", "--rm", "--init", "--quiet", "--name", container].map(str::to_string).to_vec();
    argv.extend(["-v".to_string(), format!("{}:{RUN_IN_CONTAINER}", dir.display())]);
    let mut vars = env.clone();
    vars.extend(signed.env.clone());
    for k in vars.keys() {
        argv.extend(["-e".to_string(), k.clone()]);
    }
    if image.gpus > 0 {
        argv.extend(["--gpus".to_string(), image.gpus.to_string()]);
    }
    if let Some(gib) = image.shm_gib {
        argv.extend(["--shm-size".to_string(), format!("{gib}g")]);
    }
    if image.pull_always {
        argv.extend(["--pull".to_string(), "always".to_string()]);
    }
    for v in &image.volumes {
        argv.extend(["-v".to_string(), v.clone()]);
    }
    argv.extend(signed.args.clone());
    argv.push(image.reference.clone());
    argv.extend(image.args.clone());
    argv.push(RUN_IN_CONTAINER.to_string());
    (argv, vars)
}

/// The command that starts an image's Run: `docker run` alone, or, when
/// the image has a `before`, a shell that runs it on the Run's folder and
/// then becomes `docker run`, so the start does not wait for it and a
/// cancel stops whichever is running. `vars` are the container's
/// variables, which the shell holds for `docker run -e` to pass by name:
/// `before` runs as this machine, without them (a cache variable that
/// names the container's mount once sent a host's warm elsewhere).
pub fn container_command(image: &Image, docker: Vec<String>, dir: &Path, vars: &BTreeMap<String, String>) -> Vec<String> {
    if image.before.is_empty() {
        return docker;
    }
    let quote = |w: &str| format!("'{}'", w.replace('\'', "'\"'\"'"));
    let unset: Vec<String> = vars.keys().flat_map(|k| ["-u".to_string(), quote(k)]).collect();
    let before: Vec<String> = ["env".to_string()].into_iter().chain(unset).chain(image.before.iter().map(|w| quote(w))).chain([quote(&dir.display().to_string())]).collect();
    let run: Vec<String> = docker.iter().map(|w| quote(w)).collect();
    vec!["sh".into(), "-c".into(), format!("set -e\n{}\nexec {}", before.join(" "), run.join(" "))]
}

impl Program {
    pub fn new(name: &str, command: Vec<String>) -> Program {
        Program {
            name: name.into(),
            command,
            env: BTreeMap::new(),
            image: None,
        }
    }
}

/// How many times a resumable program is started again before its Run
/// is lost.
pub const RESUMES: u32 = 3;

/// How long a program has, after a cancel is asked, to end on its own
/// (my choice, 2026-10-17: long enough for a launcher to ask a remote
/// queue to stop its job, short enough that a cancel is felt).
pub const CANCEL_GRACE: Duration = Duration::from_secs(30);

pub struct LocalPerformer {
    name: String,
    grace: Duration,
    /// What this Performer answers to "where": the programs run here,
    /// but a program may be a launcher whose work happens elsewhere
    /// (a job on a cloud queue), and then the Performer says so.
    locality: Locality,
    root: PathBuf,
    programs: Vec<Program>,
    children: Mutex<BTreeMap<String, std::process::Child>>,
    sign_in: Option<SignIn>,
    /// What this machine has for the work, when its programs' needs are
    /// to be held to it.
    room: Option<Room>,
}

/// What a machine has for one Run's work: a request whose `needs` ask
/// for more does not fit here (docs/PLACEMENT.md §7).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Room {
    /// The memory a container may have, in GiB; unknown when none.
    pub memory_gib: Option<f64>,
    pub gpus: Vec<stepmeld_core::orders::Gpu>,
}

impl Room {
    /// Whether `needs` fit (`memory_gib`, `gpus`, `vram_gib`), or why
    /// not. A need it does not know is not its to judge.
    pub fn fits(&self, needs: &BTreeMap<String, serde_json::Value>, here: &str) -> Result<(), String> {
        let num = |k: &str| needs.get(k).and_then(|v| v.as_f64());
        if let (Some(need), Some(have)) = (num("memory_gib"), self.memory_gib) {
            if need > have {
                return Err(format!("needs {need} GiB of memory; {here} has {have:.0}"));
            }
        }
        if let Some(need) = num("gpus") {
            if need > self.gpus.len() as f64 {
                return Err(format!("needs {need} GPUs; {here} has {}", self.gpus.len()));
            }
        }
        if let Some(need) = num("vram_gib") {
            let most = self.gpus.iter().map(|g| g.memory_mib as f64 / 1024.0).fold(0.0, f64::max);
            if need > most {
                return Err(format!("needs {need} GiB of GPU memory; {here}'s largest GPU has {most:.0}"));
            }
        }
        Ok(())
    }
}

impl LocalPerformer {
    /// Runs live under `root`, one directory each.
    pub fn new(name: &str, root: &Path, programs: Vec<Program>) -> LocalPerformer {
        LocalPerformer {
            name: name.to_string(),
            grace: CANCEL_GRACE,
            locality: Locality::ThisMachine,
            root: root.to_path_buf(),
            programs,
            children: Mutex::new(BTreeMap::new()),
            sign_in: None,
            room: None,
        }
    }

    /// Hold each request's needs to what this machine has.
    pub fn with_room(mut self, room: Room) -> LocalPerformer {
        self.room = Some(room);
        self
    }

    /// Sign each container in before it runs: what an image program's
    /// `sign_in` names is this function's to do.
    pub fn with_sign_in(mut self, sign_in: SignIn) -> LocalPerformer {
        self.sign_in = Some(sign_in);
        self
    }

    /// The name a Run's container goes by.
    pub fn container(handle: &str) -> String {
        format!("stepmeld-{}", handle.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '.', "_"))
    }

    /// Remove a Run's container, if its program is an image: it may
    /// outlive the `docker` client that started it.
    fn remove_container(&self, handle: &str, dir: &Path) {
        let image = Self::read::<StartRequest>(&dir.join("request.json")).and_then(|r| self.program_for(&r).ok()).and_then(|p| p.image);
        if image.is_some() {
            let _ = Command::new("docker").args(["rm", "-f", &Self::container(handle)]).stdout(Stdio::null()).stderr(Stdio::null()).status();
        }
    }

    /// Say where the work happens, where that is not this machine: a
    /// program may be a launcher for a job on a cloud queue.
    pub fn at(mut self, locality: Locality) -> LocalPerformer {
        self.locality = locality;
        self
    }

    /// How long a program has to end on its own once a cancel is asked
    /// (zero: stopped at once).
    pub fn with_grace(mut self, grace: Duration) -> LocalPerformer {
        self.grace = grace;
        self
    }

    /// The cancel asked for a Run, if one was: its reason, and whether
    /// its deadline has passed.
    fn cancel_asked(&self, dir: &Path) -> Option<(String, bool)> {
        let path = dir.join("cancel.json");
        let asked: serde_json::Value = Self::read(&path)?;
        let reason = asked.get("reason").and_then(|r| r.as_str()).unwrap_or("canceled").to_string();
        let since = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).unwrap_or(Duration::MAX);
        Some((reason, since >= self.grace))
    }

    /// Stop the program of a Run, whoever started it.
    fn stop(&self, handle: &str, dir: &Path) {
        self.remove_container(handle, dir);
        if let Some(mut child) = self.children.lock().unwrap().remove(handle) {
            let _ = child.kill();
            let _ = child.wait();
        } else if let Some(pid) = std::fs::read_to_string(dir.join("pid")).ok().and_then(|p| p.trim().parse::<u32>().ok()) {
            let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status();
        }
    }

    /// End a Run as canceled, unless the program said how it ended.
    fn canceled(dir: &Path, reason: &str) -> Outcome {
        if let Some(outcome) = Self::read::<Outcome>(&dir.join("outcome.json")) {
            return outcome;
        }
        let outcome = Outcome::canceled(reason);
        let _ = write_atomic(&dir.join("outcome.json"), &serde_json::to_string_pretty(&outcome).unwrap());
        outcome
    }

    fn dir(&self, handle: &str) -> PathBuf {
        self.root.join(handle)
    }

    fn program_for(&self, request: &StartRequest) -> Result<Program, String> {
        let name = request.program.as_deref().ok_or_else(|| format!("{} names no program", request.definition))?;
        self.programs
            .iter()
            .find(|p| p.name == name)
            .cloned()
            .ok_or_else(|| format!("{} has no program {name:?} (for {})", self.name, request.definition))
    }

    /// Start the program on a run directory: its log appended (a
    /// resume continues the same log), its pid written, the child kept.
    fn spawn(&self, handle: &str, dir: &Path, program: &Program, resume: bool) -> Result<(), String> {
        let (command, env) = match &program.image {
            None => (program.command.clone(), program.env.clone()),
            Some(image) => {
                let signed = match (&image.sign_in, &self.sign_in) {
                    (None, _) => SignedIn::default(),
                    (Some(_), Some(sign_in)) => sign_in(image)?,
                    (Some(kind), None) => return Err(format!("{} cannot sign in {kind:?} for program {}", self.name, program.name)),
                };
                // a container left from before (a Worker restarted) goes first
                let _ = Command::new("docker").args(["rm", "-f", &Self::container(handle)]).stdout(Stdio::null()).stderr(Stdio::null()).status();
                let dir = std::fs::canonicalize(dir).map_err(|e| e.to_string())?;
                let (docker, vars) = docker_run(image, &Self::container(handle), &dir, &program.env, &signed);
                let command = container_command(image, docker, &dir, &vars);
                (command, vars)
            }
        };
        // a command is handed the Run's folder; `docker run` already
        // names the folder inside the container
        let folder = program.image.is_none().then_some(dir);
        let (exe, args) = command.split_first().ok_or_else(|| format!("{} has no command for program {}", self.name, program.name))?;
        let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log.txt")).map_err(|e| format!("cannot open the log: {e}"))?;
        {
            // the Performer's own lines are stamped like the program's
            // (contracts/run-directory.v1.md)
            use std::io::Write;
            let at = stepmeld_core::iso(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()));
            if resume {
                let _ = writeln!(&log, "{at} == resumed by {} ({})", self.name, Self::read::<u32>(&dir.join("resumes")).unwrap_or(0));
            }
            if let Some(image) = &program.image {
                let _ = writeln!(&log, "{at} == {} runs {} in a container", self.name, image.reference);
            }
        }
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let child = Command::new(exe)
            .args(args)
            .args(folder)
            .envs(&env)
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
        let program = self.program_for(&request)?;
        std::fs::write(dir.join("resumes"), (resumes + 1).to_string()).map_err(|e| e.to_string())?;
        self.spawn(handle, dir, &program, true)
    }

    fn handle_for(request: &StartRequest) -> String {
        request.run.replace(['/', '\\', ':'], "_")
    }

    fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }

    /// The log's last words, without the line's stamp.
    fn last_line(path: &Path) -> Option<String> {
        let text = std::fs::read_to_string(path).ok()?;
        let unstamped = |l: &str| match l.get(..20).and_then(stepmeld_core::secs) {
            Some(_) => l[20..].trim_start().to_string(),
            None => l.to_string(),
        };
        text.lines().map(unstamped).rev().find(|l| !l.trim().is_empty())
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
            pool: false,
            name: self.name.clone(),
            locality: self.locality,
            // what it can run is its programs, by name; which
            // StepDefinitions those are is the Library's to say
            verbs: Vec::new(),
        }
    }

    fn fits(&self, request: &StartRequest) -> Result<(), String> {
        match &self.room {
            Some(room) => room.fits(&request.needs, &self.name),
            None => Ok(()),
        }
    }

    fn start(&self, request: &StartRequest) -> Result<String, String> {
        let handle = Self::handle_for(request);
        let dir = self.dir(&handle);
        if dir.join("request.json").exists() {
            return Ok(handle);
        }
        let program = self.program_for(request)?;
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
            Some(_) if self.cancel_asked(&dir).is_some() => {
                // gone after a cancel was asked, without saying how it
                // ended: canceled, never resumed
                let (reason, _) = self.cancel_asked(&dir).unwrap_or_default();
                Observation::Ended { outcome: Self::canceled(&dir, &reason) }
            }
            Some(status) => {
                // a container may outlive its `docker` client
                self.remove_container(handle, &dir);
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
        if !dir.join("request.json").exists() || dir.join("outcome.json").exists() {
            return;
        }
        if self.cancel_asked(&dir).is_none() {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let asked = serde_json::json!({"reason": reason, "at": stepmeld_core::iso(now), "deadline": stepmeld_core::iso(now + self.grace.as_secs())});
            let _ = write_atomic(&dir.join("cancel.json"), &serde_json::to_string_pretty(&asked).unwrap());
        }
        // the program has until the deadline to end on its own; asked
        // again past it, it is stopped
        if let Some((reason, true)) = self.cancel_asked(&dir) {
            self.stop(handle, &dir);
            Self::canceled(&dir, &format!("{reason} (stopped at the deadline)"));
        }
    }

    fn log(&self, handle: &str) -> Option<String> {
        std::fs::read_to_string(self.dir(handle).join("log.txt")).ok()
    }
}

/// What this machine can run, asked of the programs that know:
/// whether Docker answers, whether a GPU container can run (Docker's
/// `nvidia` runtime and a GPU `nvidia-smi` lists), and the GPUs with
/// their memory in use (docs/PLACEMENT.md §6).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Probe {
    pub docker: bool,
    pub cuda: bool,
    pub gpus: Vec<stepmeld_core::orders::Gpu>,
    /// The memory Docker may give a container, in GiB (its machine's,
    /// or its virtual machine's where it runs in one).
    pub memory_gib: Option<f64>,
}

impl Probe {
    /// The capability words it found: `docker`, `cuda`.
    pub fn capabilities(&self) -> Vec<String> {
        [("docker", self.docker), ("cuda", self.cuda)].into_iter().filter(|(_, has)| *has).map(|(w, _)| w.to_string()).collect()
    }

    /// What it found for one Run's work.
    pub fn room(&self) -> Room {
        Room {
            memory_gib: self.memory_gib,
            gpus: self.gpus.clone(),
        }
    }
}

fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The GPUs `nvidia-smi` lists, with their memory; none without it.
pub fn gpus() -> Vec<stepmeld_core::orders::Gpu> {
    let Some(text) = output("nvidia-smi", &["--query-gpu=name,memory.total,memory.used", "--format=csv,noheader,nounits"]) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut f = line.split(',').map(str::trim);
            Some(stepmeld_core::orders::Gpu {
                name: f.next()?.to_string(),
                memory_mib: f.next()?.parse().ok()?,
                used_mib: f.next()?.parse().ok()?,
            })
        })
        .collect()
}

/// Ask Docker and `nvidia-smi` what this machine can run.
pub fn probe() -> Probe {
    let runtimes = output("docker", &["info", "--format", "{{json .Runtimes}}"]);
    let gpus = gpus();
    let memory_gib = output("docker", &["info", "--format", "{{.MemTotal}}"]).and_then(|t| t.trim().parse::<f64>().ok()).map(|b| b / (1u64 << 30) as f64);
    Probe {
        docker: runtimes.is_some(),
        cuda: runtimes.is_some_and(|r| r.contains("nvidia")) && !gpus.is_empty(),
        gpus,
        memory_gib,
    }
}

/// Write a file the program may be reading: whole, or not at all.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stepmeld_core::definition::{OutputDefinition, Ref};

    fn request(run: &str, outputs: &[&str]) -> StartRequest {
        StartRequest {
            needs: Default::default(),
            workflow: "wf".into(),
            step: "s".into(),
            run: run.into(),
            definition: Ref::new("stepdef_echo", 1),
            program: Some("echo-program".into()),
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
        LocalPerformer::new("local", &root.join("runs"), vec![Program::new("echo-program", vec!["sh".into(), path.display().to_string()])])
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
        let p = performer(dir.path(), r#"echo '{"phase":"working","done":1,"total":4,"at":"t"}' > "$1/progress.json"; sleep 30"#).with_grace(Duration::ZERO);
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

    fn doc(dir: &Path, handle: &str, file: &str) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join("runs").join(handle).join(file)).unwrap()).unwrap()
    }

    /// The files of the run directory are the contracts' documents: the
    /// request the Performer writes, the cancel it asks, the outcome
    /// either side writes.
    #[test]
    fn the_run_directory_holds_to_its_contracts() {
        let dir = tempfile::tempdir().unwrap();
        let p = performer(dir.path(), r#"while [ ! -f "$1/cancel.json" ]; do sleep 0.02; done"#).with_grace(Duration::ZERO);
        let mut req = request("wf/s/1", &["words"]);
        req.inputs.insert("text".into(), serde_json::json!({"file": "notes.txt"}));
        let h = p.start(&req).unwrap();
        p.cancel(&h, "enough");
        assert!(matches!(settle(&p, &h), Observation::Ended { .. }));
        for (file, contract) in [("request.json", "stepmeld/run-request.v1"), ("cancel.json", "stepmeld/run-cancel.v1"), ("outcome.json", "stepmeld/run-outcome.v1")] {
            let d = doc(dir.path(), &h, file);
            assert_eq!(stepmeld_core::contracts::validate(&d, contract), Ok(()), "{file}: {d}");
        }
    }

    /// A program told of a cancel ends on its own, in its own words,
    /// before the deadline: a launcher stops its remote job first.
    #[test]
    fn a_program_that_hears_the_cancel_ends_in_its_own_words() {
        let dir = tempfile::tempdir().unwrap();
        let p = performer(
            dir.path(),
            r#"while [ ! -f "$1/cancel.json" ]; do sleep 0.02; done
echo '{"state":"canceled","class":"canceled","reason":"stopped its remote job"}' > "$1/outcome.json""#,
        );
        let h = p.start(&request("wf/s/1", &[])).unwrap();
        p.cancel(&h, "enough");
        match settle(&p, &h) {
            Observation::Ended { outcome } => assert_eq!(outcome.reason.as_deref(), Some("stopped its remote job")),
            other => panic!("{other:?}"),
        }
        assert_eq!(doc(dir.path(), &h, "cancel.json")["reason"], "enough");
    }

    /// A program that ignores the cancel runs on until the deadline, and
    /// is stopped when the cancel is asked again past it.
    #[test]
    fn a_program_that_ignores_the_cancel_is_stopped_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let p = performer(dir.path(), "sleep 30").with_grace(Duration::from_millis(300));
        let h = p.start(&request("wf/s/1", &[])).unwrap();
        p.cancel(&h, "enough");
        assert!(matches!(p.observe(&h), Observation::Running { .. }), "within its grace it runs");
        std::thread::sleep(Duration::from_millis(400));
        p.cancel(&h, "enough");
        match p.observe(&h) {
            Observation::Ended { outcome } => assert!(outcome.class.as_deref() == Some("canceled") && outcome.reason.as_deref().unwrap_or("").contains("deadline"), "{outcome:?}"),
            other => panic!("{other:?}"),
        }
    }

    /// A resumable program gone after a cancel is canceled, never
    /// started again.
    #[test]
    fn a_resumable_program_gone_after_a_cancel_is_not_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let p = performer(
            dir.path(),
            r#"echo '{"job":"j-1"}' > "$1/state.json"; touch "$1/resumable"
while [ ! -f "$1/cancel.json" ]; do sleep 0.02; done
exit 1"#,
        );
        let h = p.start(&request("wf/s/1", &[])).unwrap();
        p.cancel(&h, "enough");
        match settle(&p, &h) {
            Observation::Ended { outcome } => assert_eq!(outcome.class.as_deref(), Some("canceled")),
            other => panic!("{other:?}"),
        }
        assert!(!dir.path().join("runs").join(&h).join("resumes").exists(), "never resumed");
    }

    /// A launcher for work elsewhere says so, and the gate's confirm
    /// policies see it (2026-10-06: a cloud performer that answered
    /// this-machine launched cloud work past a confirm-before-cloud
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

    #[test]
    fn an_image_runs_by_docker_run_with_its_folder_mounted_and_secrets_by_name() {
        let image = Image {
            reference: "registry.example/typeset:3".into(),
            args: vec!["typeset".into(), "--fast".into()],
            gpus: 1,
            shm_gib: Some(8),
            pull_always: true,
            sign_in: Some("registry".into()),
            ..Default::default()
        };
        let env = BTreeMap::from([("FONT_TOKEN".to_string(), "s3cret".to_string())]);
        let signed = SignedIn {
            env: BTreeMap::from([("AWS_SESSION_TOKEN".to_string(), "t".to_string())]),
            args: vec!["-v".into(), "/cache:/cache:ro".into()],
        };
        let (argv, vars) = docker_run(&image, "stepmeld-wf_s_1", Path::new("/runs/wf_s_1"), &env, &signed);
        assert_eq!(
            argv.join(" "),
            "docker run --rm --init --quiet --name stepmeld-wf_s_1 -v /runs/wf_s_1:/stepmeld/run -e AWS_SESSION_TOKEN -e FONT_TOKEN --gpus 1 --shm-size 8g --pull always -v /cache:/cache:ro registry.example/typeset:3 typeset --fast /stepmeld/run"
        );
        assert!(!argv.iter().any(|a| a.contains("s3cret")), "no secret on the command line");
        assert_eq!(vars.len(), 2);
        assert_eq!(LocalPerformer::container("wf/s:1"), "stepmeld-wf_s_1");
        assert_eq!(container_command(&image, argv.clone(), Path::new("/runs/wf_s_1"), &vars), argv, "no `before`: docker run itself");

        let warmed = Image {
            before: vec!["/opt/warm".into(), "it's".into()],
            volumes: vec!["/cache:/objects:ro".into()],
            ..image.clone()
        };
        let (argv, _) = docker_run(&warmed, "c", Path::new("/runs/r"), &BTreeMap::new(), &SignedIn::default());
        assert!(argv.join(" ").contains(" -v /cache:/objects:ro "), "{argv:?}");
        let vars = BTreeMap::from([("CACHE".to_string(), "/objects".to_string())]);
        let cmd = container_command(&warmed, argv, Path::new("/runs/r"), &vars);
        assert_eq!(&cmd[..2], ["sh", "-c"]);
        assert!(cmd[2].starts_with("set -e\nenv -u 'CACHE' '/opt/warm' 'it'\"'\"'s' '/runs/r'\nexec 'docker' 'run' "), "{}", cmd[2]);
    }

    #[test]
    fn a_machine_holds_the_needs_it_has_room_for() {
        let gpu = |gib: u64| stepmeld_core::orders::Gpu {
            name: "card".into(),
            memory_mib: gib * 1024,
            used_mib: 0,
        };
        let room = Room { memory_gib: Some(64.0), gpus: vec![gpu(24)] };
        let needs = |v: serde_json::Value| serde_json::from_value::<BTreeMap<String, serde_json::Value>>(v).unwrap();
        assert_eq!(room.fits(&needs(serde_json::json!({"memory_gib": 60, "gpus": 1, "vram_gib": 20, "capacity": "spot"})), "box"), Ok(()));
        assert_eq!(room.fits(&needs(serde_json::json!({"memory_gib": 96})), "box"), Err("needs 96 GiB of memory; box has 64".into()));
        assert_eq!(room.fits(&needs(serde_json::json!({"gpus": 2})), "box"), Err("needs 2 GPUs; box has 1".into()));
        assert_eq!(room.fits(&needs(serde_json::json!({"vram_gib": 40})), "box"), Err("needs 40 GiB of GPU memory; box's largest GPU has 24".into()));
        assert_eq!(Room::default().fits(&needs(serde_json::json!({"memory_gib": 900})), "box"), Ok(()), "memory unknown: not judged");
        let p = LocalPerformer::new("box", Path::new("/nonexistent"), Vec::new()).with_room(room);
        let mut r = request("r", &[]);
        r.needs = needs(serde_json::json!({"memory_gib": 96}));
        assert!(p.fits(&r).is_err());
        assert!(LocalPerformer::new("box", Path::new("/nonexistent"), Vec::new()).fits(&r).is_ok(), "no room said: everything fits");
    }

    /// With Docker here: an image that speaks the run directory runs as
    /// any program does, reads a secret by name, and a canceled one's
    /// container is removed. Skipped where Docker does not answer.
    #[test]
    fn an_image_runs_in_docker_when_docker_is_here() {
        if !probe().docker {
            eprintln!("skipped: Docker does not answer here");
            return;
        }
        // under the repository: a Docker VM may share only the user's
        // folders, and a mount from elsewhere comes up empty
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target");
        std::fs::create_dir_all(&target).unwrap();
        let dir = tempfile::tempdir_in(&target).unwrap();
        let build = dir.path().join("build");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(
            build.join("verb.sh"),
            "#!/bin/sh\nrun=\"$1\"\nif grep -q slow \"$run/request.json\"; then sleep 60; fi\necho \"token is $TOKEN\"\n[ -f /cache/note ] && echo \"cached $(cat /cache/note)\"\necho '{\"state\":\"succeeded\",\"outputs\":{\"out\":\"from a container\"}}' > \"$run/outcome.json\"\n",
        )
        .unwrap();
        std::fs::write(build.join("Dockerfile"), "FROM alpine:3.20\nCOPY verb.sh /verb.sh\nRUN chmod +x /verb.sh\nENTRYPOINT [\"/verb.sh\"]\n").unwrap();
        let built = Command::new("docker").args(["build", "-q", "-t", "stepmeld-test-verb:1"]).arg(&build).output().unwrap();
        if !built.status.success() {
            eprintln!("skipped: the test image did not build: {}", String::from_utf8_lossy(&built.stderr));
            return;
        }
        // a folder of this machine, made ready by `before` and mounted
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let program = Program {
            env: BTreeMap::from([("TOKEN".to_string(), "t-42".to_string())]),
            image: Some(Image {
                reference: "stepmeld-test-verb:1".into(),
                before: vec!["sh".into(), "-c".into(), format!("echo warm > {}/note; echo \"made ready for $0 (token: ${{TOKEN:-none}})\"", cache.display())],
                volumes: vec![format!("{}:/cache:ro", cache.display())],
                ..Default::default()
            }),
            ..Program::new("echo-program", Vec::new())
        };
        let p = LocalPerformer::new("bench", &dir.path().join("runs"), vec![program]).with_grace(Duration::ZERO);
        let h = p.start(&request("wf/s/1", &["out"])).unwrap();
        match settle_long(&p, &h) {
            Observation::Ended { outcome } => assert_eq!(outcome.outputs.get("out"), Some(&serde_json::json!("from a container")), "{outcome:?} {:?}", p.log(&h)),
            other => panic!("{other:?}"),
        }
        assert!(p.log(&h).unwrap().contains("token is t-42"), "the secret reached the container by name");
        let log = p.log(&h).unwrap();
        assert!(log.contains("made ready for ") && log.contains("cached warm"), "`before` ran first, on the Run's folder, and the mount is there: {log}");
        assert!(log.contains("(token: none)"), "`before` runs as the machine, without the container's variables: {log}");

        let mut slow = request("wf/slow/1", &["out"]);
        slow.inputs.insert("speed".into(), serde_json::json!("slow"));
        let h = p.start(&slow).unwrap();
        let name = LocalPerformer::container(&h);
        for _ in 0..100 {
            if output("docker", &["inspect", "-f", "{{.State.Running}}", &name]).is_some_and(|o| o.trim() == "true") {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        p.cancel(&h, "enough");
        p.cancel(&h, "enough");
        assert!(matches!(p.observe(&h), Observation::Ended { .. }));
        assert!(output("docker", &["inspect", &name]).is_none(), "the container is gone");
    }

    fn settle_long(p: &LocalPerformer, handle: &str) -> Observation {
        for _ in 0..600 {
            match p.observe(handle) {
                Observation::Running { .. } => std::thread::sleep(std::time::Duration::from_millis(50)),
                other => return other,
            }
        }
        panic!("the run did not end");
    }
}
