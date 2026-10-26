//! Typed run lifecycle stored in named columns on the existing SQLite actors.
use crate::{
    error::{Error, Result, required},
    store::{Db, Store},
};
use cairn_contracts::{AgentProvider, Retry, RunState, RunStatus};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

mod change;
pub use change::RunChanges;

pub(crate) const SCHEMA: &str = r#"CREATE TABLE runs(
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL,
  project_id TEXT,
  status TEXT NOT NULL CHECK(status IN ('queued','running','waiting','succeeded','failed','cancelled','interrupted')),
  trigger TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
  started_at INTEGER CHECK(started_at BETWEEN 0 AND 9007199254740991),
  finished_at INTEGER CHECK(finished_at BETWEEN 0 AND 9007199254740991),
  summary TEXT NOT NULL DEFAULT '',
  error TEXT,
  session_id TEXT,
  workspace TEXT,
  workspace_cleaned_at INTEGER CHECK(workspace_cleaned_at BETWEEN 0 AND 9007199254740991),
  recovery_pending INTEGER NOT NULL CHECK(recovery_pending IN (0,1)),
  resume_available INTEGER NOT NULL CHECK(resume_available IN (0,1)),
  resume_count INTEGER NOT NULL CHECK(resume_count BETWEEN 0 AND 4294967295),
  cancel_requested_at INTEGER CHECK(cancel_requested_at BETWEEN 0 AND 9007199254740991),
  account_id TEXT,
  account_name TEXT,
  account_wait_reason TEXT,
  account_required TEXT CHECK(account_required IN ('codex','claude','opencode')),
  capacity_wait_until INTEGER CHECK(capacity_wait_until BETWEEN 0 AND 9007199254740991),
  retry_attempt INTEGER CHECK(retry_attempt BETWEEN 1 AND 5),
  retry_limit INTEGER CHECK(retry_limit=5),
  retry_next_at INTEGER CHECK(retry_next_at BETWEEN 0 AND 9007199254740991),
  retry_started_at INTEGER CHECK(retry_started_at BETWEEN 0 AND 9007199254740991),
  retry_deadline_at INTEGER CHECK(retry_deadline_at BETWEEN 0 AND 9007199254740991),
  retry_cause TEXT CHECK(retry_cause IS NULL OR json_valid(retry_cause)),
  requested_by TEXT CHECK(requested_by IS NULL OR json_valid(requested_by)),
  boundary TEXT NOT NULL CHECK(json_valid(boundary)),
  dedupe TEXT UNIQUE,
  data TEXT GENERATED ALWAYS AS (json_set(boundary,'$.id',id,'$.taskId',task_id,'$.projectId',project_id,'$.status',status,'$.trigger',trigger,'$.createdAt',created_at,'$.startedAt',started_at,'$.finishedAt',finished_at,'$.summary',summary,'$.error',error,'$.sessionId',session_id,'$.workspace',workspace,'$.workspaceCleanedAt',workspace_cleaned_at,'$.recoveryPending',json(CASE WHEN recovery_pending THEN 'true' ELSE 'false' END),'$.resumeAvailable',json(CASE WHEN resume_available THEN 'true' ELSE 'false' END),'$.resumeCount',resume_count,'$.cancelRequestedAt',cancel_requested_at,'$.accountId',account_id,'$.accountName',account_name,'$.accountWaitReason',account_wait_reason,'$.accountRequired',account_required,'$.capacityWaitUntil',capacity_wait_until,'$.requestedBy',json(requested_by),'$.retry',json(CASE WHEN retry_attempt IS NULL THEN NULL ELSE json_object('attempt',retry_attempt,'limit',retry_limit,'nextAttemptAt',retry_next_at,'startedAt',retry_started_at,'deadlineAt',retry_deadline_at,'cause',json(retry_cause)) END)) ) VIRTUAL,
  CHECK((retry_attempt IS NULL AND retry_limit IS NULL AND retry_cause IS NULL AND retry_next_at IS NULL AND retry_started_at IS NULL AND retry_deadline_at IS NULL) OR (retry_attempt IS NOT NULL AND retry_limit IS NOT NULL AND retry_cause IS NOT NULL)),
  CHECK(retry_deadline_at IS NULL OR retry_started_at IS NULL OR retry_deadline_at>=retry_started_at)
);
CREATE INDEX runs_status ON runs(status,created_at);
CREATE INDEX runs_task ON runs(task_id,created_at DESC);
CREATE INDEX runs_created ON runs(created_at DESC,id DESC);
CREATE UNIQUE INDEX runs_active_task ON runs(task_id) WHERE status IN ('queued','running','waiting');
"#;

impl Db<'_> {
    pub fn run_state(&self, id: &str) -> Result<Option<RunState>> {
        Ok(self.0.prepare_cached("SELECT id,task_id,project_id,status,trigger,created_at,started_at,finished_at,summary,error,session_id,workspace,workspace_cleaned_at,recovery_pending,resume_available,resume_count,cancel_requested_at,account_id,account_name,account_wait_reason,account_required,capacity_wait_until,retry_attempt,retry_limit,retry_next_at,retry_started_at,retry_deadline_at,retry_cause,requested_by FROM runs WHERE id=?")?
            .query_row([id], |row| Ok(RunState {
            id: row.get(0)?,
            task_id: row.get(1)?,
            project_id: row.get(2)?,
            status: RunStatus::parse(&row.get::<_, String>(3)?).ok_or_else(|| rusqlite::Error::InvalidQuery)?,
            trigger: row.get(4)?,
            created_at: row.get(5)?,
            started_at: row.get(6)?,
            finished_at: row.get(7)?,
            summary: row.get(8)?,
            error: row.get(9)?,
            session_id: row.get(10)?,
            workspace: row.get(11)?,
            workspace_cleaned_at: row.get(12)?,
            recovery_pending: row.get(13)?,
            resume_available: row.get(14)?,
            resume_count: row.get(15)?,
            cancel_requested_at: row.get(16)?,
            account_id: row.get(17)?,
            account_name: row.get(18)?,
            account_wait_reason: row.get(19)?,
            account_required: row.get::<_, Option<String>>(20)?.map(|s| serde_json::from_value(Value::String(s))).transpose().map_err(|e| rusqlite::Error::FromSqlConversionFailure(20, rusqlite::types::Type::Text, Box::new(e)))?,
            capacity_wait_until: row.get(21)?,
            requested_by: row.get::<_, Option<String>>(28)?.map(|s| serde_json::from_str(&s)).transpose().map_err(|e| rusqlite::Error::FromSqlConversionFailure(28, rusqlite::types::Type::Text, Box::new(e)))?,
            retry: match row.get::<_, Option<u32>>(22)? {
                None => None,
                Some(attempt) => Some(Retry {
                    attempt: u64::from(attempt),
                    limit: u64::from(row.get::<_, u32>(23)?),
                    next_attempt_at: row.get(24)?,
                    started_at: row.get(25)?,
                    deadline_at: row.get(26)?,
                    cause: serde_json::from_str(&row.get::<_, String>(27)?).map_err(|e| rusqlite::Error::FromSqlConversionFailure(27, rusqlite::types::Type::Text, Box::new(e)))?,
                }),
            },
            })).optional()?)
    }

    pub(crate) fn insert_run_state(
        &self,
        state: &RunState,
        boundary: &Value,
        dedupe: Option<&str>,
    ) -> Result<()> {
        self.0.prepare_cached("INSERT INTO runs(id,task_id,project_id,status,trigger,created_at,started_at,finished_at,summary,error,session_id,workspace,workspace_cleaned_at,recovery_pending,resume_available,resume_count,cancel_requested_at,account_id,account_name,account_wait_reason,account_required,capacity_wait_until,retry_attempt,retry_limit,retry_next_at,retry_started_at,retry_deadline_at,retry_cause,requested_by,boundary,dedupe) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")?.execute(params![
            state.id,
            state.task_id,
            state.project_id,
            state.status.as_str(),
            state.trigger,
            state.created_at,
            state.started_at,
            state.finished_at,
            state.summary,
            state.error,
            state.session_id,
            state.workspace,
            state.workspace_cleaned_at,
            state.recovery_pending,
            state.resume_available,
            state.resume_count,
            state.cancel_requested_at,
            state.account_id,
            state.account_name,
            state.account_wait_reason,
            state.account_required.as_ref().map(|p| match p { AgentProvider::Codex => "codex", AgentProvider::Claude => "claude", AgentProvider::Opencode => "opencode" }),
            state.capacity_wait_until,
            state.retry.as_ref().map(|r| r.attempt as i64),
            state.retry.as_ref().map(|r| r.limit as i64),
            state.retry.as_ref().and_then(|r| r.next_attempt_at),
            state.retry.as_ref().and_then(|r| r.started_at),
            state.retry.as_ref().and_then(|r| r.deadline_at),
            state.retry.as_ref().map(|r| serde_json::to_string(&r.cause)).transpose()?,
            state.requested_by.as_ref().map(serde_json::to_string).transpose()?,
            serde_json::to_string(boundary)?,
            dedupe
        ])?;
        Ok(())
    }

    /// Named worker/control commands edit lifecycle state inside their existing transaction.
    /// Payloads owned by other domains cannot be changed by this operation.
    pub fn change_run(&self, id: &str, change: RunChanges) -> Result<Value> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let result = Db(&tx).change_run(id, change)?;
            tx.commit()?;
            return Ok(result);
        }
        let mut state = required(self.run_state(id)?, "Run not found")?;
        let boundary = change.apply(&mut state)?;
        self.write_run_state(&state)?;
        if let Some(boundary) = boundary {
            self.update_run_boundary(id, &boundary)?;
        }
        required(self.run(id)?, "Run not found")
    }

    pub(crate) fn write_run_state(&self, state: &RunState) -> Result<()> {
        self.0.prepare_cached("UPDATE runs SET task_id=?,project_id=?,status=?,trigger=?,created_at=?,started_at=?,finished_at=?,summary=?,error=?,session_id=?,workspace=?,workspace_cleaned_at=?,recovery_pending=?,resume_available=?,resume_count=?,cancel_requested_at=?,account_id=?,account_name=?,account_wait_reason=?,account_required=?,capacity_wait_until=?,retry_attempt=?,retry_limit=?,retry_next_at=?,retry_started_at=?,retry_deadline_at=?,retry_cause=?,requested_by=? WHERE id=?")?.execute(params![
            state.task_id,
            state.project_id,
            state.status.as_str(),
            state.trigger,
            state.created_at,
            state.started_at,
            state.finished_at,
            state.summary,
            state.error,
            state.session_id,
            state.workspace,
            state.workspace_cleaned_at,
            state.recovery_pending,
            state.resume_available,
            state.resume_count,
            state.cancel_requested_at,
            state.account_id,
            state.account_name,
            state.account_wait_reason,
            state.account_required.as_ref().map(|p| match p { AgentProvider::Codex => "codex", AgentProvider::Claude => "claude", AgentProvider::Opencode => "opencode" }),
            state.capacity_wait_until,
            state.retry.as_ref().map(|r| r.attempt as i64),
            state.retry.as_ref().map(|r| r.limit as i64),
            state.retry.as_ref().and_then(|r| r.next_attempt_at),
            state.retry.as_ref().and_then(|r| r.started_at),
            state.retry.as_ref().and_then(|r| r.deadline_at),
            state.retry.as_ref().map(|r| serde_json::to_string(&r.cause)).transpose()?,
            state.requested_by.as_ref().map(serde_json::to_string).transpose()?,
            state.id
        ])?;
        Ok(())
    }

    // Temporary adapter for unconverted chat/node/plugin domains (A6-A13/A18).
    // It cannot merge decisions owned by the run lifecycle.
    pub fn update_run_boundary(&self, id: &str, patch: &Value) -> Result<Value> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let result = Db(&tx).update_run_boundary(id, patch)?;
            tx.commit()?;
            return Ok(result);
        }
        let fields = patch
            .as_object()
            .ok_or_else(|| Error::bad("Expected a domain payload object"))?;
        const CORE: &[&str] = &[
            "id",
            "taskId",
            "projectId",
            "status",
            "trigger",
            "createdAt",
            "startedAt",
            "finishedAt",
            "summary",
            "error",
            "sessionId",
            "workspace",
            "workspaceCleanedAt",
            "recoveryPending",
            "resumeAvailable",
            "resumeCount",
            "cancelRequestedAt",
            "accountId",
            "accountName",
            "accountWaitReason",
            "accountRequired",
            "capacityWaitUntil",
            "requestedBy",
            "retry",
        ];
        if fields.keys().any(|key| CORE.contains(&key.as_str())) {
            return Err(Error::bad("Use typed run transitions for lifecycle fields"));
        }
        let boundary: String =
            self.0
                .query_row("SELECT boundary FROM runs WHERE id=?", [id], |row| {
                    row.get(0)
                })?;
        let mut boundary: Value = serde_json::from_str(&boundary)?;
        crate::store::merge(&mut boundary, patch);
        self.0.execute(
            "UPDATE runs SET boundary=? WHERE id=?",
            params![serde_json::to_string(&boundary)?, id],
        )?;
        required(self.run(id)?, "Run not found")
    }
}

impl Store {
    pub async fn run_state(&self, id: &str) -> Result<RunState> {
        let id = id.to_owned();
        self.read(move |db| required(db.run_state(&id)?, "Run not found"))
            .await
    }

    pub async fn change_run(&self, id: &str, change: RunChanges) -> Result<Value> {
        let id = id.to_owned();
        self.transaction(move |db| db.change_run(&id, change)).await
    }

    pub async fn update_run_boundary(&self, id: &str, patch: Value) -> Result<Value> {
        let id = id.to_owned();
        self.transaction(move |db| db.update_run_boundary(&id, &patch))
            .await
    }
}
