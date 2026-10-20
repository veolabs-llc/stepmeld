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
pub mod orders;
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

/// The seconds since the epoch a time spelled by [`iso`] stands for;
/// `None` for any other spelling. For the edge pieces that hold a lease
/// for so many seconds from now.
pub fn secs(iso: &str) -> Option<u64> {
    let b = iso.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| iso.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

/// `seconds` after `now`, spelled as [`iso`] spells it.
pub fn later(now: &str, seconds: u64) -> Result<String, Error> {
    secs(now)
        .map(|s| iso(s + seconds))
        .ok_or_else(|| Error::Protocol(format!("{now:?} is not a time as stepmeld spells one (YYYY-MM-DDTHH:MM:SSZ)")))
}

#[cfg(test)]
mod iso_tests {
    #[test]
    fn the_spelling_is_whole_seconds_utc_with_a_z() {
        assert_eq!(super::iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::iso(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(super::iso(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn a_spelled_time_reads_back() {
        for s in [0, 1_790_000_000, 951_782_400, 1_792_000_000] {
            assert_eq!(super::secs(&super::iso(s)), Some(s));
        }
        assert_eq!(super::secs("2026-10-17T00:01:00+00:00"), None, "only the one spelling");
        assert_eq!(super::later("2026-10-17T00:59:30Z", 45).unwrap(), "2026-10-17T01:00:15Z");
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
