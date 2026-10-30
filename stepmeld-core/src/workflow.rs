//! The Workflow side: one occurrence of a WorkflowDefinition, the only
//! place Values live. Status is derived here, never stored. A Workflow
//! carries its own copy of the WorkflowDefinition, made when it was
//! created: the Library's may change afterwards, or be edited on this
//! Workflow alone, and neither reaches the other.

use crate::definition::{DoneBy, Guard, Locality, OnFailure, OnStale, Policy, Ref, Start, WorkflowDefinition};
use crate::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const WORKFLOW: &str = "stepmeld/workflow.v1";

/// Who is responsible for a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Actor {
    Person {
        id: String,
    },
    Agent {
        id: String,
    },
    /// A rule acting on its own, named so the record says why.
    Policy {
        rule: String,
        step: String,
    },
    /// A parent Workflow's Run, for the Commands it issues to a child.
    Run {
        workflow: String,
        step: String,
        run: String,
    },
}

impl Actor {
    pub fn person(id: &str) -> Actor {
        Actor::Person { id: id.to_string() }
    }

    pub fn policy(rule: &str, step: &str) -> Actor {
        Actor::Policy {
            rule: rule.to_string(),
            step: step.to_string(),
        }
    }

    /// A person or an agent: someone who can confirm.
    pub fn is_someone(&self) -> bool {
        matches!(self, Actor::Person { .. } | Actor::Agent { .. })
    }
}

/// When, by whom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub at: String,
    pub by: Actor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Input {
    pub tag: String,
    pub required: bool,
    /// Fed by a Binding; a `set` is refused.
    pub bound: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    pub tag: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// None: the default is in effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen: Option<Stamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

/// The Performer chosen for the next Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub performer: String,
    pub locality: Locality,
    pub by: Actor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub at: String,
}

/// How a Run ended. The four never change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    Succeeded,
    Failed,
    Canceled,
    Lost,
}

/// The Performer's account of what it did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub headline: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub state: RunState,
    /// The failure's class, the Performer's word (`lost` for a lost
    /// Run; `refused` for a start that never ran; `fault` for a
    /// Performer that broke its contract).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Value>,
}

impl Outcome {
    pub fn succeeded(outputs: BTreeMap<String, Value>) -> Outcome {
        Outcome {
            state: RunState::Succeeded,
            class: None,
            reason: None,
            summary: None,
            outputs,
            metrics: None,
        }
    }

    pub fn failed(class: &str, reason: &str) -> Outcome {
        Outcome {
            state: RunState::Failed,
            class: Some(class.to_string()),
            reason: Some(reason.to_string()),
            summary: None,
            outputs: BTreeMap::new(),
            metrics: None,
        }
    }

    pub fn lost(reason: &str) -> Outcome {
        Outcome {
            state: RunState::Lost,
            class: Some("lost".into()),
            reason: Some(reason.to_string()),
            summary: None,
            outputs: BTreeMap::new(),
            metrics: None,
        }
    }

    pub fn canceled(reason: &str) -> Outcome {
        Outcome {
            state: RunState::Canceled,
            class: Some("canceled".into()),
            reason: Some(reason.to_string()),
            summary: None,
            outputs: BTreeMap::new(),
            metrics: None,
        }
    }

    /// The class a retake Policy is matched against.
    pub fn class_word(&self) -> &str {
        self.class.as_deref().unwrap_or(match self.state {
            RunState::Succeeded => "succeeded",
            RunState::Failed => "failed",
            RunState::Canceled => "canceled",
            RunState::Lost => "lost",
        })
    }
}

/// One execution of a Step. Append-only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub number: u32,
    pub inputs: BTreeMap<String, Value>,
    pub parameters: BTreeMap<String, Value>,
    pub performer: String,
    /// None for a person's Run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locality: Option<Locality>,
    pub started: Stamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// A cancel asked for and not yet carried out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<Stamp>,
}

impl Run {
    pub fn live(&self) -> bool {
        self.outcome.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub definition: Ref,
    pub done_by: DoneBy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub inputs: BTreeMap<String, Input>,
    pub parameters: BTreeMap<String, Parameter>,
    pub outputs: BTreeMap<String, Output>,
    pub policy: Policy,
    /// A `start` given and not yet consumed by a Run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub go: Option<Stamp>,
    /// A confirmation of the placement, not yet consumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<Stamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<Placement>,
    /// A `skip` given by an Actor. Final.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<Stamp>,
    /// The Performers offering the verb, noted by a driver while a
    /// placement is somebody's to make (several offer it, none chosen),
    /// so a reader with only the document sees the wait and the choice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offered: Vec<String>,
    #[serde(default)]
    pub runs: Vec<Run>,
}

impl Step {
    pub fn latest(&self) -> Option<&Run> {
        self.runs.last()
    }

    pub fn latest_mut(&mut self) -> Option<&mut Run> {
        self.runs.last_mut()
    }

    /// The Run whose Outputs the Step shows.
    pub fn current(&self) -> Option<&Run> {
        self.runs.iter().rev().find(|r| r.outcome.as_ref().is_some_and(|o| o.state == RunState::Succeeded))
    }

    pub fn run(&self, id: &str) -> Option<&Run> {
        self.runs.iter().find(|r| r.id == id)
    }

    pub fn run_mut(&mut self, id: &str) -> Option<&mut Run> {
        self.runs.iter_mut().find(|r| r.id == id)
    }

    /// Inputs and Parameters as a Run would record them now.
    pub fn resolved(&self) -> (BTreeMap<String, Value>, BTreeMap<String, Value>) {
        let inputs = self.inputs.iter().filter_map(|(k, i)| Some((k.clone(), i.value.clone()?))).collect();
        let parameters = self.parameters.iter().filter_map(|(k, p)| Some((k.clone(), p.value.clone()?))).collect();
        (inputs, parameters)
    }

    /// Succeeded, and the Values now differ from the current Run's.
    pub fn stale(&self) -> bool {
        match self.current() {
            Some(run) if self.latest().is_some_and(|l| l.id == run.id) => self.resolved() != (run.inputs.clone(), run.parameters.clone()),
            _ => false,
        }
    }

    /// Runs the retake Policy has made since the last Run somebody
    /// started.
    fn automatic_retakes(&self) -> u32 {
        self.runs.iter().rev().take_while(|r| matches!(&r.started.by, Actor::Policy { rule, .. } if rule == "retake")).count() as u32
    }
}

/// Why a Step has no Performer to go to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unplaced {
    /// No Performer offers the verb (or the one named does not).
    Nobody,
    /// Several offer it and none is chosen.
    Several,
}

/// Why a Gate is closed on something other than upstream work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "kebab-case")]
pub enum Waiting {
    /// An Input nobody feeds needs a Value.
    Input {
        name: String,
    },
    Parameter {
        name: String,
    },
    /// A person must `complete` the Step.
    Completion,
    /// A person must `start` it.
    Go,
    /// A person must confirm the placement.
    Confirmation {
        locality: Locality,
    },
    /// No Performer offers the verb.
    Performer,
    /// The Workflow as a whole has not been started.
    Start,
    /// Several Performers offer the verb and neither an Actor nor the
    /// Policy has chosen among them: a placement is somebody's to make.
    Placement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum StepStatus {
    /// Upstream work has not produced what it needs.
    NotReady,
    /// The Gate is open.
    Ready,
    Running,
    Waiting(Waiting),
    Succeeded,
    Failed,
    Canceled,
    /// No path leads to it, or an Actor skipped it. Final.
    Skipped,
    /// Succeeded, and its Values changed since.
    Stale,
}

impl StepStatus {
    pub fn word(&self) -> &'static str {
        match self {
            StepStatus::NotReady => "not-ready",
            StepStatus::Ready => "ready",
            StepStatus::Running => "running",
            StepStatus::Waiting(_) => "waiting",
            StepStatus::Succeeded => "succeeded",
            StepStatus::Failed => "failed",
            StepStatus::Canceled => "canceled",
            StepStatus::Skipped => "skipped",
            StepStatus::Stale => "stale",
        }
    }

    /// Counts as done for the Workflow.
    pub fn is_done(&self) -> bool {
        matches!(self, StepStatus::Succeeded | StepStatus::Skipped)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowStatus {
    NotReady,
    Ready,
    Running,
    /// Somebody must act; which Step says what.
    Waiting,
    Succeeded,
    Failed,
    Canceled,
    Stale,
}

/// What the Gate says of a Step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Open,
    Closed(StepStatus),
}

/// What a bound Input gets.
#[derive(Debug, Clone, PartialEq)]
pub enum Feed {
    Value(Value),
    /// An earlier Binding cannot be told yet.
    Undecided,
    /// No Binding will ever carry one.
    Never,
}

/// A fact that may not be known yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tri {
    True,
    False,
    Unknown,
}

impl Tri {
    fn not(self) -> Tri {
        match self {
            Tri::True => Tri::False,
            Tri::False => Tri::True,
            Tri::Unknown => Tri::Unknown,
        }
    }

    fn all(it: impl Iterator<Item = Tri>) -> Tri {
        let mut out = Tri::True;
        for t in it {
            match t {
                Tri::False => return Tri::False,
                Tri::Unknown => out = Tri::Unknown,
                Tri::True => {}
            }
        }
        out
    }

    fn any(it: impl Iterator<Item = Tri>) -> Tri {
        let mut out = Tri::False;
        for t in it {
            match t {
                Tri::True => return Tri::True,
                Tri::Unknown => out = Tri::Unknown,
                Tri::False => {}
            }
        }
        out
    }
}

/// What a Performer looks like to the Gate: enough to place a Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerformerFace {
    pub name: String,
    pub locality: Locality,
    pub verbs: Vec<Ref>,
    /// A pool: any of several Workers, the first free to take it. Never
    /// chosen on its own; an Actor or a Policy names it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pool: bool,
    /// Performs nested WorkflowDefinitions, whatever their names: the
    /// nested-workflow Performer, which makes a child Workflow of each.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub nests: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    pub contract: String,
    pub id: String,
    /// This Workflow's own WorkflowDefinition: the steps its `steps`
    /// are occurrences of, and the Bindings between them. A copy of
    /// the Library's at creation, this Workflow's alone since.
    pub workflow_definition: WorkflowDefinition,
    /// The Library's WorkflowDefinition this one was made from, by
    /// name and version; none when it was made from nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Ref>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created: Stamp,
    pub steps: BTreeMap<String, Step>,
    /// The go for the Workflow as a whole: until an Actor gives it no
    /// Step's Run is minted, whatever the Steps' own Policies say. A
    /// root Workflow is created without it (2026-10-13: starting the
    /// moment the Inputs are in made it easy to start before an optional
    /// Parameter was set); a nested one starts as it is created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<Stamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canceled: Option<Stamp>,
}

impl Workflow {
    /// A Workflow document from before each carried its own
    /// WorkflowDefinition (`definition` named the Library's by name and
    /// version), brought to this shape: the Library's definition of that
    /// name and version copied in, `from` set to it. A document already
    /// in this shape, or one naming a definition the Library does not
    /// hold, is refused in words.
    pub fn adopt(doc: &serde_json::Value, lib: &crate::library::Library) -> Result<Workflow, crate::Error> {
        let refused = |w: String| crate::Error::Refused(w);
        let id = doc.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        if doc.get("workflow_definition").is_some() {
            return Err(refused(format!("workflow {id} already carries its WorkflowDefinition")));
        }
        let reference: Ref = match doc.get("definition") {
            Some(r) => serde_json::from_value(r.clone()).map_err(|e| crate::Error::Protocol(format!("workflow {id}: definition cannot be read: {e}")))?,
            None => return Err(refused(format!("workflow {id} names no definition to adopt"))),
        };
        let def = lib.workflow(&reference).ok_or_else(|| refused(format!("workflow {id} was made from {reference}, which is not in the library")))?;
        let mut doc = doc.clone();
        let Some(map) = doc.as_object_mut() else {
            return Err(crate::Error::Protocol(format!("workflow {id} is not an object")));
        };
        map.remove("definition");
        // whole on its own, as a Workflow made now would be
        map.insert("workflow_definition".into(), serde_json::to_value(lib.inlined(def)).map_err(|e| crate::Error::Protocol(e.to_string()))?);
        map.insert("from".into(), serde_json::to_value(&reference).map_err(|e| crate::Error::Protocol(e.to_string()))?);
        serde_json::from_value(doc).map_err(|e| crate::Error::Protocol(format!("workflow {id} cannot be read once adopted: {e}")))
    }

    pub fn step(&self, name: &str) -> Result<&Step, crate::Error> {
        self.steps.get(name).ok_or_else(|| crate::Error::Refused(format!("no step {name:?} in workflow {}", self.id)))
    }

    pub fn step_mut(&mut self, name: &str) -> Result<&mut Step, crate::Error> {
        let id = self.id.clone();
        self.steps.get_mut(name).ok_or_else(|| crate::Error::Refused(format!("no step {name:?} in workflow {id}")))
    }

    // ---- derived facts ----

    /// Whether `step` has ended that way, as a Guard sees it.
    fn settled(&self, def: &WorkflowDefinition, step: &str) -> Option<StepStatus> {
        let status = self.step_status(def, step);
        matches!(status, StepStatus::Succeeded | StepStatus::Stale | StepStatus::Failed | StepStatus::Skipped | StepStatus::Canceled).then_some(status)
    }

    pub fn guard(&self, def: &WorkflowDefinition, g: &Guard) -> Tri {
        match g {
            Guard::Decision { step, output, is } => match self.settled(def, step) {
                Some(StepStatus::Succeeded | StepStatus::Stale) => match self.steps.get(step).and_then(|s| s.outputs.get(output)).and_then(|o| o.value.as_ref()) {
                    Some(v) => {
                        if v.as_str() == Some(is) {
                            Tri::True
                        } else {
                            Tri::False
                        }
                    }
                    None => Tri::False,
                },
                Some(_) => Tri::False,
                None => Tri::Unknown,
            },
            Guard::Status { step, is } => match self.settled(def, step) {
                Some(status) => {
                    let word = match status {
                        StepStatus::Stale => "succeeded",
                        other => other.word(),
                    };
                    if word == is {
                        Tri::True
                    } else {
                        Tri::False
                    }
                }
                None => Tri::Unknown,
            },
            Guard::Present { step, output } => match self.settled(def, step) {
                Some(StepStatus::Succeeded | StepStatus::Stale) => {
                    if self.steps.get(step).and_then(|s| s.outputs.get(output)).is_some_and(|o| o.value.is_some()) {
                        Tri::True
                    } else {
                        Tri::False
                    }
                }
                Some(_) => Tri::False,
                None => Tri::Unknown,
            },
            Guard::All(gs) => Tri::all(gs.iter().map(|g| self.guard(def, g))),
            Guard::Any(gs) => Tri::any(gs.iter().map(|g| self.guard(def, g))),
            Guard::Not(g) => self.guard(def, g).not(),
        }
    }

    /// Whether a Binding will carry a Value: its Guard holds and its
    /// source has Outputs, or will. A source that succeeded once keeps
    /// its Outputs while it is retaken.
    fn binding_alive(&self, def: &WorkflowDefinition, b: &crate::definition::Binding) -> Tri {
        let Some((src, _)) = b.from.split() else { return Tri::False };
        let source = match self.settled(def, src) {
            Some(StepStatus::Skipped) => Tri::False,
            Some(StepStatus::Succeeded | StepStatus::Stale) => Tri::True,
            _ if self.steps.get(src).is_some_and(|s| s.current().is_some()) => Tri::True,
            _ => Tri::Unknown,
        };
        let guard = b.guard.as_ref().map_or(Tri::True, |g| self.guard(def, g));
        Tri::all([source, guard].into_iter())
    }

    /// What a bound Input gets from its Bindings, taken in order: the
    /// first alive one wins; one that cannot be told yet holds the
    /// answer; none alive at all means the Input will never be fed.
    fn feed(&self, def: &WorkflowDefinition, step: &str, input: &str) -> Feed {
        for b in def.bindings_into(step, input) {
            match self.binding_alive(def, b) {
                Tri::Unknown => return Feed::Undecided,
                Tri::False => continue,
                Tri::True => {
                    let value = b.from.split().and_then(|(src, port)| self.steps.get(src)?.outputs.get(port)?.value.clone());
                    return match value {
                        Some(v) => Feed::Value(v),
                        None => Feed::Undecided,
                    };
                }
            }
        }
        Feed::Never
    }

    /// Carry Outputs into the Inputs bound to them, everywhere.
    pub fn propagate(&mut self, def: &WorkflowDefinition) {
        let mut changes = Vec::new();
        for (name, step) in &self.steps {
            for input in step.inputs.keys() {
                if def.bindings_into(name, input).next().is_some() {
                    let value = match self.feed(def, name, input) {
                        Feed::Value(v) => Some(v),
                        _ => None,
                    };
                    changes.push((name.clone(), input.clone(), value));
                }
            }
        }
        for (step, input, value) in changes {
            if let Some(i) = self.steps.get_mut(&step).and_then(|s| s.inputs.get_mut(&input)) {
                i.bound = true;
                i.value = value;
            }
        }
    }

    /// Status for display: the Gate's answer, or where the Step stands.
    pub fn step_status(&self, def: &WorkflowDefinition, name: &str) -> StepStatus {
        match self.gate(def, name, &[]) {
            Gate::Open => StepStatus::Ready,
            Gate::Closed(s) => s,
        }
    }

    /// The Gate, with the Performers on offer for placement. With none
    /// given, placement is not checked (status only).
    pub fn gate(&self, def: &WorkflowDefinition, name: &str, performers: &[PerformerFace]) -> Gate {
        let Some(step) = self.steps.get(name) else { return Gate::Closed(StepStatus::NotReady) };
        use Gate::Closed;
        // a skip is final: it was refused while a Run was live or after
        // a success, so nothing below could say otherwise
        if step.skipped.is_some() {
            return Closed(StepStatus::Skipped);
        }
        // 1. no live Run; where it stands if it ended
        if let Some(run) = step.latest() {
            match &run.outcome {
                None => return Closed(StepStatus::Running),
                Some(o) => match o.state {
                    RunState::Succeeded if !step.stale() => return Closed(StepStatus::Succeeded),
                    RunState::Succeeded => {}
                    RunState::Canceled => {
                        if step.go.is_none() {
                            return Closed(StepStatus::Canceled);
                        }
                    }
                    RunState::Failed | RunState::Lost => {}
                },
            }
        }
        if self.canceled.is_some() {
            return Closed(StepStatus::Canceled);
        }
        // 2. on a live path; 3. inputs present
        for (iname, input) in &step.inputs {
            let bindings: Vec<_> = def.bindings_into(name, iname).collect();
            if bindings.is_empty() {
                if input.required && input.value.is_none() {
                    return Closed(StepStatus::Waiting(Waiting::Input { name: iname.clone() }));
                }
                continue;
            }
            match self.feed(def, name, iname) {
                Feed::Never if input.required => return Closed(StepStatus::Skipped),
                // an optional Input a Binding may still carry waits for its
                // source too: starting without it would take the Value the
                // recipe meant it to have as one the person declined
                Feed::Undecided => return Closed(StepStatus::NotReady),
                Feed::Never | Feed::Value(_) => {}
            }
        }
        // 4. parameters present
        for (pname, p) in &step.parameters {
            if p.required && p.value.is_none() {
                return Closed(StepStatus::Waiting(Waiting::Parameter { name: pname.clone() }));
            }
        }
        // where a succeeded Step stands once its path and inputs are known
        let stale = step.stale();
        if step.done_by == DoneBy::Person {
            return Closed(match step.current() {
                Some(_) if !stale => StepStatus::Succeeded,
                Some(_) => StepStatus::Stale,
                None => StepStatus::Waiting(Waiting::Completion),
            });
        }
        // 5. go given
        let go = step.go.is_some()
            || match step.latest().and_then(|r| r.outcome.as_ref()) {
                None => step.policy.start == Start::Automatic,
                Some(o) if o.state == RunState::Succeeded => stale && step.policy.on_stale == OnStale::Rerun,
                Some(o) => match &step.policy.on_failure {
                    OnFailure::Retake { max, classes } => classes.iter().any(|c| c == o.class_word()) && step.automatic_retakes() < *max,
                    OnFailure::Stop => false,
                },
            };
        if !go {
            return Closed(match step.latest().and_then(|r| r.outcome.as_ref()).map(|o| o.state) {
                Some(RunState::Succeeded) => StepStatus::Stale,
                Some(RunState::Failed | RunState::Lost) => StepStatus::Failed,
                Some(RunState::Canceled) => StepStatus::Canceled,
                None => StepStatus::Waiting(Waiting::Go),
            });
        }
        // 6. placed and confirmed. With no Performers to choose from
        // (a reader with only the document), the placement the driver
        // noted stands in.
        let locality = if performers.is_empty() {
            if step.placement.is_none() && step.offered.len() > 1 {
                return Closed(StepStatus::Waiting(Waiting::Placement));
            }
            step.placement.as_ref().map(|p| p.locality)
        } else {
            let nested = def.entry(name).is_some_and(|e| e.definition.is_some());
            match Self::placement_among(step, nested, performers) {
                Ok(face) => Some(face.locality),
                Err(Unplaced::Nobody) => return Closed(StepStatus::Waiting(Waiting::Performer)),
                Err(Unplaced::Several) => return Closed(StepStatus::Waiting(Waiting::Placement)),
            }
        };
        let confirmed = step.confirmed.is_some() || step.go.as_ref().is_some_and(|g| g.by.is_someone());
        if let Some(locality) = locality {
            if step.policy.placement.confirm.contains(&locality) && !confirmed {
                return Closed(StepStatus::Waiting(Waiting::Confirmation { locality }));
            }
        }
        // 7. the Workflow has its go. Last, so a reader sees what else
        // the Step needs before the start is given.
        if self.started.is_none() {
            return Closed(StepStatus::Waiting(Waiting::Start));
        }
        Gate::Open
    }

    /// The Performer a Run of `step` would go to: the one placed by an
    /// Actor, else the Policy's, else the only one offering the verb.
    /// With several offering it and none chosen there is no answer:
    /// "the first" is an accident of one driver's list, and drivers
    /// that share a store need not list alike.
    pub fn placement_for<'a>(&self, name: &str, performers: &'a [PerformerFace]) -> Option<&'a PerformerFace> {
        let step = self.steps.get(name)?;
        let nested = self.workflow_definition.entry(name).is_some_and(|e| e.definition.is_some());
        Self::placement_among(step, nested, performers).ok()
    }

    /// The Performers that could take a Run of `step`: those offering
    /// its verb (any that nests, for a nested definition), in the
    /// Policy's locality when it names one and no Performer.
    pub fn offering<'a>(step: &Step, nested: bool, performers: &'a [PerformerFace]) -> impl Iterator<Item = &'a PerformerFace> + 'a {
        let verb = step.definition.clone();
        let wanted = match step.policy.placement.performer {
            Some(_) => None,
            None => step.policy.placement.locality,
        };
        performers.iter().filter(move |p| (p.verbs.contains(&verb) || (nested && p.nests)) && wanted.is_none_or(|l| p.locality == l))
    }

    /// `placement_for`, saying why there is none. `nested`: the step is
    /// an occurrence of a nested WorkflowDefinition carried inline.
    pub fn placement_among<'a>(step: &Step, nested: bool, performers: &'a [PerformerFace]) -> Result<&'a PerformerFace, Unplaced> {
        let offers = |p: &&PerformerFace| p.verbs.contains(&step.definition) || (nested && p.nests);
        let named = step.placement.as_ref().map(|p| &p.performer).or(step.policy.placement.performer.as_ref());
        if let Some(named) = named {
            return performers.iter().find(|p| &p.name == named).filter(offers).ok_or(Unplaced::Nobody);
        }
        // a pool is taken only when named: with one Worker offering
        // the verb, that Worker is the placement, pool or no pool
        let mut offering = Self::offering(step, nested, performers).filter(|p| !p.pool);
        match (offering.next(), offering.next()) {
            (Some(only), None) => Ok(only),
            (None, _) => Err(Unplaced::Nobody),
            (Some(_), Some(_)) => Err(Unplaced::Several),
        }
    }

    pub fn status(&self, def: &WorkflowDefinition) -> WorkflowStatus {
        if self.canceled.is_some() {
            return WorkflowStatus::Canceled;
        }
        let statuses: Vec<StepStatus> = self.steps.keys().map(|n| self.step_status(def, n)).collect();
        if statuses.iter().all(StepStatus::is_done) {
            return WorkflowStatus::Succeeded;
        }
        let has = |f: fn(&StepStatus) -> bool| statuses.iter().any(f);
        if has(|s| matches!(s, StepStatus::Running)) {
            WorkflowStatus::Running
        } else if has(|s| matches!(s, StepStatus::Waiting(_))) {
            WorkflowStatus::Waiting
        } else if has(|s| matches!(s, StepStatus::Failed | StepStatus::Canceled)) {
            WorkflowStatus::Failed
        } else if has(|s| matches!(s, StepStatus::Stale)) {
            WorkflowStatus::Stale
        } else if has(|s| matches!(s, StepStatus::Ready)) {
            WorkflowStatus::Ready
        } else {
            WorkflowStatus::NotReady
        }
    }

    /// Every Step's status, for a reader.
    pub fn statuses(&self, def: &WorkflowDefinition) -> BTreeMap<String, StepStatus> {
        self.steps.keys().map(|n| (n.clone(), self.step_status(def, n))).collect()
    }
}
