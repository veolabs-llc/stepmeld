//! The contracts, compiled in: `schemas/*.schema.json`, a copy of the
//! repository's `contracts/schemas` (a published crate carries only its
//! own directory), held equal by test.
//! [`validate`] holds a document to one by name.

use jsonschema::Validator;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

pub const SCHEMAS: [(&str, &str); 10] = [
    ("stepmeld/step-definition.v1", include_str!("../schemas/step-definition.v1.schema.json")),
    ("stepmeld/workflow-definition.v1", include_str!("../schemas/workflow-definition.v1.schema.json")),
    ("stepmeld/workflow.v1", include_str!("../schemas/workflow.v1.schema.json")),
    ("stepmeld/history.v1", include_str!("../schemas/history.v1.schema.json")),
    // the run directory protocol (contracts/run-directory.v1.md)
    ("stepmeld/run-request.v1", include_str!("../schemas/run-request.v1.schema.json")),
    ("stepmeld/run-progress.v1", include_str!("../schemas/run-progress.v1.schema.json")),
    ("stepmeld/run-outcome.v1", include_str!("../schemas/run-outcome.v1.schema.json")),
    ("stepmeld/run-cancel.v1", include_str!("../schemas/run-cancel.v1.schema.json")),
    // Orchestrators and Workers (docs/ORCHESTRATOR-WORKER.md)
    ("stepmeld/order.v1", include_str!("../schemas/order.v1.schema.json")),
    ("stepmeld/worker.v1", include_str!("../schemas/worker.v1.schema.json")),
];

fn compiled() -> &'static HashMap<&'static str, Result<Validator, String>> {
    static TABLE: OnceLock<HashMap<&'static str, Result<Validator, String>>> = OnceLock::new();
    TABLE.get_or_init(|| {
        SCHEMAS
            .iter()
            .map(|(name, text)| (*name, serde_json::from_str::<Value>(text).map_err(|e| e.to_string()).and_then(|s| jsonschema::validator_for(&s).map_err(|e| e.to_string()))))
            .collect()
    })
}

pub fn names() -> impl Iterator<Item = &'static str> {
    SCHEMAS.iter().map(|(n, _)| *n)
}

/// Where `doc` departs from the contract, or that there is no such
/// contract.
pub fn validate(doc: &Value, name: &str) -> Result<(), String> {
    match compiled().get(name) {
        None => Err(format!("no contract {name:?}; there are {:?}", names().collect::<Vec<_>>())),
        Some(Err(e)) => Err(format!("contract {name:?} does not compile: {e}")),
        Some(Ok(v)) => {
            let errors: Vec<String> = v.iter_errors(doc).map(|e| format!("{} at {}", e, e.instance_path())).collect();
            if errors.is_empty() {
                Ok(())
            } else {
                Err(errors.join("; "))
            }
        }
    }
}

/// The schemas that do not compile.
pub fn broken() -> Vec<String> {
    compiled().iter().filter_map(|(n, r)| r.as_ref().err().map(|e| format!("{n}: {e}"))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../contracts/fixtures")
    }

    #[test]
    fn every_schema_compiles_and_is_the_directory() {
        assert_eq!(broken(), Vec::<String>::new());
        let mut on_disk: Vec<String> = std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../contracts/schemas"))
            .unwrap()
            .flatten()
            .filter_map(|e| Some(format!("stepmeld/{}", e.file_name().to_str()?.strip_suffix(".schema.json")?)))
            .collect();
        on_disk.sort();
        let mut compiled: Vec<String> = names().map(str::to_string).collect();
        compiled.sort();
        assert_eq!(on_disk, compiled, "a schema on disk is compiled in, and the reverse");
        for (name, text) in SCHEMAS {
            let file = format!("{}.schema.json", name.strip_prefix("stepmeld/").unwrap());
            let theirs = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../contracts/schemas").join(&file)).unwrap();
            assert_eq!(text, theirs, "stepmeld-core/schemas/{file} is a copy of contracts/schemas; copy again");
        }
    }

    /// A Run's outcome and progress are one shape in two places: inside
    /// a Workflow, and as files of the run directory.
    #[test]
    fn the_run_directory_files_are_the_workflows_shapes() {
        let schema = |name: &str| -> Value { serde_json::from_str(SCHEMAS.iter().find(|(n, _)| *n == name).unwrap().1).unwrap() };
        let body = |mut v: Value| {
            for k in ["$schema", "$id", "title", "$defs"] {
                v.as_object_mut().unwrap().remove(k);
            }
            v
        };
        let workflow = schema("stepmeld/workflow.v1");
        assert_eq!(body(schema("stepmeld/run-outcome.v1")), workflow["$defs"]["outcome"], "run-outcome.v1 is workflow.v1's outcome");
        assert_eq!(body(schema("stepmeld/run-progress.v1")), workflow["$defs"]["progress"], "run-progress.v1 is workflow.v1's progress");
        let order = schema("stepmeld/order.v1");
        assert_eq!(order["$defs"]["outcome"], workflow["$defs"]["outcome"], "an order's outcome is a Run's");
        assert_eq!(order["$defs"]["progress"], workflow["$defs"]["progress"], "an order's progress is a Run's");
        assert_eq!(order["$defs"]["request"], body(schema("stepmeld/run-request.v1")), "an order's request is the run directory's");
    }

    /// Every golden fixture validates against the contract its
    /// directory is named after; a `.refused.json` fixture does not.
    #[test]
    fn every_fixture_validates() {
        let mut seen = 0;
        for dir in std::fs::read_dir(fixtures()).unwrap().flatten() {
            let name = format!("stepmeld/{}", dir.file_name().to_str().unwrap());
            if name == "stepmeld/scenarios" {
                continue;
            }
            for file in std::fs::read_dir(dir.path()).unwrap().flatten() {
                let path = file.path();
                let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                let result = validate(&doc, &name);
                let should_fail = path.to_str().unwrap().ends_with(".refused.json");
                assert_eq!(result.is_err(), should_fail, "{}: {result:?}", path.display());
                seen += 1;
            }
        }
        assert!(seen >= 4, "there are fixtures to check ({seen})");
    }
}
