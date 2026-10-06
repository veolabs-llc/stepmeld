//! The stepmeld workflow engine (docs/DESIGN.md).
//!
//! A Library of verbs ([`StepDefinition`]) and recipes
//! ([`WorkflowDefinition`]); a [`Workflow`] made of Steps, each run by a
//! Performer when its Gate opens; every Run recorded, nothing
//! overwritten. The core ([`engine`]) is a pure function over
//! documents: no clock, no I/O, no threads. Time is passed in.
//! Persistence ([`store`]), Performers ([`performer`]) and the loop that
//! joins them ([`driver`]) sit at the edge.

pub mod contracts;
pub mod definition;
pub mod driver;
pub mod engine;
pub mod history;
pub mod library;
pub mod performer;
pub mod store;
pub mod workflow;

pub use definition::{Binding, Guard, Policy, StepDefinition, WorkflowDefinition};
pub use library::Library;
pub use workflow::{Actor, Run, Step, Workflow};

/// An opaque Value: the engine never looks inside one.
pub type Value = serde_json::Value;

/// Why something was not done. Each is a different thing to say to a
/// person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A definition that does not hold together: a Binding to a step
    /// that is not there, tags that differ, a cycle.
    Invalid(String),
    /// A Command the engine understood and will not do: a Step that is
    /// running, a Value for an Input that is bound, an unknown name.
    Refused(String),
    /// A document this build cannot read.
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Invalid(e) | Error::Refused(e) | Error::Protocol(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for Error {}
