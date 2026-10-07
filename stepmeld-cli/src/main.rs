//! `stepmeld`: the engine from a shell. A Library and Workflows in one
//! SQLite file; this machine's programs as a Performer; recipes nested.
//! Refusals are one line and exit 1; nothing here is a UI.

use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use stepmeld_core::definition::{Locality, Ref, StepDefinition, WorkflowDefinition};
use stepmeld_core::driver::Driver;
use stepmeld_core::engine::Command;
use stepmeld_core::store::StateStore;
use stepmeld_core::workflow::{Actor, StepStatus, Waiting, Workflow};
use stepmeld_core::{iso, Error, Value};
use stepmeld_sqlite::SqliteStore;

#[derive(Parser)]
#[command(name = "stepmeld", version, about = "A generic workflow engine: verbs, recipes, gates, performers")]
struct Cli {
    /// The StateStore: one SQLite file.
    #[arg(long, default_value = "stepmeld.sqlite", global = true)]
    store: PathBuf,
    /// This machine's programs, one per verb (JSON; see `performers`).
    #[arg(long, global = true)]
    performers: Option<PathBuf>,
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
    /// Add definitions to the Library (step or workflow, told apart by their contract).
    Add { files: Vec<PathBuf> },
    /// The Library: verbs and recipes.
    Library,
    /// A Workflow from a recipe.
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
        step: String,
        #[arg(long, conflicts_with = "parameter")]
        input: Option<String>,
        #[arg(long)]
        parameter: Option<String>,
        /// JSON; a bare word is taken as a string.
        #[arg(long, allow_hyphen_values = true)]
        value: String,
    },
    /// Give the go: start, retake, or rerun a stale Step.
    Start { workflow: String, step: String },
    /// Finish a Step a person does: `--output name=json` for each.
    Complete {
        workflow: String,
        step: String,
        #[arg(long = "output")]
        outputs: Vec<String>,
    },
    /// Confirm a placement a Policy asks about.
    Confirm { workflow: String, step: String },
    /// Change a Step's Policy: `--to` takes JSON in the verb's shape (start, on_failure, on_stale, placement).
    Policy {
        workflow: String,
        step: String,
        #[arg(long)]
        to: String,
    },
    /// Choose the Performer for a Step's next Run.
    Place { workflow: String, step: String, performer: String, locality: String },
    /// Stop a Step's live Run, or the whole Workflow.
    Cancel {
        workflow: String,
        #[arg(long)]
        step: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Forget a finished Workflow and the children its Runs made (cancel a live one first).
    Remove { workflow: String },
    /// Advance every Workflow once; with --watch, keep going until nothing is running.
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
        step: String,
        #[arg(long)]
        run: Option<u32>,
    },
    /// What the performers file looks like.
    Performers,
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

fn driver(cli: &Cli) -> Result<(Arc<SqliteStore>, Driver), Error> {
    let store = Arc::new(SqliteStore::open(&cli.store)?);
    let mut driver = Driver::new(store.clone(), &format!("stepmeld@{}", std::process::id())).with_nesting();
    if let Some(path) = &cli.performers {
        for p in stepmeld_local::file::load(path)? {
            driver = driver.with_performer(Arc::new(p));
        }
    }
    Ok((store, driver))
}

fn run(cli: Cli) -> Result<(), Error> {
    let (store, driver) = driver(&cli)?;
    let by = actor(&cli)?;
    let now = iso(now_secs());
    let until = || iso(now_secs() + 60);
    match &cli.command {
        Cmd::Performers => {
            println!("{}", serde_json::to_string_pretty(&stepmeld_local::file::example()).unwrap());
            println!("\nEach program is run as: <command...> <run-dir>; it reads <run-dir>/request.json and writes <run-dir>/outcome.json (and progress.json as it goes).");
        }
        Cmd::Add { files } => {
            // verbs first, then recipes in whatever order they resolve:
            // a recipe that nests another may be named before it
            let mut steps = Vec::new();
            let mut recipes = Vec::new();
            for path in files {
                let text = std::fs::read_to_string(path).map_err(|e| Error::Refused(format!("cannot read {}: {e}", path.display())))?;
                let doc: Value = serde_json::from_str(&text).map_err(|e| Error::Refused(format!("{} is not JSON: {e}", path.display())))?;
                let contract = doc.get("contract").and_then(Value::as_str).unwrap_or("").to_string();
                stepmeld_core::contracts::validate(&doc, &contract).map_err(|e| Error::Refused(format!("{}: {e}", path.display())))?;
                match contract.as_str() {
                    stepmeld_core::definition::STEP_DEFINITION => steps.push((path.clone(), serde_json::from_value::<StepDefinition>(doc).map_err(|e| Error::Protocol(e.to_string()))?)),
                    stepmeld_core::definition::WORKFLOW_DEFINITION => recipes.push((path.clone(), serde_json::from_value::<WorkflowDefinition>(doc).map_err(|e| Error::Protocol(e.to_string()))?)),
                    other => return Err(Error::Refused(format!("{}: contract {other:?} is not a definition", path.display()))),
                }
            }
            for (_, def) in &steps {
                store.put_step(def)?;
                println!("added verb {}", def.reference());
            }
            while !recipes.is_empty() {
                let before = recipes.len();
                let mut kept = Vec::new();
                let mut errors = Vec::new();
                for (path, def) in recipes {
                    match store.put_workflow_definition(&def) {
                        Ok(()) => println!("added recipe {}", def.reference()),
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
                recipes = kept;
            }
        }
        Cmd::Library => {
            let lib = store.library()?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({"steps": lib.steps().collect::<Vec<_>>(), "workflows": lib.workflows().collect::<Vec<_>>()})).unwrap()
                );
            } else {
                for s in lib.steps() {
                    println!("verb    {:<28} {}", s.reference().to_string(), s.label.as_deref().unwrap_or(""));
                }
                for w in lib.workflows() {
                    println!("recipe  {:<28} {} ({} steps)", w.reference().to_string(), w.label.as_deref().unwrap_or(""), w.steps.len());
                }
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
                let status = lib.workflow(&wf.definition).map(|d| format!("{:?}", wf.status(d)).to_lowercase()).unwrap_or_else(|| "(recipe missing)".into());
                println!("{:<40} {:<24} {}", wf.id, wf.definition.to_string(), status);
            }
        }
        Cmd::Show { workflow } => {
            let (wf, _) = store.get(workflow)?.ok_or_else(|| Error::Refused(format!("no workflow {workflow:?}")))?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&wf).unwrap());
            } else {
                show(&store, &wf)?;
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
            driver.command(workflow, &Command::Start { step: step.clone() }, &by, &now)?;
            println!("go given to {step}; tick to run it");
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
        Cmd::Place { workflow, step, performer, locality } => {
            let locality = match locality.as_str() {
                "this-machine" => Locality::ThisMachine,
                "local-network" => Locality::LocalNetwork,
                "cloud" => Locality::Cloud,
                other => return Err(Error::Refused(format!("locality is this-machine, local-network or cloud, not {other:?}"))),
            };
            driver.command(
                workflow,
                &Command::Place {
                    step: step.clone(),
                    performer: performer.clone(),
                    locality,
                },
                &by,
                &now,
            )?;
            println!("placed {step} on {performer}");
        }
        Cmd::Cancel { workflow, step, reason } => {
            driver.command(workflow, &Command::Cancel { step: step.clone(), reason: reason.clone() }, &by, &now)?;
            println!("cancel asked; tick to carry it out");
        }
        Cmd::Remove { workflow } => {
            let gone = driver.remove(workflow, &now, &until())?;
            println!("removed {}", gone.join(", "));
        }
        Cmd::Tick { watch, every } => loop {
            let statuses = driver.tick_all(&iso(now_secs()), &until())?;
            for (id, status) in &statuses {
                println!("{id:<40} {}", format!("{status:?}").to_lowercase());
            }
            let busy = statuses.values().any(|s| matches!(s, stepmeld_core::workflow::WorkflowStatus::Running | stepmeld_core::workflow::WorkflowStatus::Ready));
            if !*watch || !busy {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(*every));
        },
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
                .ok_or_else(|| Error::Refused(format!("performer {:?} is not in this driver", r.performer)))?;
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

fn show(store: &SqliteStore, wf: &Workflow) -> Result<(), Error> {
    let lib = store.library()?;
    let def = lib.workflow(&wf.definition).ok_or_else(|| Error::Refused(format!("{} is not in the library", wf.definition)))?;
    println!("{}  ({}, {})", wf.id, wf.definition, format!("{:?}", wf.status(def)).to_lowercase());
    for (name, step) in &wf.steps {
        let status = wf.step_status(def, name);
        let reason = match &status {
            StepStatus::Waiting(Waiting::Input { name }) => format!("needs input {name}"),
            StepStatus::Waiting(Waiting::Parameter { name }) => format!("needs parameter {name}"),
            StepStatus::Waiting(Waiting::Completion) => "waiting for someone to complete it".into(),
            StepStatus::Waiting(Waiting::Go) => "waiting for a start".into(),
            StepStatus::Waiting(Waiting::Confirmation { locality }) => format!("waiting for the {} placement to be confirmed", locality.as_str()),
            StepStatus::Waiting(Waiting::Performer) => "no performer offers it".into(),
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
            _ => String::new(),
        };
        println!("  {:<16} {:<10} {}", name, status.word(), reason);
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
