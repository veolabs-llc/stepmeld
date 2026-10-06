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

/// Seconds since the epoch as the documents spell a time:
/// `YYYY-MM-DDTHH:MM:SSZ`, whole seconds, UTC. The core has no clock
/// (time is passed in); this is the one spelling every driver writes,
/// so leases and History compare as strings.
pub fn iso(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod iso_tests {
    #[test]
    fn the_spelling_is_whole_seconds_utc_with_a_z() {
        assert_eq!(super::iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::iso(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(super::iso(951_782_400), "2000-02-29T00:00:00Z");
    }
}

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
