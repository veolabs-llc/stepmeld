//! The performers file: how a machine says which programs it runs for
//! which verbs, read by whatever drives the engine there (the
//! `stepmeld` command, a daemon). One Performer, or several
//! (`{"performers": [...]}`), each with a name, a locality (what it
//! answers to "where": `this-machine` unless its programs are launchers
//! for work elsewhere), where its Runs keep their files, and a program
//! per verb. When no placement names a Performer, the first in the
//! file that offers the verb is the one chosen.

use crate::{LocalPerformer, Program};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use stepmeld_core::definition::Locality;
use stepmeld_core::Error;

#[derive(Deserialize)]
#[serde(untagged)]
enum PerformersFile {
    Several { performers: Vec<PerformerEntry> },
    One(PerformerEntry),
}

/// One Performer as the file gives it.
#[derive(Deserialize)]
pub struct PerformerEntry {
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub locality: Option<Locality>,
    /// Where Runs keep their files.
    pub root: PathBuf,
    pub programs: Vec<Program>,
}

fn default_name() -> String {
    "local".into()
}

impl PerformerEntry {
    pub fn performer(self) -> LocalPerformer {
        LocalPerformer::new(&self.name, &self.root, self.programs).at(self.locality.unwrap_or(Locality::ThisMachine))
    }
}

/// The file's Performers, in its order. A missing or malformed file
/// is a refusal that names it.
pub fn load(path: &Path) -> Result<Vec<LocalPerformer>, Error> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::Refused(format!("cannot read {}: {e}", path.display())))?;
    parse(&text).map_err(|e| Error::Refused(format!("{} is not a performers file: {e}", path.display())))
}

/// The text of a performers file, as [`load`] reads it.
pub fn parse(text: &str) -> Result<Vec<LocalPerformer>, serde_json::Error> {
    let entries = match serde_json::from_str::<PerformersFile>(text)? {
        PerformersFile::Several { performers } => performers,
        PerformersFile::One(one) => vec![one],
    };
    Ok(entries.into_iter().map(PerformerEntry::performer).collect())
}

/// What a performers file looks like, for a `performers` help command.
pub fn example() -> serde_json::Value {
    serde_json::json!({"name": "local", "root": "/path/for/runs", "programs": [{"verb": {"name": "detect-targets", "version": 1}, "command": ["python3", "/path/to/detect.py"]}]})
}

#[cfg(test)]
mod tests {
    use super::*;
    use stepmeld_core::performer::Performer;

    #[test]
    fn one_or_several_in_order_with_their_localities() {
        let one = parse(&example().to_string()).unwrap();
        assert_eq!(one.len(), 1);
        let face = one[0].describe();
        assert_eq!((face.name.as_str(), face.locality, face.verbs.len()), ("local", Locality::ThisMachine, 1));

        let several = parse(
            r#"{"performers": [
                {"name": "box", "root": "/runs", "programs": [{"verb": {"name": "solve", "version": 1}, "command": ["groundtruth", "step"]}]},
                {"name": "batch", "locality": "cloud", "root": "/runs", "programs": [{"verb": {"name": "dense", "version": 1}, "command": ["groundtruth", "--executor", "batch", "step"]}]}
            ]}"#,
        )
        .unwrap();
        let faces: Vec<(String, Locality)> = several.iter().map(|p| (p.describe().name, p.describe().locality)).collect();
        assert_eq!(faces, vec![("box".to_string(), Locality::ThisMachine), ("batch".to_string(), Locality::Cloud)]);

        assert!(parse(r#"{"programs": "nope"}"#).is_err());
        let missing = match load(Path::new("/nowhere/performers.json")) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a missing file loaded"),
        };
        assert!(missing.starts_with("cannot read /nowhere/performers.json"), "{missing}");
    }
}
