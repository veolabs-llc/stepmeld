//! The Library side: verbs and recipes. Nothing here holds a Value.

use crate::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const STEP_DEFINITION: &str = "stepmeld/step-definition.v1";
pub const WORKFLOW_DEFINITION: &str = "stepmeld/workflow-definition.v1";
/// The tag of a Decision Output: the one kind of Value the engine reads.
pub const DECISION: &str = "decision";

/// A definition, by name and version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Ref {
    pub name: String,
    pub version: u32,
}

impl Ref {
    pub fn new(name: &str, version: u32) -> Ref {
        Ref { name: name.to_string(), version }
    }
}

impl std::fmt::Display for Ref {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} v{}", self.name, self.version)
    }
}

/// Who finishes a Step: a Performer, or a person issuing `complete`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DoneBy {
    #[default]
    Performer,
    Person,
}

/// Where a Performer runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Locality {
    ThisMachine,
    LocalNetwork,
    Cloud,
}

impl Locality {
    pub fn as_str(self) -> &'static str {
        match self {
            Locality::ThisMachine => "this-machine",
            Locality::LocalNetwork => "local-network",
            Locality::Cloud => "cloud",
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputDefinition {
    pub name: String,
    pub tag: String,
    #[serde(default = "yes")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParameterDefinition {
    pub name: String,
    pub tag: String,
    #[serde(default)]
    pub required: bool,
    /// The verb's own default, in effect until an Actor chooses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputDefinition {
    pub name: String,
    pub tag: String,
    /// Present on a Decision: the words its Value may be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl OutputDefinition {
    pub fn is_decision(&self) -> bool {
        self.options.is_some()
    }
}

/// When a Step starts, once its Gate is otherwise open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Start {
    /// As soon as the Gate opens, the first time.
    #[default]
    Automatic,
    /// Only on a `start` Command.
    Manual,
}

/// What happens when a Run fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase", tag = "do")]
pub enum OnFailure {
    #[default]
    Stop,
    /// Retake, up to `max` more Runs, when the Outcome's class is one
    /// of `classes` (`lost` is a class).
    Retake { max: u32, classes: Vec<String> },
}

/// What happens when a succeeded Step goes stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OnStale {
    #[default]
    Wait,
    Rerun,
}

/// How a Performer is chosen for a Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlacementPolicy {
    /// A Performer by name; none means the driver's choice among those
    /// offering the verb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performer: Option<String>,
    /// Localities a person must confirm before a Run starts there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirm: Vec<Locality>,
}

/// A Step's rules, from a fixed menu. The verb carries its defaults;
/// a Workflow's Step may be changed by an Actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Policy {
    #[serde(default)]
    pub start: Start,
    #[serde(default)]
    pub on_failure: OnFailure,
    #[serde(default)]
    pub on_stale: OnStale,
    #[serde(default)]
    pub placement: PlacementPolicy,
}

/// A verb.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepDefinition {
    pub contract: String,
    pub name: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub done_by: DoneBy,
    #[serde(default)]
    pub inputs: Vec<InputDefinition>,
    #[serde(default)]
    pub parameters: Vec<ParameterDefinition>,
    #[serde(default)]
    pub outputs: Vec<OutputDefinition>,
    #[serde(default)]
    pub policy: Policy,
}

impl StepDefinition {
    pub fn new(name: &str, version: u32) -> StepDefinition {
        StepDefinition {
            contract: STEP_DEFINITION.to_string(),
            name: name.to_string(),
            version,
            label: None,
            description: None,
            done_by: DoneBy::Performer,
            inputs: Vec::new(),
            parameters: Vec::new(),
            outputs: Vec::new(),
            policy: Policy::default(),
        }
    }

    pub fn reference(&self) -> Ref {
        Ref::new(&self.name, self.version)
    }

    pub fn input(&self, name: &str) -> Option<&InputDefinition> {
        self.inputs.iter().find(|i| i.name == name)
    }

    pub fn output(&self, name: &str) -> Option<&OutputDefinition> {
        self.outputs.iter().find(|o| o.name == name)
    }

    /// The definition holds together: the contract word, names unique,
    /// Decisions tagged as such with at least one option.
    pub fn check(&self) -> Result<(), crate::Error> {
        let invalid = |m: String| crate::Error::Invalid(format!("step definition {}: {m}", self.reference()));
        if self.contract != STEP_DEFINITION {
            return Err(invalid(format!("contract is {:?}, not {STEP_DEFINITION:?}", self.contract)));
        }
        if self.name.is_empty() || self.name.contains('.') {
            return Err(invalid("the name is empty or contains a dot".into()));
        }
        // what is given (inputs, parameters) shares one namespace; what
        // is produced has its own, so a refine may take `model` and
        // give `model`
        let mut given = BTreeSet::new();
        let mut produced = BTreeSet::new();
        let names = self
            .inputs
            .iter()
            .map(|i| (&i.name, false))
            .chain(self.parameters.iter().map(|p| (&p.name, false)))
            .chain(self.outputs.iter().map(|o| (&o.name, true)));
        for (name, is_output) in names {
            if name.is_empty() {
                return Err(invalid("a port name is empty".into()));
            }
            let seen = if is_output { &mut produced } else { &mut given };
            if !seen.insert(name) {
                return Err(invalid(format!("port name {name:?} is used twice")));
            }
        }
        for o in &self.outputs {
            match &o.options {
                Some(opts) if o.tag != DECISION => return Err(invalid(format!("output {:?} has options but its tag is {:?}, not {DECISION:?}", o.name, o.tag))),
                Some(opts) if opts.is_empty() => return Err(invalid(format!("decision {:?} has no options", o.name))),
                None if o.tag == DECISION => return Err(invalid(format!("output {:?} is tagged {DECISION:?} but lists no options", o.name))),
                _ => {}
            }
        }
        Ok(())
    }
}

/// `step.port`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PortRef(pub String);

impl PortRef {
    pub fn new(step: &str, port: &str) -> PortRef {
        PortRef(format!("{step}.{port}"))
    }

    /// (step, port). A step name never holds a dot; a port name may,
    /// on a nested recipe's face (`publish.chunk.cloud`).
    pub fn split(&self) -> Option<(&str, &str)> {
        self.0.split_once('.')
    }
}

impl std::fmt::Display for PortRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A condition on a Binding, over facts the engine holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Guard {
    /// A Decision Output of `step` named `output` holds `is`.
    Decision {
        step: String,
        output: String,
        is: String,
    },
    /// `step` ended that way: `succeeded`, `failed`, `skipped`.
    Status {
        step: String,
        is: String,
    },
    /// `step`'s Output `output` holds a Value.
    Present {
        step: String,
        output: String,
    },
    All(Vec<Guard>),
    Any(Vec<Guard>),
    Not(Box<Guard>),
}

/// One use of a verb in a recipe. Two entries for the same verb differ
/// by name and nothing else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepEntry {
    pub name: String,
    pub step: Ref,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// From one step's Output to another step's Input. Several Bindings
/// into one Input are alternatives, in order: the first whose source
/// holds a Value and whose Guard holds wins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub from: PortRef,
    pub to: PortRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<Guard>,
}

/// A recipe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub contract: String,
    pub name: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub steps: Vec<StepEntry>,
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

impl WorkflowDefinition {
    pub fn new(name: &str, version: u32) -> WorkflowDefinition {
        WorkflowDefinition {
            contract: WORKFLOW_DEFINITION.to_string(),
            name: name.to_string(),
            version,
            label: None,
            description: None,
            steps: Vec::new(),
            bindings: Vec::new(),
        }
    }

    pub fn reference(&self) -> Ref {
        Ref::new(&self.name, self.version)
    }

    pub fn entry(&self, name: &str) -> Option<&StepEntry> {
        self.steps.iter().find(|s| s.name == name)
    }

    /// The Bindings into one Input, in recipe order.
    pub fn bindings_into<'a>(&'a self, step: &'a str, input: &'a str) -> impl Iterator<Item = &'a Binding> + 'a {
        self.bindings.iter().filter(move |b| b.to.split() == Some((step, input)))
    }

    /// The steps whose Outputs feed `step`, or whose Decisions its
    /// Guards read.
    pub fn upstream(&self, step: &str) -> BTreeSet<&str> {
        let mut out = BTreeSet::new();
        for b in &self.bindings {
            if b.to.split().map(|(s, _)| s) == Some(step) {
                if let Some((s, _)) = b.from.split() {
                    out.insert(s);
                }
                if let Some(g) = &b.guard {
                    guard_steps(g, &mut out);
                }
            }
        }
        out
    }
}

fn guard_steps<'a>(g: &'a Guard, out: &mut BTreeSet<&'a str>) {
    match g {
        Guard::Decision { step, .. } | Guard::Status { step, .. } | Guard::Present { step, .. } => {
            out.insert(step);
        }
        Guard::All(gs) | Guard::Any(gs) => gs.iter().for_each(|g| guard_steps(g, out)),
        Guard::Not(g) => guard_steps(g, out),
    }
}
