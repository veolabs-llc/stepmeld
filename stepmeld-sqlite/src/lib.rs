//! A shared store in one SQLite file (docs/ORCHESTRATOR-WORKER.md):
//! [`SqliteStore`] is a [`StateStore`] (definitions, workflows with a
//! version per write, the history with a sequence per workflow, leases)
//! and an [`OrderBook`] (orders, their claims and logs, and the Workers
//! that published themselves). Every conditional write is one
//! transaction. A Worker's own settings are its machine file
//! (`stepmeld-config`), since 2026-10-20.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;
use stepmeld_core::definition::{Ref, StepDefinition, WorkflowDefinition};
use stepmeld_core::history::Entry;
use stepmeld_core::library::Library;
use stepmeld_core::orders::{CancelAsk, Claim, Order, OrderBook, Report, WorkerFace};
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
             CREATE TABLE IF NOT EXISTS leases (workflow TEXT PRIMARY KEY, holder TEXT NOT NULL, until TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS workers (name TEXT PRIMARY KEY, doc TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS orders (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, worker TEXT NOT NULL, doc TEXT NOT NULL, cancel TEXT, claim TEXT, report TEXT, log TEXT NOT NULL DEFAULT '', ended INTEGER NOT NULL DEFAULT 0);",
        )
        .map_err(db)?;
        // a store made before claims (2026-10-20) gains the column
        let has_claim: bool = conn.prepare("SELECT 1 FROM pragma_table_info('orders') WHERE name = 'claim'").and_then(|mut s| s.exists([])).map_err(db)?;
        if !has_claim {
            conn.execute_batch("ALTER TABLE orders ADD COLUMN claim TEXT;").map_err(db)?;
        }
        // and before logs were kept beside the order (the same day)
        let has_log: bool = conn.prepare("SELECT 1 FROM pragma_table_info('orders') WHERE name = 'log'").and_then(|mut s| s.exists([])).map_err(db)?;
        if !has_log {
            conn.execute_batch("ALTER TABLE orders ADD COLUMN log TEXT NOT NULL DEFAULT '';").map_err(db)?;
        }
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
    /// Every StepDefinition and WorkflowDefinition stored, read but not
    /// checked: what a person needs to see, and clean up, in a store
    /// whose definitions no longer hold together (one written before a
    /// rule changed, say), where [`StateStore::library`] refuses.
    fn definitions(&self) -> Result<(Vec<StepDefinition>, Vec<WorkflowDefinition>), Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT kind, doc FROM definitions ORDER BY kind, name, version").map_err(db)?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(db)?;
        let mut steps = Vec::new();
        let mut workflows = Vec::new();
        for row in rows {
            let (kind, text) = row.map_err(db)?;
            match kind.as_str() {
                "step" => steps.push(doc::<StepDefinition>(&text, "a StepDefinition")?),
                _ => workflows.push(doc::<WorkflowDefinition>(&text, "a WorkflowDefinition")?),
            }
        }
        Ok((steps, workflows))
    }

    fn describe(&self) -> String {
        format!("sqlite {}", self.path)
    }

    fn put_step(&self, def: &StepDefinition) -> Result<(), Error> {
        def.check()?;
        if self.library()?.workflow(&def.reference()).is_some() {
            return Err(Error::Invalid(format!("StepDefinition {}: a WorkflowDefinition has that name and version, and they share one namespace", def.reference())));
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

    fn remove_definition(&self, reference: &Ref) -> Result<(), Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM definitions WHERE name = ?1 AND version = ?2", params![reference.name, reference.version]).map_err(db)?;
        Ok(())
    }

    fn definition_refs(&self) -> Result<Vec<Ref>, Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT name, version FROM definitions ORDER BY kind, name, version").map_err(db)?;
        let rows = stmt.query_map([], |r| Ok(Ref::new(&r.get::<_, String>(0)?, r.get::<_, u32>(1)?))).map_err(db)?;
        rows.map(|r| r.map_err(db)).collect()
    }

    fn library(&self) -> Result<Library, Error> {
        let (steps, workflows) = self.definitions()?;
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

    fn listing(&self) -> Result<std::collections::BTreeMap<String, String>, Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, version FROM workflows").map_err(db)?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.to_string()))).map_err(db)?;
        rows.collect::<Result<_, _>>().map_err(db)
    }

    fn remove(&self, id: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = write(&mut conn)?;
        // its Runs' ids are `<id>/<step>/<n>`: the orders under that
        // prefix, each checked by the Workflow its request names
        let prefix = format!("{id}/");
        let mine: Vec<(String, bool)> = {
            let mut stmt = tx.prepare("SELECT id, doc, ended FROM orders WHERE substr(id, 1, length(?1)) = ?1").map_err(db)?;
            let rows = stmt.query_map(params![prefix], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)? != 0))).map_err(db)?;
            let mut out = Vec::new();
            for row in rows {
                let (order, text, ended) = row.map_err(db)?;
                let held: Order = doc(&text, "an order")?;
                if held.request.workflow == id {
                    out.push((order, ended));
                }
            }
            out
        };
        if let Some((live, _)) = mine.iter().find(|(_, ended)| !ended) {
            return Err(stepmeld_core::orders::live_order(id, live));
        }
        for (order, _) in &mine {
            tx.execute("DELETE FROM orders WHERE id = ?1", params![order]).map_err(db)?;
        }
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

/// An order as stored: the Orchestrator's part, then the cancel, the
/// claim and the report, each written by its own side.
fn order_from(text: &str, cancel: Option<String>, claim: Option<String>, report: Option<String>) -> Result<Order, Error> {
    let mut order: Order = doc(text, "an order")?;
    order.cancel = cancel.map(|c| doc::<CancelAsk>(&c, "an order's cancel")).transpose()?;
    order.claim = claim.map(|c| doc::<Claim>(&c, "an order's claim")).transpose()?;
    order.report = report.map(|r| doc::<Report>(&r, "an order's report")).transpose()?;
    Ok(order)
}

type OrderRow = (String, Option<String>, Option<String>, Option<String>);

fn order_row(r: &rusqlite::Row) -> rusqlite::Result<OrderRow> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
}

impl OrderBook for SqliteStore {
    fn publish_worker(&self, face: &WorkerFace) -> Result<(), Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("INSERT OR REPLACE INTO workers (name, doc) VALUES (?1, ?2)", params![face.name, serde_json::to_string(face).unwrap()])
            .map_err(db)?;
        Ok(())
    }

    fn workers(&self) -> Result<Vec<WorkerFace>, Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT doc FROM workers ORDER BY name").map_err(db)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(db)?;
        rows.map(|r| r.map_err(db).and_then(|t| doc(&t, "a worker"))).collect()
    }

    fn place(&self, order: &Order) -> Result<(), Error> {
        let bare = Order {
            cancel: None,
            claim: None,
            report: None,
            ..order.clone()
        };
        let conn = self.conn.lock().unwrap();
        // an open order is kept under the empty name
        conn.execute(
            "INSERT OR IGNORE INTO orders (id, worker, doc) VALUES (?1, ?2, ?3)",
            params![order.id, order.worker.as_deref().unwrap_or(""), serde_json::to_string(&bare).unwrap()],
        )
        .map_err(db)?;
        Ok(())
    }

    fn order(&self, id: &str) -> Result<Option<Order>, Error> {
        let conn = self.conn.lock().unwrap();
        let row: Option<OrderRow> = conn.query_row("SELECT doc, cancel, claim, report FROM orders WHERE id = ?1", params![id], order_row).optional().map_err(db)?;
        row.map(|(d, c, cl, r)| order_from(&d, c, cl, r)).transpose()
    }

    fn open_orders(&self, worker: &str) -> Result<Vec<Order>, Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT doc, cancel, claim, report FROM orders WHERE (worker = ?1 OR worker = '') AND ended = 0 ORDER BY seq").map_err(db)?;
        let rows = stmt.query_map(params![worker], order_row).map_err(db)?;
        rows.map(|r| r.map_err(db).and_then(|(d, c, cl, rep)| order_from(&d, c, cl, rep))).collect()
    }

    fn claim(&self, id: &str, expect: Option<&Claim>, claim: &Claim) -> Result<bool, Error> {
        let conn = self.conn.lock().unwrap();
        let n = conn
            .execute(
                "UPDATE orders SET claim = ?3 WHERE id = ?1 AND ended = 0 AND claim IS ?2",
                params![id, expect.map(|c| serde_json::to_string(c).unwrap()), serde_json::to_string(claim).unwrap()],
            )
            .map_err(db)?;
        Ok(n == 1)
    }

    fn end(&self, id: &str, expect: Option<&Claim>, report: &Report) -> Result<bool, Error> {
        if report.outcome.is_none() {
            return Err(Error::Refused(format!("ending order {id} needs an outcome")));
        }
        let conn = self.conn.lock().unwrap();
        let n = conn
            .execute(
                "UPDATE orders SET report = ?3, ended = 1 WHERE id = ?1 AND ended = 0 AND claim IS ?2",
                params![id, expect.map(|c| serde_json::to_string(c).unwrap()), serde_json::to_string(report).unwrap()],
            )
            .map_err(db)?;
        Ok(n == 1)
    }

    fn report(&self, id: &str, worker: &str, report: &Report) -> Result<(), Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = write(&mut conn)?;
        let row: Option<(i64, Option<String>)> = tx.query_row("SELECT ended, claim FROM orders WHERE id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(db)?;
        match row {
            None => return Err(Error::Refused(format!("no order {id:?}"))),
            Some((1, _)) => return Err(Error::Refused(format!("order {id} has ended"))),
            Some((_, claim)) => {
                let holder = claim.map(|c| doc::<Claim>(&c, "an order's claim")).transpose()?.map(|c| c.worker);
                if holder.as_deref() != Some(worker) {
                    return Err(Error::Refused(format!("order {id} is not claimed by {worker}")));
                }
            }
        }
        tx.execute("UPDATE orders SET report = ?2, ended = ?3 WHERE id = ?1", params![id, serde_json::to_string(report).unwrap(), report.outcome.is_some() as i64])
            .map_err(db)?;
        tx.commit().map_err(db)
    }

    fn log(&self, id: &str) -> Result<String, Error> {
        let conn = self.conn.lock().unwrap();
        let log: Option<String> = conn.query_row("SELECT log FROM orders WHERE id = ?1", params![id], |r| r.get(0)).optional().map_err(db)?;
        Ok(log.unwrap_or_default())
    }

    fn append_log(&self, id: &str, worker: &str, at: u64, text: &str) -> Result<bool, Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = write(&mut conn)?;
        let row: Option<(i64, Option<String>, i64)> = tx
            .query_row("SELECT ended, claim, length(CAST(log AS BLOB)) FROM orders WHERE id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()
            .map_err(db)?;
        let Some((ended, claim, len)) = row else { return Ok(false) };
        let holder = claim.map(|c| doc::<Claim>(&c, "an order's claim")).transpose()?.map(|c| c.worker);
        if ended != 0 || holder.as_deref() != Some(worker) || len as u64 != at {
            return Ok(false);
        }
        tx.execute("UPDATE orders SET log = log || ?2 WHERE id = ?1", params![id, text]).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(true)
    }

    fn ask_cancel(&self, id: &str, ask: &CancelAsk) -> Result<(), Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE orders SET cancel = ?2 WHERE id = ?1 AND cancel IS NULL AND ended = 0", params![id, serde_json::to_string(ask).unwrap()])
            .map_err(db)?;
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

    #[test]
    fn the_sqlite_store_keeps_orders_as_the_book_says() {
        stepmeld_core::orders::conformance::run(&SqliteStore::in_memory().unwrap());
        stepmeld_core::orders::conformance::removing(&SqliteStore::in_memory().unwrap());
    }

    /// A definition stored before a rule it now breaks (a StepDefinition
    /// with no program) is still read, so it can be seen and removed,
    /// while the Library refuses.
    #[test]
    fn definitions_are_read_unchecked() {
        let store = SqliteStore::in_memory().unwrap();
        let mut stale = StepDefinition::new("add", 1);
        stale.label = Some("from before programs".into());
        store
            .conn
            .lock()
            .unwrap()
            .execute("INSERT INTO definitions (kind, name, version, doc) VALUES ('step', 'add', 1, ?1)", params![serde_json::to_string(&stale).unwrap()])
            .unwrap();
        assert!(store.library().is_err());
        let (steps, workflows) = store.definitions().unwrap();
        assert_eq!((steps.len(), workflows.len()), (1, 0));
        store.remove_definition(&Ref::new("add", 1)).unwrap();
        assert!(store.library().is_ok());
    }

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
        let mut def = StepDefinition::new("stepdef_note", 1);
        def.program = Some("note-program".into());
        def.label = Some("Take a note".into());
        {
            let store = SqliteStore::open(&path).unwrap();
            store.put_step(&def).unwrap();
            stepmeld_core::store::conformance::run(&store);
        }
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.library().unwrap().step(&def.reference()).unwrap().label.as_deref(), Some("Take a note"));
        // the suite removes its Workflow at the end and puts it again: no history
        assert!(store.get("conformance-wf").unwrap().is_some());
        assert!(store.history("conformance-wf").unwrap().is_empty(), "removed and put again, with no history");
    }

    /// A store made before claims had no claim column: opening it adds
    /// one, and its orders read as nobody's.
    #[test]
    fn a_store_from_before_claims_gains_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE orders (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, worker TEXT NOT NULL, doc TEXT NOT NULL, cancel TEXT, report TEXT, ended INTEGER NOT NULL DEFAULT 0);")
                .unwrap();
            let doc = r#"{"contract":"stepmeld/order.v1","id":"o-1","worker":"w1","placed":"2026-10-17T00:00:00Z","request":{"workflow":"wf","step":"s","run":"o-1","definition":{"name":"d","version":1},"inputs":{},"parameters":{},"outputs":[]}}"#;
            conn.execute("INSERT INTO orders (id, worker, doc) VALUES ('o-1', 'w1', ?1)", params![doc]).unwrap();
        }
        let store = SqliteStore::open(&path).unwrap();
        let order = store.order("o-1").unwrap().unwrap();
        assert!(order.claim.is_none());
        let claim = Claim {
            worker: "w1".into(),
            at: "2026-10-20T00:00:00Z".into(),
            until: "2026-10-20T00:05:00Z".into(),
            handle: None,
            portable: false,
        };
        assert!(store.claim("o-1", None, &claim).unwrap());
    }

    /// Two Orchestrators on two connections to one file, ticking the
    /// same Workflows as fast as they can while a Worker on a third
    /// runs what they order: every Workflow finishes, and nothing is
    /// minted, started or ordered twice (docs/PLACEMENT.md §2).
    #[test]
    fn two_orchestrators_keep_one_workflow_moving_without_doing_anything_twice() {
        use std::collections::BTreeMap;
        use std::sync::Arc;
        use stepmeld_core::definition::*;
        use stepmeld_core::driver::Driver;
        use stepmeld_core::engine::Command;
        use stepmeld_core::orders::{wall_clock, OrderPerformer, Worker};
        use stepmeld_core::performer::MemoryPerformer;
        use stepmeld_core::workflow::{Actor, Outcome, PerformerFace, WorkflowStatus};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.sqlite");
        let setup = SqliteStore::open(&path).unwrap();
        let mut count = StepDefinition::new("count", 1);
        count.program = Some("count-program".into());
        count.inputs.push(InputDefinition {
            name: "x".into(),
            tag: "n".into(),
            required: false,
            label: None,
        });
        count.outputs.push(OutputDefinition {
            name: "n".into(),
            tag: "n".into(),
            options: None,
            label: None,
        });
        setup.put_step(&count).unwrap();
        let mut two = WorkflowDefinition::new("two", 1);
        for name in ["first", "second"] {
            two.steps.push(StepEntry {
                name: name.into(),
                step: Ref::new("count", 1),
                label: None,
            });
        }
        two.bindings.push(Binding {
            from: PortRef::new("first", "n"),
            to: PortRef::new("second", "x"),
            guard: None,
        });
        setup.put_workflow_definition(&two).unwrap();
        fn now() -> String {
            wall_clock()()
        }
        fn until() -> String {
            stepmeld_core::later(&now(), 60).unwrap()
        }
        let ids: Vec<String> = (0..4).map(|i| format!("wf-{i}")).collect();
        {
            let d = Driver::new(Arc::new(SqliteStore::open(&path).unwrap()), "setup");
            for id in &ids {
                d.create(&Ref::new("two", 1), id, None, &Actor::person("t"), &now()).unwrap();
                d.command(id, &Command::StartWorkflow, &Actor::person("t"), &now()).unwrap();
            }
        }

        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = {
            let (path, stop) = (path.clone(), stop.clone());
            std::thread::spawn(move || {
                let store = Arc::new(SqliteStore::open(&path).unwrap());
                let mechanism = Arc::new(MemoryPerformer::new(PerformerFace {
                    name: "m".into(),
                    locality: Locality::ThisMachine,
                    verbs: vec![Ref::new("count", 1)],
                    pool: false,
                }));
                let w = Worker::new("bench", "h", false, vec!["count-program".into()], store, mechanism.clone()).with_slots(8);
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    w.tick(&now()).unwrap();
                    for req in mechanism.started() {
                        let h = mechanism.handle_of(&req.run).unwrap();
                        if mechanism.run(&h).unwrap().outcome.is_none() {
                            mechanism.finish(&h, Outcome::succeeded(BTreeMap::from([("n".to_string(), serde_json::json!(1))])));
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            })
        };
        let orchestrators: Vec<_> = ["o-1", "o-2"]
            .into_iter()
            .map(|holder| {
                let (path, ids) = (path.clone(), ids.clone());
                std::thread::spawn(move || {
                    let store = Arc::new(SqliteStore::open(&path).unwrap());
                    for _ in 0..3000 {
                        let mut d = Driver::new(store.clone(), holder);
                        for p in OrderPerformer::for_workers(store.clone(), &store.library().unwrap(), "h", wall_clock()).unwrap() {
                            d = d.with_performer(Arc::new(p));
                        }
                        let statuses = d.tick_all(&now(), &until()).unwrap();
                        if ids.iter().all(|id| statuses.get(id) == Some(&WorkflowStatus::Succeeded)) {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    panic!("{holder} gave up before every Workflow finished");
                })
            })
            .collect();
        for o in orchestrators {
            o.join().unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        worker.join().unwrap();

        let store = SqliteStore::open(&path).unwrap();
        for id in &ids {
            let (wf, _) = store.get(id).unwrap().unwrap();
            for (name, step) in &wf.steps {
                assert_eq!(step.runs.len(), 1, "{id}/{name} has one Run");
                assert!(store.order(&step.runs[0].id).unwrap().unwrap().ended());
            }
            let kinds: Vec<String> = store.history(id).unwrap().into_iter().map(|e| e.kind).collect();
            for kind in ["run-minted", "run-started"] {
                assert_eq!(kinds.iter().filter(|k| *k == kind).count(), 2, "{id}: {kind} once per Step: {kinds:?}");
            }
        }
    }
}
