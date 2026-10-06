//! The pure core: Commands and observations in, a changed Workflow and
//! History entries out. No clock (time is passed in), no I/O.

use crate::definition::{DoneBy, Locality, OutputDefinition, Policy, Ref, WorkflowDefinition, DECISION};
use crate::history::Entry;
use crate::library::Library;
use crate::workflow::{Actor, Gate, Input, Outcome, Output, Parameter, PerformerFace, Placement, Progress, Run, RunState, Stamp, Step, StepStatus, Waiting, Workflow, WORKFLOW};
use crate::{Error, Value};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

/// What an Actor may ask.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum Command {
    /// Give an Input or a Parameter a Value (None: clear it; a
    /// Parameter goes back to its default).
    Set {
        step: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parameter: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Value>,
    },
    /// Give the go: start now, or retake, or rerun a stale Step.
    Start {
        step: String,
    },
    /// Finish a Step a person does, with its Outputs.
    Complete {
        step: String,
        outputs: BTreeMap<String, Value>,
    },
    /// Confirm the placement the Policy asks a person about.
    Confirm {
        step: String,
    },
    /// Choose the Performer for the next Run.
    Place {
        step: String,
        performer: String,
        locality: Locality,
    },
    SetPolicy {
        step: String,
        policy: Policy,
    },
    /// Stop a Step's live Run, or the whole Workflow.
    Cancel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// What a Performer is handed to start a Run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartRequest {
    pub workflow: String,
    pub step: String,
    /// The Run's id: a Performer asked twice for the same one returns
    /// the same handle.
    pub run: String,
    pub definition: Ref,
    pub inputs: BTreeMap<String, Value>,
    pub parameters: BTreeMap<String, Value>,
    /// What the Performer must produce on success.
    pub outputs: Vec<OutputDefinition>,
}

/// What a Performer says of a Run when asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "observation", rename_all = "kebab-case")]
pub enum Observation {
    Running {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<Progress>,
        /// Needs a person without having ended.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attention: Option<String>,
    },
    Ended {
        outcome: Outcome,
    },
    /// The Performer does not know this handle.
    Absent,
    /// It could not be asked; nothing changes.
    Unreachable {
        reason: String,
    },
}

/// What a driver should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// The Gate is open and no Run is minted: mint one and start it.
    Mint {
        step: String,
    },
    /// A minted Run has no handle yet: ask its Performer to start it.
    Start {
        step: String,
        run: String,
    },
    Observe {
        step: String,
        run: String,
        performer: String,
        handle: String,
    },
    Cancel {
        step: String,
        run: String,
        performer: String,
        handle: String,
        reason: String,
    },
}

fn stamp(at: &str, by: &Actor) -> Stamp {
    Stamp { at: at.to_string(), by: by.clone() }
}

/// A Workflow from a recipe: every Step with its slots empty, every
/// Parameter at its default.
pub fn create(lib: &Library, def: &WorkflowDefinition, id: &str, label: Option<&str>, by: &Actor, now: &str) -> Result<(Workflow, Vec<Entry>), Error> {
    lib.check_workflow(def)?;
    let mut steps = BTreeMap::new();
    for entry in &def.steps {
        let sdef = lib.step(&entry.step).ok_or_else(|| Error::Invalid(format!("{} is not in the library", entry.step)))?;
        let inputs = sdef
            .inputs
            .iter()
            .map(|i| {
                (
                    i.name.clone(),
                    Input {
                        tag: i.tag.clone(),
                        required: i.required,
                        bound: def.bindings_into(&entry.name, &i.name).next().is_some(),
                        value: None,
                    },
                )
            })
            .collect();
        let parameters = sdef
            .parameters
            .iter()
            .map(|p| {
                (
                    p.name.clone(),
                    Parameter {
                        tag: p.tag.clone(),
                        required: p.required,
                        value: p.default.clone(),
                        chosen: None,
                    },
                )
            })
            .collect();
        let outputs = sdef
            .outputs
            .iter()
            .map(|o| {
                (
                    o.name.clone(),
                    Output {
                        tag: o.tag.clone(),
                        options: o.options.clone(),
                        value: None,
                    },
                )
            })
            .collect();
        steps.insert(
            entry.name.clone(),
            Step {
                definition: entry.step.clone(),
                done_by: sdef.done_by,
                label: entry.label.clone().or(sdef.label.clone()),
                inputs,
                parameters,
                outputs,
                policy: sdef.policy.clone(),
                go: None,
                confirmed: None,
                placement: None,
                runs: Vec::new(),
            },
        );
    }
    let wf = Workflow {
        contract: WORKFLOW.to_string(),
        id: id.to_string(),
        definition: def.reference(),
        label: label.map(str::to_string),
        created: stamp(now, by),
        steps,
        canceled: None,
    };
    let entry = Entry::new(now, by, id, "created").detail(json!({"definition": def.reference()}));
    Ok((wf, vec![entry]))
}

fn refused(words: String) -> Error {
    Error::Refused(words)
}

/// Apply a Command. The Workflow is changed only when Ok.
pub fn apply(lib: &Library, def: &WorkflowDefinition, wf: &mut Workflow, cmd: &Command, by: &Actor, now: &str) -> Result<Vec<Entry>, Error> {
    let id = wf.id.clone();
    let entry = |kind: &str, step: &str| Entry::new(now, by, &id, kind).step(step);
    let not_running = |wf: &Workflow, step: &str| -> Result<(), Error> {
        if wf.step(step)?.latest().is_some_and(Run::live) {
            return Err(refused(format!("step {step:?} is running; cancel it first")));
        }
        Ok(())
    };
    let out = match cmd {
        Command::Set { step, input, parameter, value } => {
            not_running(wf, step)?;
            let s = wf.step_mut(step)?;
            match (input, parameter) {
                (Some(name), None) => {
                    let i = s.inputs.get_mut(name).ok_or_else(|| refused(format!("step {step:?} has no input {name:?}")))?;
                    if i.bound {
                        return Err(refused(format!("input {step}.{name} is fed by a binding; set the step it comes from")));
                    }
                    i.value = value.clone();
                    vec![entry("set", step).detail(json!({"input": name, "value": value}))]
                }
                (None, Some(name)) => {
                    let p = s.parameters.get_mut(name).ok_or_else(|| refused(format!("step {step:?} has no parameter {name:?}")))?;
                    match value {
                        Some(v) => {
                            p.value = Some(v.clone());
                            p.chosen = Some(stamp(now, by));
                        }
                        None => {
                            let sdef = lib.step(&s.definition).ok_or_else(|| Error::Invalid(format!("{} is not in the library", s.definition)))?;
                            p.value = sdef.parameters.iter().find(|d| &d.name == name).and_then(|d| d.default.clone());
                            p.chosen = None;
                        }
                    }
                    vec![entry("set", step).detail(json!({"parameter": name, "value": value}))]
                }
                _ => return Err(refused("set names one input or one parameter".into())),
            }
        }
        Command::Start { step } => {
            not_running(wf, step)?;
            let s = wf.step_mut(step)?;
            if s.done_by == DoneBy::Person {
                return Err(refused(format!("step {step:?} is a person's: complete it, do not start it")));
            }
            s.go = Some(stamp(now, by));
            vec![entry("go", step)]
        }
        Command::Complete { step, outputs } => {
            not_running(wf, step)?;
            let status = wf.step_status(def, step);
            let wf_id = wf.id.clone();
            let s = wf.step_mut(step)?;
            if s.done_by != DoneBy::Person {
                return Err(refused(format!("step {step:?} is a performer's: it completes itself")));
            }
            if !matches!(status, StepStatus::Waiting(Waiting::Completion) | StepStatus::Stale | StepStatus::Succeeded) {
                return Err(refused(format!("step {step:?} is {}, not waiting to be completed", status.word())));
            }
            check_outputs(&s.outputs, outputs).map_err(refused)?;
            let (inputs, parameters) = s.resolved();
            let number = s.runs.len() as u32 + 1;
            let run_id = format!("{wf_id}/{step}/{number}");
            let mut outcome = Outcome::succeeded(outputs.clone());
            outcome.summary = Some(crate::workflow::Summary {
                headline: format!("completed by {}", actor_word(by)),
                details: None,
            });
            s.runs.push(Run {
                id: run_id.clone(),
                number,
                inputs,
                parameters,
                performer: actor_word(by),
                locality: None,
                started: stamp(now, by),
                handle: None,
                progress: None,
                attention: None,
                outcome: Some(outcome),
                ended_at: Some(now.to_string()),
                cancel: None,
            });
            for (k, v) in outputs {
                if let Some(o) = s.outputs.get_mut(k) {
                    o.value = Some(v.clone());
                }
            }
            vec![entry("completed", step).run(&run_id).detail(json!({"outputs": outputs}))]
        }
        Command::Confirm { step } => {
            wf.step_mut(step)?.confirmed = Some(stamp(now, by));
            vec![entry("confirmed", step)]
        }
        Command::Place { step, performer, locality } => {
            not_running(wf, step)?;
            let s = wf.step_mut(step)?;
            s.placement = Some(Placement {
                performer: performer.clone(),
                locality: *locality,
                by: by.clone(),
            });
            s.confirmed = None;
            vec![entry("placed", step).detail(json!({"performer": performer, "locality": locality}))]
        }
        Command::SetPolicy { step, policy } => {
            wf.step_mut(step)?.policy = policy.clone();
            vec![entry("policy-set", step).detail(serde_json::to_value(policy).unwrap_or(Value::Null))]
        }
        Command::Cancel { step, reason } => {
            let reason = reason.clone().unwrap_or_else(|| format!("canceled by {}", actor_word(by)));
            let targets: Vec<String> = match step {
                Some(s) => vec![wf.step(s).map(|_| s.clone())?],
                None => wf.steps.keys().cloned().collect(),
            };
            let mut entries = Vec::new();
            for name in &targets {
                let s = wf.step_mut(name)?;
                if let Some(run) = s.latest_mut().filter(|r| r.live()) {
                    run.cancel = Some(stamp(now, by));
                    entries.push(entry("cancel-asked", name).run(&run.id).detail(json!({"reason": reason})));
                } else if step.is_some() {
                    return Err(refused(format!("step {name:?} has no live run to cancel")));
                }
                s.go = None;
            }
            if step.is_none() {
                wf.canceled = Some(stamp(now, by));
                entries.push(Entry::new(now, by, &id, "canceled").detail(json!({"reason": reason})));
            }
            entries
        }
    };
    wf.propagate(def);
    Ok(out)
}

fn actor_word(a: &Actor) -> String {
    match a {
        Actor::Person { id } => format!("person:{id}"),
        Actor::Agent { id } => format!("agent:{id}"),
        Actor::Policy { rule, step } => format!("policy:{rule}@{step}"),
        Actor::Run { workflow, step, run } => format!("run:{workflow}/{step}/{run}"),
    }
}

/// Every declared Output present, a Decision's Value one of its options.
fn check_outputs(declared: &BTreeMap<String, Output>, given: &BTreeMap<String, Value>) -> Result<(), String> {
    for (name, o) in declared {
        let v = given.get(name).ok_or_else(|| format!("output {name:?} was not produced"))?;
        if let Some(options) = &o.options {
            let word = v.as_str().ok_or_else(|| format!("{DECISION} {name:?} must be one of {options:?}"))?;
            if !options.iter().any(|w| w == word) {
                return Err(format!("{DECISION} {name:?} is {word:?}, not one of {options:?}"));
            }
        }
    }
    for name in given.keys() {
        if !declared.contains_key(name) {
            return Err(format!("output {name:?} is not declared"));
        }
    }
    Ok(())
}

/// Note, on each Step waiting for a confirmation, which Performer it
/// would go to, so a reader with only the document sees the same
/// status the driver does.
pub fn note_placements(def: &WorkflowDefinition, wf: &mut Workflow, performers: &[PerformerFace], now: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let names: Vec<String> = wf.steps.keys().cloned().collect();
    for name in names {
        let waiting = matches!(wf.gate(def, &name, performers), Gate::Closed(StepStatus::Waiting(Waiting::Confirmation { .. })));
        let step = &wf.steps[&name];
        if !waiting || step.placement.is_some() {
            continue;
        }
        let Some(face) = wf.placement_for(step, performers) else { continue };
        let by = Actor::policy("placement", &name);
        let placement = Placement {
            performer: face.name.clone(),
            locality: face.locality,
            by: by.clone(),
        };
        entries.push(
            Entry::new(now, &by, &wf.id, "placed")
                .step(&name)
                .detail(json!({"performer": face.name, "locality": face.locality, "awaiting": "confirmation"})),
        );
        wf.steps.get_mut(&name).unwrap().placement = Some(placement);
    }
    entries
}

/// What to do now, in step order.
pub fn plan(def: &WorkflowDefinition, wf: &Workflow, performers: &[PerformerFace]) -> Vec<Effect> {
    let mut out = Vec::new();
    for (name, step) in &wf.steps {
        if let Some(run) = step.latest().filter(|r| r.live()) {
            match (&run.handle, &run.cancel) {
                (Some(handle), Some(c)) => out.push(Effect::Cancel {
                    step: name.clone(),
                    run: run.id.clone(),
                    performer: run.performer.clone(),
                    handle: handle.clone(),
                    reason: format!("asked by {}", actor_word(&c.by)),
                }),
                (Some(handle), None) => out.push(Effect::Observe {
                    step: name.clone(),
                    run: run.id.clone(),
                    performer: run.performer.clone(),
                    handle: handle.clone(),
                }),
                (None, _) => out.push(Effect::Start { step: name.clone(), run: run.id.clone() }),
            }
            continue;
        }
        if wf.gate(def, name, performers) == Gate::Open {
            out.push(Effect::Mint { step: name.clone() });
        }
    }
    out
}

/// Mint a Run for a Step whose Gate is open: its Values as resolved
/// now, its placement, who gave the go. The request to hand the
/// Performer comes back with it.
pub fn mint(lib: &Library, def: &WorkflowDefinition, wf: &mut Workflow, step: &str, performers: &[PerformerFace], now: &str) -> Result<(StartRequest, Vec<Entry>), Error> {
    match wf.gate(def, step, performers) {
        Gate::Open => {}
        Gate::Closed(s) => return Err(refused(format!("step {step:?} is {}; its gate is not open", s.word()))),
    }
    let id = wf.id.clone();
    let s = wf.step_mut(step)?;
    let face = wf_placement(s, performers)?;
    let sdef = lib.step(&s.definition).ok_or_else(|| Error::Invalid(format!("{} is not in the library", s.definition)))?;
    let by = match s.go.take() {
        Some(go) => go.by,
        None => match s.latest().and_then(|r| r.outcome.as_ref()).map(|o| o.state) {
            None => Actor::policy("start-automatically", step),
            Some(RunState::Succeeded) => Actor::policy("rerun-on-stale", step),
            Some(_) => Actor::policy("retake", step),
        },
    };
    // a confirmation stands until the placement changes: a retake of
    // the same placement is not asked about again
    let (inputs, parameters) = s.resolved();
    let number = s.runs.len() as u32 + 1;
    let run_id = format!("{id}/{step}/{number}");
    s.runs.push(Run {
        id: run_id.clone(),
        number,
        inputs: inputs.clone(),
        parameters: parameters.clone(),
        performer: face.name.clone(),
        locality: Some(face.locality),
        started: stamp(now, &by),
        handle: None,
        progress: None,
        attention: None,
        outcome: None,
        ended_at: None,
        cancel: None,
    });
    let request = StartRequest {
        workflow: id.clone(),
        step: step.to_string(),
        run: run_id.clone(),
        definition: s.definition.clone(),
        inputs,
        parameters,
        outputs: sdef.outputs.clone(),
    };
    let entry = Entry::new(now, &by, &id, "run-minted")
        .step(step)
        .run(&run_id)
        .detail(json!({"performer": face.name, "locality": face.locality, "number": number}));
    Ok((request, vec![entry]))
}

fn wf_placement(step: &Step, performers: &[PerformerFace]) -> Result<PerformerFace, Error> {
    let offers = |p: &&PerformerFace| p.verbs.contains(&step.definition);
    let found = if let Some(placed) = &step.placement {
        performers.iter().find(|p| p.name == placed.performer).filter(offers)
    } else if let Some(named) = &step.policy.placement.performer {
        performers.iter().find(|p| &p.name == named).filter(offers)
    } else {
        performers.iter().find(offers)
    };
    found.cloned().ok_or_else(|| refused(format!("no performer offers {}", step.definition)))
}

/// The request to start a minted Run again (after a crash between
/// minting and starting).
pub fn request_for(lib: &Library, wf: &Workflow, step: &str, run: &str) -> Result<StartRequest, Error> {
    let s = wf.step(step)?;
    let r = s.run(run).ok_or_else(|| refused(format!("no run {run:?}")))?;
    let sdef = lib.step(&s.definition).ok_or_else(|| Error::Invalid(format!("{} is not in the library", s.definition)))?;
    Ok(StartRequest {
        workflow: wf.id.clone(),
        step: step.to_string(),
        run: run.to_string(),
        definition: s.definition.clone(),
        inputs: r.inputs.clone(),
        parameters: r.parameters.clone(),
        outputs: sdef.outputs.clone(),
    })
}

/// The Performer accepted: record its handle.
pub fn started(wf: &mut Workflow, step: &str, run: &str, handle: &str, now: &str) -> Result<Vec<Entry>, Error> {
    let id = wf.id.clone();
    let r = wf.step_mut(step)?.run_mut(run).ok_or_else(|| refused(format!("no run {run:?}")))?;
    r.handle = Some(handle.to_string());
    let by = r.started.by.clone();
    Ok(vec![Entry::new(now, &by, &id, "run-started").step(step).run(run).detail(json!({"handle": handle}))])
}

/// The Performer refused to start: the Run ends failed, class
/// `refused`, in the Performer's words. Nothing ran.
pub fn refused_start(def: &WorkflowDefinition, wf: &mut Workflow, step: &str, run: &str, words: &str, now: &str) -> Result<Vec<Entry>, Error> {
    let mut outcome = Outcome::failed("refused", words);
    outcome.summary = Some(crate::workflow::Summary {
        headline: format!("refused: {words}"),
        details: None,
    });
    observed(def, wf, step, run, &Observation::Ended { outcome }, now)
}

/// What a Performer said when asked. Ending a Run that has ended is
/// not an error; unreachable changes nothing.
pub fn observed(def: &WorkflowDefinition, wf: &mut Workflow, step: &str, run: &str, obs: &Observation, now: &str) -> Result<Vec<Entry>, Error> {
    let id = wf.id.clone();
    let declared: BTreeMap<String, Output> = wf.step(step)?.outputs.clone();
    let s = wf.step_mut(step)?;
    let r = s.run_mut(run).ok_or_else(|| refused(format!("no run {run:?}")))?;
    let by = r.started.by.clone();
    let entry = |kind: &str| Entry::new(now, &by, &id, kind).step(step).run(run);
    if r.outcome.is_some() {
        return Ok(Vec::new());
    }
    let out = match obs {
        Observation::Unreachable { .. } => Vec::new(),
        Observation::Running { progress, attention } => {
            let mut entries = Vec::new();
            if progress.is_some() && *progress != r.progress {
                r.progress = progress.clone();
                entries.push(entry("progress").detail(serde_json::to_value(progress).unwrap_or(Value::Null)));
            }
            if *attention != r.attention {
                r.attention = attention.clone();
                entries.push(entry("attention").detail(json!({"attention": attention})));
            }
            entries
        }
        Observation::Absent => {
            r.outcome = Some(Outcome::lost("the performer no longer knows this run"));
            r.ended_at = Some(now.to_string());
            vec![entry("run-ended").detail(serde_json::to_value(&r.outcome).unwrap_or(Value::Null))]
        }
        Observation::Ended { outcome } => {
            let mut outcome = outcome.clone();
            if outcome.state == RunState::Succeeded {
                if let Err(why) = check_outputs(&declared, &outcome.outputs) {
                    outcome = Outcome::failed("fault", &format!("the performer reported success but {why}"));
                }
            }
            r.attention = None;
            r.outcome = Some(outcome.clone());
            r.ended_at = Some(now.to_string());
            if outcome.state == RunState::Succeeded {
                for (k, v) in &outcome.outputs {
                    if let Some(o) = s.outputs.get_mut(k) {
                        o.value = Some(v.clone());
                    }
                }
            }
            vec![entry("run-ended").detail(serde_json::to_value(&outcome).unwrap_or(Value::Null))]
        }
    };
    wf.propagate(def);
    Ok(out)
}
