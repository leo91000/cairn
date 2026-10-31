//! Side questions: explanations the running agent answers from its current context,
//! beside its reply. They never join the queue, steer the turn or enter the native
//! session's history. Stored as `chat-side-question:{chatId}:{id}` key-value records,
//! with a small `chat-side-question-open:{chatId}:{id}` marker while the reply may still
//! receive the question or its withdrawal, so ticks and lists never read past answers.
//!
//! The adapter of a running reply announces `chat.side.available` once its native
//! session can answer one: Claude Code's `side_question` control request, which runs
//! with every tool denied. Codex and OpenCode have no side mechanism that Cairn can make
//! tool-less before the model runs (ADR-0078), so their adapters never announce it.
//! Adapters that do not announce it never receive side questions in their inbox and
//! cannot mistake one for steering.
use super::{authorize_conversation, chat_for_run, conversation, is_compaction, require_live};
use crate::{
    config::now,
    error::{Error, Result, required},
    provider::Provider,
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::text,
};
use cairn_contracts::{
    ChatSideQuestion, SideQuestionInput, SideQuestionStatus, SideQuestionUnavailable,
};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

/// Unanswered side questions a conversation can hold at once.
const MAX_PENDING: usize = 5;
/// Open side questions one reply holds at once: waiting, or withdrawn while it runs (its
/// inbox carries each withdrawal until it ends). Keeps the inbox and reads bounded.
const MAX_OPEN: usize = 10;
/// Side questions kept per conversation; the oldest settled ones are forgotten first.
/// Conversation views carry them all, with their answers.
const MAX_KEPT: usize = 30;
/// Answers longer than this are cut, as no explanation needs more.
const MAX_ANSWER: usize = 20_000;
/// How much of a reply a question about the whole reply carries to the agent.
const MAX_ABOUT: usize = 2_000;
const MAX_ERROR: usize = 1_000;

/// Shorten only after the worker has redacted complete secrets.
fn bounded_answer(answer: &str) -> String {
    if answer.chars().count() > MAX_ANSWER {
        format!("{}…", answer.chars().take(MAX_ANSWER).collect::<String>())
    } else {
        answer.to_owned()
    }
}

fn bounded_error(error: &str) -> String {
    error.chars().take(MAX_ERROR).collect()
}

/// An adapter does not have the worker's redaction secrets. It must omit an oversized
/// field whole rather than cut through a secret before sending it to the worker.
pub(crate) fn answer_fits(answer: &str) -> bool {
    answer.chars().count() <= MAX_ANSWER
}

pub(crate) fn error_fits(error: &str) -> bool {
    error.chars().count() <= MAX_ERROR
}

pub(crate) const EXPIRED: &str = "The reply stopped before this side question was answered. Ask it again while the agent works, or ask in the conversation.";

const OPEN: &str = "chat-side-question-open:";

/// A question the reply may still receive: pending, or withdrawn while it runs.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Open {
    run_id: String,
    /// The turn asked about: its run continues with other turns under the same identifier.
    #[serde(default)]
    turn: String,
    /// The start of the reply a question without a quoted passage is about.
    #[serde(default)]
    about: String,
    status: SideQuestionStatus,
}

/// The turn a run is working on.
fn turn(run: &Value) -> &str {
    text(&run["chatExecution"], "messageId")
}

/// Whether `run` is still running the turn an open question was asked during. One that
/// waits, retries later, stopped or moved on to another turn no longer answers it.
fn answers(run: Option<&Value>, open: &Open) -> bool {
    run.is_some_and(|run| {
        RunStatus::of(run) == Some(RunStatus::Running)
            && text(run, "id") == open.run_id
            && turn(run) == open.turn
    })
}

fn side_question_prefix(chat: &str) -> String {
    format!("chat-side-question:{chat}:")
}

fn open_prefix(chat: &str) -> String {
    format!("{OPEN}{chat}:")
}

/// Key prefixes of a conversation's side-question records.
pub fn side_question_prefixes(chat: &str) -> [String; 2] {
    [side_question_prefix(chat), open_prefix(chat)]
}

fn key(chat: &str, id: &str) -> String {
    format!("{}{id}", side_question_prefix(chat))
}

fn open_key(chat: &str, id: &str) -> String {
    format!("{}{id}", open_prefix(chat))
}

/// The conversation's open questions by identifier.
fn open(db: &Db<'_>, chat: &str) -> Result<Vec<(String, Open)>> {
    let prefix = open_prefix(chat);
    Ok(db
        .keys_as::<Open>(&prefix)?
        .into_iter()
        .map(|(key, open)| (key[prefix.len()..].to_owned(), open))
        .collect())
}

pub(crate) fn side_questions(db: &Db<'_>, chat: &str) -> Result<Vec<ChatSideQuestion>> {
    let mut questions = db
        .keys_as::<ChatSideQuestion>(&side_question_prefix(chat))?
        .into_iter()
        .map(|(_, question)| question)
        .collect::<Vec<_>>();
    questions.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(questions)
}

fn find(db: &Db<'_>, chat: &str, id: &str) -> Result<Option<ChatSideQuestion>> {
    db.kv_as(&key(chat, id))
}

/// Saves a question asked during `turn` of its run, and what the reply receives about it.
fn open_question(db: &Db<'_>, question: &ChatSideQuestion, turn: &str, about: &str) -> Result<()> {
    db.set_as(&key(&question.chat_id, &question.id), question, None)?;
    if !matches!(
        question.status,
        SideQuestionStatus::Pending | SideQuestionStatus::Cancelled
    ) {
        return Ok(());
    }
    let open = Open {
        run_id: question.run_id.clone(),
        turn: turn.to_owned(),
        about: about.to_owned(),
        status: question.status,
    };
    db.set_as(&open_key(&question.chat_id, &question.id), &open, None)
}

/// Saves a question's new state. It keeps its open marker only while the reply may still
/// receive it or its withdrawal.
fn save(db: &Db<'_>, question: &ChatSideQuestion) -> Result<()> {
    db.set_as(&key(&question.chat_id, &question.id), question, None)?;
    let open_key = open_key(&question.chat_id, &question.id);
    let open = db.kv_as::<Open>(&open_key)?;
    match (question.status, open) {
        (SideQuestionStatus::Pending | SideQuestionStatus::Cancelled, Some(mut open)) => {
            open.status = question.status;
            db.set_as(&open_key, &open, None)
        }
        _ => db.delete(&open_key),
    }
}

/// The start of the agent reply `item_id`, so the agent knows which reply a question is
/// about when it quotes no passage.
fn reply_start(db: &Db<'_>, run_id: &str, item_id: &str, event_id: Option<i64>) -> Result<String> {
    if item_id.is_empty() {
        return Ok(String::new());
    }
    if event_id.is_none() {
        let turns: i64 = db.0.query_row(
            "SELECT COUNT(DISTINCT COALESCE((SELECT MAX(t.id) FROM events t WHERE t.run_id=e.run_id AND t.type IN ('turn.started','turn.completed','turn.failed') AND t.id<e.id),0))
             FROM events e WHERE e.run_id=?1 AND json_extract(e.payload,'$.item.type')='agent_message' AND json_extract(e.payload,'$.item.id')=?2",
            rusqlite::params![run_id, item_id],
            |row| row.get(0),
        )?;
        if turns > 1 {
            return Err(Error::conflict(
                "This reply ID is ambiguous. Include replyEventId to identify the reply.",
            ));
        }
    }
    let reply: Option<String> = if let Some(event_id) = event_id {
        db.0.prepare_cached(
            "SELECT COALESCE(json_extract(e.payload,'$.item.text'),e.text) FROM events e
             WHERE e.run_id=?1 AND e.id>=?2
               AND e.id<COALESCE((SELECT MIN(t.id) FROM events t WHERE t.run_id=?1 AND t.type IN ('turn.started','turn.completed','turn.failed') AND t.id>?2),9223372036854775807)
               AND json_extract(e.payload,'$.item.type')='agent_message' AND json_extract(e.payload,'$.item.id')=?3
               AND COALESCE(json_extract(e.payload,'$.item.text'),e.text)!=''
               AND EXISTS (SELECT 1 FROM events a WHERE a.run_id=?1 AND a.id=?2 AND json_extract(a.payload,'$.item.type')='agent_message' AND json_extract(a.payload,'$.item.id')=?3)
             ORDER BY e.id DESC LIMIT 1",
        )?
        .query_row(rusqlite::params![run_id, event_id, item_id], |row| row.get(0))
        .optional()?
        .flatten()
    } else {
        db
        .0
        .prepare_cached(
            "SELECT json_extract(payload,'$.item.text') FROM events WHERE run_id=?1 AND json_extract(payload,'$.item.type')='agent_message' AND json_extract(payload,'$.item.id')=?2 ORDER BY id DESC LIMIT 1",
        )?
        .query_row(rusqlite::params![run_id, item_id], |row| row.get(0))
        .optional()?
        .flatten()
    };
    let reply = if event_id.is_some() {
        reply.ok_or_else(|| Error::bad("Reply event not found in this conversation."))?
    } else {
        reply.unwrap_or_default()
    };
    let reply = reply.trim();
    if reply.chars().count() <= MAX_ABOUT {
        return Ok(reply.to_owned());
    }
    Ok(format!(
        "{}…",
        reply.chars().take(MAX_ABOUT).collect::<String>()
    ))
}

/// Why the chat's current reply cannot take a side question, if it cannot.
pub(crate) fn unavailable(run: Option<&Value>) -> Option<SideQuestionUnavailable> {
    // Queued and recovering runs have no adapter that announced it yet.
    let executing = |run: &&Value| RunStatus::of(run) == Some(RunStatus::Running);
    let Some(run) = run.filter(executing) else {
        return Some(SideQuestionUnavailable::BetweenReplies);
    };
    match Provider::of_run(run) {
        Ok(Provider::Claude) => {}
        Ok(Provider::Codex) => return Some(SideQuestionUnavailable::Codex),
        Ok(Provider::Opencode) => return Some(SideQuestionUnavailable::Opencode),
        Err(_) => return Some(SideQuestionUnavailable::UnknownAgent),
    }
    if !run["cancelRequestedAt"].is_null() {
        return Some(SideQuestionUnavailable::Stopping);
    }
    if is_compaction(run) {
        return Some(SideQuestionUnavailable::Compacting);
    }
    if run["sideQuestions"] == false {
        return Some(SideQuestionUnavailable::ClaudeRelease);
    }
    if run["sideQuestions"] != true {
        return Some(SideQuestionUnavailable::NotOffered);
    }
    None
}

/// What the API and tools say for each reason.
pub(crate) fn reason(unavailable: SideQuestionUnavailable) -> &'static str {
    match unavailable {
        SideQuestionUnavailable::BetweenReplies => {
            "Side questions are answered while the agent works on a reply. Send a message instead."
        }
        SideQuestionUnavailable::Codex => {
            "Codex cannot answer side questions in Cairn: its side conversations can still use tools. Ask in the conversation instead."
        }
        SideQuestionUnavailable::Opencode => {
            "OpenCode cannot answer side questions in Cairn yet. Ask in the conversation instead."
        }
        SideQuestionUnavailable::UnknownAgent => {
            "This agent cannot answer side questions in Cairn. Ask in the conversation instead."
        }
        SideQuestionUnavailable::Stopping => {
            "The reply is stopping. Ask in the conversation once it continues."
        }
        SideQuestionUnavailable::Compacting => {
            "The conversation is compacting its context. Try again once it finishes."
        }
        SideQuestionUnavailable::ClaudeRelease => {
            "This Claude Code release cannot answer side questions in Cairn: version 2.1.308 or later is needed. Ask in the conversation instead."
        }
        SideQuestionUnavailable::NotOffered => {
            "The agent has not offered side questions for this reply yet. Try again in a moment; if it never does, its runtime may need an update."
        }
    }
}

/// The question as the agent receives it, with the passage or reply it is about.
fn wire_text(question: &ChatSideQuestion, about: &str) -> String {
    let (heading, excerpt) = if !question.quote.is_empty() {
        ("About this passage of your reply:", question.quote.as_str())
    } else if !about.is_empty() {
        ("About this reply of yours:", about)
    } else {
        return question.text.clone();
    };
    let quoted = excerpt
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{heading}\n{quoted}\n\n{}", question.text)
}

fn same_question(existing: &ChatSideQuestion, input: &SideQuestionInput) -> bool {
    existing.text == input.text
        && existing.quote == input.quote
        && existing.item_id == input.item_id
        && existing.reply_event_id == input.reply_event_id
}

/// Forgets the oldest settled side questions beyond what a conversation keeps.
fn prune(db: &Db<'_>, chat_id: &str) -> Result<()> {
    let questions = side_questions(db, chat_id)?;
    let excess = questions.len().saturating_sub(MAX_KEPT);
    // A withdrawal the reply has not received yet stays until it does.
    let open = open(db, chat_id)?
        .into_iter()
        .map(|(id, _)| id)
        .collect::<HashSet<_>>();
    let settled = questions
        .iter()
        .filter(|question| !open.contains(&question.id))
        .take(excess);
    for question in settled {
        db.delete(&key(chat_id, &question.id))?;
        db.delete(&open_key(chat_id, &question.id))?;
    }
    Ok(())
}

fn ask(
    db: &Db<'_>,
    chat_id: &str,
    input: SideQuestionInput,
    origin: &crate::native::policy::Origin,
) -> Result<ChatSideQuestion> {
    let mut chat = conversation(db, chat_id)?;
    let before = serde_json::to_value(&chat)?;
    // The agent answers with the conversation's native grant, as for a message.
    authorize_conversation(db, &mut chat, origin)?;
    require_live(&chat)?;
    if serde_json::to_value(&chat)? != before {
        db.save_conversation(&chat)?;
    }

    // Retried submissions are idempotent; a reused identifier with other content is not.
    if let Some(existing) = find(db, chat_id, &input.id)? {
        if !same_question(&existing, &input) {
            return Err(Error::conflict(
                "This side question identifier has already been used.",
            ));
        }
        return Ok(existing);
    }

    let run = db.run(chat.run_id.as_deref().unwrap_or(""))?;
    if let Some(unavailable) = unavailable(run.as_ref()) {
        return Err(Error::conflict(reason(unavailable)));
    }
    let Some(run) = run else {
        return Err(Error::conflict(reason(
            SideQuestionUnavailable::BetweenReplies,
        )));
    };
    // This reply's open questions; those of an earlier turn expire with its next tick.
    let open = open(db, chat_id)?
        .into_iter()
        .filter(|(_, open)| answers(Some(&run), open))
        .collect::<Vec<_>>();
    let pending = open
        .iter()
        .filter(|(_, open)| open.status == SideQuestionStatus::Pending)
        .count();
    if pending >= MAX_PENDING {
        return Err(Error::conflict(
            "Wait for an answer before asking more side questions.",
        ));
    }
    if open.len() >= MAX_OPEN {
        return Err(Error::conflict(
            "This reply already holds as many open side questions as it can, withdrawn ones included. Ask in the conversation, or again during the next reply.",
        ));
    }

    let provider = Provider::of_run(&run)
        .ok()
        .map(|provider| serde_json::from_value(Value::from(provider.as_str())))
        .transpose()?;
    let question = ChatSideQuestion {
        id: input.id,
        chat_id: chat_id.to_owned(),
        run_id: text(&run, "id").to_owned(),
        text: input.text,
        quote: input.quote,
        item_id: input.item_id,
        reply_event_id: input.reply_event_id,
        provider,
        status: SideQuestionStatus::Pending,
        answer: None,
        error: None,
        created_at: now(),
        settled_at: None,
    };
    let reply = reply_start(
        db,
        &question.run_id,
        &question.item_id,
        question.reply_event_id.parse().ok(),
    )?;
    let about = if question.quote.is_empty() {
        reply
    } else {
        String::new()
    };
    open_question(db, &question, turn(&run), &about)?;
    prune(db, chat_id)?;
    Ok(question)
}

fn cancel(db: &Db<'_>, chat_id: &str, id: &str) -> Result<ChatSideQuestion> {
    require_live(&conversation(db, chat_id)?)?;
    let mut question = required(find(db, chat_id, id)?, "Side question not found")?;
    match question.status {
        // Already settled without an answer: there is nothing left to withdraw.
        SideQuestionStatus::Cancelled | SideQuestionStatus::Failed => Ok(question),
        SideQuestionStatus::Pending => {
            question.status = SideQuestionStatus::Cancelled;
            question.settled_at = Some(now());
            save(db, &question)?;
            Ok(question)
        }
        _ => Err(Error::conflict("This side question already has an answer.")),
    }
}

/// Inbox entries for the running reply: side questions to answer and withdrawals of
/// questions it may still be answering. Adapters answer each identifier once.
pub(crate) fn deliveries(db: &Db<'_>, chat_id: &str, run: &Value) -> Result<Vec<Value>> {
    let announced = run["sideQuestions"] == true
        && RunStatus::of(run) == Some(RunStatus::Running)
        && matches!(Provider::of_run(run), Ok(Provider::Claude));
    if !announced {
        return Ok(Vec::new());
    }
    let mut questions = Vec::new();
    for (id, open) in open(db, chat_id)? {
        if !answers(Some(run), &open) {
            continue;
        }
        if let Some(question) = find(db, chat_id, &id)? {
            questions.push((question, open.about));
        }
    }
    // In the order they were asked.
    questions.sort_by(|(a, _), (b, _)| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut entries = Vec::new();
    for (question, about) in questions {
        match question.status {
            SideQuestionStatus::Pending => entries.push(json!({
                "id": format!("side-question:{}", question.id),
                "sideQuestion": {
                    "id": question.id,
                    "text": wire_text(&question, &about),
                },
            })),
            SideQuestionStatus::Cancelled => entries.push(json!({
                "id": format!("withdraw-side-question:{}", question.id),
                "sideQuestionId": question.id,
                "sideQuestionCancelled": true,
            })),
            _ => {}
        }
    }
    Ok(entries)
}

/// Records the agent's answer, or why it could not answer. Settled questions keep
/// their first outcome: a withdrawal or a repeated answer after restart changes nothing.
fn settle(
    db: &Db<'_>,
    run_id: &str,
    id: &str,
    outcome: std::result::Result<&str, &str>,
) -> Result<()> {
    let Some(chat) = chat_for_run(db, run_id)? else {
        return Ok(());
    };
    let Some(mut question) = find(db, text(&chat, "id"), id)? else {
        return Ok(());
    };
    if question.status != SideQuestionStatus::Pending || question.run_id != run_id {
        return Ok(());
    }
    // Only the turn it was asked during answers it; it expires otherwise.
    let open = db.kv_as::<Open>(&open_key(&question.chat_id, id))?;
    let run = db.run(run_id)?;
    if !open.is_some_and(|open| answers(run.as_ref(), &open)) {
        return Ok(());
    }
    match outcome {
        Ok(answer) => {
            question.status = SideQuestionStatus::Answered;
            question.answer = Some(bounded_answer(answer));
        }
        Err(error) => {
            question.status = SideQuestionStatus::Failed;
            question.error = Some(bounded_error(error));
        }
    }
    question.settled_at = Some(now());
    save(db, &question)
}

/// Conversations with a side question their reply may still receive.
pub(crate) fn open_chats(db: &Db<'_>) -> Result<HashSet<String>> {
    Ok(db
        .keys_as::<Open>(OPEN)?
        .into_iter()
        .filter_map(|(key, _)| Some(key[OPEN.len()..].split_once(':')?.0.to_owned()))
        .collect())
}

/// Whether some open side question's turn is not running any more.
pub(crate) fn expiring(db: &Db<'_>, chat_id: &str) -> Result<bool> {
    let chat = conversation(db, chat_id)?;
    let run = db.run(chat.run_id.as_deref().unwrap_or(""))?;
    Ok(open(db, chat_id)?
        .iter()
        .any(|(_, open)| !answers(run.as_ref(), open)))
}

/// Side questions the chat's reply can no longer answer or withdraw: the turn they were
/// asked during is not running any more.
pub(crate) fn expire(db: &Db<'_>, chat_id: &str) -> Result<()> {
    let chat = conversation(db, chat_id)?;
    let run = db.run(chat.run_id.as_deref().unwrap_or(""))?;
    for (id, open) in open(db, chat_id)? {
        if answers(run.as_ref(), &open) {
            continue;
        }
        expire_question(db, chat_id, &id)?;
    }
    Ok(())
}

fn expire_question(db: &Db<'_>, chat_id: &str, id: &str) -> Result<()> {
    let question = find(db, chat_id, id)?;
    let Some(mut question) =
        question.filter(|question| question.status == SideQuestionStatus::Pending)
    else {
        return db.delete(&open_key(chat_id, id));
    };
    question.status = SideQuestionStatus::Failed;
    question.error = Some(EXPIRED.into());
    question.settled_at = Some(now());
    save(db, &question)
}

fn expire_attempt(db: &Db<'_>, chat_id: &str, run_id: &str) -> Result<()> {
    for (question_id, marker) in open(db, chat_id)? {
        if marker.run_id == run_id {
            expire_question(db, chat_id, &question_id)?;
        }
    }
    Ok(())
}

/// What the conversation view shows about side questions.
pub(crate) fn view(
    db: &Db<'_>,
    chat: &mut cairn_contracts::Chat,
    run: Option<&Value>,
) -> Result<()> {
    chat.pending_side_questions = open(db, &chat.id)?
        .iter()
        .filter(|(_, open)| open.status == SideQuestionStatus::Pending)
        .count() as u32;
    let unavailable = unavailable(run);
    chat.can_ask_side_question = unavailable.is_none();
    chat.side_question_reason = unavailable.map(|unavailable| reason(unavailable).to_owned());
    chat.side_question_unavailable = unavailable;
    Ok(())
}

/// Serialises inbox writes with the attempts that withdraw an announcement, so a write
/// decided before an attempt never restores side questions it removed.
static INBOX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn is_side(entry: &Value) -> bool {
    entry["sideQuestion"].is_object() || entry["sideQuestionCancelled"] == true
}

/// Removes side questions from a run's inbox.
pub(crate) async fn forget_deliveries(run_directory: &std::path::Path) -> Result<()> {
    let directory = run_directory.join("chat-input");
    // An inbox beyond the transport limit is unreadable, by adapters too.
    let Ok(Some(bytes)) =
        crate::chat_inbox::messages(&directory, crate::chat_inbox::TRANSPORT_LIMIT).await
    else {
        return Ok(());
    };
    let inbox = directory.join("messages.json");
    let Ok(Value::Array(entries)) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(());
    };
    let kept = entries
        .iter()
        .filter(|entry| !is_side(entry))
        .collect::<Vec<_>>();
    if kept.len() == entries.len() {
        return Ok(());
    }
    crate::skills::atomic_write(&inbox, &serde_json::to_vec(&kept)?).await
}

/// End an attempt's side-question capability and fail its pending questions before a
/// replacement attempt can announce support, even if the run and turn are unchanged.
pub(crate) async fn withdraw_announcement(
    store: &crate::store::Store,
    run_id: &str,
    run_directory: &std::path::Path,
) -> Result<()> {
    let _inbox = INBOX.lock().await;
    let id = run_id.to_owned();
    store
        .transaction(move |db| {
            db.update_run_boundary(&id, &json!({ "sideQuestions": null }))?;
            if let Some(chat) = chat_for_run(db, &id)? {
                expire_attempt(db, text(&chat, "id"), &id)?;
            }
            Ok(())
        })
        .await?;
    forget_deliveries(run_directory).await
}

/// Writes the run's inbox, with side questions only while its announcement stands and it
/// still runs `side_turn`, the turn they were chosen for.
pub(crate) async fn publish_inbox(
    store: &crate::store::Store,
    run_id: &str,
    side_turn: &str,
    directory: &std::path::Path,
    mut entries: Vec<Value>,
) -> Result<()> {
    // Without side questions, nothing can be restored: no lock or run read.
    let _inbox = if entries.iter().any(is_side) {
        let inbox = INBOX.lock().await;
        let id = run_id.to_owned();
        let expected_turn = side_turn.to_owned();
        let current = store
            .read(move |db| {
                let Some(run) = db.run(&id)? else {
                    return Ok(Vec::new());
                };
                if run["sideQuestions"] != true || turn(&run) != expected_turn {
                    return Ok(Vec::new());
                }
                let Some(chat) = chat_for_run(db, &id)? else {
                    return Ok(Vec::new());
                };
                deliveries(db, text(&chat, "id"), &run)
            })
            .await?;
        // A replacement adapter may have announced on the same turn after retiring
        // the old attempt. Never restore entries selected before that retirement.
        entries.retain(|entry| !is_side(entry) || current.contains(entry));
        Some(inbox)
    } else {
        None
    };
    crate::skills::private_dir(directory).await?;
    crate::skills::atomic_write(
        &directory.join("messages.json"),
        &serde_json::to_vec(&entries)?,
    )
    .await
}

impl Service {
    pub(crate) async fn side_question_ask_authorized<F>(
        &self,
        chat_id: &str,
        mut input: SideQuestionInput,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_chat(self, chat_id).await?;
        }
        input.validate().map_err(Error::bad)?;
        let chat_id = chat_id.to_owned();
        let question = self
            .store
            .transaction(move |db| {
                authorize(db)?;
                ask(db, &chat_id, input, &origin)
            })
            .await?;
        self.worker.notify();
        Ok(serde_json::to_value(question)?)
    }

    pub(crate) async fn side_question_cancel_authorized<F>(
        &self,
        chat_id: &str,
        id: &str,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let (chat_id, id) = (chat_id.to_owned(), id.to_owned());
        let question = self
            .store
            .transaction(move |db| {
                authorize(db)?;
                cancel(db, &chat_id, &id)
            })
            .await?;
        self.worker.notify();
        Ok(serde_json::to_value(question)?)
    }

    /// Records whether the run's current attempt answers side questions: its adapter says
    /// so once its native session started.
    pub async fn side_questions_available(&self, run_id: &str, available: bool) -> Result<()> {
        self.store
            .update_run_boundary(run_id, json!({ "sideQuestions": available }))
            .await?;
        Ok(())
    }

    pub async fn side_question_settle(&self, run_id: &str, event: &Value) -> Result<()> {
        let id = text(event, "sideQuestionId");
        if crate::validation::uuid(id).is_err() {
            return Ok(());
        }
        let (run_id, id) = (run_id.to_owned(), id.to_owned());
        let answered = event["type"] == "chat.side.answered";
        let body = if answered {
            text(event, "text").to_owned()
        } else {
            text(event, "error").to_owned()
        };
        self.store
            .transaction(move |db| {
                let outcome = if answered && !body.trim().is_empty() {
                    Ok(body.as_str())
                } else if answered {
                    Err("The agent returned an empty answer.")
                } else if body.is_empty() {
                    Err("The agent could not answer this side question.")
                } else {
                    Err(body.as_str())
                };
                settle(db, &run_id, &id, outcome)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_fixture::DbRunFixture;

    fn question(quote: &str) -> ChatSideQuestion {
        ChatSideQuestion {
            id: "a".into(),
            chat_id: "c".into(),
            run_id: "r".into(),
            text: "Why this order?".into(),
            quote: quote.into(),
            item_id: String::new(),
            reply_event_id: String::new(),
            provider: None,
            status: SideQuestionStatus::Pending,
            answer: None,
            error: None,
            created_at: 0,
            settled_at: None,
        }
    }

    #[test]
    fn quotes_each_line_of_the_passage_before_the_question() {
        assert_eq!(wire_text(&question(""), ""), "Why this order?");
        assert_eq!(
            wire_text(&question("first\nsecond"), "the whole reply"),
            "About this passage of your reply:\n> first\n> second\n\nWhy this order?"
        );
        // Without a passage, the agent still learns which reply the question is about.
        assert_eq!(
            wire_text(&question(""), "I traced it.\nThen I fixed it."),
            "About this reply of yours:\n> I traced it.\n> Then I fixed it.\n\nWhy this order?"
        );
    }

    #[tokio::test]
    async fn reply_event_distinguishes_reused_native_item_ids() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store.transaction(|db| {
            db.add_run_fixture(&json!({ "id": "r", "taskId": "r", "createdAt": 0, "status": "running" }), None)?;
            for (id, text) in [(11, "First draft"), (12, "First reply"), (14, "Later reply")] {
                db.0.execute(
                    "INSERT INTO events(id,run_id,created_at,type,text,payload) VALUES(?1,'r',0,'item.completed','',?2)",
                    rusqlite::params![id, json!({ "item": { "type": "agent_message", "id": "reused", "text": text } }).to_string()],
                )?;
            }
            db.0.execute("INSERT INTO events(id,run_id,created_at,type,text) VALUES(13,'r',0,'turn.completed','')", [])?;
            assert_eq!(reply_start(db, "r", "reused", Some(11))?, "First reply");
            assert_eq!(reply_start(db, "r", "reused", Some(14))?, "Later reply");
            assert!(reply_start(db, "r", "another", Some(11)).is_err());
            assert!(reply_start(db, "r", "reused", None).is_err());
            // A fresh history page keeps the first snapshot's identity even though
            // it only returns the latest text snapshot for each turn.
            let (history, _) = db.events_before("r", i64::MAX)?;
            let first = history.iter().find(|event| event.id == 12).unwrap();
            let later = history.iter().find(|event| event.id == 14).unwrap();
            assert_eq!(first.display_id, Some(11));
            assert_eq!(later.display_id, None);
            Ok(())
        }).await.unwrap();
    }

    #[test]
    fn only_a_running_announced_claude_reply_takes_side_questions() {
        let running = |provider: &str, announced: bool| {
            json!({
                "status": "running",
                "sideQuestions": announced,
                "snapshot": { "agent": { "provider": provider } },
            })
        };
        assert!(unavailable(None).is_some());
        assert!(unavailable(Some(&json!({ "status": "succeeded" }))).is_some());
        assert!(unavailable(Some(&running("claude", false))).is_some());
        assert!(unavailable(Some(&running("opencode", true))).is_some());
        // Codex forks keep tools Cairn cannot remove before they run: never offered,
        // even if an adapter announced it.
        assert!(unavailable(Some(&running("codex", true))).is_some());
        assert!(unavailable(Some(&running("claude", true))).is_none());
        // A run whose engine cannot be read is refused without naming another engine.
        let unknown = unavailable(Some(&running("", true))).unwrap();
        assert_eq!(unknown, SideQuestionUnavailable::UnknownAgent);
        assert!(reason(unknown).starts_with("This agent"));
    }

    #[tokio::test]
    async fn only_open_questions_keep_a_marker_until_their_reply_stops() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        crate::chats::list_measurements::seed_chats(&store).await;
        let chat = crate::chats::list_measurements::chat_id(0);
        let other = crate::chats::list_measurements::chat_id(1);
        let (opened, kept, expired, after) = store
            .transaction(move |db| {
                let ask = |id: &str, chat: &str, status| {
                    let mut question = question("");
                    question.id = id.into();
                    question.chat_id = chat.into();
                    question.status = status;
                    open_question(db, &question, "", "")
                };
                ask("pending", &chat, SideQuestionStatus::Pending)?;
                ask("withdrawn", &chat, SideQuestionStatus::Cancelled)?;
                ask("answered", &chat, SideQuestionStatus::Answered)?;
                ask("elsewhere", &other, SideQuestionStatus::Answered)?;
                let opened = open_chats(db)?;
                let kept = open(db, &chat)?
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>();

                // The chat has no running reply: nothing can receive them any more.
                expire(db, &chat)?;
                let expired = find(db, &chat, "pending")?.unwrap();
                Ok((
                    opened,
                    kept,
                    expired,
                    (open_chats(db)?, side_questions(db, &chat)?.len()),
                ))
            })
            .await
            .unwrap();
        assert_eq!(
            opened,
            HashSet::from([crate::chats::list_measurements::chat_id(0)])
        );
        assert_eq!(kept, ["pending", "withdrawn"]);
        assert_eq!(expired.status, SideQuestionStatus::Failed);
        assert_eq!(expired.error.as_deref(), Some(EXPIRED));
        assert_eq!(after, (HashSet::new(), 3));
    }

    #[tokio::test]
    async fn ending_an_attempt_fails_its_questions_before_the_same_turn_resumes() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        let chat = crate::chats::list_measurements::chat_id(0);
        let chat_for_assertions = chat.clone();
        store
            .transaction(move |db| {
                db.add_run_fixture(
                    &json!({
                        "id": "r", "taskId": "r", "createdAt": 0, "status": "running",
                        "snapshot": { "agent": { "provider": "claude" } },
                        "chatExecution": { "messageId": "same-turn" }, "sideQuestions": true,
                    }),
                    None,
                )?;
                let conversation: crate::chats::Conversation = serde_json::from_value(json!({
                    "id": chat, "slug": "test-chat", "title": "Test", "agentId": "agent",
                    "createdAt": 0, "updatedAt": 0, "runId": "r",
                }))?;
                db.save_conversation(&conversation)?;
                for (id, run, status) in [
                    ("pending", "r", SideQuestionStatus::Pending),
                    ("withdrawn", "r", SideQuestionStatus::Cancelled),
                    ("answered", "r", SideQuestionStatus::Answered),
                    ("other-run", "other", SideQuestionStatus::Pending),
                ] {
                    let mut entry = question("");
                    entry.id = id.into();
                    entry.chat_id = chat.clone();
                    entry.run_id = run.into();
                    entry.status = status;
                    open_question(db, &entry, "same-turn", "")?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let inbox = root.path().join("chat-input");
        std::fs::create_dir(&inbox).unwrap();
        let selected_chat = chat_for_assertions.clone();
        let old_delivery = store
            .read(move |db| deliveries(db, &selected_chat, &db.run("r")?.unwrap()))
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry["sideQuestion"]["id"] == "pending")
            .unwrap();
        std::fs::write(
            inbox.join("messages.json"),
            json!([
                old_delivery,
                { "id": "steer", "text": "continue" },
            ])
            .to_string(),
        )
        .unwrap();
        withdraw_announcement(&store, "r", root.path())
            .await
            .unwrap();
        let run = store.run("r").await.unwrap();
        assert!(run["sideQuestions"].is_null());
        assert!(unavailable(Some(&run)).is_some());
        let cleaned: Value =
            serde_json::from_slice(&std::fs::read(inbox.join("messages.json")).unwrap()).unwrap();
        assert_eq!(cleaned, json!([{ "id": "steer", "text": "continue" }]));
        // A later adapter can announce again on the same run and turn, but cannot
        // receive or settle a question belonging to the exhausted attempt.
        store
            .update_run_boundary("r", json!({ "sideQuestions": true }))
            .await
            .unwrap();
        publish_inbox(
            &store,
            "r",
            "same-turn",
            &inbox,
            vec![old_delivery, json!({ "id": "steer", "text": "continue" })],
        )
        .await
        .unwrap();
        let inbox_after: Value =
            serde_json::from_slice(&std::fs::read(inbox.join("messages.json")).unwrap()).unwrap();
        assert_eq!(inbox_after, json!([{ "id": "steer", "text": "continue" }]));
        store
            .read(move |db| {
                let failed = find(db, &chat_for_assertions, "pending")?.unwrap();
                assert_eq!(failed.status, SideQuestionStatus::Failed);
                assert_eq!(failed.error.as_deref(), Some(EXPIRED));
                assert_eq!(
                    find(db, &chat_for_assertions, "withdrawn")?.unwrap().status,
                    SideQuestionStatus::Cancelled
                );
                assert_eq!(
                    find(db, &chat_for_assertions, "answered")?.unwrap().status,
                    SideQuestionStatus::Answered
                );
                let remaining = open(db, &chat_for_assertions)?;
                assert_eq!(remaining.len(), 1);
                assert_eq!(remaining[0].0, "other-run");
                assert!(deliveries(db, &chat_for_assertions, &db.run("r")?.unwrap())?.is_empty());
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_new_execution_reads_no_side_question_left_in_the_inbox() {
        let root = tempfile::tempdir().unwrap();
        let inbox = root.path().join("chat-input");
        std::fs::create_dir(&inbox).unwrap();
        let entries = json!([
            { "id": "message-1", "text": "Steer" },
            { "id": "side-question:a", "sideQuestion": { "id": "a", "text": "Why?" } },
            { "id": "withdraw-side-question:b", "sideQuestionId": "b", "sideQuestionCancelled": true },
        ]);
        std::fs::write(inbox.join("messages.json"), entries.to_string()).unwrap();
        forget_deliveries(root.path()).await.unwrap();
        let kept: Value =
            serde_json::from_slice(&std::fs::read(inbox.join("messages.json")).unwrap()).unwrap();
        assert_eq!(kept, json!([{ "id": "message-1", "text": "Steer" }]));
        // A run without an inbox yet has nothing to forget.
        forget_deliveries(&root.path().join("missing"))
            .await
            .unwrap();
    }

    #[test]
    fn an_open_question_belongs_to_the_turn_it_was_asked_during() {
        let open = Open {
            run_id: "run".into(),
            turn: "message-1".into(),
            about: String::new(),
            status: SideQuestionStatus::Pending,
        };
        let run = |status: &str, turn: &str| json!({ "id": "run", "status": status, "chatExecution": { "messageId": turn } });
        assert!(answers(Some(&run("running", "message-1")), &open));
        // Retrying later, waiting, finishing or another turn does not.
        assert!(!answers(Some(&run("queued", "message-1")), &open));
        assert!(!answers(Some(&run("waiting", "message-1")), &open));
        assert!(!answers(Some(&run("succeeded", "message-1")), &open));
        assert!(!answers(Some(&run("running", "message-2")), &open));
        assert!(!answers(None, &open));
    }

    /// Local side-question record measurement against the conversation list
    /// without them (`list_measurements` runs unchanged on the reference commit).
    #[tokio::test]
    #[ignore = "explicit local side-question storage measurement"]
    async fn side_question_storage_measurements() {
        use crate::chats::list_measurements::{CHATS, chat_id, cpu_ms, distribution, measure_list};
        use crate::store::Store;
        use std::time::Instant;

        const PER_CHAT: usize = 5;
        let typical = |chat: usize, index: usize| ChatSideQuestion {
            id: format!("00000000-0000-4000-9000-{:06}{index:06}", chat),
            chat_id: chat_id(chat),
            run_id: format!("run-{chat}"),
            text: "Why does the migration have to run before the new handler?".into(),
            quote: "Run the migration first, then deploy the handler.".repeat(4),
            item_id: format!("item-{index}"),
            reply_event_id: String::new(),
            provider: Some(serde_json::from_value(json!("claude")).unwrap()),
            status: SideQuestionStatus::Answered,
            answer: Some("Because the handler reads the new column. ".repeat(24)),
            error: None,
            created_at: (chat * PER_CHAT + index) as i64,
            settled_at: Some((chat * PER_CHAT + index) as i64 + 1),
        };
        let mut largest = typical(0, 0);
        largest.answer = Some("x".repeat(MAX_ANSWER));
        let encoded = |question: &ChatSideQuestion| serde_json::to_string(question).unwrap().len();

        let without = measure_list("200 conversations, no side questions", |_| async {}).await;
        let with = measure_list(
            "200 conversations, 5 answered side questions each",
            |store| async move {
                store
                    .transaction(move |db| {
                        for chat in 0..CHATS {
                            for index in 0..PER_CHAT {
                                save(db, &typical(chat, index))?;
                            }
                        }
                        Ok(())
                    })
                    .await
                    .unwrap();
            },
        )
        .await;

        // Writes as the service performs them: one transaction to ask (save, prune),
        // one to settle with the answer.
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        crate::chats::list_measurements::seed_chats(&store).await;
        let mut asks = Vec::new();
        let mut settles = Vec::new();
        let cpu = cpu_ms();
        for chat in 0..CHATS {
            let mut question = typical(chat, 0);
            question.status = SideQuestionStatus::Pending;
            question.answer = None;
            question.settled_at = None;
            let pending = question.clone();
            let started = Instant::now();
            store
                .transaction(move |db| {
                    open_question(db, &pending, "", "")?;
                    prune(db, &pending.chat_id)
                })
                .await
                .unwrap();
            asks.push(started.elapsed().as_secs_f64() * 1000.0);
            let answered = typical(chat, 0);
            let started = Instant::now();
            store
                .transaction(move |db| {
                    let mut question = find(db, &answered.chat_id, &answered.id)?.unwrap();
                    question.status = SideQuestionStatus::Answered;
                    question.answer = answered.answer;
                    question.settled_at = answered.settled_at;
                    save(db, &question)
                })
                .await
                .unwrap();
            settles.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let write_cpu = (cpu_ms() - cpu) / (CHATS * 2) as f64;

        // Reads the chat tick and conversation views add.
        let mut chat_reads = Vec::new();
        let mut tick_scans = Vec::new();
        let cpu = cpu_ms();
        for round in 0..200 {
            let chat = chat_id(round % CHATS);
            let started = Instant::now();
            store
                .read(move |db| side_questions(db, &chat))
                .await
                .unwrap();
            chat_reads.push(started.elapsed().as_secs_f64() * 1000.0);
            let started = Instant::now();
            store.read(|db| open_chats(db)).await.unwrap();
            tick_scans.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let read_cpu = (cpu_ms() - cpu) / 400.0;

        // The largest view: every kept question, each with the longest two-byte answer.
        let heaviest = chat_id(CHATS - 1);
        let filled = heaviest.clone();
        store
            .transaction(move |db| {
                for index in 0..MAX_KEPT {
                    let mut question = typical(CHATS - 1, index + 1);
                    question.chat_id = filled.clone();
                    question.answer = Some("é".repeat(MAX_ANSWER));
                    save(db, &question)?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let mut views = Vec::new();
        let mut view_bytes = 0;
        for _ in 0..50 {
            let chat = heaviest.clone();
            let started = Instant::now();
            view_bytes = store
                .read(move |db| Ok(serde_json::to_vec(&side_questions(db, &chat)?)?.len()))
                .await
                .unwrap();
            views.push(started.elapsed().as_secs_f64() * 1000.0);
        }

        let result = json!({
            "encodedBytes": {
                "typicalRecord": encoded(&typical(0, 0)),
                "largestAnswerRecord": encoded(&largest),
            },
            "listWithout": without,
            "listWith": with,
            "writes": {
                "ask": distribution(asks),
                "settle": distribution(settles),
                "cpuMsPerWrite": write_cpu,
            },
            "reads": {
                "chatSideQuestions": distribution(chat_reads),
                "tickOpenScan": distribution(tick_scans),
                "cpuMsPerRead": read_cpu,
                "records": CHATS,
            },
            "largestView": {
                "questions": MAX_KEPT,
                "encodedBytes": view_bytes,
                "readAndEncode": distribution(views),
            },
        });
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    }
}
