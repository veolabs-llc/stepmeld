//! A deployment and a machine, as files (docs/PLACEMENT.md §8).
//!
//! - The **deployment file** is the same on every machine of a
//!   deployment: which store is shared, each program's settings for
//!   each kind of Performer that may run it (an image, a cloud job), the
//!   secrets by name, and a provider's settings.
//! - The **machine file** belongs to one machine: its Worker's name, where
//!   Runs keep their files, the command for each program it runs as a
//!   command, the capabilities it adds, its slots, whether it is a cloud
//!   Worker.
//! - [`Kinds`] is what a deployment may name: kinds of store, of secret
//!   and of Performer. stepmeld registers its own (`sqlite`; `env`,
//!   `file`; `command`); a provider's crate registers more.
//!
//! ```toml
//! # stepmeld.toml
//! store = "shared"
//!
//! [stores.shared]
//! kind = "sqlite"
//! path = "stepmeld.sqlite"        # beside this file
//!
//! [programs.typeset-program]
//! secrets = ["FONT_TOKEN"]        # handed to the program by name
//!
//! [secrets.FONT_TOKEN]
//! kind = "env"                    # read from this variable where the Worker runs
//! ```
//!
//! ```toml
//! # stepmeld-machine.toml
//! [worker]
//! name = "bench"
//! capabilities = ["fonts-full"]
//!
//! [programs]
//! typeset-program = ["/opt/typeset/bin/typeset"]
//! ```

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use stepmeld_core::definition::Locality;
use stepmeld_core::orders::{OrderBook, Worker};
use stepmeld_core::performer::{Performer, Router};
use stepmeld_core::store::SharedStore;
use stepmeld_core::workflow::PerformerFace;
use stepmeld_core::Error;
use stepmeld_local::{LocalPerformer, Program};
use toml::{Table, Value};

/// The variable naming the deployment file, and the machine file.
pub const CONFIG_ENV: &str = "STEPMELD_CONFIG";
pub const MACHINE_ENV: &str = "STEPMELD_MACHINE";
/// Their names, in the current folder or in the user's configuration
/// folder.
pub const DEPLOYMENT_FILE: &str = "stepmeld.toml";
pub const MACHINE_FILE: &str = "stepmeld-machine.toml";

/// The user's configuration folder for stepmeld: macOS
/// `~/Library/Application Support/stepmeld`, Windows
/// `%APPDATA%\stepmeld`, elsewhere `$XDG_CONFIG_HOME/stepmeld` or
/// `~/.config/stepmeld`.
pub fn config_dir() -> Option<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        var("HOME").map(|h| h.join("Library/Application Support"))
    } else if cfg!(windows) {
        var("APPDATA")
    } else {
        var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|h| h.join(".config")))
    };
    base.map(|b| b.join("stepmeld"))
}

/// Which file: the one named, else the variable's, else the one in the
/// current folder, else the one in the user's configuration folder;
/// `None` when there is none of those.
fn find_file(explicit: Option<&Path>, env: &str, file: &str, in_config: &str) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    if let Some(p) = std::env::var_os(env).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let here = PathBuf::from(file);
    if here.exists() {
        return Some(here);
    }
    config_dir().map(|d| d.join(in_config)).filter(|p| p.exists())
}

fn invalid(path: &Option<PathBuf>, msg: String) -> Error {
    match path {
        Some(p) => Error::Invalid(format!("{}: {msg}", p.display())),
        None => Error::Invalid(msg),
    }
}

fn read(path: &Path) -> Result<String, Error> {
    std::fs::read_to_string(path).map_err(|e| Error::Refused(format!("cannot read {}: {e}", path.display())))
}

/// A deployment's file, or the one stepmeld assumes without one: a
/// SQLite store, `stepmeld.sqlite`, in the current folder.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Deployment {
    /// Where it was read from; none for the assumed one.
    pub path: Option<PathBuf>,
    pub table: Table,
}

impl Deployment {
    /// The file named, else `STEPMELD_CONFIG`'s, else `stepmeld.toml`
    /// in the current folder, else in the user's configuration folder,
    /// else the assumed one.
    pub fn find(explicit: Option<&Path>) -> Result<Deployment, Error> {
        match find_file(explicit, CONFIG_ENV, DEPLOYMENT_FILE, DEPLOYMENT_FILE) {
            Some(p) => Deployment::read(&p),
            None => Ok(Deployment::default()),
        }
    }

    pub fn read(path: &Path) -> Result<Deployment, Error> {
        Deployment::parse(&read(path)?, Some(path.to_path_buf()))
    }

    pub fn parse(text: &str, path: Option<PathBuf>) -> Result<Deployment, Error> {
        let table: Table = text.parse().map_err(|e: toml::de::Error| invalid(&path, e.message().to_string()))?;
        Ok(Deployment { path, table })
    }

    /// The folder relative paths in the file are read against: the
    /// file's own, else the current folder.
    pub fn dir(&self) -> PathBuf {
        self.path.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf).unwrap_or_default()
    }

    /// A path from the file, read against [`Self::dir`].
    pub fn resolve(&self, p: &str) -> PathBuf {
        let p = Path::new(p);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.dir().join(p)
        }
    }

    fn section(&self, name: &str) -> Option<&Table> {
        self.table.get(name).and_then(Value::as_table)
    }

    /// The store the deployment shares, with its `kind`: the one `store`
    /// names, else the only one in `[stores]`, else SQLite in
    /// `stepmeld.sqlite`.
    pub fn store(&self) -> Result<Table, Error> {
        let stores = self.section("stores");
        let named = self
            .table
            .get("store")
            .map(|v| v.as_str().map(str::to_string).ok_or_else(|| invalid(&self.path, "`store` names a [stores] table".into())))
            .transpose()?;
        let chosen = match (named, stores) {
            (Some(n), Some(s)) => s.get(&n).and_then(Value::as_table).ok_or_else(|| invalid(&self.path, format!("`store` names {n:?}, which [stores] does not have")))?.clone(),
            (Some(n), None) => return Err(invalid(&self.path, format!("`store` names {n:?}, and there is no [stores] table"))),
            (None, Some(s)) if s.len() == 1 => s.values().next().and_then(Value::as_table).cloned().ok_or_else(|| invalid(&self.path, "[stores] holds tables".into()))?,
            (None, Some(s)) if !s.is_empty() => return Err(invalid(&self.path, "several [stores]: say which with `store = \"…\"`".into())),
            _ => Table::from_iter([("kind".to_string(), Value::from("sqlite")), ("path".to_string(), Value::from("stepmeld.sqlite"))]),
        };
        if !chosen.get("kind").is_some_and(Value::is_str) {
            return Err(invalid(&self.path, "the store has no `kind`".into()));
        }
        Ok(chosen)
    }

    /// A program's settings: `[programs.<name>]`.
    pub fn program(&self, name: &str) -> Option<&Table> {
        self.section("programs")?.get(name)?.as_table()
    }

    /// The programs it has settings for.
    pub fn programs(&self) -> Vec<String> {
        self.section("programs").map(|t| t.keys().cloned().collect()).unwrap_or_default()
    }

    /// A secret's settings: `[secrets.<name>]`.
    pub fn secret(&self, name: &str) -> Option<&Table> {
        self.section("secrets")?.get(name)?.as_table()
    }

    /// A provider's settings: `[providers.<name>]`, for a provider's
    /// crate to read (a region, say).
    pub fn provider(&self, name: &str) -> Option<&Table> {
        self.section("providers")?.get(name)?.as_table()
    }
}

fn is_false(b: &bool) -> bool {
    !b
}

/// A machine file's contents.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MachineFile {
    #[serde(default)]
    pub worker: WorkerSection,
    /// The command for each program this machine runs as a command;
    /// the Run's folder is appended.
    #[serde(default)]
    pub programs: BTreeMap<String, Vec<String>>,
}

/// `[worker]`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WorkerSection {
    /// Its name in the shared store; the host name by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Where its Runs keep their files, beside this file; `runs` by
    /// default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<String>,
    /// It launches its work in the cloud.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cloud: bool,
    /// How many orders it runs at once: one by default, no limit for a
    /// cloud Worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slots: Option<u32>,
    /// Words it offers beyond those it detects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// One machine's file: read where [`Machine::find`] finds it, written
/// back by [`Machine::save`].
#[derive(Debug, Clone, PartialEq)]
pub struct Machine {
    pub path: PathBuf,
    /// Whether the file exists yet.
    pub exists: bool,
    pub file: MachineFile,
}

impl Machine {
    /// The file named, else `STEPMELD_MACHINE`'s, else
    /// `stepmeld-machine.toml` in the current folder, else `machine.toml`
    /// in the user's configuration folder; else a new one in the current
    /// folder, written on the first [`Self::save`].
    pub fn find(explicit: Option<&Path>) -> Result<Machine, Error> {
        let path = find_file(explicit, MACHINE_ENV, MACHINE_FILE, "machine.toml").unwrap_or_else(|| PathBuf::from(MACHINE_FILE));
        Machine::read(&path)
    }

    /// The file at `path`, or an empty one there when it does not exist.
    pub fn read(path: &Path) -> Result<Machine, Error> {
        if !path.exists() {
            return Ok(Machine {
                path: path.to_path_buf(),
                exists: false,
                file: MachineFile::default(),
            });
        }
        let file = toml::from_str(&read(path)?).map_err(|e: toml::de::Error| invalid(&Some(path.to_path_buf()), e.message().to_string()))?;
        Ok(Machine {
            path: path.to_path_buf(),
            exists: true,
            file,
        })
    }

    /// Write the file; whether this made it.
    pub fn save(&mut self) -> Result<bool, Error> {
        let text = toml::to_string_pretty(&self.file).map_err(|e| Error::Invalid(e.to_string()))?;
        std::fs::write(&self.path, text).map_err(|e| Error::Refused(format!("cannot write {}: {e}", self.path.display())))?;
        let made = !self.exists;
        self.exists = true;
        Ok(made)
    }

    /// Its Worker's name: the file's, else `host`.
    pub fn name(&self, host: &str) -> String {
        self.file.worker.name.clone().unwrap_or_else(|| host.to_string())
    }

    /// Where its Runs keep their files, read against the file's folder.
    pub fn runs(&self) -> PathBuf {
        let runs = self.file.worker.runs.as_deref().unwrap_or("runs");
        let dir = self.path.parent().map(Path::to_path_buf).unwrap_or_default();
        if Path::new(runs).is_absolute() {
            PathBuf::from(runs)
        } else {
            dir.join(runs)
        }
    }

    /// How many orders it runs at once.
    pub fn slots(&self) -> u32 {
        self.file.worker.slots.unwrap_or(if self.file.worker.cloud { u32::MAX } else { 1 })
    }
}

/// Opens a kind of store from its settings in the deployment file.
pub type StoreKind = fn(&Deployment, &Table) -> Result<Arc<dyn SharedStore>, Error>;
/// Reads a kind of secret's value from its settings.
pub type SecretKind = fn(&Deployment, &Table) -> Result<String, Error>;

/// A program routed to a kind of Performer, with what it needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgramPlan {
    pub name: String,
    /// Its settings for this kind: `[programs.<name>.<kind>]` from the
    /// deployment file, or `{ command = [...] }` from the machine file.
    pub settings: Table,
    /// The secrets its settings list, read: name to value.
    pub env: BTreeMap<String, String>,
}

/// The Worker a kind of Performer is built for.
pub struct WorkerPlan<'a> {
    pub name: String,
    pub runs: PathBuf,
    pub deployment: &'a Deployment,
    pub machine: &'a Machine,
}

/// Builds a kind of Performer for a Worker, from the programs routed to it.
pub type MakePerformer = fn(&WorkerPlan, &[ProgramPlan]) -> Result<Arc<dyn Performer>, Error>;

/// A kind of Performer a Worker may run programs with. `cloud` kinds
/// run a cloud Worker's programs; the others a machine's own.
#[derive(Clone, Copy)]
pub struct PerformerKind {
    pub cloud: bool,
    pub make: MakePerformer,
}

/// What a deployment may name.
#[derive(Clone)]
pub struct Kinds {
    stores: BTreeMap<String, StoreKind>,
    secrets: BTreeMap<String, SecretKind>,
    performers: BTreeMap<String, PerformerKind>,
}

impl Default for Kinds {
    fn default() -> Kinds {
        Kinds::builtin()
    }
}

/// The one Performer kind every machine has: a program's command, from
/// the machine file, through the run directory.
pub const COMMAND: &str = "command";

impl Kinds {
    /// stepmeld's own: the `sqlite` store; `env` and `file` secrets; the
    /// `command` Performer.
    pub fn builtin() -> Kinds {
        Kinds {
            stores: BTreeMap::new(),
            secrets: BTreeMap::new(),
            performers: BTreeMap::new(),
        }
        .with_store("sqlite", sqlite_store)
        .with_secret("env", env_secret)
        .with_secret("file", file_secret)
        .with_performer(COMMAND, PerformerKind { cloud: false, make: command_performer })
    }

    pub fn with_store(mut self, kind: &str, open: StoreKind) -> Kinds {
        self.stores.insert(kind.into(), open);
        self
    }

    pub fn with_secret(mut self, kind: &str, read: SecretKind) -> Kinds {
        self.secrets.insert(kind.into(), read);
        self
    }

    pub fn with_performer(mut self, kind: &str, performer: PerformerKind) -> Kinds {
        self.performers.insert(kind.into(), performer);
        self
    }

    /// The deployment's shared store.
    pub fn open_store(&self, d: &Deployment) -> Result<Arc<dyn SharedStore>, Error> {
        let settings = d.store()?;
        let kind = settings["kind"].as_str().unwrap_or_default();
        let open = self
            .stores
            .get(kind)
            .ok_or_else(|| invalid(&d.path, format!("no store kind {kind:?} in this build (it has {:?})", self.stores.keys().collect::<Vec<_>>())))?;
        open(d, &settings)
    }

    /// A secret's value, by its name in the deployment file.
    pub fn secret(&self, d: &Deployment, name: &str) -> Result<String, Error> {
        let settings = d.secret(name).ok_or_else(|| invalid(&d.path, format!("no [secrets.{name}]")))?;
        let kind = settings.get("kind").and_then(Value::as_str).ok_or_else(|| invalid(&d.path, format!("[secrets.{name}] has no `kind`")))?;
        let read = self.secrets.get(kind).ok_or_else(|| invalid(&d.path, format!("no secret kind {kind:?} in this build")))?;
        let mut settings = settings.clone();
        settings.entry("name").or_insert_with(|| Value::from(name));
        read(d, &settings)
    }

    /// How each program the machine or the deployment knows would run on
    /// this machine's Worker: by its command in the machine file (for a
    /// cloud Worker, a launcher), else by a kind the program has
    /// settings for: a cloud kind for a cloud Worker, another kind for
    /// any other. A program with no way to run is left out.
    pub fn routes(&self, d: &Deployment, m: &Machine) -> Result<BTreeMap<String, Vec<ProgramPlan>>, Error> {
        let mut names: Vec<String> = m.file.programs.keys().cloned().collect();
        names.extend(d.programs());
        names.sort();
        names.dedup();
        let mut routes: BTreeMap<String, Vec<ProgramPlan>> = BTreeMap::new();
        for name in names {
            let common = d.program(&name);
            let cloud = m.file.worker.cloud;
            let route = match m.file.programs.get(&name) {
                Some(command) => Some((COMMAND.to_string(), Table::from_iter([("command".to_string(), Value::from(command.clone()))]))),
                None => self
                    .performers
                    .iter()
                    .filter(|(kind, k)| k.cloud == cloud && *kind != COMMAND)
                    .find_map(|(kind, _)| common.and_then(|c| c.get(kind)).and_then(Value::as_table).map(|t| (kind.clone(), t.clone()))),
            };
            let Some((kind, settings)) = route else { continue };
            let mut env = BTreeMap::new();
            for secret in common.and_then(|c| c.get("secrets")).and_then(Value::as_array).into_iter().flatten() {
                let s = secret.as_str().ok_or_else(|| invalid(&d.path, format!("[programs.{name}] secrets are names")))?;
                env.insert(s.to_string(), self.secret(d, s)?);
            }
            routes.entry(kind).or_default().push(ProgramPlan { name, settings, env });
        }
        Ok(routes)
    }

    /// This machine's Worker, over the shared store `book`, on `host`.
    pub fn worker(&self, d: &Deployment, m: &Machine, book: Arc<dyn OrderBook>, host: &str) -> Result<Worker, Error> {
        let plan = WorkerPlan {
            name: m.name(host),
            runs: m.runs(),
            deployment: d,
            machine: m,
        };
        let routes = self.routes(d, m)?;
        if routes.is_empty() {
            return Err(Error::Refused(format!(
                "Worker {} has no programs it can run: `stepmeld worker program set <program> -- <command...>`{}",
                plan.name,
                if m.file.worker.cloud { ", or a cloud kind's settings for the program in the deployment file" } else { "" }
            )));
        }
        let mut built: Vec<(String, Vec<String>, Arc<dyn Performer>)> = Vec::new();
        for (kind, programs) in &routes {
            let make = self.performers[kind].make;
            built.push((kind.clone(), programs.iter().map(|p| p.name.clone()).collect(), make(&plan, programs)?));
        }
        let programs: Vec<String> = built.iter().flat_map(|r| r.1.clone()).collect();
        let performer: Arc<dyn Performer> = if built.len() == 1 {
            built.remove(0).2
        } else {
            let face = PerformerFace {
                name: plan.name.clone(),
                locality: if m.file.worker.cloud { Locality::Cloud } else { Locality::ThisMachine },
                verbs: Vec::new(),
                pool: false,
            };
            Arc::new(Router::new(face, built))
        };
        Ok(Worker::new(&plan.name, host, m.file.worker.cloud, programs, book, performer)
            .with_slots(m.slots())
            .with_capabilities(m.file.worker.capabilities.clone()))
    }
}

fn sqlite_store(d: &Deployment, settings: &Table) -> Result<Arc<dyn SharedStore>, Error> {
    let path = d.resolve(settings.get("path").and_then(Value::as_str).unwrap_or("stepmeld.sqlite"));
    Ok(Arc::new(stepmeld_sqlite::SqliteStore::open(&path)?))
}

/// A variable where the Worker runs: `var`, else the secret's name.
fn env_secret(d: &Deployment, settings: &Table) -> Result<String, Error> {
    let var = settings.get("var").or_else(|| settings.get("name")).and_then(Value::as_str).unwrap_or_default();
    std::env::var(var).map_err(|_| invalid(&d.path, format!("the secret's variable {var} is not set where this Worker runs")))
}

/// A file's contents, without a trailing newline: `path`, read against
/// the deployment file's folder.
fn file_secret(d: &Deployment, settings: &Table) -> Result<String, Error> {
    let path = d.resolve(settings.get("path").and_then(Value::as_str).ok_or_else(|| invalid(&d.path, "a file secret has a `path`".into()))?);
    Ok(read(&path)?.trim_end_matches(['\n', '\r']).to_string())
}

fn command_performer(w: &WorkerPlan, programs: &[ProgramPlan]) -> Result<Arc<dyn Performer>, Error> {
    let programs = programs
        .iter()
        .map(|p| {
            let command: Vec<String> = p
                .settings
                .get("command")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            Program {
                env: p.env.clone(),
                ..Program::new(&p.name, command)
            }
        })
        .collect();
    Ok(Arc::new(LocalPerformer::new(&w.name, &w.runs, programs)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_file_the_store_is_sqlite_here() {
        let d = Deployment::default();
        let s = d.store().unwrap();
        assert_eq!((s["kind"].as_str(), s["path"].as_str()), (Some("sqlite"), Some("stepmeld.sqlite")));
        assert_eq!(d.resolve("stepmeld.sqlite"), PathBuf::from("stepmeld.sqlite"));
    }

    #[test]
    fn the_store_is_the_one_named_and_paths_are_read_beside_the_file() {
        let d = Deployment::parse(
            "store = \"b\"\n[stores.a]\nkind = \"sqlite\"\npath = \"a.sqlite\"\n[stores.b]\nkind = \"sqlite\"\npath = \"../shared.sqlite\"\n",
            Some(PathBuf::from("/x/bench/stepmeld.toml")),
        )
        .unwrap();
        assert_eq!(d.store().unwrap()["path"].as_str(), Some("../shared.sqlite"));
        assert_eq!(d.resolve("../shared.sqlite"), PathBuf::from("/x/bench/../shared.sqlite"));
        let several = Deployment::parse("[stores.a]\nkind = \"sqlite\"\n[stores.b]\nkind = \"sqlite\"\n", None).unwrap();
        assert!(several.store().is_err(), "several stores and none named");
        let missing = Deployment::parse("store = \"c\"\n[stores.a]\nkind = \"sqlite\"\n", None).unwrap();
        assert!(missing.store().unwrap_err().to_string().contains("\"c\""));
    }

    #[test]
    fn a_machine_file_reads_back_as_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stepmeld-machine.toml");
        let mut m = Machine::read(&path).unwrap();
        assert!(!m.exists);
        assert_eq!((m.name("host-1"), m.slots()), ("host-1".to_string(), 1));
        m.file.worker.name = Some("bench".into());
        m.file.worker.capabilities = vec!["cuda".into()];
        m.file.programs.insert("p".into(), vec!["/bin/p".into(), "--fast".into()]);
        assert!(m.save().unwrap(), "made");
        assert!(!m.save().unwrap(), "written again");
        let back = Machine::read(&path).unwrap();
        assert_eq!(back.file, m.file);
        assert_eq!(back.runs(), dir.path().join("runs"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[worker]") && text.contains("name = \"bench\""), "{text}");
    }

    #[test]
    fn secrets_are_read_by_kind_and_handed_to_their_programs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("token"), "s3cret\n").unwrap();
        let d = Deployment::parse(
            "[programs.p]\nsecrets = [\"TOKEN\", \"PATH_SEEN\"]\n[secrets.TOKEN]\nkind = \"file\"\npath = \"token\"\n[secrets.PATH_SEEN]\nkind = \"env\"\nvar = \"PATH\"\n",
            Some(dir.path().join("stepmeld.toml")),
        )
        .unwrap();
        let kinds = Kinds::builtin();
        assert_eq!(kinds.secret(&d, "TOKEN").unwrap(), "s3cret");
        let mut m = Machine::read(&dir.path().join("m.toml")).unwrap();
        m.file.programs.insert("p".into(), vec!["/bin/p".into()]);
        let routes = kinds.routes(&d, &m).unwrap();
        let plan = &routes[COMMAND][0];
        assert_eq!((plan.env["TOKEN"].as_str(), plan.env["PATH_SEEN"].clone()), ("s3cret", std::env::var("PATH").unwrap()));
        assert!(kinds.secret(&d, "NOPE").is_err());
    }

    /// A Worker runs a program by its command, else by a kind of its
    /// own sort: cloud kinds for a cloud Worker, the others otherwise.
    #[test]
    fn programs_are_routed_by_the_kind_of_worker() {
        fn fake(_: &WorkerPlan, _: &[ProgramPlan]) -> Result<Arc<dyn Performer>, Error> {
            Err(Error::Refused("not built in this test".into()))
        }
        let kinds = Kinds::builtin()
            .with_performer("image", PerformerKind { cloud: false, make: fake })
            .with_performer("queue", PerformerKind { cloud: true, make: fake });
        let d = Deployment::parse("[programs.a.image]\nref = \"a:1\"\n[programs.a.queue]\nname = \"q\"\n[programs.b.image]\nref = \"b:1\"\n", None).unwrap();
        let mut m = Machine::read(Path::new("/nonexistent/m.toml")).unwrap();
        m.file.programs.insert("b".into(), vec!["/bin/b".into()]);
        let names = |r: &BTreeMap<String, Vec<ProgramPlan>>| r.iter().map(|(k, v)| (k.clone(), v.iter().map(|p| p.name.clone()).collect::<Vec<_>>())).collect::<Vec<_>>();
        assert_eq!(names(&kinds.routes(&d, &m).unwrap()), [("command".to_string(), vec!["b".to_string()]), ("image".to_string(), vec!["a".to_string()])]);
        m.file.worker.cloud = true;
        assert_eq!(
            names(&kinds.routes(&d, &m).unwrap()),
            [("command".to_string(), vec!["b".to_string()]), ("queue".to_string(), vec!["a".to_string()])],
            "a cloud Worker: its launcher command, else the cloud kind, never a machine kind"
        );
        assert_eq!(m.slots(), u32::MAX);
    }
}
