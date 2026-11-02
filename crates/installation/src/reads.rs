//! What each Cairn account has read. A conversation turn or a mission run that finished
//! successfully stays unread in the Fil until the account opens it. Marks belong to the
//! account, so every device and the web share them, and each member reads on their own.
//!
//! A mission that reports it is blocked asks for attention until the account sets that outcome
//! aside: dismissed for good, or snoozed until a time. Both last until the mission finishes again.
use crate::{
    config::now,
    error::{Error, Result},
    http::Input,
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::text,
};
use cairn_contracts::{
    MarkReadInput, MarkReadOutput, OutcomeAsideAction, OutcomeDismissal, ReadTarget,
    SetOutcomeAsideInput, SetOutcomeAsideOutput,
};
use rusqlite::OptionalExtension;
use serde_json::Value;
use sql::{ReadKind, StoredDismissal};
use std::collections::HashMap;

pub(crate) mod migration;
pub mod sql;

const MAX_ITEMS: usize = 200;
/// The longest a blocked mission can be snoozed.
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

/// One account's read marks and the mission outcomes it set aside. Without an account,
/// nothing is unread and nothing is set aside: personal state is never shared.
pub(crate) struct Reads {
    since: i64,
    marks: HashMap<(ReadKind, String), i64>,
    dismissals: HashMap<String, StoredDismissal>,
}

impl Reads {
    pub(crate) fn load(db: &Db<'_>, account: &str) -> Result<Self> {
        // Until `begin` records a start, nothing is unread.
        Ok(Self {
            since: db.read_start(account)?.unwrap_or(i64::MAX),
            marks: db.read_marks(account)?,
            dismissals: db.dismissals(account)?,
        })
    }

    pub(crate) fn load_for(db: &Db<'_>, account: Option<&str>) -> Result<Self> {
        match account {
            Some(account) => Self::load(db, account),
            None => Ok(Self {
                since: i64::MAX,
                marks: HashMap::new(),
                dismissals: HashMap::new(),
            }),
        }
    }

    fn unread(&self, kind: ReadKind, id: &str, finished_at: Option<i64>) -> bool {
        let read_at = self
            .marks
            .get(&(kind, id.to_owned()))
            .copied()
            .unwrap_or(0)
            .max(self.since);
        finished_at.is_some_and(|finished| finished > read_at)
    }

    /// Marks finished conversation turns the account has not opened since, with the agent's
    /// last words as a preview. A plugin's conversations answer in their own channel, so their
    /// replies wait on nobody here.
    pub(crate) fn chats(&self, db: &Db<'_>, chats: &mut [Value]) -> Result<()> {
        for chat in chats {
            let id = text(chat, "id");
            let finished = chat["status"] == RunStatus::Succeeded
                && chat["paused"] != true
                && db.kv(&format!("plugin-conversation:{id}"))?.is_none();
            let unread = finished && self.unread(ReadKind::Chat, id, chat["finishedAt"].as_i64());
            chat["unread"] = unread.into();
            if unread {
                chat["preview"] = summary_preview(db, text(chat, "runId"))?;
            }
        }
        Ok(())
    }

    /// Marks mission runs that finished after the account last opened their mission, and the
    /// outcomes it set aside.
    pub(crate) fn runs(&self, db: &Db<'_>, runs: &mut [Value]) -> Result<()> {
        for run in runs {
            let mission = run["trigger"] != "chat" && run["status"] == RunStatus::Succeeded;
            let task = text(run, "taskId").to_owned();
            let unread =
                mission && self.unread(ReadKind::Mission, &task, run["finishedAt"].as_i64());
            run["unread"] = unread.into();
            if unread {
                run["preview"] = summary_preview(db, text(run, "id"))?;
            }
            run["dismissal"] = serde_json::to_value(self.dismissal(&task, run))?;
        }
        Ok(())
    }

    /// How the account set this run's outcome aside, if it still applies. A later finish asks
    /// again, and input the agent asked for can only be snoozed.
    fn dismissal(&self, task: &str, run: &Value) -> Option<OutcomeDismissal> {
        let waiting = run["trigger"] != "chat"
            && run["status"] == RunStatus::Succeeded
            && run["outcome"]["status"]
                .as_str()
                .is_some_and(|status| status != "completed");
        let dismissal = self.dismissals.get(task).filter(|dismissal| {
            waiting
                && run["finishedAt"]
                    .as_i64()
                    .is_some_and(|finished| finished <= dismissal.finished_at)
        })?;
        if dismissal.until.is_none() && run["outcome"]["status"] == "needs_input" {
            return None;
        }
        Some(OutcomeDismissal {
            until: dismissal.until,
        })
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
/// request cannot consume newer work.
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
                let (kind, id) = match &item.target {
                    ReadTarget::Chat { chat_id } => (ReadKind::Chat, chat_id.to_string()),
                    ReadTarget::Mission { task_id } => (ReadKind::Mission, task_id.to_string()),
                };
                db.mark_read(&account, kind, &id, item.finished_at)?;
            }
            Ok(())
        })
        .await?;
    Ok(MarkReadOutput { read, read_at })
}

/// Sets a blocked mission outcome aside for the account, or brings it back.
pub(crate) async fn set_aside(
    s: &Service,
    account: &str,
    input: SetOutcomeAsideInput,
) -> Result<SetOutcomeAsideOutput> {
    let at = now();
    let finished = |finished_at: i64| {
        if finished_at <= 0 || finished_at > at {
            return Err(Error::bad(
                "Include the finish time of the outcome you set aside.",
            ));
        }
        Ok(finished_at)
    };
    let dismissal = match input.action {
        OutcomeAsideAction::Restore => None,
        OutcomeAsideAction::Dismiss { finished_at } => Some(StoredDismissal {
            finished_at: finished(finished_at)?,
            until: None,
        }),
        OutcomeAsideAction::Snooze { finished_at, until } => {
            if until <= at || until > at + MAX_SNOOZE_MS {
                return Err(Error::bad("Snooze a mission for up to 30 days."));
            }
            Some(StoredDismissal {
                finished_at: finished(finished_at)?,
                until: Some(until),
            })
        }
    };
    let account = account.to_owned();
    let task = input.task_id.to_string();
    s.store
        .transaction(move |db| db.set_dismissal(&account, &task, dismissal))
        .await?;
    Ok(SetOutcomeAsideOutput {
        task_id: input.task_id,
        dismissal: dismissal.map(|dismissal| OutcomeDismissal {
            until: dismissal.until,
        }),
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
        // SQLite retargets dependent views when the physical table is renamed.
        // Rebind the wire view so this probe also guards its snapshot access.
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
