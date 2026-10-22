//! Programs run as AWS Batch jobs (docs/PLACEMENT.md §5, §9).
//!
//! A Run's folder is kept in S3, beside the deployment's other runs
//! (`[providers.aws] runs = "s3://bucket/prefix/"`): `request.json` in;
//! `progress.json`, `log.txt`, `state.json`, `outcome.json` out, as on a
//! machine (`contracts/run-directory.v1.md`). The job is told where with
//! `STEPMELD_RUN`, and its image runs the program under `stepmeld-run`
//! ([`wrapped`]), which keeps a local copy of the folder for the program
//! and the S3 one up to date. The Performer submits the job, observes it
//! by its state and its folder, and cancels by the folder's
//! `cancel.json` then, at the deadline, by terminating it. Its handles
//! are portable: any cloud Worker that can ask Batch and S3 observes
//! the job another launched.
//!
//! `[programs.<name>.batch]`:
//!
//! ```toml
//! definition = "example-typeset"                  # the job definition
//! queues = { on-demand = "example-ondemand", spot = "example-spot" }
//! capacity = "on-demand"                          # unless a Step's needs say `capacity`
//! ladder = [[28, 4], [56, 8], [120, 16]]          # (GiB, vCPU) sizes; the smallest that holds the need
//! ```
//!
//! What a Step needs (`needs`): `memory_gib`, `vcpus`, `capacity`. A
//! need above the top of the ladder is refused in words. Secrets reach a
//! Batch job through its job definition, never through a submit: a
//! submit's variables can be read back from the job's description, so
//! the secrets a program names are not sent; its job definition injects
//! them. A program's `env` (`[programs.<name>] env`, no secret) is sent
//! with the submit, beside `STEPMELD_RUN`.

use crate::block_on;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use stepmeld_core::definition::Locality;
use stepmeld_core::engine::{Observation, StartRequest};
use stepmeld_core::objects::{Cond, Objects};
use stepmeld_core::performer::Performer;
use stepmeld_core::workflow::{Outcome, PerformerFace, Progress};
use stepmeld_core::Error;

/// The variable that tells a job where its Run's folder is.
pub const RUN_ENV: &str = "STEPMELD_RUN";
/// What Batch is told when stepmeld cancels a job; its reason begins so.
pub const CANCELED: &str = "canceled: ";
/// How long a job has, after a cancel is asked, to end on its own.
pub const CANCEL_GRACE: Duration = Duration::from_secs(30);
/// How many times in a row a job may be unknown to Batch before its Run
/// is lost (a job just submitted may not be listed yet).
pub const MISSES: u32 = 4;

/// What a submit asks for.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct JobSpec {
    pub name: String,
    pub queue: String,
    pub definition: String,
    pub env: Vec<(String, String)>,
    /// (GiB, vCPU), when the need named a size.
    pub size: Option<(u64, u64)>,
}

/// Where a job stands, in stepmeld's words.
#[derive(Debug, Clone, PartialEq)]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed { reason: String, exit_code: Option<i32> },
    Canceled { reason: String },
}

/// A batch service: what the Performer asks of Batch. [`BatchJobs`] is
/// AWS's; [`MemoryJobs`] the tests'.
pub trait Jobs: Send + Sync {
    fn submit(&self, spec: &JobSpec) -> Result<String, String>;
    /// `None`: the service does not know the job.
    fn state(&self, id: &str) -> Result<Option<JobState>, String>;
    fn terminate(&self, id: &str, reason: &str) -> Result<(), String>;
}

/// AWS Batch.
pub struct BatchJobs {
    client: aws_sdk_batch::Client,
}

impl BatchJobs {
    pub fn new(client: aws_sdk_batch::Client) -> BatchJobs {
        BatchJobs { client }
    }
}

impl Jobs for BatchJobs {
    fn submit(&self, spec: &JobSpec) -> Result<String, String> {
        use aws_sdk_batch::types::{ContainerOverrides, KeyValuePair, ResourceRequirement, ResourceType};
        let env: Vec<KeyValuePair> = spec.env.iter().map(|(k, v)| KeyValuePair::builder().name(k).value(v).build()).collect();
        let mut overrides = ContainerOverrides::builder().set_environment(Some(env));
        if let Some((gib, vcpu)) = spec.size {
            overrides = overrides
                .resource_requirements(ResourceRequirement::builder().r#type(ResourceType::Memory).value((gib * 1024).to_string()).build())
                .resource_requirements(ResourceRequirement::builder().r#type(ResourceType::Vcpu).value(vcpu.to_string()).build());
        }
        let sent = block_on(
            self.client
                .submit_job()
                .job_name(&spec.name)
                .job_queue(&spec.queue)
                .job_definition(&spec.definition)
                .container_overrides(overrides.build())
                .send(),
        );
        sent.map_err(|e| crate::refused("submit the job", &spec.name, e).to_string())?
            .job_id()
            .map(str::to_string)
            .ok_or_else(|| "Batch gave no job id".into())
    }

    fn state(&self, id: &str) -> Result<Option<JobState>, String> {
        use aws_sdk_batch::types::JobStatus;
        let out = block_on(self.client.describe_jobs().jobs(id).send()).map_err(|e| crate::refused("ask after the job", id, e).to_string())?;
        let Some(job) = out.jobs().first() else { return Ok(None) };
        let reason = job.status_reason().unwrap_or_default().to_string();
        Ok(Some(match job.status() {
            Some(JobStatus::Submitted | JobStatus::Pending | JobStatus::Runnable) => JobState::Queued,
            Some(JobStatus::Starting | JobStatus::Running) => JobState::Running,
            Some(JobStatus::Succeeded) => JobState::Succeeded,
            _ if reason.starts_with(CANCELED) => JobState::Canceled {
                reason: reason[CANCELED.len()..].to_string(),
            },
            _ => {
                let container = job.container();
                let why = container.and_then(|c| c.reason()).map(str::to_string).filter(|r| !r.is_empty()).unwrap_or(reason);
                JobState::Failed {
                    reason: why,
                    exit_code: container.and_then(|c| c.exit_code()),
                }
            }
        }))
    }

    fn terminate(&self, id: &str, reason: &str) -> Result<(), String> {
        let reason: String = format!("{CANCELED}{reason}").chars().take(250).collect();
        block_on(self.client.terminate_job().job_id(id).reason(reason).send())
            .map(|_| ())
            .map_err(|e| crate::refused("terminate the job", id, e).to_string())
    }
}

/// Each job's spec and state, and the last id handed out.
type Held = (BTreeMap<String, (JobSpec, JobState)>, u64);

/// Jobs held in memory: the tests'. A test moves each along.
#[derive(Default)]
pub struct MemoryJobs {
    inner: Mutex<Held>,
}

impl MemoryJobs {
    pub fn new() -> MemoryJobs {
        MemoryJobs::default()
    }

    pub fn set(&self, id: &str, state: JobState) {
        if let Some(j) = self.inner.lock().unwrap().0.get_mut(id) {
            j.1 = state;
        }
    }

    pub fn forget(&self, id: &str) {
        self.inner.lock().unwrap().0.remove(id);
    }

    pub fn submitted(&self) -> Vec<(String, JobSpec)> {
        self.inner.lock().unwrap().0.iter().map(|(id, (spec, _))| (id.clone(), spec.clone())).collect()
    }
}

impl Jobs for MemoryJobs {
    fn submit(&self, spec: &JobSpec) -> Result<String, String> {
        let mut inner = self.inner.lock().unwrap();
        inner.1 += 1;
        let id = format!("job-{}", inner.1);
        inner.0.insert(id.clone(), (spec.clone(), JobState::Queued));
        Ok(id)
    }

    fn state(&self, id: &str) -> Result<Option<JobState>, String> {
        Ok(self.inner.lock().unwrap().0.get(id).map(|j| j.1.clone()))
    }

    fn terminate(&self, id: &str, reason: &str) -> Result<(), String> {
        self.set(id, JobState::Canceled { reason: reason.into() });
        Ok(())
    }
}

/// How one program runs on Batch: `[programs.<name>.batch]`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct BatchProgram {
    pub definition: String,
    pub queues: BTreeMap<String, String>,
    #[serde(default = "on_demand")]
    pub capacity: String,
    /// (GiB, vCPU) sizes, smallest first.
    #[serde(default)]
    pub ladder: Vec<(u64, u64)>,
    /// The GPU memory each of its machines has, in GiB, where its jobs
    /// get a GPU: a request that needs more (`needs.vram_gib`) is
    /// refused before anything is submitted.
    #[serde(default)]
    pub vram_gib: Option<f64>,
    /// Variables sent with each submit: the program's `env`, never a
    /// secret.
    #[serde(skip)]
    pub env: BTreeMap<String, String>,
}

fn on_demand() -> String {
    "on-demand".into()
}

impl BatchProgram {
    /// The queue and size a request's needs come to.
    pub fn place(&self, needs: &BTreeMap<String, serde_json::Value>) -> Result<(String, Option<(u64, u64)>), String> {
        let capacity = needs.get("capacity").and_then(|c| c.as_str()).unwrap_or(&self.capacity);
        let queue = self
            .queues
            .get(capacity)
            .ok_or_else(|| format!("no {capacity} queue for {} (there are {:?})", self.definition, self.queues.keys().collect::<Vec<_>>()))?;
        if let (Some(need), Some(have)) = (needs.get("vram_gib").and_then(|v| v.as_f64()), self.vram_gib) {
            if need > have {
                return Err(format!("{} needs {need} GiB of GPU memory, more than its machines' GPUs have ({have})", self.definition));
            }
        }
        let memory = needs.get("memory_gib").and_then(|v| v.as_f64());
        let vcpus = needs.get("vcpus").and_then(|v| v.as_f64());
        if memory.is_none() && vcpus.is_none() {
            return Ok((queue.clone(), None));
        }
        let rung = self.ladder.iter().find(|(g, v)| memory.is_none_or(|m| *g as f64 >= m) && vcpus.is_none_or(|c| *v as f64 >= c));
        match rung {
            Some(r) => Ok((queue.clone(), Some(*r))),
            None => Err(format!(
                "{} needs {} GiB and {} vCPU, more than the largest size there is ({:?})",
                self.definition,
                memory.unwrap_or(0.0),
                vcpus.unwrap_or(0.0),
                self.ladder.last()
            )),
        }
    }
}

/// Runs programs as Batch jobs, their folders in an object store.
pub struct BatchPerformer {
    name: String,
    jobs: Arc<dyn Jobs>,
    folders: Arc<dyn Objects>,
    /// The folders' key prefix in the object store, and the same as a
    /// URL for the job (`s3://bucket/prefix/`).
    prefix: String,
    url: String,
    programs: BTreeMap<String, BatchProgram>,
    grace: Duration,
    misses: Mutex<BTreeMap<String, u32>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Launched {
    job: String,
}

impl BatchPerformer {
    /// `url` is where the folders are, as the job is told
    /// (`s3://bucket/prefix/`); `prefix` the same within `folders`.
    pub fn new(name: &str, jobs: Arc<dyn Jobs>, folders: Arc<dyn Objects>, url: &str, prefix: &str, programs: BTreeMap<String, BatchProgram>) -> BatchPerformer {
        let slash = |s: &str| if s.is_empty() || s.ends_with('/') { s.to_string() } else { format!("{s}/") };
        BatchPerformer {
            name: name.into(),
            jobs,
            folders,
            prefix: slash(prefix),
            url: slash(url),
            programs,
            grace: CANCEL_GRACE,
            misses: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn with_grace(mut self, grace: Duration) -> BatchPerformer {
        self.grace = grace;
        self
    }

    fn key(&self, handle: &str, file: &str) -> String {
        format!("{}{handle}/{file}", self.prefix)
    }

    fn read<T: serde::de::DeserializeOwned>(&self, handle: &str, file: &str) -> Option<T> {
        let (body, _) = self.folders.get(&self.key(handle, file)).ok()??;
        serde_json::from_slice(&body).ok()
    }

    fn write(&self, handle: &str, file: &str, value: &impl serde::Serialize, cond: Cond) -> Result<bool, String> {
        self.folders.put(&self.key(handle, file), &serde_json::to_vec_pretty(value).unwrap(), cond).map(|v| v.is_some()).map_err(|e| e.to_string())
    }

    fn handle_for(request: &StartRequest) -> String {
        request.run.replace(['/', '\\', ':'], "_")
    }

    /// End a Run with this outcome, unless one was written first.
    fn end(&self, handle: &str, outcome: Outcome) -> Outcome {
        match self.write(handle, "outcome.json", &outcome, Cond::IfNoneMatch) {
            Ok(false) => self.read(handle, "outcome.json").unwrap_or(outcome),
            _ => outcome,
        }
    }

    fn cancel_asked(&self, handle: &str) -> Option<(String, bool)> {
        let asked: serde_json::Value = self.read(handle, "cancel.json")?;
        let reason = asked.get("reason").and_then(|r| r.as_str()).unwrap_or("canceled").to_string();
        let deadline = asked.get("deadline").and_then(|d| d.as_str()).unwrap_or_default().to_string();
        let now = stepmeld_core::iso(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()));
        Some((reason, deadline.as_str() <= now.as_str()))
    }
}

impl Performer for BatchPerformer {
    fn describe(&self) -> PerformerFace {
        PerformerFace {
            name: self.name.clone(),
            locality: Locality::Cloud,
            verbs: Vec::new(),
            pool: false,
        }
    }

    fn start(&self, request: &StartRequest) -> Result<String, String> {
        let handle = Self::handle_for(request);
        if self.read::<Launched>(&handle, "job.json").is_some() {
            return Ok(handle);
        }
        let name = request.program.as_deref().unwrap_or_default();
        let program = self.programs.get(name).ok_or_else(|| format!("{} has no Batch settings for program {name:?}", self.name))?;
        let (queue, size) = program.place(&request.needs)?;
        self.write(&handle, "request.json", request, Cond::None).map_err(|e| format!("unreachable: the Run's folder: {e}"))?;
        let job_name: String = format!("stepmeld-{handle}").chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(128).collect();
        let spec = JobSpec {
            name: job_name,
            queue,
            definition: program.definition.clone(),
            env: program.env.iter().map(|(k, v)| (k.clone(), v.clone())).chain([(RUN_ENV.to_string(), format!("{}{handle}/", self.url))]).collect(),
            size,
        };
        let job = self.jobs.submit(&spec)?;
        if !self.write(&handle, "job.json", &Launched { job: job.clone() }, Cond::IfNoneMatch)? {
            // another launched it first: keep theirs, stop ours
            let _ = self.jobs.terminate(&job, "launched twice; the other one stands");
        }
        Ok(handle)
    }

    fn observe(&self, handle: &str) -> Observation {
        if let Some(outcome) = self.read::<Outcome>(handle, "outcome.json") {
            return Observation::Ended { outcome };
        }
        let Some(launched) = self.read::<Launched>(handle, "job.json") else {
            return if self.read::<serde_json::Value>(handle, "request.json").is_some() {
                Observation::Running { progress: None, attention: None }
            } else {
                Observation::Absent
            };
        };
        let progress = || self.read::<Progress>(handle, "progress.json");
        match self.jobs.state(&launched.job) {
            Err(reason) => Observation::Unreachable { reason },
            Ok(None) => {
                let mut misses = self.misses.lock().unwrap();
                let n = misses.entry(handle.to_string()).or_default();
                *n += 1;
                if *n > MISSES {
                    misses.remove(handle);
                    Observation::Ended {
                        outcome: self.end(handle, Outcome::lost(&format!("Batch no longer knows job {}", launched.job))),
                    }
                } else {
                    Observation::Running { progress: progress(), attention: None }
                }
            }
            Ok(Some(state)) => {
                self.misses.lock().unwrap().remove(handle);
                match state {
                    JobState::Queued | JobState::Running => Observation::Running { progress: progress(), attention: None },
                    // the wrapper writes the outcome before it exits;
                    // one more look, then judge as on a machine
                    JobState::Succeeded => {
                        let declared = self.read::<StartRequest>(handle, "request.json").map(|r| r.outputs.len()).unwrap_or(0);
                        let outcome = if declared == 0 {
                            Outcome::succeeded(BTreeMap::new())
                        } else {
                            Outcome::failed("fault", "the job succeeded without writing outcome.json")
                        };
                        Observation::Ended { outcome: self.end(handle, outcome) }
                    }
                    JobState::Canceled { reason } => Observation::Ended {
                        outcome: self.end(handle, Outcome::canceled(&reason)),
                    },
                    JobState::Failed { reason, exit_code } => {
                        let why = match exit_code {
                            Some(c) => format!("the job ended with exit {c}: {reason}"),
                            None => format!("the job failed: {reason}"),
                        };
                        Observation::Ended {
                            outcome: self.end(handle, Outcome::failed("exit", &why)),
                        }
                    }
                }
            }
        }
    }

    fn cancel(&self, handle: &str, reason: &str) {
        if self.read::<Outcome>(handle, "outcome.json").is_some() {
            return;
        }
        if self.cancel_asked(handle).is_none() {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let asked = serde_json::json!({"reason": reason, "at": stepmeld_core::iso(now), "deadline": stepmeld_core::iso(now + self.grace.as_secs())});
            let _ = self.write(handle, "cancel.json", &asked, Cond::IfNoneMatch);
        }
        let Some(launched) = self.read::<Launched>(handle, "job.json") else {
            self.end(handle, Outcome::canceled(reason));
            return;
        };
        // a job not yet running has nothing to stop on its own
        let queued = matches!(self.jobs.state(&launched.job), Ok(Some(JobState::Queued)));
        if let Some((reason, past)) = self.cancel_asked(handle) {
            if past || queued {
                let _ = self.jobs.terminate(&launched.job, &reason);
                self.end(handle, Outcome::canceled(&format!("{reason} (stopped at the deadline)")));
            }
        }
    }

    fn log(&self, handle: &str) -> Option<String> {
        let (body, _) = self.folders.get(&self.key(handle, "log.txt")).ok()??;
        Some(String::from_utf8_lossy(&body).into_owned())
    }

    fn portable(&self) -> bool {
        true
    }
}

/// The files the job's side keeps in step, from the local copy to the
/// object store.
const UP: [&str; 4] = ["progress.json", "log.txt", "state.json", "resumable"];

/// How long a program told to stop (SIGTERM) has before it is killed.
/// Under Batch's default stop timeout (30 s), after which the container
/// itself is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(10);

/// Run `program` (its executable and leading arguments) on a local copy
/// of the Run's folder kept at `prefix` in `folders`: what `stepmeld-run`
/// does inside a Batch job. The request (and any state from an earlier
/// attempt) comes down; progress, log, state and, last, the outcome go
/// up every `every` and at the end; a cancel asked comes down. The
/// program's exit code.
///
/// A cancel is kept as on a machine: the program has until the
/// deadline in `cancel.json` to end on its own; then it is told to stop
/// (SIGTERM to its process group) and, [`STOP_GRACE`] later, killed. A
/// program gone after a cancel without an outcome of its own ends
/// canceled.
pub fn wrapped(folders: &dyn Objects, prefix: &str, program: &[String], local: &Path, every: Duration) -> Result<i32, Error> {
    wrapped_until(folders, prefix, program, local, every, &AtomicBool::new(false))
}

/// [`wrapped`], also stopping the program as soon as `term` is set:
/// `stepmeld-run` sets it on SIGTERM, which Batch sends the job's first
/// process (the wrapper, not the program) when the job is terminated.
pub fn wrapped_until(folders: &dyn Objects, prefix: &str, program: &[String], local: &Path, every: Duration, term: &AtomicBool) -> Result<i32, Error> {
    let prefix = if prefix.ends_with('/') { prefix.to_string() } else { format!("{prefix}/") };
    std::fs::create_dir_all(local).map_err(|e| Error::Refused(format!("cannot make {}: {e}", local.display())))?;
    for file in ["request.json", "state.json", "resumable"] {
        if let Some((body, _)) = folders.get(&format!("{prefix}{file}"))? {
            std::fs::write(local.join(file), body).map_err(|e| Error::Refused(e.to_string()))?;
        }
    }
    if !local.join("request.json").exists() {
        return Err(Error::Refused(format!("no request.json at {prefix}")));
    }
    let (exe, args) = program.split_first().ok_or_else(|| Error::Refused("stepmeld-run needs the program to run".into()))?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(local.join("log.txt")).map_err(|e| Error::Refused(e.to_string()))?;
    let err = log.try_clone().map_err(|e| Error::Refused(e.to_string()))?;
    let mut command = std::process::Command::new(exe);
    command.args(args).arg(local).stdout(std::process::Stdio::from(log)).stderr(std::process::Stdio::from(err));
    // its own process group, so a stop reaches what it started too
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().map_err(|e| Error::Refused(format!("cannot run {exe}: {e}")))?;
    let mut sent: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    let sync = |sent: &mut BTreeMap<&'static str, Vec<u8>>| -> Result<(), Error> {
        for file in UP {
            if let Ok(body) = std::fs::read(local.join(file)) {
                if sent.get(file) != Some(&body) {
                    folders.put(&format!("{prefix}{file}"), &body, Cond::None)?;
                    sent.insert(file, body);
                }
            }
        }
        if !local.join("cancel.json").exists() {
            if let Some((body, _)) = folders.get(&format!("{prefix}cancel.json"))? {
                let _ = std::fs::write(local.join("cancel.json"), body);
            }
        }
        Ok(())
    };
    // the program is watched more often than the folder is synced
    let tick = every.min(Duration::from_millis(200));
    let mut synced: Option<Instant> = None;
    let mut stopping: Option<Instant> = None;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| Error::Refused(e.to_string()))? {
            break status;
        }
        if synced.is_none_or(|at| at.elapsed() >= every) {
            // an object store that is out of reach for a moment is tried again next time
            let _ = sync(&mut sent);
            synced = Some(Instant::now());
        }
        match stopping {
            None if term.load(Ordering::SeqCst) || cancel_due(local) => {
                stop(&mut child, false);
                stopping = Some(Instant::now());
            }
            Some(at) if at.elapsed() >= STOP_GRACE => stop(&mut child, true),
            _ => {}
        }
        std::thread::sleep(tick);
    };
    sync(&mut sent)?;
    if !local.join("outcome.json").exists() {
        if let Some(reason) = cancel_reason(local) {
            let reason = if stopping.is_some() { format!("{reason} (stopped at the deadline)") } else { reason };
            let _ = std::fs::write(local.join("outcome.json"), serde_json::to_vec_pretty(&Outcome::canceled(&reason)).unwrap());
        }
    }
    // an outcome the Performer wrote first (canceled at the deadline) stands
    if let Ok(body) = std::fs::read(local.join("outcome.json")) {
        folders.put(&format!("{prefix}outcome.json"), &body, Cond::IfNoneMatch)?;
    }
    Ok(status.code().unwrap_or(1))
}

/// The reason of the cancel asked in the local copy, if one was.
fn cancel_reason(local: &Path) -> Option<String> {
    let asked: serde_json::Value = serde_json::from_slice(&std::fs::read(local.join("cancel.json")).ok()?).ok()?;
    Some(asked.get("reason").and_then(|r| r.as_str()).unwrap_or("canceled").to_string())
}

/// Whether a cancel was asked and its deadline has passed.
fn cancel_due(local: &Path) -> bool {
    let Some(asked) = std::fs::read(local.join("cancel.json")).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) else {
        return false;
    };
    let deadline = asked.get("deadline").and_then(|d| d.as_str()).unwrap_or_default();
    let now = stepmeld_core::iso(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()));
    deadline <= now.as_str()
}

/// Tell the program's process group to stop (SIGTERM), or kill it.
fn stop(child: &mut std::process::Child, kill: bool) {
    #[cfg(unix)]
    {
        let signal = if kill { libc::SIGKILL } else { libc::SIGTERM };
        // SAFETY: a plain kill(2) on the group the child leads
        unsafe { libc::kill(-(child.id() as libc::pid_t), signal) };
    }
    #[cfg(not(unix))]
    {
        let _ = kill;
        let _ = child.kill();
    }
}
