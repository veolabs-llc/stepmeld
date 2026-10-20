//! `stepmeld`: the engine from a shell (docs/ORCHESTRATOR-WORKER.md,
//! docs/PLACEMENT.md). The deployment file says which store is shared
//! (definitions, Workflows, orders, published Workers; a SQLite file in
//! the current folder without one); the machine file holds this
//! machine's Worker. `tick` orchestrates, `work` runs the Worker.
//! Refusals are one line and exit 1; nothing here is a UI.

use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use stepmeld_config::{Deployment, Kinds, Machine};
use stepmeld_core::definition::{Locality, Ref, StepDefinition, WorkflowDefinition};
use stepmeld_core::driver::Driver;
use stepmeld_core::engine::Command;
use stepmeld_core::library::Library;
use stepmeld_core::orders::{wall_clock, OrderPerformer};
use stepmeld_core::store::SharedStore;
use stepmeld_core::workflow::{Actor, Gate, StepStatus, Waiting, Workflow, WorkflowStatus};
use stepmeld_core::{iso, Error, Value};
use stepmeld_sqlite::SqliteStore;

#[derive(Parser)]
#[command(name = "stepmeld", version, about = "A generic workflow engine: StepDefinitions, WorkflowDefinitions, Workflows, Orchestrators and Workers")]
struct Cli {
    /// The deployment file (else `STEPMELD_CONFIG`, else `stepmeld.toml`
    /// here, else in the user's configuration folder; without one, the
    /// shared store is `stepmeld.sqlite` here).
    #[arg(long, global = true)]
    deployment: Option<PathBuf>,
    /// A SQLite file to use as the shared store, whatever the deployment says.
    #[arg(long, global = true)]
    store: Option<PathBuf>,
    /// This machine's file (else `STEPMELD_MACHINE`, else
    /// `stepmeld-machine.toml` here, else `machine.toml` in the user's
    /// configuration folder; made here when first written).
    #[arg(long, global = true)]
    machine: Option<PathBuf>,
    /// Who is acting: `person:<id>` or `agent:<id>`.
    #[arg(long = "as", global = true)]
    actor: Option<String>,
    /// Print documents as JSON rather than text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// The Library: show it, or add StepDefinitions and WorkflowDefinitions to it.
    Library {
        #[command(subcommand)]
        what: Option<LibraryCmd>,
    },
    /// A Workflow from a WorkflowDefinition.
    Create {
        definition: String,
        #[arg(long, default_value_t = 1)]
        version: u32,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        label: Option<String>,
    },
    /// Every Workflow and its status.
    List,
    /// One Workflow: its Steps, where each stands, its Runs.
    Show { workflow: String },
    /// Give an Input or a Parameter a Value.
    Set {
        workflow: String,
        /// The Step, by its name in the WorkflowDefinition (not the StepDefinition's name).
        #[arg(long)]
        step: String,
        #[arg(long, conflicts_with = "parameter")]
        input: Option<String>,
        #[arg(long)]
        parameter: Option<String>,
        /// JSON; a bare word is taken as a string.
        #[arg(long, allow_hyphen_values = true)]
        value: String,
    },
    /// Give the go: the Workflow's (no step named: a root Workflow runs
    /// nothing until then), or a Step's start, retake, or rerun.
    Start {
        workflow: String,
        /// A Step to start, retake or rerun, by its name in the WorkflowDefinition; none gives the Workflow its go.
        #[arg(long)]
        step: Option<String>,
    },
    /// Finish a Step a person does: `--output name=json` for each.
    Complete {
        workflow: String,
        /// The Step, by its name in the WorkflowDefinition (not the StepDefinition's name).
        #[arg(long)]
        step: String,
        #[arg(long = "output")]
        outputs: Vec<String>,
    },
    /// Confirm a placement a Policy asks about.
    Confirm {
        workflow: String,
        /// The Step, by its name in the WorkflowDefinition (not the StepDefinition's name).
        #[arg(long)]
        step: String,
    },
    /// Change a Step's Policy: `--to` takes JSON in the StepDefinition's shape (start, on_failure, on_stale, placement).
    Policy {
        workflow: String,
        /// The Step, by its name in the WorkflowDefinition (not the StepDefinition's name).
        #[arg(long)]
        step: String,
        #[arg(long)]
        to: String,
    },
    /// Choose the Worker for a Step's next Run.
    Place {
        workflow: String,
        /// The Step, by its name in the WorkflowDefinition (not the StepDefinition's name).
        #[arg(long)]
        step: String,
        /// The Worker, by name; `any` for the first Worker free to take it.
        #[arg(long)]
        worker: String,
    },
    /// Pass a Step by, for good: it never runs, and an Input bound from its Outputs takes its next Binding.
    Skip {
        workflow: String,
        /// The Step, by its name in the WorkflowDefinition (not the StepDefinition's name).
        #[arg(long)]
        step: String,
    },
    /// Stop a Step's live Run, or the whole Workflow.
    Cancel {
        workflow: String,
        /// A Step whose live Run to stop, by its name in the WorkflowDefinition; none cancels the Workflow.
        #[arg(long)]
        step: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Forget a finished Workflow and the children its Runs made (cancel a live one first).
    Remove { workflow: String },
    /// Orchestrate: advance every Workflow once; with --watch, keep going until nothing is running.
    Tick {
        #[arg(long)]
        watch: bool,
        /// Seconds between ticks when watching.
        #[arg(long, default_value_t = 2)]
        every: u64,
    },
    /// What happened, in order.
    History { workflow: String },
    /// A Run's raw output.
    Log {
        workflow: String,
        /// The Step, by its name in the WorkflowDefinition (not the StepDefinition's name).
        #[arg(long)]
        step: String,
        #[arg(long)]
        run: Option<u32>,
    },
    /// This machine's Worker: show it, or set it up.
    Worker {
        #[command(subcommand)]
        what: Option<WorkerCmd>,
    },
    /// Run this machine's Worker: take its orders, run them, report; with --watch, until stopped.
    Work {
        #[arg(long)]
        watch: bool,
        /// Seconds between rounds when watching.
        #[arg(long, default_value_t = 1)]
        every: u64,
    },
}

#[derive(Subcommand)]
enum WorkerCmd {
    /// The Worker's name, settings and programs.
    Show,
    /// Change the Worker's settings.
    Set {
        /// Its name in the shared store (default: this machine's host name).
        #[arg(long)]
        name: Option<String>,
        /// Where its Runs keep their files, beside the machine file (default: `runs`).
        #[arg(long)]
        runs: Option<String>,
        /// Whether it launches its work in the cloud.
        #[arg(long)]
        cloud: Option<bool>,
        /// How many orders it runs at once (default 1; no limit for a cloud Worker).
        #[arg(long)]
        slots: Option<u32>,
        /// A word it offers to what StepDefinitions require; repeat for several. Replaces the list.
        #[arg(long = "capability")]
        capabilities: Option<Vec<String>>,
    },
    /// This Worker's programs: the command that runs each, by the name StepDefinitions use.
    Program {
        #[command(subcommand)]
        what: ProgramCmd,
    },
}

#[derive(Subcommand)]
enum ProgramCmd {
    /// Set the command for a program: `program set <program> -- <command...>` (the Run's directory is appended).
    Set {
        program: String,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Forget a program.
    Remove { program: String },
}

#[derive(Subcommand)]
enum LibraryCmd {
    /// Every StepDefinition and WorkflowDefinition.
    Show,
    /// Add definitions (StepDefinitions and WorkflowDefinitions, told apart by their contract).
    Add { files: Vec<PathBuf> },
    /// Remove a StepDefinition or WorkflowDefinition nothing uses; for a WorkflowDefinition, offer to remove the StepDefinitions it leaves unused.
    Remove {
        name: String,
        /// Which version, when there are several.
        #[arg(long)]
        version: Option<u32>,
        /// Remove the StepDefinitions it leaves unused without asking.
        #[arg(long, conflicts_with = "keep_orphans")]
        with_orphans: bool,
        /// Keep the StepDefinitions it leaves unused without asking.
        #[arg(long)]
        keep_orphans: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

/// Seconds since the epoch as RFC 3339, UTC.
fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn actor(cli: &Cli) -> Result<Actor, Error> {
    let word = cli
        .actor
        .clone()
        .unwrap_or_else(|| format!("person:{}", std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "someone".into())));
    match word.split_once(':') {
        Some(("person", id)) => Ok(Actor::person(id)),
        Some(("agent", id)) => Ok(Actor::Agent { id: id.into() }),
        _ => Err(Error::Refused(format!("--as takes person:<id> or agent:<id>, not {word:?}"))),
    }
}

fn value(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

/// This machine's name, as the machine gives it.
fn host() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "localhost".into())
}

/// The Orchestrator: a driver over the shared store with one Performer
/// per published Worker, and WorkflowDefinitions nested.
fn driver(cli: &Cli) -> Result<(Arc<dyn SharedStore>, Driver), Error> {
    let store = shared_store(cli)?;
    let mut driver = Driver::new(store.clone(), &format!("stepmeld@{}:{}", host(), std::process::id())).with_nesting();
    let lib = store.library()?;
    for p in OrderPerformer::for_workers(store.clone(), &lib, &host(), wall_clock())? {
        driver = driver.with_performer(Arc::new(p));
    }
    Ok((store, driver))
}

/// What this build can name in a deployment file.
fn kinds() -> Kinds {
    Kinds::builtin()
}

fn deployment(cli: &Cli) -> Result<Deployment, Error> {
    Deployment::find(cli.deployment.as_deref())
}

/// The shared store, said aloud when this command made it: a command run
/// from the wrong folder starts an empty store there, which is easy to
/// miss otherwise.
fn shared_store(cli: &Cli) -> Result<Arc<dyn SharedStore>, Error> {
    if let Some(path) = &cli.store {
        let new = !path.exists();
        let store = SqliteStore::open(path)?;
        if new {
            announce("the shared store", path);
        }
        return Ok(Arc::new(store));
    }
    let d = deployment(cli)?;
    let settings = d.store()?;
    let made = (settings["kind"].as_str() == Some("sqlite"))
        .then(|| d.resolve(settings.get("path").and_then(|p| p.as_str()).unwrap_or("stepmeld.sqlite")))
        .filter(|p| !p.exists());
    let store = kinds().open_store(&d)?;
    if let Some(path) = made {
        announce("the shared store", &path);
    }
    Ok(store)
}

/// One line on standard error: a new file, and where it is.
fn announce(what: &str, path: &Path) {
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    eprintln!("(made {what}: {})", full.display());
}

fn machine(cli: &Cli) -> Result<Machine, Error> {
    Machine::find(cli.machine.as_deref())
}

/// Write the machine file, saying so when this made it.
fn save(m: &mut Machine) -> Result<(), Error> {
    if m.save()? {
        announce("this machine's file", &m.path);
    }
    Ok(())
}

fn show_worker(cli: &Cli, store: &dyn SharedStore, m: &Machine) -> Result<(), Error> {
    let w = &m.file.worker;
    println!("worker   {}  (on {}; {}{})", m.name(&host()), host(), m.path.display(), if m.exists { "" } else { ", not written yet" });
    println!("runs in  {}", m.runs().display());
    println!("cloud    {}", if w.cloud { "yes" } else { "no" });
    println!("slots    {}", if m.slots() == u32::MAX { "no limit".to_string() } else { m.slots().to_string() });
    if !w.capabilities.is_empty() {
        println!("has      {}  (capabilities)", w.capabilities.join(", "));
    }
    let routes = kinds().routes(&deployment(cli)?, m)?;
    if routes.is_empty() {
        println!("programs none yet: `stepmeld worker program set <program> -- <command...>`");
    }
    for (kind, programs) in &routes {
        for p in programs {
            let how = match p.settings.get("command").and_then(|c| c.as_array()) {
                Some(c) => c.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" "),
                None => format!("({kind})"),
            };
            println!("program  {:<20} {how}", p.name);
        }
    }
    let programs: Vec<String> = routes.values().flatten().map(|p| p.name.clone()).collect();
    let face = stepmeld_core::orders::WorkerFace {
        contract: stepmeld_core::orders::WORKER.into(),
        name: m.name(&host()),
        host: host(),
        cloud: w.cloud,
        programs,
        capabilities: w.capabilities.clone(),
        slots: m.slots(),
        holds: Vec::new(),
        beat_s: stepmeld_core::orders::BEAT_S,
        seen: String::new(),
    };
    let offers: Vec<String> = store.library()?.steps().filter(|s| face.offers(s)).map(|s| s.reference().to_string()).collect();
    println!("offers   {}", if offers.is_empty() { "nothing in the Library yet".into() } else { offers.join(", ") });
    Ok(())
}

fn run(cli: Cli) -> Result<(), Error> {
    if let Cmd::Library { what } = &cli.command {
        return library(&cli, shared_store(&cli)?.as_ref(), what);
    }
    let (store, driver) = driver(&cli)?;
    let by = actor(&cli)?;
    let now = iso(now_secs());
    let until = || iso(now_secs() + 60);
    match &cli.command {
        // handled above, before the Library is checked; here for completeness
        Cmd::Library { what } => library(&cli, store.as_ref(), what)?,
        Cmd::Worker { what } => {
            let mut m = machine(&cli)?;
            match what {
                None | Some(WorkerCmd::Show) => show_worker(&cli, store.as_ref(), &m)?,
                Some(WorkerCmd::Set { name, runs, cloud, slots, capabilities }) => {
                    let w = &mut m.file.worker;
                    if let Some(n) = name {
                        if n == stepmeld_core::orders::ANY {
                            return Err(Error::Refused(format!("{n:?} names the pool of every Worker; choose another name")));
                        }
                        w.name = Some(n.clone());
                    }
                    if let Some(r) = runs {
                        w.runs = Some(r.clone());
                    }
                    if let Some(c) = cloud {
                        w.cloud = *c;
                    }
                    if let Some(n) = slots {
                        w.slots = Some(*n);
                    }
                    if let Some(c) = capabilities {
                        w.capabilities = c.clone();
                    }
                    save(&mut m)?;
                    show_worker(&cli, store.as_ref(), &m)?;
                }
                Some(WorkerCmd::Program { what: ProgramCmd::Set { program, command } }) => {
                    m.file.programs.insert(program.clone(), command.clone());
                    save(&mut m)?;
                    println!("program {program}: {}", command.join(" "));
                }
                Some(WorkerCmd::Program { what: ProgramCmd::Remove { program } }) => {
                    if m.file.programs.remove(program).is_none() {
                        return Err(Error::Refused(format!("this Worker has no program {program:?}")));
                    }
                    save(&mut m)?;
                    println!("forgot program {program}");
                }
            }
        }
        Cmd::Work { watch, every } => {
            let m = machine(&cli)?;
            let worker = kinds().worker(&deployment(&cli)?, &m, store.clone(), &host())?;
            println!("worker {} on {}, with programs {}", worker.name, host(), worker.programs.join(", "));
            loop {
                for m in worker.tick(&iso(now_secs()))? {
                    println!("{}", format!("{:<32} {:<10} {}", m.order, m.word, m.detail).trim_end());
                }
                if !*watch {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(*every));
            }
        }
        Cmd::Create { definition, version, id, label } => {
            let id = id.clone().unwrap_or_else(|| format!("{definition}-{}", now_secs()));
            let wf = driver.create(&Ref::new(definition, *version), &id, label.as_deref(), &by, &now)?;
            println!("created workflow {}", wf.id);
        }
        Cmd::List => {
            let lib = store.library()?;
            for id in store.list()? {
                let (wf, _) = store.get(&id)?.unwrap();
                let status = lib.workflow(&wf.definition).map(|d| format!("{:?}", wf.status(d)).to_lowercase()).unwrap_or_else(|| "(WorkflowDefinition missing)".into());
                println!("{:<40} {:<24} {}", wf.id, wf.definition.to_string(), status);
            }
        }
        Cmd::Show { workflow } => {
            let (wf, _) = store.get(workflow)?.ok_or_else(|| Error::Refused(format!("no workflow {workflow:?}")))?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&wf).unwrap());
            } else {
                let faces: Vec<_> = driver.performers.iter().map(|p| p.describe()).collect();
                show(store.as_ref(), &wf, &faces)?;
            }
        }
        Cmd::Set { workflow, step, input, parameter, value: v } => {
            if input.is_none() && parameter.is_none() {
                return Err(Error::Refused("set needs --input or --parameter".into()));
            }
            driver.command(
                workflow,
                &Command::Set {
                    step: step.clone(),
                    input: input.clone(),
                    parameter: parameter.clone(),
                    value: Some(value(v)),
                },
                &by,
                &now,
            )?;
            println!("set");
        }
        Cmd::Start { workflow, step } => {
            let cmd = match step {
                Some(step) => Command::Start { step: step.clone() },
                None => Command::StartWorkflow,
            };
            driver.command(workflow, &cmd, &by, &now)?;
            match step {
                Some(step) => println!("go given to {step}; tick to run it"),
                None => println!("go given to {workflow}; tick to run it"),
            }
        }
        Cmd::Complete { workflow, step, outputs } => {
            let mut map = BTreeMap::new();
            for o in outputs {
                let (k, v) = o.split_once('=').ok_or_else(|| Error::Refused(format!("--output takes name=json, not {o:?}")))?;
                map.insert(k.to_string(), value(v));
            }
            driver.command(workflow, &Command::Complete { step: step.clone(), outputs: map }, &by, &now)?;
            println!("completed {step}");
        }
        Cmd::Confirm { workflow, step } => {
            driver.command(workflow, &Command::Confirm { step: step.clone() }, &by, &now)?;
            println!("confirmed {step}");
        }
        Cmd::Policy { workflow, step, to } => {
            let policy: stepmeld_core::definition::Policy = serde_json::from_str(to).map_err(|e| Error::Refused(format!("--to is not a policy: {e}")))?;
            driver.command(workflow, &Command::SetPolicy { step: step.clone(), policy }, &by, &now)?;
            println!("policy set on {step}");
        }
        Cmd::Place { workflow, step, worker } => {
            let locality: Locality = driver
                .performers
                .iter()
                .map(|p| p.describe())
                .find(|f| &f.name == worker)
                .map(|f| f.locality)
                .ok_or_else(|| Error::Refused(format!("no Worker {worker:?} has published itself to this store")))?;
            driver.command(
                workflow,
                &Command::Place {
                    step: step.clone(),
                    performer: worker.clone(),
                    locality,
                },
                &by,
                &now,
            )?;
            println!("placed {step} on {worker}");
        }
        Cmd::Skip { workflow, step } => {
            driver.command(workflow, &Command::Skip { step: step.clone() }, &by, &now)?;
            println!("skipped {step}; tick to carry on past it");
        }
        Cmd::Cancel { workflow, step, reason } => {
            driver.command(workflow, &Command::Cancel { step: step.clone(), reason: reason.clone() }, &by, &now)?;
            println!("cancel asked; tick to carry it out");
        }
        Cmd::Remove { workflow } => {
            let gone = driver.remove(workflow, &now, &until())?;
            println!("removed {}", gone.join(", "));
        }
        Cmd::Tick { watch, every } => {
            // what was last said of each Workflow and Step; a line is
            // printed only when that changes. Workflows already finished
            // are taken as said, so they stay quiet unless they move.
            let faces = |d: &Driver| d.performers.iter().map(|p| p.describe()).collect::<Vec<_>>();
            let mut said: BTreeMap<(String, String), String> = BTreeMap::new();
            let lib = store.library()?;
            for id in store.list()? {
                if let Some(now) = snapshot(store.as_ref(), &lib, &faces(&driver), &id)? {
                    if matches!(now.0.as_str(), "succeeded" | "failed" | "canceled") {
                        said.insert((id.clone(), String::new()), now.0.clone());
                        for (step, line) in now.1 {
                            said.insert((id.clone(), step), line);
                        }
                    }
                }
            }
            let mut spoke = false;
            loop {
                // Workers come and go while this watches: who is here is
                // read again each tick
                let driver = self::driver(&cli)?.1;
                let statuses = driver.tick_all(&iso(now_secs()), &until())?;
                let lib = store.library()?;
                let mut busy = false;
                for id in statuses.keys() {
                    let Some((word, steps)) = snapshot(store.as_ref(), &lib, &faces(&driver), id)? else { continue };
                    for (step, line) in steps {
                        if said.get(&(id.clone(), step.clone())) != Some(&line) {
                            println!("{id:<16} {step:<16} {line}");
                            said.insert((id.clone(), step), line);
                            spoke = true;
                        }
                    }
                    if said.get(&(id.clone(), String::new())) != Some(&word) {
                        println!("{id:<16} {:<16} {word}", "(workflow)");
                        said.insert((id.clone(), String::new()), word.clone());
                        spoke = true;
                    }
                    busy |= word == "running" || word == "ready";
                }
                if !*watch || !busy {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(*every));
            }
            if !spoke {
                println!("nothing to advance: {} Workflows, all finished", store.list()?.len());
            }
        }
        Cmd::History { workflow } => {
            for e in store.history(workflow)? {
                if cli.json {
                    println!("{}", serde_json::to_string(&e).unwrap());
                } else {
                    println!(
                        "{:>4}  {}  {:<12} {:<14} {:<16} {}",
                        e.seq,
                        e.at,
                        e.kind,
                        e.step.as_deref().unwrap_or(""),
                        who(&e.by),
                        if e.detail.is_null() { String::new() } else { e.detail.to_string() }
                    );
                }
            }
        }
        Cmd::Log { workflow, step, run } => {
            let (wf, _) = store.get(workflow)?.ok_or_else(|| Error::Refused(format!("no workflow {workflow:?}")))?;
            let s = wf.step(step)?;
            let r = match run {
                Some(n) => s.runs.iter().find(|r| r.number == *n),
                None => s.latest(),
            }
            .ok_or_else(|| Error::Refused(format!("{step} has no such run")))?;
            let handle = r.handle.as_deref().ok_or_else(|| Error::Refused(format!("run {} was never started", r.id)))?;
            let p = driver
                .performers
                .iter()
                .find(|p| p.describe().name == r.performer)
                .ok_or_else(|| Error::Refused(format!("Worker {:?} has not published itself to this store", r.performer)))?;
            print!("{}", p.log(handle).unwrap_or_else(|| "(no log kept)\n".into()));
        }
    }
    Ok(())
}

fn who(a: &Actor) -> String {
    match a {
        Actor::Person { id } => id.clone(),
        Actor::Agent { id } => format!("agent {id}"),
        Actor::Policy { rule, .. } => format!("policy {rule}"),
        Actor::Run { workflow, step, .. } => format!("{workflow}/{step}"),
    }
}

fn show(store: &dyn SharedStore, wf: &Workflow, faces: &[stepmeld_core::workflow::PerformerFace]) -> Result<(), Error> {
    let lib = store.library()?;
    let def = lib.workflow(&wf.definition).ok_or_else(|| Error::Refused(format!("{} is not in the library", wf.definition)))?;
    println!("{}  ({}, {})", wf.id, wf.definition, format!("{:?}", wf.status(def)).to_lowercase());
    for (name, step) in &wf.steps {
        let status = match wf.gate(def, name, faces) {
            stepmeld_core::workflow::Gate::Closed(s @ StepStatus::Waiting(_)) => s,
            _ => wf.step_status(def, name),
        };
        let reason = reason(wf, name, &status);
        println!("  {:<16} {:<24} {:<10} {}", name, format!("({})", step.definition), status.word(), reason);
        for (k, i) in &step.inputs {
            println!("      in   {k:<14} {}", i.value.as_ref().map(Value::to_string).unwrap_or_else(|| "-".into()));
        }
        for (k, p) in &step.parameters {
            println!(
                "      par  {k:<14} {}{}",
                p.value.as_ref().map(Value::to_string).unwrap_or_else(|| "-".into()),
                if p.chosen.is_some() { "" } else { "  (default)" }
            );
        }
        for (k, o) in &step.outputs {
            println!("      out  {k:<14} {}", o.value.as_ref().map(Value::to_string).unwrap_or_else(|| "-".into()));
        }
        for r in &step.runs {
            let ended = r
                .outcome
                .as_ref()
                .map(|o| format!("{:?}{}", o.state, o.summary.as_ref().map(|s| format!(": {}", s.headline)).unwrap_or_default()).to_lowercase())
                .unwrap_or_else(|| "running".into());
            println!("      run {} on {} by {}: {ended}", r.number, r.performer, who(&r.started.by));
        }
    }
    Ok(())
}

/// Where a Step stands, in words: what it waits for, how far it is,
/// why it failed.
fn reason(wf: &Workflow, name: &str, status: &StepStatus) -> String {
    let Some(step) = wf.steps.get(name) else { return String::new() };
    match status {
        StepStatus::Waiting(Waiting::Input { name }) => format!("needs input {name}"),
        StepStatus::Waiting(Waiting::Parameter { name }) => format!("needs parameter {name}"),
        StepStatus::Waiting(Waiting::Completion) => "waiting for someone to complete it".into(),
        StepStatus::Waiting(Waiting::Go) => "waiting for a start".into(),
        StepStatus::Waiting(Waiting::Start) => "waiting for the workflow to be started (`start <workflow>`)".into(),
        StepStatus::Waiting(Waiting::Confirmation { locality }) => format!("waiting for the {} placement to be confirmed", locality.as_str()),
        StepStatus::Waiting(Waiting::Performer) => "no Worker offers it (`stepmeld work` where its program is)".into(),
        StepStatus::Waiting(Waiting::Placement) => "waiting for a placement: several Workers offer it (`place`)".into(),
        StepStatus::Running => step
            .latest()
            .and_then(|r| r.progress.as_ref())
            .map(|p| {
                format!(
                    "{}{}",
                    p.phase.clone().unwrap_or_default(),
                    match (p.done, p.total) {
                        (Some(d), Some(t)) => format!(" {d} of {t} {}", p.unit.clone().unwrap_or_default()),
                        _ => String::new(),
                    }
                )
            })
            .unwrap_or_default(),
        StepStatus::Failed => step.latest().and_then(|r| r.outcome.as_ref()).and_then(|o| o.reason.clone()).unwrap_or_default(),
        StepStatus::Skipped => match &step.skipped {
            Some(s) => format!("skipped by {}", who(&s.by)),
            None => "no path leads to it".into(),
        },
        _ => String::new(),
    }
}

/// The Library's commands. They read the definitions without checking
/// them, so a store holding one that breaks a newer rule can still be
/// shown and cleaned.
fn library(cli: &Cli, store: &dyn SharedStore, what: &Option<LibraryCmd>) -> Result<(), Error> {
    match what {
        None | Some(LibraryCmd::Show) => {
            let (steps, workflows) = store.definitions()?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({"steps": steps, "workflows": workflows})).unwrap());
            } else {
                for s in &steps {
                    let by = match (&s.done_by, &s.program) {
                        (stepmeld_core::definition::DoneBy::Person, _) => "  (done by a person)".to_string(),
                        (_, Some(p)) => format!("  (program {p})"),
                        (_, None) => "  (no program)".to_string(),
                    };
                    // stored before a rule it now breaks: shown, so it can be removed
                    let stale = s.check().err().map(|e| format!("  NOT VALID NOW: {e}")).unwrap_or_default();
                    println!("StepDefinition      {:<28} {}{by}{stale}", s.reference().to_string(), s.label.as_deref().unwrap_or(""));
                }
                for w in &workflows {
                    println!("WorkflowDefinition  {:<28} {} ({} steps)", w.reference().to_string(), w.label.as_deref().unwrap_or(""), w.steps.len());
                }
            }
        }
        Some(LibraryCmd::Add { files }) => {
            // StepDefinitions first, then WorkflowDefinitions in whatever
            // order they resolve: one that nests another may be named
            // before it
            let mut steps = Vec::new();
            let mut workflows = Vec::new();
            for path in files {
                let text = std::fs::read_to_string(path).map_err(|e| Error::Refused(format!("cannot read {}: {e}", path.display())))?;
                let doc: Value = serde_json::from_str(&text).map_err(|e| Error::Refused(format!("{} is not JSON: {e}", path.display())))?;
                let contract = doc.get("contract").and_then(Value::as_str).unwrap_or("").to_string();
                stepmeld_core::contracts::validate(&doc, &contract).map_err(|e| Error::Refused(format!("{}: {e}", path.display())))?;
                match contract.as_str() {
                    stepmeld_core::definition::STEP_DEFINITION => steps.push((path.clone(), serde_json::from_value::<StepDefinition>(doc).map_err(|e| Error::Protocol(e.to_string()))?)),
                    stepmeld_core::definition::WORKFLOW_DEFINITION => workflows.push((path.clone(), serde_json::from_value::<WorkflowDefinition>(doc).map_err(|e| Error::Protocol(e.to_string()))?)),
                    other => return Err(Error::Refused(format!("{}: contract {other:?} is not a definition", path.display()))),
                }
            }
            for (_, def) in &steps {
                store.put_step(def)?;
                println!("added StepDefinition {}", def.reference());
            }
            while !workflows.is_empty() {
                let before = workflows.len();
                let mut kept = Vec::new();
                let mut errors = Vec::new();
                for (path, def) in workflows {
                    match store.put_workflow_definition(&def) {
                        Ok(()) => println!("added WorkflowDefinition {}", def.reference()),
                        Err(Error::Invalid(e)) if e.contains("not in the library") => {
                            errors.push(format!("{}: {e}", path.display()));
                            kept.push((path, def));
                        }
                        Err(e) => return Err(e),
                    }
                }
                if kept.len() == before {
                    return Err(Error::Refused(errors.join("\n")));
                }
                workflows = kept;
            }
        }
        Some(LibraryCmd::Remove { name, version, with_orphans, keep_orphans }) => {
            let (steps, workflows) = store.definitions()?;
            let all: Vec<(Ref, &str)> = steps.iter().map(|s| (s.reference(), "StepDefinition")).chain(workflows.iter().map(|w| (w.reference(), "WorkflowDefinition"))).collect();
            let found: Vec<&(Ref, &str)> = all.iter().filter(|(r, _)| &r.name == name && version.is_none_or(|v| r.version == v)).collect();
            let (target, kind) = match found.as_slice() {
                [] => {
                    return Err(Error::Refused(format!(
                        "the Library has no StepDefinition or WorkflowDefinition {name}{}",
                        version.map(|v| format!(" v{v}")).unwrap_or_default()
                    )))
                }
                [one] => (one.0.clone(), one.1),
                many => {
                    let versions: Vec<String> = many.iter().map(|(r, _)| r.version.to_string()).collect();
                    return Err(Error::Refused(format!("{name} has versions {}; say which with --version", versions.join(", "))));
                }
            };
            let users = Library::users_of(&workflows, &target);
            if !users.is_empty() {
                let names: Vec<String> = users.iter().map(|r| r.to_string()).collect();
                return Err(Error::Refused(format!("{kind} {target} is used by WorkflowDefinition {}; remove that first", names.join(", "))));
            }
            let mut in_use = Vec::new();
            for id in store.list()? {
                if let Some((wf, _)) = store.get(&id)? {
                    if wf.definition == target || wf.steps.values().any(|s| s.definition == target) {
                        in_use.push(id);
                    }
                }
            }
            if !in_use.is_empty() {
                return Err(Error::Refused(format!("{kind} {target} is used by Workflow {}; remove those first (`stepmeld remove <workflow>`)", in_use.join(", "))));
            }
            let orphans = Library::orphaned_by(&steps, &workflows, &target);
            store.remove_definition(&target)?;
            println!("removed {kind} {target}");
            if !orphans.is_empty() {
                let names: Vec<String> = orphans.iter().map(|r| r.to_string()).collect();
                println!("no other WorkflowDefinition uses StepDefinition {}", names.join(", "));
                let remove = if *with_orphans {
                    true
                } else if *keep_orphans {
                    false
                } else {
                    ask("Remove them too? [y/N] ")
                };
                if remove {
                    for r in &orphans {
                        store.remove_definition(r)?;
                        println!("removed StepDefinition {r}");
                    }
                } else {
                    println!("kept them");
                }
            }
        }
    }
    Ok(())
}

/// A yes-or-no question on the terminal; anything but y or yes, or no
/// answer at all, is no.
fn ask(question: &str) -> bool {
    use std::io::Write;
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).is_ok() && matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Where a Run with no progress is, as its order says: on the Worker
/// that took it (an open order's too), not yet taken, or on a Worker
/// that is away.
fn where_it_runs(store: &dyn SharedStore, run: &str, placed: &str) -> String {
    let Ok(Some(order)) = store.order(run) else { return format!("on {placed}") };
    let who = order.claim.as_ref().map_or(placed, |c| c.worker.as_str());
    let away = store.workers().ok().and_then(|ws| ws.into_iter().find(|w| w.name == who)).is_some_and(|w| w.away(&iso(now_secs())));
    match (&order.claim, away) {
        (_, true) => format!("on {who} (away)"),
        (None, false) => format!("on {who}, not yet taken"),
        (Some(_), false) => format!("on {who}"),
    }
}

/// A Workflow's status in a word, and each Step's line (name, "status  detail").
type Snapshot = (String, Vec<(String, String)>);

/// Where a Workflow and each of its Steps stand, in words, judged
/// against the Workers that exist: the Workflow's status, then each
/// Step in its WorkflowDefinition's order as "status  detail" (what it
/// waits for, how far it is, its headline, why it failed).
fn snapshot(store: &dyn SharedStore, lib: &Library, faces: &[stepmeld_core::workflow::PerformerFace], id: &str) -> Result<Option<Snapshot>, Error> {
    let Some((wf, _)) = store.get(id)? else { return Ok(None) };
    let Some(def) = lib.workflow(&wf.definition) else { return Ok(None) };
    let status = wf.status(def);
    let mut steps = Vec::new();
    let mut any_open = false;
    for entry in &def.steps {
        let name = &entry.name;
        let gate = wf.gate(def, name, faces);
        any_open |= gate == Gate::Open;
        let st = match gate {
            Gate::Closed(s @ StepStatus::Waiting(_)) => s,
            _ => wf.step_status(def, name),
        };
        let step = &wf.steps[name];
        let detail = match &st {
            StepStatus::Succeeded => step.latest().and_then(|r| r.outcome.as_ref()).and_then(|o| o.summary.as_ref()).map(|s| s.headline.clone()).unwrap_or_default(),
            StepStatus::Canceled => step.latest().and_then(|r| r.outcome.as_ref()).and_then(|o| o.reason.clone()).unwrap_or_default(),
            StepStatus::Running => {
                let progress = reason(&wf, name, &st);
                if progress.is_empty() {
                    step.latest().map(|r| where_it_runs(store, &r.id, &r.performer)).unwrap_or_default()
                } else {
                    progress
                }
            }
            other => reason(&wf, name, other),
        };
        steps.push((name.clone(), format!("{:<10} {detail}", st.word()).trim_end().to_string()));
    }
    // a Workflow whose ready Steps no Worker offers is waiting
    let word = match status {
        WorkflowStatus::Ready if !any_open => "waiting".to_string(),
        other => format!("{other:?}").to_lowercase(),
    };
    Ok(Some((word, steps)))
}
