use crate::{
    config::now,
    error::{Error, Result, required},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: i64,
    run_id: String,
    created_at: i64,
    #[serde(rename = "type")]
    kind: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<Box<serde_json::value::RawValue>>,
}

#[derive(Clone)]
pub struct Store(Arc<Pool>);

struct Pool {
    writer: mpsc::Sender<Job>,
    readers: mpsc::Sender<Job>,
    changes: watch::Sender<u64>,
}

/// Read connections. WAL readers do not block each other or the writer; a few
/// cover the live streams of several open tabs without queueing behind one page.
const READERS: usize = 4;

const DATABASE_VERSION: i64 = 7;

/// Connections that take their next job from one queue. A slow read occupies
/// only its own connection: the next job goes to whichever is free.
fn actors(connections: Vec<Connection>) -> mpsc::Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>(256);
    let rx = Arc::new(Mutex::new(rx));
    for mut db in connections {
        let rx = rx.clone();
        std::thread::Builder::new()
            .name("cairn-sqlite".into())
            .spawn(move || {
                loop {
                    // The guard is released before the job runs.
                    let job = rx.lock().ok().and_then(|mut rx| rx.blocking_recv());
                    let Some(job) = job else {
                        break;
                    };
                    job(&mut db);
                }
            })
            .expect("database thread");
    }
    tx
}

impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(directory)?;
        let file = directory.join("manager.db");
        let fresh = !file.exists();
        let mut writer = Connection::open(&file)?;
        let version: i64 = writer.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if !fresh && version != DATABASE_VERSION {
            return Err(Error::bad(
                "Unsupported installation database version; a complete reinstallation is required.",
            ));
        }
        writer.busy_timeout(std::time::Duration::from_secs(5))?;
        writer.set_prepared_statement_cache_capacity(128);
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        if fresh {
            let tx = writer.transaction()?;
            tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS records(id TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind NOT IN ('projects','agents','tasks','mcps','plugin-connections')),data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS records_kind ON records(kind,updated_at DESC);
CREATE INDEX IF NOT EXISTS records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE TABLE IF NOT EXISTS runs(id TEXT PRIMARY KEY,task_id TEXT NOT NULL,project_id TEXT NOT NULL,status TEXT NOT NULL,created_at INTEGER NOT NULL,data TEXT NOT NULL,dedupe TEXT UNIQUE);
CREATE INDEX IF NOT EXISTS runs_status ON runs(status,created_at);
CREATE INDEX IF NOT EXISTS runs_task ON runs(task_id,created_at DESC);
CREATE INDEX IF NOT EXISTS runs_created ON runs(created_at DESC,id DESC);
CREATE UNIQUE INDEX IF NOT EXISTS runs_active_task ON runs(task_id) WHERE status IN ('queued','running','waiting');
CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,created_at INTEGER NOT NULL,type TEXT NOT NULL,text TEXT NOT NULL,payload TEXT);
CREATE INDEX IF NOT EXISTS events_run ON events(run_id,id);
CREATE TABLE IF NOT EXISTS kv(key TEXT PRIMARY KEY,data TEXT NOT NULL,expires INTEGER);
CREATE TABLE IF NOT EXISTS audit(id INTEGER PRIMARY KEY AUTOINCREMENT,created_at INTEGER NOT NULL,action TEXT NOT NULL,detail TEXT NOT NULL);",
        )?;
            tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS chat_messages(id TEXT PRIMARY KEY,chat_id TEXT NOT NULL,data TEXT NOT NULL,created_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS chat_messages_chat ON chat_messages(chat_id,created_at);",
        )?;
            tx.pragma_update(None, "user_version", DATABASE_VERSION)?;
            // Backwards history excludes superseded text snapshots. These small keys
            // bound both the next-turn lookup and the same-message revision lookup.
            tx.execute_batch("CREATE INDEX IF NOT EXISTS events_turns ON events(run_id,id) WHERE type='turn.started';
CREATE INDEX IF NOT EXISTS events_user_input ON events(run_id,id) WHERE type='chat.user';
CREATE INDEX IF NOT EXISTS events_messages ON events(run_id,json_extract(payload,'$.item.id'),id) WHERE json_extract(payload,'$.item.type')='agent_message';")?;
            tx.execute_batch(crate::nodes::shared_blocks::SCHEMA)?;
            tx.execute_batch(crate::projects::SCHEMA)?;
            tx.execute_batch(crate::agents::SCHEMA)?;
            tx.execute_batch(crate::mcps::sql::SCHEMA)?;
            tx.execute_batch(crate::plugins::instances::SCHEMA)?;
            tx.execute_batch(crate::agent_grants::SCHEMA)?;
            tx.execute_batch(crate::missions::SCHEMA)?;
            tx.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';")?;
            tx.commit()?;
        }
        let mut readers = Vec::new();
        for _ in 0..READERS {
            let connection = Connection::open(&file)?;
            connection.busy_timeout(std::time::Duration::from_secs(5))?;
            connection.execute_batch("PRAGMA query_only=ON; PRAGMA foreign_keys=ON;")?;
            connection.set_prepared_statement_cache_capacity(128);
            readers.push(connection);
        }
        Ok(Self(Arc::new(Pool {
            writer: actors(vec![writer]),
            readers: actors(readers),
            changes: watch::channel(0).0,
        })))
    }

    async fn call<T: Send + 'static>(
        &self,
        tx: &mpsc::Sender<Job>,
        f: impl FnOnce(&mut Db<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (reply, rx) = oneshot::channel();
        tx.send(Box::new(move |connection| {
            let _ = reply.send(f(&mut Db(connection)));
        }))
        .await
        .map_err(|_| Error::unavailable("Database is closing."))?;
        rx.await
            .map_err(|_| Error::unavailable("Database operation stopped."))?
    }

    pub async fn read<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Db<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.call(&self.0.readers, f).await
    }

    pub async fn write<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Db<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let changes = self.0.changes.clone();
        self.call(&self.0.writer, move |db| {
            let before = db.0.total_changes();
            let result = f(db);
            // Notify on the database actor, after commit, even if the caller was cancelled.
            // Empty scheduler transactions must not wake every live UI subscriber.
            if db.0.total_changes() != before {
                changes.send_modify(|revision| *revision = revision.wrapping_add(1));
            }
            result
        })
        .await
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.0.changes.subscribe()
    }

    /// Counts the commits that changed something, as notified to subscribers.
    pub fn revision(&self) -> u64 {
        *self.0.changes.borrow()
    }

    pub async fn transaction<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Db<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.write(move |db| {
            let tx = rusqlite::Transaction::new_unchecked(
                db.0,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let value = f(&mut Db(&tx))?;
            tx.commit()?;
            Ok(value)
        })
        .await
    }

    pub async fn get(&self, kind: &str, id: &str) -> Result<Option<Value>> {
        self.get_as(kind, id).await
    }

    /// Typed variant of [`Store::get`].
    pub async fn get_as<T: DeserializeOwned + Send + 'static>(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<Option<T>> {
        let (kind, id) = (kind.to_owned(), id.to_owned());
        self.read(move |db| db.get_as(&kind, &id)).await
    }

    pub async fn list(&self, kind: &str) -> Result<Vec<Value>> {
        let kind = kind.to_owned();
        self.read(move |db| db.list(&kind)).await
    }

    pub async fn node_backups_for_run(&self, run: &str) -> Result<Vec<Value>> {
        let run = run.to_owned();
        self.read(move |db| db.node_backups_for_run(&run)).await
    }

    pub async fn kv(&self, key: &str) -> Result<Option<Value>> {
        self.kv_as(key).await
    }

    /// Typed variant of [`Store::kv`].
    pub async fn kv_as<T: DeserializeOwned + Send + 'static>(
        &self,
        key: &str,
    ) -> Result<Option<T>> {
        let key = key.to_owned();
        self.read(move |db| db.kv_as(&key)).await
    }

    pub async fn set(&self, key: &str, value: Value, expires: Option<i64>) -> Result<()> {
        self.set_as(key, value, expires).await
    }

    /// Typed variant of [`Store::set`].
    pub async fn set_as<T: Serialize + Send + 'static>(
        &self,
        key: &str,
        value: T,
        expires: Option<i64>,
    ) -> Result<()> {
        let key = key.to_owned();
        self.write(move |db| db.set_as(&key, &value, expires)).await
    }

    /// Typed variant of [`Store::keys`].
    pub async fn keys_as<T: DeserializeOwned + Send + 'static>(
        &self,
        prefix: &str,
    ) -> Result<Vec<(String, T)>> {
        let prefix = prefix.to_owned();
        self.read(move |db| db.keys_as(&prefix)).await
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        let key = key.to_owned();
        self.write(move |db| db.delete(&key)).await
    }

    pub async fn put(&self, kind: &str, value: Value) -> Result<Value> {
        let kind = kind.to_owned();
        self.write(move |db| db.put(&kind, &value)).await
    }

    pub async fn save(&self, kind: &str, value: Value, action: &str) -> Result<Value> {
        let (kind, action) = (kind.to_owned(), action.to_owned());
        self.transaction(move |db| {
            let result = db.put(&kind, &value)?;
            db.audit(&action, &json!({"id": result["id"]}))?;
            Ok(result)
        })
        .await
    }

    pub async fn run(&self, id: &str) -> Result<Value> {
        let id = id.to_owned();
        self.read(move |db| required(db.run(&id)?, "Run not found"))
            .await
    }

    pub async fn patch_run(&self, id: &str, patch: Value) -> Result<Value> {
        let id = id.to_owned();
        // Read and rewrite the document under one write lock: another connection,
        // such as a second process on this database, must not interleave its patch.
        self.transaction(move |db| db.patch_run(&id, &patch)).await
    }

    pub async fn event(
        &self,
        id: &str,
        kind: &str,
        text: &str,
        payload: Option<Value>,
    ) -> Result<()> {
        let (id, kind, text) = (id.to_owned(), kind.to_owned(), text.to_owned());
        self.write(move |db| db.event(&id, &kind, &text, payload.as_ref()))
            .await
    }

    pub async fn keys(&self, prefix: &str) -> Result<Vec<(String, Value)>> {
        let prefix = prefix.to_owned();
        self.read(move |db| db.keys(&prefix)).await
    }

    pub async fn audit(&self, action: &str, detail: Value) -> Result<()> {
        let action = action.to_owned();
        self.write(move |db| db.audit(&action, &detail)).await
    }
}

/// Run list projection: drops the heavy snapshot and summary, keeps display names and the
/// explicit coding agent saved in the snapshot.
pub const RUN_SUMMARY: &str = "json_set(
    json_remove(data,'$.snapshot','$.summary'),
    '$.taskName',json_extract(data,'$.snapshot.task.name'),
    '$.agentName',json_extract(data,'$.snapshot.agent.name'),
    '$.provider',json_extract(data,'$.snapshot.agent.provider')
)";

// Database transactions and statement caches stay on their owning database thread.
pub struct Db<'a>(pub &'a Connection);

impl Db<'_> {
    /// First column of at most one row, decoded from its JSON text.
    fn json_row<T: DeserializeOwned>(
        &self,
        sql: &str,
        parameters: impl rusqlite::Params,
    ) -> Result<Option<T>> {
        self.0
            .prepare_cached(sql)?
            .query_row(parameters, |row| row.get::<_, String>(0))
            .optional()?
            .map(|data| Ok(serde_json::from_str(&data)?))
            .transpose()
    }

    pub fn json_rows(&self, sql: &str, parameters: impl rusqlite::Params) -> Result<Vec<Value>> {
        self.json_rows_as(sql, parameters)
    }

    /// Typed variant of [`Db::json_rows`].
    pub fn json_rows_as<T: DeserializeOwned>(
        &self,
        sql: &str,
        parameters: impl rusqlite::Params,
    ) -> Result<Vec<T>> {
        let mut stmt = self.0.prepare_cached(sql)?;
        let rows = stmt.query_map(parameters, |row| row.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn list(&self, kind: &str) -> Result<Vec<Value>> {
        if self.0.is_autocommit() && matches!(kind, "nodes") {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            return Db(&tx).list(kind);
        }
        let mut values = self.json_rows(
            "SELECT data FROM records WHERE kind=? ORDER BY updated_at DESC",
            [kind],
        )?;
        for value in &mut values {
            self.resource_grants(kind, value)?;
        }
        Ok(values)
    }

    pub fn node_backups_for_run(&self, run: &str) -> Result<Vec<Value>> {
        self.json_rows(
            "SELECT data FROM records WHERE kind='node-backups' AND json_extract(data,'$.runId')=?",
            [run],
        )
    }

    pub fn get(&self, kind: &str, id: &str) -> Result<Option<Value>> {
        self.get_as(kind, id)
    }

    /// Typed variant of [`Db::get`].
    pub fn get_as<T: DeserializeOwned>(&self, kind: &str, id: &str) -> Result<Option<T>> {
        if !matches!(kind, "nodes") {
            return self.json_row(
                "SELECT data FROM records WHERE kind=? AND id=?",
                params![kind, id],
            );
        }
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            return Db(&tx).get_as(kind, id);
        }
        let value: Option<Value> = self.json_row(
            "SELECT data FROM records WHERE kind=? AND id=?",
            params![kind, id],
        )?;
        value
            .map(|mut value| {
                self.resource_grants(kind, &mut value)?;
                Ok(serde_json::from_value(value)?)
            })
            .transpose()
    }

    pub fn chat_id_for_slug(&self, slug: &str) -> Result<Option<String>> {
        Ok(self
            .0
            .prepare_cached(
                "SELECT id FROM records WHERE kind='chats' AND json_extract(data,'$.slug')=?",
            )?
            .query_row([slug], |row| row.get(0))
            .optional()?)
    }

    fn new_chat_slug(&self) -> Result<String> {
        loop {
            let slug = cairn_protocol::slugs::readable(uuid::Uuid::new_v4().as_bytes());
            let used: bool = self.0.query_row(
                "SELECT EXISTS(SELECT 1 FROM records WHERE kind='chats' AND json_extract(data,'$.slug')=?)",
                [&slug],
                |row| row.get(0),
            )?;
            if !used {
                return Ok(slug);
            }
        }
    }

    pub fn put(&self, kind: &str, value: &Value) -> Result<Value> {
        if matches!(kind, "projects" | "agents" | "mcps" | "plugin-connections") {
            return Err(Error::bad("Use typed domain commands."));
        }
        let projection = match kind {
            "nodes" => Some("exclusiveAgents"),
            _ => None,
        };
        let mut resource;
        let value = if let Some(field) = projection {
            resource = value.clone();
            resource.as_object_mut().unwrap().remove(field);
            &resource
        } else {
            value
        };
        let mut chat;
        let value = if kind == "chats" {
            chat = value.clone();
            let existing = self.get(kind, value["id"].as_str().unwrap_or(""))?;
            chat["slug"] = match existing {
                Some(existing) => existing["slug"]
                    .as_str()
                    .ok_or_else(|| Error::bad("Missing persisted chat slug."))?
                    .into(),
                None => self.new_chat_slug()?.into(),
            };
            &chat
        } else {
            value
        };

        self.0
            .prepare_cached(
                "INSERT INTO records VALUES(?,?,?,?)
                 ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated_at=excluded.updated_at",
            )?
            .execute(params![
                value["id"].as_str(),
                kind,
                value.to_string(),
                now()
            ])?;
        let mut result = value.clone();
        self.resource_grants(kind, &mut result)?;
        Ok(result)
    }

    pub fn remove(&self, kind: &str, id: &str) -> Result<()> {
        self.0
            .prepare_cached("DELETE FROM records WHERE kind=? AND id=?")?
            .execute(params![kind, id])?;
        Ok(())
    }

    pub fn kv(&self, key: &str) -> Result<Option<Value>> {
        self.kv_as(key)
    }

    /// Typed variant of [`Db::kv`].
    pub fn kv_as<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        self.json_row(
            "SELECT data FROM kv WHERE key=? AND (expires IS NULL OR expires>?)",
            params![key, now()],
        )
    }

    pub fn set(&self, key: &str, value: &Value, expires: Option<i64>) -> Result<()> {
        self.set_as(key, value, expires)
    }

    /// Typed variant of [`Db::set`].
    pub fn set_as<T: Serialize + ?Sized>(
        &self,
        key: &str,
        value: &T,
        expires: Option<i64>,
    ) -> Result<()> {
        self.0
            .prepare_cached(
                "INSERT INTO kv VALUES(?,?,?)
                 ON CONFLICT(key) DO UPDATE SET data=excluded.data,expires=excluded.expires",
            )?
            .execute(params![key, serde_json::to_string(value)?, expires])?;
        Ok(())
    }

    pub fn delete(&self, key: &str) -> Result<()> {
        self.0
            .prepare_cached("DELETE FROM kv WHERE key=?")?
            .execute([key])?;
        Ok(())
    }

    pub fn keys(&self, prefix: &str) -> Result<Vec<(String, Value)>> {
        self.keys_as(prefix)
    }

    /// Typed variant of [`Db::keys`].
    pub fn keys_as<T: DeserializeOwned>(&self, prefix: &str) -> Result<Vec<(String, T)>> {
        let mut stmt = self.0.prepare_cached(
            "SELECT key,data FROM kv WHERE key>=?1 AND key<?2 AND (expires IS NULL OR expires>?3)",
        )?;
        let rows = stmt.query_map(params![prefix, format!("{prefix}\u{10ffff}"), now()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.map(|r| {
            let (k, v) = r?;
            Ok((k, serde_json::from_str(&v)?))
        })
        .collect()
    }

    pub fn run(&self, id: &str) -> Result<Option<Value>> {
        self.json_row("SELECT data FROM runs WHERE id=?", [id])
    }

    pub fn add_run(&self, run: &Value, dedupe: Option<&str>) -> Result<Value> {
        self.0
            .prepare_cached("INSERT INTO runs VALUES(?,?,?,?,?,?,?)")?
            .execute(params![
                run["id"].as_str(),
                run["taskId"].as_str(),
                run["projectId"].as_str().unwrap_or(""),
                run["status"].as_str(),
                run["createdAt"].as_i64(),
                run.to_string(),
                dedupe
            ])?;
        Ok(run.clone())
    }

    pub fn patch_run(&self, id: &str, patch: &Value) -> Result<Value> {
        let mut run = required(self.run(id)?, "Run not found")?;
        merge(&mut run, patch);
        self.0
            .prepare_cached("UPDATE runs SET status=?,data=? WHERE id=?")?
            .execute(params![run["status"].as_str(), run.to_string(), id])?;
        Ok(run)
    }

    pub fn active(&self) -> Result<Vec<Value>> {
        self.json_rows(
            "SELECT data FROM runs WHERE status IN ('queued','running','waiting') ORDER BY created_at",
            [],
        )
    }

    pub fn runs(
        &self,
        status: Option<&str>,
        task: Option<&str>,
        limit: i64,
        offset: i64,
        full: bool,
    ) -> Result<Vec<Value>> {
        let (query, values) = Self::run_query(status, task, limit, offset, full);
        self.json_rows(&query, rusqlite::params_from_iter(values))
    }

    pub fn run_page(
        &self,
        status: Option<&str>,
        task: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Box<serde_json::value::RawValue>>> {
        let (query, values) = Self::run_query(status, task, limit, offset, false);
        let mut statement = self.0.prepare_cached(&query)?;
        statement
            .query_map(rusqlite::params_from_iter(values), |row| {
                row.get::<_, String>(0)
            })?
            .map(|row| Ok(serde_json::value::RawValue::from_string(row?)?))
            .collect()
    }

    fn run_query(
        status: Option<&str>,
        task: Option<&str>,
        limit: i64,
        offset: i64,
        full: bool,
    ) -> (String, Vec<rusqlite::types::Value>) {
        let fields = if full { "data" } else { RUN_SUMMARY };
        let mut filters = vec![
            "NOT EXISTS (
               SELECT 1 FROM records c
               WHERE c.kind='chats' AND json_extract(c.data,'$.runId')=runs.id
                 AND COALESCE(json_extract(c.data,'$.lifecycle'),'active')<>'active'
             )",
        ];
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(status) = status {
            filters.push("status=?");
            values.push(status.to_owned().into());
        }
        if let Some(task) = task {
            filters.push("task_id=?");
            values.push(task.to_owned().into());
        }
        values.push(limit.into());
        values.push(offset.into());
        let clause = filters.join(" AND ");
        (
            format!(
                "SELECT {fields} FROM runs WHERE {clause} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"
            ),
            values,
        )
    }

    pub fn stats(&self) -> Result<Value> {
        let mut stmt = self
            .0
            .prepare_cached("SELECT status,COUNT(*) FROM runs GROUP BY status")?;
        let counts = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .map(|row| row.map(|(status, count)| (status, Value::from(count))))
            .collect::<std::result::Result<serde_json::Map<_, _>, _>>()?;
        Ok(counts.into())
    }

    pub fn event(&self, run: &str, kind: &str, text: &str, payload: Option<&Value>) -> Result<()> {
        let text = text.chars().take(16000).collect::<String>();
        self.0
            .prepare_cached(
                "INSERT INTO events(run_id,created_at,type,text,payload) VALUES(?,?,?,?,?)",
            )?
            .execute(params![
                run,
                now(),
                kind,
                text,
                payload.map(Value::to_string)
            ])?;
        // Prune in batches instead of executing a delete for every streamed delta.
        if self.0.last_insert_rowid() % 256 == 0 {
            self.0
                .prepare_cached(
                    "DELETE FROM events
                     WHERE run_id=?1
                       AND id<(SELECT COALESCE(MAX(id),0)-10000 FROM events WHERE run_id=?1)
                       AND run_id IN (SELECT id FROM runs WHERE json_extract(data,'$.trigger')!='chat')",
                )?
                .execute([run])?;
        }
        Ok(())
    }

    /// Repair completed chats whose answer survived only in the run summary.
    /// Match within the latest user turn so repeated answers in later turns survive.
    pub(crate) fn restore_chat_summaries(&self, run: Option<&str>) -> Result<()> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Summary {
            id: String,
            summary: String,
            finished_at: Option<i64>,
        }
        let summaries: Vec<Summary> = self.json_rows_as(
            "SELECT json_object('id',r.id,'summary',json_extract(r.data,'$.summary'),
                'finishedAt',json_extract(r.data,'$.finishedAt')) FROM runs r
             WHERE r.status='succeeded' AND json_extract(r.data,'$.trigger')='chat'
               AND (?1 IS NULL OR r.id=?1)
               AND NOT EXISTS (
                 SELECT 1 FROM records c
                 WHERE c.kind='chats' AND json_extract(c.data,'$.runId')=r.id
                   AND COALESCE(json_extract(c.data,'$.lifecycle'),'active')!='active'
               )
               AND length(trim(COALESCE(json_extract(r.data,'$.summary'),''),char(9)||char(10)||char(13)||' '))>0
               AND NOT EXISTS (
                 SELECT 1 FROM events e WHERE e.run_id=r.id
                   AND e.id>COALESCE((SELECT MAX(u.id) FROM events u WHERE u.run_id=r.id AND u.type='chat.user'),0)
                   AND (json_extract(e.payload,'$.item.type')='agent_message'
                     OR (e.type='item.completed' AND json_extract(e.payload,'$.item.type') IS NULL))
                   AND trim(substr(COALESCE(json_extract(e.payload,'$.item.text'),e.text),1,100000),char(9)||char(10)||char(13)||' ')
                     =trim(json_extract(r.data,'$.summary'),char(9)||char(10)||char(13)||' ')
               )",
            [run],
        )?;
        for Summary {
            id,
            summary,
            finished_at,
        } in summaries
        {
            let finished = finished_at.unwrap_or_else(now);
            let payload = json!({
                "type": "item.completed",
                "item": {
                    "id": format!("recovered-summary:{id}:{finished}"),
                    "type": "agent_message",
                    "phase": "final",
                    "text": summary,
                    "recovered": true,
                },
            });
            self.event(&id, "item.completed", &summary, Some(&payload))?;
            self.0.execute(
                "UPDATE events SET created_at=? WHERE id=?",
                params![finished, self.0.last_insert_rowid()],
            )?;
        }
        Ok(())
    }

    pub fn events(&self, run: &str, after: i64, limit: i64) -> Result<Vec<Value>> {
        self.event_page(run, after, limit)?
            .into_iter()
            .map(|event| serde_json::to_value(event).map_err(Error::from))
            .collect()
    }

    // Stored payloads are already JSON. Validate their bytes without allocating
    // thousands of intermediate object nodes just to serialize them again.
    pub fn event_page(&self, run: &str, after: i64, limit: i64) -> Result<Vec<Event>> {
        self.event_batch(run, after, limit, usize::MAX)
    }

    pub fn event_batch(
        &self,
        run: &str,
        after: i64,
        limit: i64,
        max_bytes: usize,
    ) -> Result<Vec<Event>> {
        let sql = "SELECT id,run_id,created_at,type,text,payload FROM events WHERE run_id=? \
            AND id>? ORDER BY id LIMIT ?";
        let mut stmt = self.0.prepare_cached(sql)?;
        let rows = stmt.query_map(params![run, after, limit], event_row)?;
        within_budget(rows, limit, max_bytes)
    }

    pub fn events_before(&self, run: &str, before: i64) -> Result<(Vec<Event>, bool)> {
        self.events_between(run, 0, before)
    }

    /// The newest page of events between two IDs (both exclusive), in order, without
    /// text snapshots a later snapshot of the same message replaced within its turn.
    /// Also reports whether earlier such events remain above `after`.
    pub fn events_between(&self, run: &str, after: i64, before: i64) -> Result<(Vec<Event>, bool)> {
        let sql = "SELECT e.id,e.run_id,e.created_at,e.type,e.text,e.payload FROM events e
WHERE e.run_id=? AND e.id>? AND e.id<? AND (
 json_extract(e.payload,'$.item.type') IS NOT 'agent_message'
 OR COALESCE(json_extract(e.payload,'$.item.id'),'')=''
 OR NOT EXISTS (
  SELECT 1 FROM events n WHERE n.run_id=e.run_id
   AND json_extract(n.payload,'$.item.type')='agent_message'
   AND json_extract(n.payload,'$.item.id')=json_extract(e.payload,'$.item.id')
   AND n.id>e.id AND n.id<COALESCE((SELECT t.id FROM events t WHERE t.run_id=e.run_id AND t.type='turn.started' AND t.id>e.id ORDER BY t.id LIMIT 1),9223372036854775807)
 )) ORDER BY e.id DESC LIMIT ?";
        let mut stmt = self.0.prepare_cached(sql)?;
        let mut rows = stmt.query_map(
            params![run, after, before, HISTORY_PAGE_EVENTS + 1],
            event_row,
        )?;
        let mut events = within_budget(&mut rows, HISTORY_PAGE_EVENTS, HISTORY_PAGE_BYTES)?;
        let more = rows.next().transpose()?.is_some();
        events.reverse();
        Ok((events, more))
    }

    pub fn has_run(&self, id: &str) -> Result<bool> {
        Ok(self
            .0
            .prepare_cached("SELECT 1 FROM runs WHERE id=?")?
            .exists([id])?)
    }

    pub fn require_run(&self, id: &str) -> Result<()> {
        if !self.has_run(id)? {
            return Err(Error::not_found("Run not found."));
        }
        Ok(())
    }

    pub fn messages(&self, chat: &str) -> Result<Vec<Value>> {
        self.json_rows(
            "SELECT data FROM chat_messages WHERE chat_id=? ORDER BY created_at,rowid",
            [chat],
        )
    }

    pub fn put_message(&self, message: &Value) -> Result<Value> {
        self.0
            .prepare_cached(
                "INSERT INTO chat_messages VALUES(?,?,?,?)
                 ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            )?
            .execute(params![
                message["id"].as_str(),
                message["chatId"].as_str(),
                message.to_string(),
                message["createdAt"].as_i64()
            ])?;
        Ok(message.clone())
    }

    pub fn audit(&self, action: &str, detail: &Value) -> Result<()> {
        self.0
            .prepare_cached("INSERT INTO audit(created_at,action,detail) VALUES(?,?,?)")?
            .execute(params![now(), action, detail.to_string()])?;
        Ok(())
    }
}

/// Events per page of history, and their approximate byte budget.
const HISTORY_PAGE_EVENTS: i64 = 100;
const HISTORY_PAGE_BYTES: usize = 256 * 1024;

type EventRow = (i64, String, i64, String, String, Option<String>);

fn event_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
    ))
}

/// Stops reading rows at the byte budget, rather than allocating an entire page
/// of large historical outputs. Stored payloads are already JSON: their bytes are
/// kept as they are instead of being parsed into intermediate object nodes.
fn within_budget(
    rows: impl Iterator<Item = rusqlite::Result<EventRow>>,
    limit: i64,
    max_bytes: usize,
) -> Result<Vec<Event>> {
    let mut events = Vec::new();
    let mut bytes = 0;
    for row in rows {
        let (id, run, created, kind, text, payload) = row?;
        bytes += text.len() + payload.as_ref().map_or(0, String::len) + 256;
        events.push(Event {
            id,
            run_id: run,
            created_at: created,
            kind,
            text,
            payload: payload
                .map(serde_json::value::RawValue::from_string)
                .transpose()?,
        });
        if bytes >= max_bytes || events.len() >= limit as usize {
            break;
        }
    }
    Ok(events)
}

/// Shallow merge: every top-level key of `patch` replaces the key in `value`
/// (a `null` is written, nested objects are replaced whole); other keys stay.
pub fn merge(value: &mut Value, patch: &Value) {
    if let (Some(target), Some(patch)) = (value.as_object_mut(), patch.as_object()) {
        target.extend(patch.clone());
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    use crate::agent_fixture::StoreAgentFixture;

    #[tokio::test]
    async fn initial_schema_reopens_with_events_and_records_intact() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .put_agent_fixture(
                json!({ "id": "00000000-0000-4000-8000-000000000099", "name": "Agent" }),
            )
            .await
            .unwrap();
        let reopened = Store::open(root.path()).unwrap();
        assert_eq!(
            reopened
                .agent_snapshot("00000000-0000-4000-8000-000000000099")
                .await
                .unwrap()
                .unwrap()["name"],
            "Agent"
        );
        let db = Connection::open(root.path().join("manager.db")).unwrap();
        assert_eq!(
            db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            DATABASE_VERSION
        );
        db.prepare("SELECT payload FROM events").unwrap();
        assert!(!db.table_exists(None, "disk_block_migration").unwrap());
    }

    #[test]
    fn existing_other_versions_fail_without_changing_the_database() {
        for version in [0, 1, 2, 3, 4, 5, 6, 12, 99] {
            let root = tempfile::tempdir().unwrap();
            let db = Connection::open(root.path().join("manager.db")).unwrap();
            db.pragma_update(None, "user_version", version).unwrap();
            let error = Store::open(root.path()).err().unwrap();
            assert!(error.message.contains("complete reinstallation"));
            assert_eq!(
                db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                    .unwrap(),
                version
            );
            assert!(!db.table_exists(None, "records").unwrap());
        }
    }
}
