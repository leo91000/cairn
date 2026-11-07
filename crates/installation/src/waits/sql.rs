//! Current durable continuation SQL. Plugin input and typed result snapshots are the JSON boundaries.
use crate::{
    error::{Error, Result},
    store::Db,
};
use cairn_contracts::{
    ChatWakeup, PluginWaitData, TimeCondition, WaitCondition, WaitMemberProgress,
};
use rusqlite::{OptionalExtension, Row, params};
use std::collections::HashMap;

pub(crate) const SCHEMA: &str = "
CREATE TABLE durable_wakeups(
    key INTEGER PRIMARY KEY,
    chat_id TEXT REFERENCES conversations(id) ON DELETE CASCADE,
    mission_run_id TEXT REFERENCES runs(id) ON DELETE CASCADE,
    id TEXT NOT NULL CHECK(length(id)>0),
    message_id TEXT,
    reason TEXT NOT NULL,
    seconds INTEGER NOT NULL CHECK(seconds BETWEEN 1 AND 86400),
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    wake_at INTEGER NOT NULL CHECK(wake_at BETWEEN 0 AND 9007199254740991),
    group_mode TEXT CHECK(group_mode IN ('all','any')),
    time_kind TEXT CHECK(time_kind IN ('delay','at')),
    time_seconds INTEGER CHECK(time_seconds BETWEEN 1 AND 86400),
    time_date TEXT,
    observation TEXT CHECK(observation IS NULL OR json_valid(observation)),
    display TEXT CHECK(display IS NULL OR json_valid(display)),
    next_check_at INTEGER CHECK(next_check_at BETWEEN 0 AND 9007199254740991),
    last_error TEXT,
    CHECK((chat_id IS NOT NULL) != (mission_run_id IS NOT NULL)),
    CHECK((time_kind IS NULL AND time_seconds IS NULL AND time_date IS NULL) OR (time_kind='delay' AND time_seconds IS NOT NULL AND time_date IS NULL) OR (time_kind='at' AND time_date IS NOT NULL AND time_seconds IS NULL))
);
CREATE UNIQUE INDEX durable_wakeups_chat ON durable_wakeups(chat_id) WHERE chat_id IS NOT NULL;
CREATE UNIQUE INDEX durable_wakeups_mission ON durable_wakeups(mission_run_id) WHERE mission_run_id IS NOT NULL;
CREATE TABLE wait_conditions(
    wakeup_key INTEGER NOT NULL REFERENCES durable_wakeups(key) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position>=0),
    kind TEXT NOT NULL CHECK(kind IN ('run_finished','task_finished','plugin')),
    target_run_id TEXT,
    target_chat_id TEXT,
    plugin_id TEXT,
    condition_type TEXT,
    project_id TEXT,
    plugin_input TEXT CHECK(plugin_input IS NULL OR (json_valid(plugin_input) AND json_type(plugin_input)='object')),
    observation TEXT CHECK(observation IS NULL OR json_valid(observation)),
    display TEXT CHECK(display IS NULL OR json_valid(display)),
    next_check_at INTEGER CHECK(next_check_at BETWEEN 0 AND 9007199254740991),
    last_error TEXT,
    CHECK((kind='run_finished' AND target_run_id IS NOT NULL AND target_chat_id IS NULL AND plugin_id IS NULL AND condition_type IS NULL AND project_id IS NULL AND plugin_input IS NULL)
       OR (kind='task_finished' AND target_chat_id IS NOT NULL AND target_run_id IS NULL AND plugin_id IS NULL AND condition_type IS NULL AND project_id IS NULL AND plugin_input IS NULL)
       OR (kind='plugin' AND target_run_id IS NULL AND target_chat_id IS NULL AND plugin_id IS NOT NULL AND condition_type IS NOT NULL AND plugin_input IS NOT NULL)),
    PRIMARY KEY(wakeup_key,position)
) WITHOUT ROWID;
CREATE TABLE conversation_lineage(
    chat_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    requester_chat_id TEXT NOT NULL,
    requester_run_id TEXT NOT NULL,
    request_id TEXT
) WITHOUT ROWID;
-- One index serves requester lists and idempotent request lookup. SQLite keeps
-- NULL request IDs distinct, so unkeyed delegations remain unrestricted.
CREATE UNIQUE INDEX conversation_lineage_request ON conversation_lineage(requester_chat_id,request_id);
";

fn decode<T: serde::de::DeserializeOwned>(
    row: &Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<T>> {
    row.get::<_, Option<String>>(index)?
        .map(|s| {
            serde_json::from_str(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })
        .transpose()
}

fn progress(row: &Row<'_>, start: usize) -> rusqlite::Result<WaitMemberProgress> {
    Ok(WaitMemberProgress {
        observation: decode(row, start)?,
        display: decode(row, start + 1)?,
        next_check_at: row.get(start + 2)?,
        last_error: row.get(start + 3)?,
    })
}

const WAKEUP_COLUMNS: &str = "w.id,w.message_id,w.reason,w.seconds,w.created_at,w.wake_at,w.group_mode,w.time_kind,w.time_seconds,w.time_date,w.observation,w.display,w.next_check_at,w.last_error";
const CONDITION_COLUMNS: &str = "c.kind,c.target_run_id,c.target_chat_id,c.plugin_id,c.condition_type,c.project_id,c.plugin_input,c.observation,c.display,c.next_check_at,c.last_error";

fn wakeup_row(row: &Row<'_>) -> rusqlite::Result<(String, Option<String>, ChatWakeup)> {
    let time_kind: Option<String> = row.get(8)?;
    let time_condition = match time_kind.as_deref() {
        Some("delay") => Some(TimeCondition::Delay {
            seconds: row.get(9)?,
        }),
        Some("at") => Some(TimeCondition::At { date: row.get(10)? }),
        _ => None,
    };
    Ok((
        row.get(0)?,
        row.get(7)?,
        ChatWakeup {
            id: row.get(1)?,
            message_id: row.get(2)?,
            reason: row.get(3)?,
            seconds: row.get(4)?,
            created_at: row.get(5)?,
            wake_at: row.get(6)?,
            condition: None,
            time_condition,
            members: None,
            progress: progress(row, 11)?,
        },
    ))
}

fn condition_row(
    row: &Row<'_>,
    start: usize,
) -> rusqlite::Result<(WaitCondition, WaitMemberProgress)> {
    let kind: String = row.get(start)?;
    let condition = match kind.as_str() {
        "run_finished" => WaitCondition::RunFinished {
            run_id: row.get(start + 1)?,
        },
        "task_finished" => WaitCondition::TaskFinished {
            chat_id: row.get(start + 2)?,
        },
        _ => WaitCondition::Plugin {
            plugin_id: row.get(start + 3)?,
            condition_type: row.get(start + 4)?,
            project_id: row.get(start + 5)?,
            input: decode::<PluginWaitData>(row, start + 6)?.unwrap(),
        },
    };
    Ok((condition, progress(row, start + 7)?))
}

fn attach_conditions(
    wakeup: &mut ChatWakeup,
    mode: Option<String>,
    slots: Vec<(WaitCondition, WaitMemberProgress)>,
) {
    if let Some(mode) = mode {
        let (conditions, progress) = slots.into_iter().unzip();
        wakeup.condition = Some(WaitCondition::Group { mode, conditions });
        wakeup.members = Some(progress);
    } else {
        wakeup.condition = slots.into_iter().next().map(|(condition, _)| condition);
    }
}

impl Db<'_> {
    pub fn wakeup(&self, chat: bool, id: &str) -> Result<Option<ChatWakeup>> {
        let owner = if chat { "chat_id" } else { "mission_run_id" };
        // One statement gives point readers an atomic header/member snapshot.
        // Integer child keys avoid repeating external UUIDs in every condition.
        let query = format!(
            "SELECT w.{owner},{WAKEUP_COLUMNS},{CONDITION_COLUMNS} FROM durable_wakeups w LEFT JOIN wait_conditions c ON c.wakeup_key=w.key WHERE w.{owner}=? ORDER BY c.position"
        );
        let mut statement = self.0.prepare_cached(&query)?;
        let mut rows = statement.query([id])?;
        let mut loaded = None;
        let mut mode = None;
        let mut slots = Vec::new();
        while let Some(row) = rows.next()? {
            if loaded.is_none() {
                let (_, group, wakeup) = wakeup_row(row)?;
                loaded = Some(wakeup);
                mode = group;
            }
            if row.get::<_, Option<String>>(15)?.is_some() {
                slots.push(condition_row(row, 15)?);
            }
        }
        if let Some(wakeup) = &mut loaded {
            attach_conditions(wakeup, mode, slots);
        }
        Ok(loaded)
    }

    pub fn wakeups(&self, chat: bool, id: Option<&str>) -> Result<HashMap<String, ChatWakeup>> {
        if let Some(id) = id {
            return Ok(self
                .wakeup(chat, id)?
                .into_iter()
                .map(|w| (id.to_owned(), w))
                .collect());
        }
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let result = Db(&tx).wakeups(chat, None)?;
            tx.commit()?;
            return Ok(result);
        }
        let owner = if chat { "chat_id" } else { "mission_run_id" };
        let query = format!(
            "SELECT w.{owner},{WAKEUP_COLUMNS} FROM durable_wakeups w WHERE w.{owner} IS NOT NULL"
        );
        let mut rows = self
            .0
            .prepare_cached(&query)?
            .query_map([], wakeup_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.is_empty() {
            return Ok(HashMap::new());
        }
        let mut conditions = HashMap::<String, Vec<(WaitCondition, WaitMemberProgress)>>::new();
        let query = format!(
            "SELECT w.{owner},{CONDITION_COLUMNS} FROM durable_wakeups w JOIN wait_conditions c ON c.wakeup_key=w.key WHERE w.{owner} IS NOT NULL ORDER BY w.{owner},c.position"
        );
        let mut statement = self.0.prepare_cached(&query)?;
        for value in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, condition_row(row, 1)?))
        })? {
            let (id, slot) = value?;
            conditions.entry(id).or_default().push(slot);
        }
        for (id, mode, wakeup) in &mut rows {
            attach_conditions(
                wakeup,
                mode.take(),
                conditions.remove(id).unwrap_or_default(),
            );
        }
        Ok(rows
            .into_iter()
            .map(|(id, _, wakeup)| (id, wakeup))
            .collect())
    }

    pub fn save_wakeup(
        &self,
        chat: bool,
        owner_id: &str,
        wakeup: Option<&ChatWakeup>,
    ) -> Result<()> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            Db(&tx).save_wakeup(chat, owner_id, wakeup)?;
            tx.commit()?;
            return Ok(());
        }
        self.0.execute_batch("SAVEPOINT durable_wait_write")?;
        let before = self.0.total_changes();
        let discarded_before = self.discarded_change_count()?;
        let result = self.write_wakeup(chat, owner_id, wakeup);
        match result {
            Ok(()) => {
                self.0.execute_batch("RELEASE durable_wait_write")?;
                Ok(())
            }
            Err(error) => {
                self.0
                    .execute_batch("ROLLBACK TO durable_wait_write; RELEASE durable_wait_write")?;
                self.discard_savepoint_changes(before, discarded_before)?;
                Err(error)
            }
        }
    }

    fn write_wakeup(&self, chat: bool, owner_id: &str, wakeup: Option<&ChatWakeup>) -> Result<()> {
        let owner = if chat { "chat_id" } else { "mission_run_id" };
        self.0.execute(
            &format!("DELETE FROM durable_wakeups WHERE {owner}=?"),
            [owner_id],
        )?;
        let Some(w) = wakeup else {
            return Ok(());
        };
        let (mode, conditions) = match &w.condition {
            Some(WaitCondition::Group { mode, conditions }) => {
                if !matches!(mode.as_str(), "all" | "any")
                    || conditions.len() < 2
                    || w.members
                        .as_ref()
                        .is_none_or(|m| m.len() != conditions.len())
                {
                    return Err(Error::bad("Invalid wait group."));
                }
                (Some(mode.as_str()), conditions.iter().collect::<Vec<_>>())
            }
            Some(condition) => (None, vec![condition]),
            None => (None, Vec::new()),
        };
        let (time_kind, time_seconds, time_date) = match &w.time_condition {
            Some(TimeCondition::Delay { seconds }) => (Some("delay"), Some(*seconds), None),
            Some(TimeCondition::At { date }) => (Some("at"), None, Some(date.as_str())),
            None => (None, None, None),
        };
        self.0
            .prepare_cached(
                "INSERT INTO durable_wakeups(chat_id,mission_run_id,id,message_id,reason,seconds,created_at,wake_at,group_mode,time_kind,time_seconds,time_date,observation,display,next_check_at,last_error) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            )?
            .execute(params![
                chat.then_some(owner_id),
                (!chat).then_some(owner_id),
                w.id,
                w.message_id,
                w.reason,
                w.seconds,
                w.created_at,
                w.wake_at,
                mode,
                time_kind,
                time_seconds,
                time_date,
                w.progress
                    .observation
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                w.progress
                    .display
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                w.progress.next_check_at,
                w.progress.last_error,
            ])?;
        let wakeup_key = self.0.last_insert_rowid();
        let empty_progress = WaitMemberProgress::default();
        for (position, condition) in conditions.into_iter().enumerate() {
            let (condition_kind, run, task, plugin, condition_type, project, input) =
                match condition {
                    WaitCondition::RunFinished { run_id } => {
                        ("run_finished", Some(run_id), None, None, None, None, None)
                    }
                    WaitCondition::TaskFinished { chat_id } => {
                        ("task_finished", None, Some(chat_id), None, None, None, None)
                    }
                    WaitCondition::Plugin {
                        plugin_id,
                        condition_type,
                        project_id,
                        input,
                    } => (
                        "plugin",
                        None,
                        None,
                        Some(plugin_id),
                        Some(condition_type),
                        project_id.as_ref(),
                        Some(serde_json::to_string(input)?),
                    ),
                    WaitCondition::Group { .. } => {
                        return Err(Error::bad("Nested wait groups are unsupported."));
                    }
                };
            let progress = w
                .members
                .as_ref()
                .and_then(|m| m.get(position))
                .unwrap_or(&empty_progress);
            self.0
                .prepare_cached("INSERT INTO wait_conditions VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)")?
                .execute(params![
                    wakeup_key,
                    position as i64,
                    condition_kind,
                    run,
                    task,
                    plugin,
                    condition_type,
                    project,
                    input,
                    progress
                        .observation
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                    progress
                        .display
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                    progress.next_check_at,
                    progress.last_error,
                ])?;
        }
        Ok(())
    }
}

impl Db<'_> {
    pub fn lineage(&self, id: &str) -> Result<Option<cairn_contracts::ConversationRequester>> {
        Ok(self.0.prepare_cached("SELECT requester_chat_id,requester_run_id,request_id FROM conversation_lineage WHERE chat_id=?")?
            .query_row([id],|row|Ok(cairn_contracts::ConversationRequester {chat_id:row.get(0)?,run_id:row.get(1)?,request_id:row.get(2)?})).optional()?)
    }

    pub fn lineages(&self) -> Result<HashMap<String, cairn_contracts::ConversationRequester>> {
        Ok(self.0.prepare_cached("SELECT chat_id,requester_chat_id,requester_run_id,request_id FROM conversation_lineage")?
            .query_map([],|row|Ok((row.get(0)?,cairn_contracts::ConversationRequester {chat_id:row.get(1)?,run_id:row.get(2)?,request_id:row.get(3)?})))?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_lineage(
        &self,
        id: &str,
        requester: Option<&cairn_contracts::ConversationRequester>,
    ) -> Result<()> {
        if let Some(requester) = requester {
            self.0.prepare_cached("INSERT INTO conversation_lineage VALUES(?,?,?,?) ON CONFLICT(chat_id) DO UPDATE SET requester_chat_id=excluded.requester_chat_id,requester_run_id=excluded.requester_run_id,request_id=excluded.request_id")?
                .execute(params![id,requester.chat_id,requester.run_id,requester.request_id])?;
        } else {
            self.0
                .execute("DELETE FROM conversation_lineage WHERE chat_id=?", [id])?;
        }
        Ok(())
    }
}
