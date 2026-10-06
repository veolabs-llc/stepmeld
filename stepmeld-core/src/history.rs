//! The append-only record: who did what, when.

use crate::workflow::Actor;
use crate::Value;
use serde::{Deserialize, Serialize};

pub const HISTORY: &str = "stepmeld/history.v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub contract: String,
    /// Set by the StateStore when appended; 0 until then.
    #[serde(default)]
    pub seq: u64,
    pub at: String,
    pub by: Actor,
    pub workflow: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// `created`, `set`, `go`, `placed`, `confirmed`, `completed`,
    /// `run-minted`, `run-started`, `run-refused`, `progress`,
    /// `attention`, `run-ended`, `canceled`, `policy-set`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub detail: Value,
}

impl Entry {
    pub fn new(at: &str, by: &Actor, workflow: &str, kind: &str) -> Entry {
        Entry {
            contract: HISTORY.to_string(),
            seq: 0,
            at: at.to_string(),
            by: by.clone(),
            workflow: workflow.to_string(),
            step: None,
            run: None,
            kind: kind.to_string(),
            detail: Value::Null,
        }
    }

    pub fn step(mut self, step: &str) -> Entry {
        self.step = Some(step.to_string());
        self
    }

    pub fn run(mut self, run: &str) -> Entry {
        self.run = Some(run.to_string());
        self
    }

    pub fn detail(mut self, detail: Value) -> Entry {
        self.detail = detail;
        self
    }
}
