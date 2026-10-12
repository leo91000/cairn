//! What each Cairn account has read. A conversation turn or a mission run that finished
//! successfully stays unread in the Fil until the account opens it. Marks belong to the
//! account, so every device and the web share them, and each member reads on their own.
use crate::{
    config::now,
    error::{Error, Result},
    http::Input,
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use serde_json::{Value, json};
use std::collections::HashMap;

const MAX_ITEMS: usize = 200;
const PREVIEW_CHARS: usize = 240;

fn since_key(account: &str) -> String {
    format!("read-since:{account}")
}

fn marks_prefix(account: &str) -> String {
    format!("read:{account}:")
}

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
    let key = since_key(account);
    if s.store.kv(&key).await?.is_some() {
        return Ok(());
    }
    s.store
        .transaction(move |db| {
            if db.kv(&key)?.is_none() {
                db.set(&key, &now().into(), None)?;
            }
            Ok(())
        })
        .await
}

/// One account's read marks, keyed by Fil item (`chat:<id>` or `task:<id>`).
pub(crate) struct Reads {
    since: i64,
    marks: HashMap<String, i64>,
}

impl Reads {
    pub(crate) fn load(db: &Db<'_>, account: &str) -> Result<Self> {
        // Until `begin` records a start, nothing is unread.
        let since = db.kv_as::<i64>(&since_key(account))?.unwrap_or(i64::MAX);
        let prefix = marks_prefix(account);
        let marks = db
            .keys_as::<i64>(&prefix)?
            .into_iter()
            .map(|(key, at)| (key[prefix.len()..].to_owned(), at))
            .collect();
        Ok(Self { since, marks })
    }

    fn unread(&self, item: &str, finished_at: Option<i64>) -> bool {
        let read_at = self.marks.get(item).copied().unwrap_or(0).max(self.since);
        finished_at.is_some_and(|finished| finished > read_at)
    }

    /// Marks finished conversation turns the account has not opened since, with the agent's
    /// last words as a preview.
    pub(crate) fn chats(&self, db: &Db<'_>, chats: &mut [Value]) -> Result<()> {
        for chat in chats {
            let finished = chat["status"] == RunStatus::Succeeded && chat["paused"] != true;
            let item = format!("chat:{}", text(chat, "id"));
            let unread = finished && self.unread(&item, chat["finishedAt"].as_i64());
            chat["unread"] = unread.into();
            if unread {
                chat["preview"] = summary_preview(db, text(chat, "runId"))?;
            }
        }
        Ok(())
    }

    /// Marks mission runs that finished after the account last opened their mission.
    pub(crate) fn runs(&self, db: &Db<'_>, runs: &mut [Value]) -> Result<()> {
        for run in runs {
            let mission = run["trigger"] != "chat" && run["status"] == RunStatus::Succeeded;
            let item = format!("task:{}", text(run, "taskId"));
            let unread = mission && self.unread(&item, run["finishedAt"].as_i64());
            run["unread"] = unread.into();
            if unread {
                run["preview"] = summary_preview(db, text(run, "id"))?;
            }
        }
        Ok(())
    }
}

fn summary_preview(db: &Db<'_>, run: &str) -> Result<Value> {
    let summary = db.run(run)?.map(|run| preview(text(&run, "summary")));
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

/// Records that the account read these Fil items now.
async fn mark(s: &Service, account: &str, body: &Value) -> Result<Value> {
    let items = body["items"]
        .as_array()
        .filter(|items| !items.is_empty() && items.len() <= MAX_ITEMS)
        .ok_or_else(|| Error::bad("List between 1 and 200 items to mark as read."))?;
    let mut keys = Vec::with_capacity(items.len());
    for item in items {
        let item = item.as_str().unwrap_or_default();
        let id = item
            .strip_prefix("chat:")
            .or_else(|| item.strip_prefix("task:"))
            .ok_or_else(|| Error::bad("Mark conversations or missions as read."))?;
        uuid(id)?;
        keys.push(format!("{}{item}", marks_prefix(account)));
    }

    begin(s, account).await?;
    let read_at = now();
    let count = keys.len();
    s.store
        .transaction(move |db| {
            for key in &keys {
                db.set(key, &read_at.into(), None)?;
            }
            Ok(())
        })
        .await?;
    Ok(json!({ "read": count, "readAt": read_at }))
}

pub(crate) async fn routes(s: &Service, input: &Input) -> Option<Result<Value>> {
    if input.method != "POST" || input.path != "/api/reads" {
        return None;
    }
    Some(match account(input) {
        Ok(account) => mark(s, account, &input.body).await,
        Err(error) => Err(error),
    })
}

#[cfg(test)]
mod tests {
    use super::preview;

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
