//! A [`StateStore`] in one SQLite file: definitions, workflows with a
//! version per write, the history with a sequence per workflow, and
//! leases. Every conditional write is one transaction.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;
use stepmeld_core::definition::{StepDefinition, WorkflowDefinition};
use stepmeld_core::history::Entry;
use stepmeld_core::library::Library;
use stepmeld_core::store::StateStore;
use stepmeld_core::workflow::Workflow;
use stepmeld_core::Error;

pub struct SqliteStore {
    conn: Mutex<Connection>,
    path: String,
}

fn db(e: rusqlite::Error) -> Error {
    Error::Protocol(format!("sqlite: {e}"))
}

fn doc<T: serde::de::DeserializeOwned>(text: &str, what: &str) -> Result<T, Error> {
    serde_json::from_str(text).map_err(|e| Error::Protocol(format!("{what} in the store cannot be read: {e}")))
}

impl SqliteStore {
    /// Open or create the file; the tables are made when missing.
    pub fn open(path: &Path) -> Result<SqliteStore, Error> {
        let conn = Connection::open(path).map_err(db)?;
        // rusqlite waits five seconds for another writer's lock; the
        // store's own transactions begin IMMEDIATE (`write`) so that the
        // wait happens at BEGIN, before any read, and never ends in a
        // stale snapshot.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS definitions (kind TEXT NOT NULL, name TEXT NOT NULL, version INTEGER NOT NULL, doc TEXT NOT NULL, PRIMARY KEY (kind, name, version));
             CREATE TABLE IF NOT EXISTS workflows (id TEXT PRIMARY KEY, version INTEGER NOT NULL, doc TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS history (workflow TEXT NOT NULL, seq INTEGER NOT NULL, doc TEXT NOT NULL, PRIMARY KEY (workflow, seq));
             CREATE TABLE IF NOT EXISTS leases (workflow TEXT PRIMARY KEY, holder TEXT NOT NULL, until TEXT NOT NULL);",
        )
        .map_err(db)?;
        Ok(SqliteStore {
            conn: Mutex::new(conn),
            path: path.display().to_string(),
        })
    }

    pub fn in_memory() -> Result<SqliteStore, Error> {
        Self::open(Path::new(":memory:"))
    }
}

/// A transaction that will write, begun IMMEDIATE: it takes the write
/// lock first (waiting for another writer, rusqlite's five seconds)
/// and reads after. Begun DEFERRED, a transaction reads at once and
/// asks for the lock at its first write; when another process (a
/// daemon's tick beside a person's Command) committed in between, the
/// read is a stale snapshot and SQLite refuses the upgrade at once,
/// "database is locked", however long the busy handler would wait
/// (seen 2026-10-08: a cancel that collided with a tick).
fn write(conn: &mut Connection) -> Result<rusqlite::Transaction<'_>, Error> {
    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db)
}

impl StateStore for SqliteStore {
    fn describe(&self) -> String {
        format!("sqlite {}", self.path)
    }

    fn put_step(&self, def: &StepDefinition) -> Result<(), Error> {
        def.check()?;
        if self.library()?.workflow(&def.reference()).is_some() {
            return Err(Error::Invalid(format!("step definition {}: a workflow definition has that name and version; verbs share one namespace", def.reference())));
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO definitions (kind, name, version, doc) VALUES ('step', ?1, ?2, ?3)",
            params![def.name, def.version, serde_json::to_string(def).unwrap()],
        )
        .map_err(db)?;
        Ok(())
    }

    fn put_workflow_definition(&self, def: &WorkflowDefinition) -> Result<(), Error> {
        let mut lib = self.library()?;
        lib.add_workflow(def.clone())?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO definitions (kind, name, version, doc) VALUES ('workflow', ?1, ?2, ?3)",
            params![def.name, def.version, serde_json::to_string(def).unwrap()],
        )
        .map_err(db)?;
        Ok(())
    }

    fn library(&self) -> Result<Library, Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT kind, doc FROM definitions ORDER BY kind, name, version").map_err(db)?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(db)?;
        let mut steps = Vec::new();
        let mut workflows = Vec::new();
        for row in rows {
            let (kind, text) = row.map_err(db)?;
            match kind.as_str() {
                "step" => steps.push(doc::<StepDefinition>(&text, "a step definition")?),
                _ => workflows.push(doc::<WorkflowDefinition>(&text, "a workflow definition")?),
            }
        }
        Library::from_parts(steps, workflows)
    }

    fn get(&self, id: &str) -> Result<Option<(Workflow, String)>, Error> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(i64, String)> = conn.query_row("SELECT version, doc FROM workflows WHERE id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(db)?;
        match row {
            None => Ok(None),
            Some((version, text)) => Ok(Some((doc(&text, "a workflow")?, version.to_string()))),
        }
    }

    fn put(&self, wf: &Workflow, expected: Option<&str>) -> Result<String, Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = write(&mut conn)?;
        let held: Option<i64> = tx.query_row("SELECT version FROM workflows WHERE id = ?1", params![wf.id], |r| r.get(0)).optional().map_err(db)?;
        let next = match (held, expected) {
            (Some(_), None) => return Err(Error::Refused(format!("workflow {} already exists", wf.id))),
            (None, Some(_)) => return Err(Error::Refused(format!("workflow {} changed underneath: it is gone", wf.id))),
            (None, None) => 1,
            (Some(v), Some(e)) if v.to_string() == e => v + 1,
            (Some(_), Some(_)) => return Err(Error::Refused(format!("workflow {} changed underneath", wf.id))),
        };
        tx.execute("INSERT OR REPLACE INTO workflows (id, version, doc) VALUES (?1, ?2, ?3)", params![wf.id, next, serde_json::to_string(wf).unwrap()])
            .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(next.to_string())
    }

    fn list(&self) -> Result<Vec<String>, Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id FROM workflows ORDER BY id").map_err(db)?;
        let ids = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(db)?;
        ids.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    fn remove(&self, id: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = write(&mut conn)?;
        tx.execute("DELETE FROM history WHERE workflow = ?1", params![id]).map_err(db)?;
        tx.execute("DELETE FROM leases WHERE workflow = ?1", params![id]).map_err(db)?;
        tx.execute("DELETE FROM workflows WHERE id = ?1", params![id]).map_err(db)?;
        tx.commit().map_err(db)
    }

    fn append(&self, entries: &[Entry]) -> Result<(), Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = write(&mut conn)?;
        for e in entries {
            let last: i64 = tx.query_row("SELECT COALESCE(MAX(seq), 0) FROM history WHERE workflow = ?1", params![e.workflow], |r| r.get(0)).map_err(db)?;
            let mut e = e.clone();
            e.seq = last as u64 + 1;
            tx.execute("INSERT INTO history (workflow, seq, doc) VALUES (?1, ?2, ?3)", params![e.workflow, e.seq as i64, serde_json::to_string(&e).unwrap()])
                .map_err(db)?;
        }
        tx.commit().map_err(db)
    }

    fn history(&self, workflow: &str) -> Result<Vec<Entry>, Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT doc FROM history WHERE workflow = ?1 ORDER BY seq").map_err(db)?;
        let rows = stmt.query_map(params![workflow], |r| r.get::<_, String>(0)).map_err(db)?;
        rows.map(|r| r.map_err(db).and_then(|t| doc(&t, "a history entry"))).collect()
    }

    fn lease(&self, workflow: &str, holder: &str, now: &str, until: &str) -> Result<bool, Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = write(&mut conn)?;
        let held: Option<(String, String)> = tx.query_row("SELECT holder, until FROM leases WHERE workflow = ?1", params![workflow], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(db)?;
        let free = match &held {
            None => true,
            Some((h, _)) if h == holder => true,
            Some((_, u)) => u.as_str() < now,
        };
        if free {
            tx.execute("INSERT OR REPLACE INTO leases (workflow, holder, until) VALUES (?1, ?2, ?3)", params![workflow, holder, until]).map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(free)
    }

    fn release(&self, workflow: &str, holder: &str) -> Result<(), Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM leases WHERE workflow = ?1 AND holder = ?2", params![workflow, holder]).map_err(db)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sqlite_store_conforms() {
        stepmeld_core::store::conformance::run(&SqliteStore::in_memory().unwrap());
    }

    /// Two processes write one file: a lease taken while another
    /// connection holds the write lock and commits a change meanwhile
    /// waits and lands, rather than being refused with a stale snapshot.
    #[test]
    fn a_write_waits_for_another_writer_and_is_not_refused_on_a_stale_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite");
        let store = SqliteStore::open(&path).unwrap();
        store.lease("wf", "daemon", "2026-10-08T10:00:00Z", "2026-10-08T10:01:00Z").unwrap();
        // the other process: the lock held, a change made, committed later
        let other = Connection::open(&path).unwrap();
        other.execute_batch("BEGIN IMMEDIATE; UPDATE leases SET until = '2026-10-08T10:02:00Z' WHERE workflow = 'wf';").unwrap();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            other.execute_batch("COMMIT").unwrap();
        });
        let t0 = std::time::Instant::now();
        let taken = store.lease("wf", "daemon", "2026-10-08T10:00:30Z", "2026-10-08T10:03:00Z").unwrap();
        assert!(taken && t0.elapsed() >= std::time::Duration::from_millis(200), "waited for the other writer");
        holder.join().unwrap();
    }

    #[test]
    fn a_file_store_keeps_what_was_put_across_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite");
        let mut def = StepDefinition::new("verb", 1);
        def.label = Some("A verb".into());
        {
            let store = SqliteStore::open(&path).unwrap();
            store.put_step(&def).unwrap();
            stepmeld_core::store::conformance::run(&store);
        }
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.library().unwrap().step(&def.reference()).unwrap().label.as_deref(), Some("A verb"));
        // the suite removes its Workflow at the end and puts it again: no history
        assert!(store.get("conformance-wf").unwrap().is_some());
        assert!(store.history("conformance-wf").unwrap().is_empty(), "removed and put again, with no history");
    }
}
