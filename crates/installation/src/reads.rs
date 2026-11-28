//! What each Cairn account has read and snoozed (ADR-0094). A conversation turn that
//! finished stays unread in the Fil until the account opens or marks it read. Marks
//! belong to the account, so every device and the web share them, and each member reads
//! on their own.
//!
//! A conversation started by a hook rule (including a clock schedule) surfaces in
//! "For you" only when it needs someone: a pending question, a failure, or a `blocked`
//! outcome. Once a person writes in it, it follows ordinary conversation rules.
//! Any conversation can be snoozed until a later time (up to 30 days).
use crate::{
    config::now,
    error::{Error, Result},
    http::Input,
    service::Service,
    store::Db,
    validation::text,
};
use cairn_contracts::{
    ConversationSnoozeAction, MarkReadInput, MarkReadOutput, ReadTarget,
    SetConversationSnoozeInput, SetConversationSnoozeOutput,
};
use rusqlite::OptionalExtension;
use serde_json::Value;
use sql::StoredSnooze;
use std::collections::HashMap;

pub mod sql;

const MAX_ITEMS: usize = 200;
/// The longest a conversation can be snoozed.
const MAX_SNOOZE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const PREVIEW_CHARS: usize = 240;

/// The account an API request reads for. Every installation request carries one.
pub(crate) fn account(input: &Input) -> Result<&str> {
    input
        .identity
        .as_ref()
        .map(|identity| identity.account_id.as_str())
        .filter(|account| !account.is_empty())
        .ok_or_else(|| Error::unauthorized("Access through the Beacon."))
}

/// When an account first reads the installation, earlier work counts as read: the Fil starts
/// clear rather than with the whole history.
pub(crate) async fn begin(s: &Service, account: &str) -> Result<()> {
    let reader = account.to_owned();
    if s.store
        .read(move |db| db.read_start(&reader))
        .await?
        .is_some()
    {
        return Ok(());
    }
    let account = account.to_owned();
    s.store
        .transaction(move |db| db.start_reading(&account, now()))
        .await
}

/// One account's read marks and snoozed conversations. Without an account,
/// nothing is unread and nothing is snoozed: personal state is never shared.
pub(crate) struct Reads {
    since: i64,
    marks: HashMap<String, i64>,
    snoozes: HashMap<String, StoredSnooze>,
}

impl Reads {
    pub(crate) fn load(db: &Db<'_>, account: &str) -> Result<Self> {
        // Until `begin` records a start, nothing is unread.
        Ok(Self {
            since: db.read_start(account)?.unwrap_or(i64::MAX),
            marks: db.read_marks(account)?,
            snoozes: db.snoozes(account)?,
        })
    }

    pub(crate) fn load_for(db: &Db<'_>, account: Option<&str>) -> Result<Self> {
        match account {
            Some(account) if !account.is_empty() => Self::load(db, account),
            _ => Ok(Self {
                since: i64::MAX,
                marks: HashMap::new(),
                snoozes: HashMap::new(),
            }),
        }
    }

    fn read_at_mark(&self, id: &str) -> i64 {
        self.marks.get(id).copied().unwrap_or(0).max(self.since)
    }

    fn unread(&self, id: &str, finished_at: Option<i64>) -> bool {
        let read_at = self.read_at_mark(id);
        finished_at.is_some_and(|finished| finished > read_at)
    }

    /// Marks conversations the account has not acknowledged since their latest relevant
    /// activity, and attaches active snooze deadlines.
    ///
    /// A plugin's conversations answer in their own channel, so their replies wait on
    /// nobody here. A rule-started conversation without human participation only asks
    /// for attention on a pending question, a failure or a `blocked` outcome (ADR-0094).
    pub(crate) fn chats(&self, db: &Db<'_>, chats: &mut [Value]) -> Result<()> {
        let at = now();
        for chat in chats {
            let id = text(chat, "id").to_owned();
            let question = chat["pendingQuestions"].as_u64().unwrap_or(0) > 0;
            let status = chat["status"].as_str();
            let failed = matches!(status, Some("failed" | "interrupted"));
            let blocked = status == Some("succeeded") && chat["outcome"]["status"] == "blocked";
            let quiet_rule = !chat["originRule"].is_null() && chat["personParticipated"] != true;
            let finished = matches!(status, Some("succeeded" | "failed" | "interrupted"))
                && chat["requestedBy"].is_null()
                && chat["paused"] != true;
            let wants_attention = question || failed || blocked || (finished && !quiet_rule);

            let activity_at = chat["readAt"]
                .as_i64()
                .or_else(|| chat["finishedAt"].as_i64())
                .or_else(|| chat["updatedAt"].as_i64());
            let matching_snooze = self.snoozes.get(&id).copied().filter(|snooze| {
                activity_at.is_some_and(|observed| observed <= snooze.finished_at)
            });

            if let Some(snooze) = matching_snooze
                && snooze.until > at
            {
                chat["snoozedUntil"] = snooze.until.into();
                chat["unread"] = false.into();
                continue;
            }
            chat["snoozedUntil"] = Value::Null;

            // An expired snooze surfaces the conversation again until acknowledged
            // after its deadline.
            let expired_snooze_at = matching_snooze
                .filter(|snooze| snooze.until <= at)
                .map(|snooze| snooze.until);
            let effective_at = match (activity_at, expired_snooze_at) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            if let Some(snoozed_at) = expired_snooze_at
                && chat["readAt"].as_i64().is_none_or(|r| snoozed_at > r)
            {
                chat["readAt"] = snoozed_at.into();
            }

            let unread = (wants_attention || expired_snooze_at.is_some())
                && db.kv(&format!("plugin-conversation:{id}"))?.is_none()
                && self.unread(&id, effective_at);
            chat["unread"] = unread.into();
            if unread {
                chat["preview"] = summary_preview(db, text(chat, "runId"))?;
            }
        }
        Ok(())
    }
}

fn summary_preview(db: &Db<'_>, run: &str) -> Result<Value> {
    let summary =
        db.0.prepare_cached("SELECT summary FROM runs WHERE id=?")?
            .query_row([run], |row| row.get::<_, String>(0))
            .optional()?
            .map(|summary| preview(&summary));
    Ok(summary.filter(|preview| !preview.is_empty()).into())
}

/// The opening of an agent's final message as one plain line, without Markdown markers.
/// Headings end as sentences and list items read as a list, so lines do not run together.
pub(crate) fn preview(summary: &str) -> String {
    let mut lines = Vec::new();
    let mut code = false;
    for line in summary.lines() {
        let line = line.trim();
        if line.starts_with("```") {
            code = !code;
            continue;
        }
        let item = line.starts_with(['-', '*', '+']) && !line.starts_with("**");
        let heading = line.starts_with('#');
        let line = line
            .trim_start_matches(['#', '>', '-', '*', '+', ' '])
            .trim();
        if code || line.is_empty() || line.starts_with('|') {
            continue;
        }
        let words = line
            .split_whitespace()
            .map(|word| word.replace("**", "").replace("__", "").replace('`', ""))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push((words, heading, item));
    }

    let mut text = String::new();
    for (index, (line, heading, item)) in lines.iter().enumerate() {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(line);
        let next_is_item = lines.get(index + 1).is_some_and(|(_, _, next)| *next);
        if (*heading || *item) && !line.ends_with(['.', '!', '?', ':', ';', ',', '…']) {
            text.push(if *item && next_is_item { ',' } else { '.' });
        }
    }
    if text.chars().count() <= PREVIEW_CHARS {
        return text;
    }
    let cut = text.chars().take(PREVIEW_CHARS).collect::<String>();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    format!("{}…", cut.trim_end_matches([',', ';', ':', '.']))
}

/// Records the completion the account actually saw, so opening an older run or a delayed
/// request cannot consume newer work. Marking read also clears any snooze on that conversation.
pub(crate) async fn mark(
    s: &Service,
    account: &str,
    input: MarkReadInput,
) -> Result<MarkReadOutput> {
    if input.items.is_empty() || input.items.len() > MAX_ITEMS {
        return Err(Error::bad("List between 1 and 200 items to mark as read."));
    }
    let read_at = now();
    if input
        .items
        .iter()
        .any(|item| item.finished_at <= 0 || item.finished_at > read_at)
    {
        return Err(Error::bad("Include the finish time of the work you read."));
    }
    let read = input.items.len() as u32;
    let account = account.to_owned();
    s.store
        .transaction(move |db| {
            db.start_reading(&account, read_at)?;
            for item in &input.items {
                let ReadTarget::Chat { chat_id } = &item.target;
                let id = chat_id.to_string();
                db.mark_read(&account, &id, item.finished_at)?;
                db.clear_snooze_through(&account, &id, item.finished_at)?;
            }
            Ok(())
        })
        .await?;
    Ok(MarkReadOutput { read, read_at })
}

/// Snoozes a conversation until a later time (up to 30 days), or brings it back early.
pub(crate) async fn snooze(
    s: &Service,
    account: &str,
    input: SetConversationSnoozeInput,
) -> Result<SetConversationSnoozeOutput> {
    let at = now();
    let snooze = match input.action {
        ConversationSnoozeAction::Restore => None,
        ConversationSnoozeAction::Snooze { finished_at, until } => {
            if finished_at <= 0 || finished_at > at {
                return Err(Error::bad(
                    "Include the observed activity time of the conversation you snooze.",
                ));
            }
            if until <= at || until > at + MAX_SNOOZE_MS {
                return Err(Error::bad("Snooze a conversation for up to 30 days."));
            }
            Some(StoredSnooze { finished_at, until })
        }
    };
    let account = account.to_owned();
    let chat = input.chat_id.to_string();
    s.store
        .transaction(move |db| {
            db.start_reading(&account, at)?;
            db.set_snooze(&account, &chat, snooze)?;
            if snooze.is_none() {
                db.clear_read_mark(&account, &chat)?;
            }
            Ok(())
        })
        .await?;
    Ok(SetConversationSnoozeOutput {
        chat_id: input.chat_id,
        snoozed_until: snooze.map(|s| s.until),
    })
}

#[cfg(test)]
mod tests {
    use super::preview;

    #[tokio::test]
    async fn unread_previews_do_not_read_execution_snapshots() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store.transaction(|db| {
            let state = serde_json::from_value(serde_json::json!({
                "id": "run", "taskId": "task", "status": "succeeded", "summary": "## Finished\n\nSaved **answer**.",
            }))?;
            db.admit_run(&state, None, &serde_json::json!({}), None)?;
            Ok(())
        }).await.unwrap();
        let connection = rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
        let columns = connection
            .prepare("PRAGMA table_xinfo(runs)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|name| {
                let name = name.unwrap();
                if name == "snapshot" {
                    "snapshot_probe(snapshot) AS snapshot".to_owned()
                } else {
                    format!("\"{name}\"")
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        connection.execute_batch(&format!("ALTER TABLE runs RENAME TO retained_runs; CREATE VIEW runs AS SELECT {columns} FROM retained_runs;")).unwrap();
        connection.execute_batch("DROP VIEW run_views").unwrap();
        connection
            .execute_batch(crate::runs::PROJECTION_SCHEMA)
            .unwrap();
        connection
            .create_scalar_function(
                "snapshot_probe",
                1,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                |_| -> rusqlite::Result<String> {
                    Err(rusqlite::Error::UserFunctionError(Box::new(
                        std::io::Error::other("snapshot was read"),
                    )))
                },
            )
            .unwrap();
        let db = crate::store::Db(&connection);
        assert_eq!(
            super::summary_preview(&db, "run").unwrap(),
            "Finished. Saved answer."
        );
        assert!(super::summary_preview(&db, "missing").unwrap().is_null());
        assert_eq!(db.runs(None, None, 10, 0, false).unwrap().len(), 1);
        assert_eq!(db.run_page(None, None, 10, 0).unwrap().len(), 1);
        assert!(
            db.runs(None, None, 10, 0, true).is_err(),
            "the full SQL wire view must exercise the probe"
        );
        assert!(
            db.run("run").is_err(),
            "the full run read must exercise the probe"
        );
    }

    #[test]
    fn previews_read_as_one_plain_line() {
        let summary = "## Done\n\nI **fixed** the `parser`:\n\n- added tests\n- updated docs\n\n```rust\nfn main() {}\n```\n| a | b |";
        assert_eq!(
            preview(summary),
            "Done. I fixed the parser: added tests, updated docs."
        );
    }

    #[test]
    fn long_previews_end_on_a_word() {
        let summary = "word ".repeat(100);
        let result = preview(&summary);
        assert!(result.ends_with("word…"));
        assert!(result.chars().count() <= 241);
    }
}
