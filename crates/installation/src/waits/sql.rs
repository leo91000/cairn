//! Current durable continuation SQL. Plugin input and typed result snapshots are the JSON boundaries.
use crate::{
    error::{Error, Result},
    store::Db,
};
use cairn_contracts::{
    ChatWakeup, PluginWaitData, TimeCondition, WaitCondition, WaitMemberProgress,
};
use rusqlite::{Row, params};
use std::collections::HashMap;

pub(crate) const SCHEMA: &str = "
CREATE TABLE durable_wakeups(
    owner_kind TEXT NOT NULL CHECK(owner_kind IN ('chat','mission')),
    owner_id TEXT NOT NULL,
    chat_id TEXT UNIQUE REFERENCES conversations(id) ON DELETE CASCADE,
    mission_run_id TEXT UNIQUE REFERENCES runs(id) ON DELETE CASCADE,
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
    CHECK((owner_kind='chat' AND chat_id=owner_id AND mission_run_id IS NULL) OR (owner_kind='mission' AND mission_run_id=owner_id AND chat_id IS NULL)),
    CHECK((time_kind IS NULL AND time_seconds IS NULL AND time_date IS NULL) OR (time_kind='delay' AND time_seconds IS NOT NULL AND time_date IS NULL) OR (time_kind='at' AND time_date IS NOT NULL AND time_seconds IS NULL)),
    PRIMARY KEY(owner_kind,owner_id)
) WITHOUT ROWID;
CREATE INDEX durable_wakeups_deadline ON durable_wakeups(wake_at,owner_kind,owner_id);
CREATE TABLE wait_conditions(
    owner_kind TEXT NOT NULL,
    owner_id TEXT NOT NULL,
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
    PRIMARY KEY(owner_kind,owner_id,position),
    FOREIGN KEY(owner_kind,owner_id) REFERENCES durable_wakeups(owner_kind,owner_id) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE INDEX wait_conditions_run ON wait_conditions(target_run_id) WHERE target_run_id IS NOT NULL;
CREATE INDEX wait_conditions_chat ON wait_conditions(target_chat_id) WHERE target_chat_id IS NOT NULL;
CREATE TABLE conversation_lineage(
    chat_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    requester_chat_id TEXT NOT NULL,
    requester_run_id TEXT NOT NULL,
    request_id TEXT
) WITHOUT ROWID;
CREATE INDEX conversation_lineage_requester ON conversation_lineage(requester_chat_id,chat_id);
CREATE UNIQUE INDEX conversation_lineage_request ON conversation_lineage(requester_chat_id,request_id) WHERE request_id IS NOT NULL;
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

impl Db<'_> {
    pub fn wakeup(&self, chat: bool, id: &str) -> Result<Option<ChatWakeup>> {
        Ok(self.wakeups(chat, Some(id))?.remove(id))
    }

    pub fn wakeups(&self, chat: bool, id: Option<&str>) -> Result<HashMap<String, ChatWakeup>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let result = Db(&tx).wakeups(chat, id)?;
            tx.commit()?;
            return Ok(result);
        }
        let kind = if chat { "chat" } else { "mission" };
        let mut rows = self.0.prepare_cached("SELECT owner_id,id,message_id,reason,seconds,created_at,wake_at,group_mode,time_kind,time_seconds,time_date,observation,display,next_check_at,last_error FROM durable_wakeups WHERE owner_kind=?1 AND (?2 IS NULL OR owner_id=?2)")?
            .query_map(params![kind,id], |r| {
                let time_kind: Option<String> = r.get(8)?;
                let time_condition = match time_kind.as_deref() {
                    Some("delay") => Some(TimeCondition::Delay {seconds:r.get(9)?}),
                    Some("at") => Some(TimeCondition::At {date:r.get(10)?}),
                    _ => None,
                };
                Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(7)?,ChatWakeup {
                    id:r.get(1)?,message_id:r.get(2)?,reason:r.get(3)?,seconds:r.get(4)?,created_at:r.get(5)?,wake_at:r.get(6)?,
                    condition:None,time_condition,members:None,progress:progress(r,11)?,
                }))
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut conditions = HashMap::<String, Vec<(WaitCondition, WaitMemberProgress)>>::new();
        let mut statement = self.0.prepare_cached("SELECT owner_id,kind,target_run_id,target_chat_id,plugin_id,condition_type,project_id,plugin_input,observation,display,next_check_at,last_error FROM wait_conditions WHERE owner_kind=?1 AND (?2 IS NULL OR owner_id=?2) ORDER BY owner_id,position")?;
        for value in statement.query_map(params![kind, id], |r| {
            let kind: String = r.get(1)?;
            let condition = match kind.as_str() {
                "run_finished" => WaitCondition::RunFinished { run_id: r.get(2)? },
                "task_finished" => WaitCondition::TaskFinished { chat_id: r.get(3)? },
                _ => WaitCondition::Plugin {
                    plugin_id: r.get(4)?,
                    condition_type: r.get(5)?,
                    project_id: r.get(6)?,
                    input: decode::<PluginWaitData>(r, 7)?.unwrap(),
                },
            };
            Ok((r.get::<_, String>(0)?, condition, progress(r, 8)?))
        })? {
            let (id, condition, progress) = value?;
            conditions
                .entry(id)
                .or_default()
                .push((condition, progress));
        }
        for (id, mode, wakeup) in &mut rows {
            let slots = conditions.remove(id).unwrap_or_default();
            if let Some(mode) = mode {
                let (conditions, progress) = slots.into_iter().unzip();
                wakeup.condition = Some(WaitCondition::Group {
                    mode: mode.clone(),
                    conditions,
                });
                wakeup.members = Some(progress);
            } else {
                wakeup.condition = slots.into_iter().next().map(|(condition, _)| condition);
            }
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
        let result = self.write_wakeup(chat, owner_id, wakeup);
        match result {
            Ok(()) => {
                self.0.execute_batch("RELEASE durable_wait_write")?;
                Ok(())
            }
            Err(error) => {
                self.0
                    .execute_batch("ROLLBACK TO durable_wait_write; RELEASE durable_wait_write")?;
                let discarded = self.0.total_changes() - before;
                if discarded>0&&self.0.query_row("SELECT EXISTS(SELECT 1 FROM pragma_function_list WHERE name='cairn_discarded_changes')",[],|r|r.get::<_,bool>(0))? {
                    self.0.query_row("SELECT cairn_discarded_changes(?)",[i64::try_from(discarded).unwrap_or(i64::MAX)],|_|Ok(()))?;
                }
                Err(error)
            }
        }
    }

    fn write_wakeup(&self, chat: bool, owner_id: &str, wakeup: Option<&ChatWakeup>) -> Result<()> {
        let kind = if chat { "chat" } else { "mission" };
        self.0.execute(
            "DELETE FROM durable_wakeups WHERE owner_kind=? AND owner_id=?",
            params![kind, owner_id],
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
                "INSERT INTO durable_wakeups VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            )?
            .execute(params![
                kind,
                owner_id,
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
                .prepare_cached("INSERT INTO wait_conditions VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)")?
                .execute(params![
                    kind,
                    owner_id,
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
    pub fn lineages(
        &self,
        id: Option<&str>,
    ) -> Result<HashMap<String, cairn_contracts::ConversationRequester>> {
        Ok(self.0.prepare_cached("SELECT chat_id,requester_chat_id,requester_run_id,request_id FROM conversation_lineage WHERE ?1 IS NULL OR chat_id=?1")?
            .query_map([id],|r|Ok((r.get(0)?,cairn_contracts::ConversationRequester {chat_id:r.get(1)?,run_id:r.get(2)?,request_id:r.get(3)?})))?.collect::<rusqlite::Result<_>>()?)
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
