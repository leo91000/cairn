pub(crate) mod continuation;
mod handoff;
pub(crate) mod questions;

pub use crate::waits as wait_for;

pub use handoff::{execution_text, with_invoked_skills};
pub use questions::{Question, QuestionField, QuestionStatus, question_prefix};

use crate::{
    config::{id, now},
    conversation_lifecycle::{is_active, require_active},
    error::{Error, Result, required},
    provider::Provider,
    run_status::RunStatus,
    service::{Service, task_projects},
    store::Db,
    validation::{parse, parse_as, string_enum, text},
};
use handoff::handoff_context;
use questions::{find_question, questions, save_question};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

string_enum! {
    pub enum MessageStatus {
        Queued => "queued",
        Sending => "sending",
        Delivered => "delivered",
        Cancelled => "cancelled",
    }
}

string_enum! {
    /// `queue` waits for the current turn; `steer` joins the running turn.
    pub enum MessageMode {
        Queue => "queue",
        Steer => "steer",
    }
}

const MAX_UNDELIVERED: usize = 20;
const TITLE_CHARS: usize = 90;
const PRIVATE_ANSWER_TEXT: &str = "Answered a private question.";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatInput {
    agent_id: String,
    project_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NewChat {
    id: String,
    title: &'static str,
    agent_id: String,
    project_id: Option<String>,
    run_id: Option<String>,
    paused: bool,
    created_at: i64,
    updated_at: i64,
}

/// `chatExecution` on a run: the user message the next agent turn delivers.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatExecution {
    message_id: Value,
    text: String,
    attachments: Value,
    recovery: bool,
    /// Transcript handed to a fresh native session.
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    /// `compact`: the execution compacts the native context instead of delivering a message.
    #[serde(skip_serializing_if = "Option::is_none")]
    operation: Option<&'static str>,
    /// What the conversation showed before a compaction, restored once it succeeds: a
    /// compaction is not a reply and never marks the conversation as newly finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    previous: Option<Value>,
}

/// The `chatExecution.operation` of a context compaction.
pub(crate) const COMPACT: &str = "compact";

pub(crate) fn is_compaction(run: &Value) -> bool {
    run["chatExecution"]["operation"] == COMPACT
}

/// Payload of the `chat.user` event recorded when a message reaches the agent.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UserMessageEvent<'a> {
    message_id: &'a str,
    text: &'a str,
    attachments: &'a Value,
    created_at: &'a Value,
}

fn chat(db: &Db<'_>, id: &str) -> Result<Value> {
    required(db.get("chats", id)?, "Chat not found")
}

pub(crate) fn chat_for_run(db: &Db<'_>, run_id: &str) -> Result<Option<Value>> {
    Ok(db
        .list("chats")?
        .into_iter()
        .find(|chat| chat["runId"] == run_id))
}

fn find_message(db: &Db<'_>, chat: &str, id: &str) -> Result<Option<Value>> {
    Ok(db
        .messages(chat)?
        .into_iter()
        .find(|message| message["id"] == id))
}

fn run_is_active(run: &Value) -> bool {
    RunStatus::of(run).is_some_and(RunStatus::is_active)
}

fn touch(chat: &mut Value) {
    chat["updatedAt"] = now().into();
    chat["lastActivityAt"] = chat["updatedAt"].clone();
}

/// Same text, agent settings and attachments.
fn same_content(a: &Value, b: &Value) -> bool {
    a["text"] == b["text"]
        && text(a, "provider") == text(b, "provider")
        && a["model"] == b["model"]
        && text(a, "reasoning") == text(b, "reasoning")
        && crate::attachments::same(a, b)
}

fn record_name(db: &Db<'_>, chat: &Value, kind: &str, key: &str, deleted: &str) -> Result<Value> {
    let id_key = if kind == "agents" {
        "agentId"
    } else {
        "projectId"
    };
    Ok(match db.get(kind, text(chat, id_key))? {
        Some(record) => record["name"].clone(),
        // Keep the name remembered on the chat after its agent or project is removed.
        None => chat.get(key).cloned().unwrap_or_else(|| deleted.into()),
    })
}

fn view(db: &Db<'_>, mut chat: Value) -> Result<Value> {
    chat["lifecycle"] = crate::conversation_lifecycle::state(&chat).into();
    chat["pendingQuestions"] = questions(db, text(&chat, "id"))?
        .iter()
        .filter(|question| question.status == QuestionStatus::Pending)
        .count()
        .into();
    chat["agentName"] = record_name(db, &chat, "agents", "agentName", "Deleted agent")?;
    chat["projectName"] = if chat["projectId"].is_null() {
        Value::Null
    } else {
        record_name(db, &chat, "projects", "projectName", "Deleted project")?
    };
    let run = db.run(text(&chat, "runId"))?;
    chat["canRequestNewSession"] =
        crate::conversation_lifecycle::can_request_new_session(&chat, run.as_ref()).into();
    chat["status"] = run
        .as_ref()
        .map_or_else(|| "idle".into(), |run| run["status"].clone());
    chat["suspendedByExclusive"] = run
        .as_ref()
        .map_or(Value::Null, |run| run["suspendedByExclusive"].clone());
    // The coding agent of the latest turn, which a message may switch from the agent's own.
    chat["provider"] = run
        .as_ref()
        .and_then(|run| Provider::of_run(run).ok())
        .map_or(Value::Null, |provider| provider.as_str().into());
    // How full the native session's context was at its latest report; absent when unknown.
    chat["contextUsage"] = run
        .as_ref()
        .map_or(Value::Null, |run| run["contextUsage"].clone());
    chat["compaction"] = compaction_state(db, &chat, run.as_ref())?;
    // When the agent's latest turn ended, for what the Fil shows as unread.
    chat["finishedAt"] = run.map_or(Value::Null, |run| run["finishedAt"].clone());
    Ok(chat)
}

/// Whether the conversation's coding agent can compact its context, whether it is
/// compacting now, and why a compaction cannot start when it cannot.
fn compaction_state(db: &Db<'_>, chat: &Value, run: Option<&Value>) -> Result<Value> {
    let provider = run.and_then(|run| Provider::of_run(run).ok());
    let supported = provider.is_some_and(Provider::compacts_context);
    let running = run.is_some_and(|run| run_is_active(run) && is_compaction(run));
    let blocker = if running {
        Some("The conversation is already compacting its context.".to_owned())
    } else {
        compaction_blocker(db, chat, run)?
    };
    Ok(json!({
        "supported": supported,
        "running": running,
        "available": blocker.is_none(),
        "reason": blocker,
    }))
}

/// Why the conversation cannot compact its native context now, if it cannot.
fn compaction_blocker(db: &Db<'_>, chat: &Value, run: Option<&Value>) -> Result<Option<String>> {
    if !is_active(chat) {
        return Ok(Some("This conversation has been deleted.".into()));
    }
    let Some(run) = run else {
        return Ok(Some("Send a first message before compacting.".into()));
    };
    let provider = Provider::of_run(run)?;
    if !provider.compacts_context() {
        return Ok(Some(format!(
            "{} does not support context compaction in Cairn.",
            provider.label()
        )));
    }
    // Codex replaces a running turn with its compaction, so compaction never overlaps a turn.
    if run_is_active(run) || run["recoveryPending"] == true {
        return Ok(Some(
            "Wait for the current turn to finish before compacting.".into(),
        ));
    }
    if run["status"] != RunStatus::Succeeded {
        return Ok(Some(
            "Resume or resolve the last turn before compacting.".into(),
        ));
    }
    if !run["workspaceCleanedAt"].is_null() {
        return Ok(Some(
            "This workspace has been cleaned up. Start a new chat.".into(),
        ));
    }
    let fresh_session = chat["sessionRestartRequested"] == true
        || db
            .kv(&format!("run-checkpoint:{}", text(run, "id")))?
            .is_some_and(|checkpoint| checkpoint["freshSession"] == true);
    if !run["sessionId"].is_string() || fresh_session {
        return Ok(Some(
            "This conversation has no saved native session to compact.".into(),
        ));
    }
    if chat["wakeup"].is_object() {
        return Ok(Some(
            "Compact after the scheduled continuation resumes.".into(),
        ));
    }
    let undelivered: bool = db.0.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE chat_id=?
         AND json_extract(data,'$.status') IN ('queued','sending'))",
        [text(chat, "id")],
        |row| row.get(0),
    )?;
    if undelivered {
        return Ok(Some(
            "Wait for queued messages to be delivered before compacting.".into(),
        ));
    }
    Ok(None)
}

/// Starts compacting the conversation's native context in its retained session. The
/// compaction runs as its own execution of the conversation's run, never as a user message.
fn compact(db: &Db<'_>, chat_id: &str, origin: &crate::native::policy::Origin) -> Result<Value> {
    let mut chat = chat(db, chat_id)?;
    crate::native::policy::authorize_chat(db, &mut chat, origin)?;
    require_active(&chat)?;
    let run = db.run(text(&chat, "runId"))?;
    if run
        .as_ref()
        .is_some_and(|run| run_is_active(run) && is_compaction(run))
    {
        // A retried request after a lost response keeps the compaction already started.
        return view(db, chat);
    }
    if let Some(reason) = compaction_blocker(db, &chat, run.as_ref())? {
        return Err(Error::conflict(reason));
    }
    let run = run.unwrap();
    let execution = ChatExecution {
        message_id: format!("compact:{}", id()).into(),
        text: String::new(),
        attachments: json!([]),
        recovery: false,
        context: None,
        operation: Some(COMPACT),
        previous: Some(json!({
            "summary": run["summary"],
            "finishedAt": run["finishedAt"],
        })),
    };
    continue_run(db, &chat, &run, &run, execution)?;
    db.event(
        text(&run, "id"),
        "status",
        "Compacting the conversation context",
        None,
    )?;
    db.put("chats", &chat)?;
    view(db, chat)
}

/// Active conversations. Deleted ones are skipped before their view is built.
pub(crate) fn list(db: &Db<'_>) -> Result<Vec<Value>> {
    db.list("chats")?
        .into_iter()
        .filter(is_active)
        .map(|chat| view(db, chat))
        .collect()
}

pub(crate) fn list_all(db: &Db<'_>) -> Result<Vec<Value>> {
    db.list("chats")?
        .into_iter()
        .map(|chat| view(db, chat))
        .collect()
}

pub(crate) fn detail(db: &Db<'_>, id: &str) -> Result<Value> {
    let mut result = view(db, chat(db, id)?)?;
    if !is_active(&result) {
        result["questions"] = json!([]);
        result["messages"] = json!([]);
        result["run"] = Value::Null;
        result["pendingQuestions"] = 0.into();
        return Ok(result);
    }
    result["questions"] = serde_json::to_value(questions(db, id)?)?;
    result["messages"] = db.messages(id)?.into();
    result["run"] = db.run(text(&result, "runId"))?.unwrap_or(Value::Null);
    Ok(result)
}

/// The run whose history an active conversation shows, without the rest of its detail.
pub(crate) fn history_run(db: &Db<'_>, id: &str) -> Result<String> {
    let chat = chat(db, id)?;
    let run = text(&chat, "runId");
    Ok(if is_active(&chat) && db.has_run(run)? {
        run.to_owned()
    } else {
        String::new()
    })
}

/// Steering joins the running turn, whose provider, model and reasoning are fixed.
fn changes_running_agent(message: &Value, run: &Value) -> Result<bool> {
    let agent = &run["snapshot"]["agent"];
    let provider = Value::from(Provider::of_run(run)?.as_str());
    let differs =
        |key: &str, current: &Value| !text(message, key).is_empty() && message[key] != *current;
    Ok(differs("provider", &provider)
        || differs("model", &agent["model"])
        || differs("reasoning", &agent["reasoning"]))
}

fn validate_steer(db: &Db<'_>, chat: &Value, message: &Value) -> Result<()> {
    if message["mode"] != MessageMode::Steer {
        return Ok(());
    }
    let Some(run) = db.run(text(chat, "runId"))? else {
        return Ok(());
    };
    if run_is_active(&run) && changes_running_agent(message, &run)? {
        return Err(Error::conflict(
            "Queue this message to change provider, model or reasoning on the next turn.",
        ));
    }
    Ok(())
}

/// A new chat is named after its first message, or its first attachment.
fn title(message: &Value) -> String {
    let source = if text(message, "text").is_empty() {
        text(&message["attachments"][0], "name")
    } else {
        text(message, "text")
    };
    source
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(TITLE_CHARS)
        .collect()
}

fn message_id_used(db: &Db<'_>, id: &str) -> Result<bool> {
    Ok(db.0.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE id=?)",
        [id],
        |row| row.get::<_, bool>(0),
    )?)
}

fn require_sendable(db: &Db<'_>, chat: &Value, messages: &[Value], id: &str) -> Result<()> {
    let undelivered = messages
        .iter()
        .filter(|message| message["status"] != MessageStatus::Delivered)
        .count();
    if undelivered >= MAX_UNDELIVERED {
        return Err(Error::conflict(
            "The queue is full. Wait for a reply or remove a queued message.",
        ));
    }
    if message_id_used(db, id)? {
        return Err(Error::conflict(
            "This message identifier has already been used.",
        ));
    }
    let agent = required(
        db.get("agents", text(chat, "agentId"))?,
        "This agent is no longer available.",
    )?;
    crate::nodes::require_node(&agent)?;
    task_projects(&agent, chat, &db.list("projects")?)?;
    let cleaned = db
        .run(text(chat, "runId"))?
        .is_some_and(|run| !run["workspaceCleanedAt"].is_null());
    if cleaned {
        return Err(Error::conflict(
            "This workspace has been cleaned up. Start a new chat.",
        ));
    }
    Ok(())
}

pub(crate) fn send(
    db: &Db<'_>,
    chat_id: &str,
    mut values: Value,
    answer: Option<(Question, Value)>,
    origin: &crate::native::policy::Origin,
) -> Result<Value> {
    let mut chat = chat(db, chat_id)?;
    crate::native::policy::authorize_chat(db, &mut chat, origin)?;
    require_active(&chat)?;
    crate::attachments::message(db, chat_id, &mut values)?;
    let messages = db.messages(chat_id)?;
    // Retried submissions are idempotent; a reused identifier with other content is not.
    if let Some(existing) = messages.iter().find(|m| m["id"] == values["id"]) {
        let same_answer = answer
            .as_ref()
            .is_none_or(|(question, _)| existing["questionId"].as_str() == Some(&question.id));
        if !same_content(existing, &values) || !same_answer {
            return Err(Error::conflict(
                "This message identifier has already been used.",
            ));
        }
        return Ok(existing.clone());
    }
    require_sendable(db, &chat, &messages, text(&values, "id"))?;
    validate_steer(db, &chat, &values)?;
    if messages.is_empty() {
        let private_answer = answer
            .as_ref()
            .is_some_and(|(question, _)| question.is_private());
        chat["title"] = if private_answer {
            PRIVATE_ANSWER_TEXT.into()
        } else {
            title(&values).into()
        };
    }
    touch(&mut chat);
    chat["wakeup"] = Value::Null;
    db.put("chats", &chat)?;
    if let Some((mut question, answers)) = answer {
        question.status = QuestionStatus::Answering;
        question.message_id = Some(text(&values, "id").to_owned());
        save_question(db, &question)?;
        values["questionId"] = question.id.into();
        values["answers"] = answers;
    }
    values["chatId"] = chat_id.into();
    values["status"] = MessageStatus::Queued.into();
    values["createdAt"] = now().into();
    db.put_message(&values)
}

/// Deletes a queued message; an answer message reopens its question.
fn withdraw(db: &Db<'_>, chat_id: &str, message: &Value) -> Result<Value> {
    if let Some(question_id) = message["questionId"].as_str()
        && let Some(mut question) = find_question(db, chat_id, question_id)?
    {
        question.status = QuestionStatus::Pending;
        question.message_id = None;
        save_question(db, &question)?;
    }
    db.0.execute(
        "DELETE FROM chat_messages WHERE chat_id=? AND id=?",
        rusqlite::params![chat_id, message["id"].as_str()],
    )?;
    Ok(json!({ "deleted": true }))
}

pub(crate) fn edit(
    db: &Db<'_>,
    chat_id: &str,
    message_id: &str,
    values: Option<Value>,
) -> Result<Value> {
    edit_as(
        db,
        chat_id,
        message_id,
        values,
        &crate::native::policy::Origin::default(),
    )
}

pub(crate) fn edit_as(
    db: &Db<'_>,
    chat_id: &str,
    message_id: &str,
    values: Option<Value>,
    origin: &crate::native::policy::Origin,
) -> Result<Value> {
    let mut chat = chat(db, chat_id)?;
    if values.is_some() {
        crate::native::policy::authorize_chat(db, &mut chat, origin)?;
        db.put("chats", &chat)?;
    }
    require_active(&chat)?;
    let mut current = required(find_message(db, chat_id, message_id)?, "Message not found")?;
    if current["status"] != MessageStatus::Queued {
        return Err(Error::conflict("This message is already being sent."));
    }
    let Some(mut values) = values else {
        return withdraw(db, chat_id, &current);
    };
    if current["questionId"].is_string() {
        return Err(Error::conflict("A submitted answer cannot be edited."));
    }
    crate::attachments::message(db, chat_id, &mut values)?;
    validate_steer(db, &chat, &values)?;
    crate::store::merge(&mut current, &values);
    db.put_message(&current)
}

fn acknowledge(db: &Db<'_>, run_id: &str, message_id: &str) -> Result<()> {
    let Some(mut chat) = chat_for_run(db, run_id)? else {
        return Ok(());
    };
    let chat_id = text(&chat, "id").to_owned();
    let Some(mut message) = find_message(db, &chat_id, message_id)? else {
        return Ok(());
    };
    let settled = message["status"] == MessageStatus::Delivered
        || message["status"] == MessageStatus::Cancelled;
    if settled || !is_active(&chat) {
        return Ok(());
    }
    message["status"] = MessageStatus::Delivered.into();
    db.put_message(&message)?;
    let mut private = false;
    if let Some(question_id) = message["questionId"].as_str()
        && let Some(mut question) = find_question(db, &chat_id, question_id)?
    {
        private = question.is_private();
        question.blocking = false;
        question.status = QuestionStatus::Answered;
        save_question(db, &question)?;
    }
    let text = if private {
        PRIVATE_ANSWER_TEXT
    } else {
        text(&message, "text")
    };
    let payload = serde_json::to_value(UserMessageEvent {
        message_id,
        text,
        attachments: &message["attachments"],
        created_at: &message["createdAt"],
    })?;
    db.event(run_id, "chat.user", text, Some(&payload))?;
    touch(&mut chat);
    db.put("chats", &chat)?;
    Ok(())
}

/// Marks steering messages as sending so the running turn picks them up.
fn claim_steering(
    db: &Db<'_>,
    chat_id: &str,
    paused: bool,
    delivering: &Value,
    run_id: &str,
) -> Result<Vec<Value>> {
    if chat(db, chat_id).is_ok_and(|chat| !is_active(&chat)) {
        return Ok(Vec::new());
    }
    let mut steering = Vec::new();
    for mut message in db.messages(chat_id)? {
        let pending = message["status"] == MessageStatus::Queued
            || message["status"] == MessageStatus::Sending;
        // Answers to questions still flow while the chat is paused.
        let deliverable = !paused || message["questionId"].is_string();
        if deliverable
            && pending
            && message["mode"] == MessageMode::Steer
            && message["id"] != *delivering
        {
            message["status"] = MessageStatus::Sending.into();
            db.put_message(&message)?;
            steering.push(message);
        }
    }
    // Withdrawal commands are not user answers or queued chat messages. Replaying
    // one is harmless: each provider only rejects a request it still holds.
    for question in questions(db, chat_id)? {
        if question.run_id == run_id && question.status == QuestionStatus::Cancelled {
            steering.push(json!({
                "id": format!("withdraw-question:{}", question.id),
                "questionId": question.id,
                "questionCancelled": true,
            }));
        }
    }
    Ok(steering)
}

/// Requeues messages left sending by an interrupted turn and returns the next one.
fn next_queued(db: &Db<'_>, chat_id: &str) -> Result<Option<Value>> {
    let mut messages = db.messages(chat_id)?;
    for message in &mut messages {
        if message["status"] == MessageStatus::Sending {
            message["status"] = MessageStatus::Queued.into();
            db.put_message(message)?;
        }
    }
    Ok(messages
        .into_iter()
        .find(|message| message["status"] == MessageStatus::Queued))
}

/// A chat whose last turn did not succeed waits for the user, unless a deletion
/// cancelled it or the user asked for a fresh session.
fn needs_attention(chat: &Value, run: Option<&Value>) -> bool {
    run.is_some_and(|run| run["status"] != RunStatus::Succeeded)
        && chat["cancelledByDeletion"] != true
        && chat["sessionRestartRequested"] != true
}

/// Applies the provider, model and reasoning chosen for this message.
fn select_agent(snapshot: &mut Value, run: Option<&Value>, message: &Value) -> Result<()> {
    let previous = run.map_or(
        &snapshot["snapshot"]["agent"],
        |run| &run["snapshot"]["agent"],
    );
    let implicit_provider = text(message, "provider").is_empty();
    let provider = if implicit_provider {
        Provider::of_agent(previous)?.as_str()
    } else {
        text(message, "provider")
    }
    .to_owned();
    let inherited_model = text(previous, "model").to_owned();
    let inherited_reasoning = text(previous, "reasoning").to_owned();

    let switched = provider != Provider::of_run(snapshot)?.as_str();
    let agent = &mut snapshot["snapshot"]["agent"];
    if implicit_provider || switched {
        // An implicit follow-up keeps the conversation's qualified selection,
        // even when its profile changed or now has different defaults.
        agent["model"] = if implicit_provider {
            inherited_model
        } else {
            String::new()
        }
        .into();
        agent["reasoning"] = if implicit_provider {
            inherited_reasoning
        } else {
            String::new()
        }
        .into();
    }
    agent["provider"] = provider.into();
    if !text(message, "model").is_empty() {
        agent["model"] = message["model"].clone();
        agent["reasoning"] = text(message, "reasoning").into();
    }
    if !text(message, "reasoning").is_empty() {
        agent["reasoning"] = message["reasoning"].clone();
    }
    Ok(())
}

/// Reuses the chat's run for the next turn, handing the transcript to a fresh
/// native session when the provider changed or a new session was requested.
fn continue_run(
    db: &Db<'_>,
    chat: &Value,
    run: &Value,
    snapshot: &Value,
    mut execution: ChatExecution,
) -> Result<()> {
    let agent = &snapshot["snapshot"]["agent"];
    // The workspace keeps what earlier turns could reach, so reduced access needs a new chat.
    if !crate::service::covers(agent, &run["snapshot"]["agent"]) {
        return Err(Error::conflict(
            "Agent access was reduced. Start a new chat with the updated permissions.",
        ));
    }
    let run_id = text(run, "id");
    let key = format!("run-checkpoint:{run_id}");
    let mut checkpoint = match db.kv(&key)? {
        Some(value) => value,
        None if chat["cancelledByDeletion"] == true => json!({}),
        None => return Err(Error::conflict("Run checkpoint not found")),
    };
    let provider = Provider::of_run(snapshot)?;
    let restart_requested =
        chat["sessionRestartRequested"] == true || checkpoint["freshSession"] == true;
    if provider != Provider::of_run(run)? || restart_requested {
        if run["sessionId"].is_string() {
            checkpoint["workspaceInitialized"] = true.into();
        }
        // The next provider cannot resolve requests from the session being replaced.
        questions::release(db, run_id, None)?;
        checkpoint["freshSession"] = false.into();
        execution.context = Some(handoff_context(db, run_id)?);
        checkpoint["launched"] = false.into();
        remove_keys(&mut checkpoint, &["controllerRecoveries"]);
        db.patch_run(
            run_id,
            &json!({
                "sessionId": null,
                "resumeAvailable": false,
                // The fresh session starts with an empty context.
                "contextUsage": null,
            }),
        )?;
        let status = format!(
            "Continuing with {} · conversation context and workspace preserved",
            provider.label()
        );
        db.event(run_id, "status", &status, None)?;
    }
    checkpoint["completed"] = false.into();
    checkpoint["remainingMs"] = crate::run_limits::budget_ms(agent).into();
    remove_keys(&mut checkpoint, &["lastMessage", "settled"]);
    db.set(&key, &checkpoint, None)?;
    let compaction = execution.operation == Some(COMPACT);
    let mut patch = json!({
        "snapshot": snapshot["snapshot"],
        "status": RunStatus::Queued,
        "error": null,
        "finishedAt": null,
        "cancelRequestedAt": null,
        "recoveryPending": true,
        "chatExecution": serde_json::to_value(execution)?,
    });
    // A compaction answers no message: the last reply and its outcome stay current.
    if !compaction {
        patch["summary"] = "".into();
        patch["outcome"] = Value::Null;
    }
    db.patch_run(run_id, &patch)?;
    Ok(())
}

fn remove_keys(value: &mut Value, keys: &[&str]) {
    if let Some(object) = value.as_object_mut() {
        for key in keys {
            object.remove(*key);
        }
    }
}

/// Starts the agent turn for `message` unless the chat or message changed meanwhile.
fn launch_turn(
    db: &Db<'_>,
    chat_id: &str,
    run: Option<Value>,
    mut message: Value,
    mut snapshot: Value,
) -> Result<()> {
    let mut chat = chat(db, chat_id)?;
    // Preparation can overlap a merge. Retry with the chat's current profile
    // rather than publishing a snapshot of the removed source.
    if chat["agentId"] != snapshot["snapshot"]["agent"]["id"] {
        return Ok(());
    }
    let waking = message["wakeup"].is_object();
    let unchanged = if waking {
        let current = db.run(text(&chat, "runId"))?;
        continuation::ready(db, &chat, current.as_ref(), now())?
            && chat["wakeup"] == message["wakeup"]
            && current.as_ref().map(|run| &run["id"]) == run.as_ref().map(|run| &run["id"])
            && (!message["wakeup"]["condition"].is_object()
                || current.as_ref().is_some_and(|run| {
                    wait_for::observation(db, &chat, run, now())
                        .is_ok_and(|observation| observation.as_ref() == message.get("observation"))
                }))
    } else {
        find_message(db, chat_id, text(&message, "id"))?.is_some_and(|current| {
            current["status"] == MessageStatus::Queued && same_content(&current, &message)
        })
    };
    if !is_active(&chat) || chat["paused"] == true || !unchanged {
        return Ok(());
    }
    let execution = ChatExecution {
        message_id: message["id"].clone(),
        text: with_invoked_skills(text(&message, "text"), &snapshot["snapshot"]["skills"]),
        attachments: message["attachments"].clone(),
        recovery: false,
        context: None,
        operation: None,
        previous: None,
    };
    if let Some(run) = run {
        continue_run(db, &chat, &run, &snapshot, execution)?;
    } else {
        crate::native::policy::apply(&mut snapshot, &chat);
        snapshot["chatExecution"] = serde_json::to_value(execution)?;
        db.add_run(&snapshot, None)?;
        chat["runId"] = snapshot["id"].clone();
    }
    chat["cancelledByDeletion"] = false.into();
    chat["sessionRestartRequested"] = false.into();
    chat["wakeup"] = Value::Null;
    db.put("chats", &chat)?;
    if waking {
        let mut observed_wakeup = message["wakeup"].clone();
        if message["observation"].is_object() {
            observed_wakeup["observation"] = message["observation"].clone();
        }
        db.event(
            text(&chat, "runId"),
            "chat.wakeup",
            "Resuming after scheduled wait",
            Some(&observed_wakeup),
        )?;
        return Ok(());
    }
    message["status"] = MessageStatus::Sending.into();
    db.put_message(&message)?;
    Ok(())
}

pub(crate) fn create_in(db: &Db<'_>, input: Value) -> Result<Value> {
    create_as(db, input, &crate::native::policy::Origin::default())
}

/// Transfers a completed mission's conversation to a chat without replaying it.
fn from_run(db: &Db<'_>, run_id: &str, origin: &crate::native::policy::Origin) -> Result<Value> {
    crate::conversation_lifecycle::require_active_run(db, run_id)?;
    if let Some(chat) = chat_for_run(db, run_id)? {
        return Ok(chat);
    }

    let run = required(db.run(run_id)?, "Run not found")?;
    if run["trigger"] == "chat" || run["status"] != RunStatus::Succeeded {
        return Err(Error::conflict(
            "Wait for this mission to finish successfully before continuing in chat.",
        ));
    }
    if run["recoveryPending"] == true || !run["workspaceCleanedAt"].is_null() {
        return Err(Error::conflict(
            "This mission's workspace is unavailable. Start a new chat.",
        ));
    }
    let checkpoint = required(
        db.kv(&format!("run-checkpoint:{run_id}"))?,
        "Run checkpoint not found",
    )?;
    if !checkpoint["prepared"].is_object() || text(&run, "sessionId").is_empty() {
        return Err(Error::conflict(
            "This mission has no saved conversation available to continue.",
        ));
    }

    crate::native::policy::authorize_run_in(db, run_id, origin)?;
    let run = required(db.run(run_id)?, "Run not found")?;
    let agent = required(
        db.get("agents", text(&run["snapshot"]["agent"], "id"))?,
        "Agent not found",
    )?;
    if !crate::service::covers(&agent, &run["snapshot"]["agent"]) {
        return Err(Error::conflict(
            "Agent access was reduced. Start a new chat with the updated permissions.",
        ));
    }

    let mut chat = json!({
        "id": id(),
        "title": run["snapshot"]["task"]["name"],
        "agentId": agent["id"],
        "projectId": run["projectId"],
        "runId": run_id,
        "paused": false,
        "createdAt": now(),
        "updatedAt": now(),
    });
    crate::native::policy::apply(&mut chat, &run);
    task_projects(&agent, &chat, &db.list("projects")?)?;

    let chat = db.put("chats", &chat)?;
    db.patch_run(
        run_id,
        &json!({
            "sourceTaskId": run["taskId"],
            "sourceTaskPrompt": run["snapshot"]["task"]["prompt"],
            "taskId": chat["id"],
            "trigger": "chat",
        }),
    )?;
    db.0.execute(
        "UPDATE runs SET task_id=? WHERE id=?",
        rusqlite::params![text(&chat, "id"), run_id],
    )?;
    // Work launched by this execution still returns to the same requester,
    // whose conversation now lives in Chats rather than the mission's history.
    db.0.execute(
        "UPDATE runs SET data=json_set(data, '$.requestedBy', json_object('runId', ?, 'chatId', ?))
         WHERE json_extract(data, '$.requestedBy.runId')=?",
        rusqlite::params![run_id, text(&chat, "id"), run_id],
    )?;
    db.restore_chat_summaries(Some(run_id))?;
    db.audit(
        "run.converted_to_chat",
        &json!({ "runId": run_id, "chatId": chat["id"] }),
    )?;
    Ok(chat)
}

pub(crate) fn create_as(
    db: &Db<'_>,
    input: Value,
    origin: &crate::native::policy::Origin,
) -> Result<Value> {
    let input = parse_as::<ChatInput>("chat", input)?;
    let agent = required(db.get("agents", &input.agent_id)?, "Agent not found")?;
    crate::native::policy::authorize_origin(db, &agent, origin)?;
    let created_at = now();
    let mut chat = serde_json::to_value(NewChat {
        id: id(),
        title: "New chat",
        agent_id: input.agent_id,
        project_id: input.project_id,
        run_id: None,
        paused: false,
        created_at,
        updated_at: created_at,
    })?;
    crate::native::policy::authorize_chat(db, &mut chat, origin)?;
    task_projects(&agent, &chat, &db.list("projects")?)?;
    db.put("chats", &chat)
}

pub(crate) fn send_in(db: &Db<'_>, chat: &str, input: Value) -> Result<Value> {
    send(
        db,
        chat,
        parse("message", input)?,
        None,
        &crate::native::policy::Origin::default(),
    )
}

pub(crate) fn pause_in(db: &Db<'_>, id: &str, paused: bool) -> Result<Value> {
    let mut chat = chat(db, id)?;
    require_active(&chat)?;
    if chat["lastActivityAt"].is_null() {
        chat["lastActivityAt"] = chat["updatedAt"].clone();
    }
    chat["paused"] = paused.into();
    chat["updatedAt"] = now().into();
    db.put("chats", &chat)
}

/// Preparation happens outside the writer. A merge may have transferred the
/// conversation since it was read; that stale attempt must simply retry next tick.
fn preparation_failed(db: &Db<'_>, prepared: &Value, error: Error) -> Result<()> {
    let id = text(prepared, "id");
    if chat(db, id)?["agentId"] != prepared["agentId"] {
        return Ok(());
    }
    pause_in(db, id, true)?;
    let shown = if error.status < 500 {
        error.message
    } else {
        "Unable to prepare this conversation.".into()
    };
    db.set(&format!("chat-error:{id}"), &shown.into(), None)
}

impl Service {
    pub async fn chat_from_run(&self, run_id: &str) -> Result<Value> {
        self.chat_from_run_authorized(run_id, |_| Ok(())).await
    }

    /// Rechecks delegated access in the transaction that transfers the conversation.
    pub(crate) async fn chat_from_run_authorized<F>(
        &self,
        run_id: &str,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let origin = crate::native::policy::origin();
        if origin.known() {
            let run = self.store.run(run_id).await?;
            crate::native::policy::refresh_agent(self, text(&run["snapshot"]["agent"], "id"))
                .await?;
        }

        let active = self.worker.active.lock().await;
        if active.contains_key(run_id) {
            return Err(Error::conflict(
                "Wait for this run to finish stopping before continuing in chat.",
            ));
        }
        let run_id = run_id.to_owned();
        self.store
            .transaction(move |db| {
                authorize(db)?;
                from_run(db, &run_id, &origin)
            })
            .await
    }

    pub async fn chat_list(&self) -> Result<Vec<Value>> {
        self.store.read(|db| list(db)).await
    }

    pub async fn chat_detail(&self, id: &str) -> Result<Value> {
        let id = id.to_owned();
        self.store.read(move |db| detail(db, &id)).await
    }

    pub async fn chat_create(&self, input: Value) -> Result<Value> {
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_agent(
                self,
                input["agentId"]
                    .as_str()
                    .unwrap_or(crate::config::MAIN_AGENT_ID),
            )
            .await?;
        }
        self.store
            .transaction(move |db| create_as(db, input, &origin))
            .await
    }

    pub async fn chat_send(&self, id: &str, input: Value) -> Result<Value> {
        let values = parse("message", input)?;
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_chat(self, id).await?;
        }
        let id = id.to_owned();
        let result = self
            .store
            .transaction(move |db| send(db, &id, values, None, &origin))
            .await?;
        self.worker.notify();
        Ok(result)
    }

    pub async fn chat_edit(&self, id: &str, message: &str, input: Option<Value>) -> Result<Value> {
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_chat(self, id).await?;
        }
        let (id, message) = (id.to_owned(), message.to_owned());
        let values = input
            .map(|mut input| {
                input["id"] = message.clone().into();
                parse("message", input)
            })
            .transpose()?;
        let result = self
            .store
            .transaction(move |db| {
                let mut record = chat(db, &id)?;
                if values.is_some() {
                    crate::native::policy::authorize_chat(db, &mut record, &origin)?;
                    db.put("chats", &record)?;
                }
                edit_as(db, &id, &message, values, &origin)
            })
            .await?;
        self.worker.notify();
        Ok(result)
    }

    /// Compacts the conversation's native context while it is idle.
    pub(crate) async fn chat_compact_authorized<F>(&self, id: &str, authorize: F) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_chat(self, id).await?;
        }
        let id = id.to_owned();
        let result = self
            .store
            .transaction(move |db| {
                authorize(db)?;
                compact(db, &id, &origin)
            })
            .await?;
        self.worker.notify();
        Ok(result)
    }

    pub async fn chat_pause(&self, id: &str, paused: bool) -> Result<Value> {
        let id = id.to_owned();
        let result = self
            .store
            .write(move |db| pause_in(db, &id, paused))
            .await?;
        self.worker.notify();
        Ok(result)
    }

    pub async fn chat_acknowledge(&self, run_id: &str, message_id: &str) -> Result<()> {
        let (run_id, message_id) = (run_id.to_owned(), message_id.to_owned());
        self.store
            .transaction(move |db| acknowledge(db, &run_id, &message_id))
            .await
    }

    pub async fn chat_tick(&self, active: &HashSet<String>) -> Result<()> {
        for chat in self.store.list("chats").await? {
            if !is_active(&chat) {
                continue;
            }
            let run = match chat["runId"].as_str() {
                Some(run) => Some(self.store.run(run).await?),
                None => None,
            };
            if let Some(run) = run.as_ref().filter(|run| run_is_active(run)) {
                // Messages sent while compacting wait for the next turn instead of steering.
                if !is_compaction(run) {
                    self.publish_steering(&chat, run).await?;
                }
                continue;
            }
            let busy = run.as_ref().is_some_and(|run| {
                active.contains(text(run, "id")) || run["recoveryPending"] == true
            });
            if busy {
                continue;
            }
            let id = text(&chat, "id").to_owned();
            if needs_attention(&chat, run.as_ref()) {
                self.chat_cancel_wait(&id).await?;
                if chat["paused"] == true {
                    continue;
                }
                let chat_id = id.clone();
                self.store
                    .transaction(move |db| next_queued(db, &chat_id).map(|_| ()))
                    .await?;
                self.chat_pause(&id, true).await?;
                continue;
            }
            if chat["paused"] == true {
                continue;
            }
            // A scheduled wait continues the current task before queued follow-ups.
            let (waiting_chat, waiting_run) = (chat.clone(), run.clone());
            let waiting = self
                .store
                .read(move |db| continuation::holds_queue(db, &waiting_chat, waiting_run.as_ref()))
                .await?;
            let chat_id = id.clone();
            let message = if waiting {
                None
            } else {
                self.store
                    .transaction(move |db| next_queued(db, &chat_id))
                    .await?
            };
            let message = match message {
                Some(message) => message,
                None => {
                    let chat = chat.clone();
                    let run = run.clone();
                    let Some(message) = self
                        .store
                        .read(move |db| continuation::message(db, &chat, run.as_ref(), now()))
                        .await?
                    else {
                        continue;
                    };
                    message
                }
            };
            if let Err(error) = self.chat_prepare(chat.clone(), run, message).await {
                self.store
                    .transaction(move |db| preparation_failed(db, &chat, error))
                    .await?;
            }
        }
        Ok(())
    }

    /// Writes steering messages where the running agent turn reads them.
    async fn publish_steering(&self, chat: &Value, run: &Value) -> Result<()> {
        let chat_id = text(chat, "id").to_owned();
        let paused = chat["paused"] == true;
        let delivering = run["chatExecution"]["messageId"].clone();
        let run_id = text(run, "id").to_owned();
        let mut steering = self
            .store
            .transaction(move |db| claim_steering(db, &chat_id, paused, &delivering, &run_id))
            .await?;
        let skills = &run["snapshot"]["skills"];
        for message in &mut steering {
            self.prepare_chat_files(text(run, "id"), &message["attachments"])
                .await?;
            message["text"] = with_invoked_skills(text(message, "text"), skills).into();
        }
        let directory = self
            .config
            .data_dir
            .join("runs")
            .join(text(chat, "runId"))
            .join("chat-input");
        crate::skills::private_dir(&directory).await?;
        crate::skills::atomic_write(
            &directory.join("messages.json"),
            &serde_json::to_vec(&steering)?,
        )
        .await
    }

    async fn chat_prepare(&self, chat: Value, run: Option<Value>, message: Value) -> Result<()> {
        let prompt = if text(&message, "text").is_empty() {
            "Review the attached files."
        } else {
            text(&message, "text")
        };
        let mut task = parse(
            "task",
            json!({
                "name": chat["title"],
                "prompt": prompt,
                "agentId": chat["agentId"],
                "projectId": chat["projectId"],
                "worktree": true,
            }),
        )?;
        task["id"] = chat["id"].clone();
        task["createdAt"] = chat["createdAt"].clone();
        task["nextRun"] = Value::Null;
        let mut snapshot = self.snapshot(task, "chat").await?;
        select_agent(&mut snapshot, run.as_ref(), &message)?;
        Provider::of_run(&snapshot)?
            .adapter()
            .validate_agent(&snapshot["snapshot"]["agent"])?;
        let chat_id = text(&chat, "id").to_owned();
        self.store
            .transaction(move |db| launch_turn(db, &chat_id, run, message, snapshot))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_merge_during_preparation_keeps_queued_work_unpaused() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store.transaction(|db| {
            let stale = json!({ "id": "chat", "agentId": "source", "paused": false });
            let transferred = json!({ "id": "chat", "agentId": "target", "paused": false });
            let transferred = db.put("chats", &transferred)?;
            let queued = json!({ "id": "message", "chatId": "chat", "createdAt": 1, "status": "queued" });
            db.put_message(&queued)?;
            // The stale agent lookup failed before launch_turn's identity guard.
            preparation_failed(db, &stale, Error::not_found("Record not found"))?;
            assert_eq!(db.get("chats", "chat")?.unwrap(), transferred);
            assert!(db.kv("chat-error:chat")?.is_none());
            assert_eq!(db.messages("chat")?, vec![queued]);
            // A real error on the current profile still pauses and reports normally.
            preparation_failed(db, &transferred, Error::bad("Current preparation failed"))?;
            assert_eq!(db.get("chats", "chat")?.unwrap()["paused"], true);
            assert_eq!(db.kv("chat-error:chat")?.unwrap(), "Current preparation failed");
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn chats_and_run_summaries_show_the_coding_agent_of_the_latest_turn() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                for (id, agent) in [
                    ("switched", json!({ "provider": "claude" })),
                    ("legacy", json!({})),
                ] {
                    let run = json!({
                        "id": id,
                        "taskId": id,
                        "createdAt": 0,
                        "status": "succeeded",
                        "snapshot": { "agent": agent },
                    });
                    db.add_run(&run, None)?;
                    db.put(
                        "chats",
                        &json!({ "id": id, "runId": id, "agentId": "agent" }),
                    )?;
                }
                db.put(
                    "chats",
                    &json!({ "id": "new", "runId": null, "agentId": "agent" }),
                )?;

                let chats = list(db)?;
                let provider = |id: &str| {
                    chats.iter().find(|chat| chat["id"] == id).unwrap()["provider"].clone()
                };
                assert_eq!(provider("switched"), "claude");
                assert_eq!(provider("legacy"), "codex");
                assert!(provider("new").is_null());

                let runs = db.runs(None, None, 10, 0, false)?;
                let provider =
                    |id: &str| runs.iter().find(|run| run["id"] == id).unwrap()["provider"].clone();
                assert_eq!(provider("switched"), "claude");
                assert_eq!(provider("legacy"), "codex");
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn compaction_runs_only_between_turns_and_keeps_the_last_reply() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let origin = crate::native::policy::Origin::default();
                let outcome = json!({ "status": "completed", "reason": "Done", "messageId": "first" });
                let run = json!({
                    "id": "run",
                    "taskId": "chat",
                    "createdAt": 0,
                    "status": "succeeded",
                    "sessionId": "native-session",
                    "summary": "Last reply",
                    "finishedAt": 500,
                    "outcome": outcome,
                    "contextUsage": { "contextTokens": 150_000, "contextWindow": 200_000, "cachedTokens": 120_000, "estimated": false },
                    "chatExecution": { "messageId": "first", "text": "First", "attachments": [], "recovery": false },
                    "snapshot": { "agent": { "provider": "codex" } },
                });
                db.add_run(&run, None)?;
                db.set("run-checkpoint:run", &json!({ "completed": true, "launched": true }), None)?;
                db.put("chats", &json!({ "id": "chat", "runId": "run", "agentId": "agent" }))?;

                let idle = view(db, chat(db, "chat")?)?;
                assert_eq!(idle["contextUsage"]["contextTokens"], 150_000);
                assert_eq!(idle["compaction"], json!({ "supported": true, "running": false, "available": true, "reason": null }));

                // A queued follow-up is delivered first: compaction never reorders the queue.
                let queued = json!({ "id": "queued", "chatId": "chat", "text": "Next", "status": "queued", "createdAt": 1 });
                db.put_message(&queued)?;
                let error = compact(db, "chat", &origin).unwrap_err();
                assert!(error.message.contains("queued messages"), "{}", error.message);
                withdraw(db, "chat", &queued)?;

                let started = compact(db, "chat", &origin)?;
                assert_eq!(started["compaction"]["running"], true);
                assert_eq!(started["compaction"]["available"], false);
                let compacting = db.run("run")?.unwrap();
                assert_eq!(compacting["status"], "queued");
                assert_eq!(compacting["recoveryPending"], true);
                assert_eq!(compacting["chatExecution"]["operation"], COMPACT);
                assert_eq!(compacting["chatExecution"]["previous"], json!({ "summary": "Last reply", "finishedAt": 500 }));
                // The previous reply and its outcome are not replaced by a compaction.
                assert_eq!(compacting["summary"], "Last reply");
                assert_eq!(compacting["outcome"], outcome);
                assert!(db.messages("chat")?.is_empty());

                // A retried request keeps the compaction already started.
                let retried = compact(db, "chat", &origin)?;
                assert_eq!(db.run("run")?.unwrap()["chatExecution"], compacting["chatExecution"]);
                assert_eq!(retried["compaction"]["running"], true);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn compaction_explains_why_it_is_unavailable() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let cases = [
                    (
                        "opencode",
                        "succeeded",
                        Some("session"),
                        "OpenCode does not support",
                    ),
                    (
                        "claude",
                        "failed",
                        Some("session"),
                        "Resume or resolve the last turn",
                    ),
                    (
                        "claude",
                        "running",
                        Some("session"),
                        "Wait for the current turn",
                    ),
                    ("codex", "succeeded", None, "no saved native session"),
                ];
                for (index, (provider, status, session, reason)) in cases.into_iter().enumerate() {
                    let id = format!("case-{index}");
                    db.add_run(
                        &json!({
                            "id": id,
                            "taskId": id,
                            "createdAt": 0,
                            "status": status,
                            "sessionId": session,
                            "snapshot": { "agent": { "provider": provider } },
                        }),
                        None,
                    )?;
                    db.put(
                        "chats",
                        &json!({ "id": id, "runId": id, "agentId": "agent" }),
                    )?;
                    let state = view(db, chat(db, &id)?)?["compaction"].clone();
                    assert_eq!(state["available"], false, "{provider} {status}");
                    assert_eq!(state["supported"], provider != "opencode");
                    assert!(text(&state, "reason").contains(reason), "{state}");
                    assert!(view(db, chat(db, &id)?)?["contextUsage"].is_null());
                    let error =
                        compact(db, &id, &crate::native::policy::Origin::default()).unwrap_err();
                    assert!(error.message.contains(reason), "{}", error.message);
                }

                db.put(
                    "chats",
                    &json!({ "id": "empty", "runId": null, "agentId": "agent" }),
                )?;
                let state = view(db, chat(db, "empty")?)?["compaction"].clone();
                assert_eq!(state["supported"], false);
                assert!(text(&state, "reason").contains("first message"));
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn continuation_consumes_durable_and_legacy_fresh_session_requests_once() {
        use crate::store::Store;

        for legacy in [false, true] {
            for (isolated, native) in [(false, false), (true, false), (true, true)] {
                let root = tempfile::tempdir().unwrap();
                let store = Store::open(root.path()).unwrap();
                store
                    .transaction(move |db| {
                        let run = json!({
                            "id": "run",
                            "taskId": "chat",
                            "createdAt": 0,
                            "status": "failed",
                            "sessionId": "old-native",
                            "snapshot": { "agent": { "provider": "codex" } },
                        });
                        db.add_run(&run, None)?;
                        db.event("run", "chat.user", "Context to preserve", None)?;
                        let chat = json!({
                            "id": "chat",
                            "runId": "run",
                            "sessionRestartRequested": !legacy,
                        });
                        db.put("chats", &chat)?;
                        db.set(
                            "chat-question:chat:old-session",
                            &json!({
                                "id": "old-session",
                                "chatId": "chat",
                                "runId": "run",
                                "status": "pending",
                                "blocking": true,
                                "fields": [],
                            }),
                            None,
                        )?;
                        let mut prepared =
                            json!({ "isolated": isolated, "workspace": "preserved" });
                        if native {
                            prepared["backend"] = "native".into();
                        }
                        db.set(
                            "run-checkpoint:run",
                            &json!({
                                "freshSession": legacy,
                                "completed": true,
                                "launched": true,
                                "prepared": prepared,
                                "controllerRecoveries": 2,
                            }),
                            None,
                        )?;
                        let message = json!({
                            "id": "next",
                            "chatId": "chat",
                            "text": "Continue",
                            "status": "queued",
                            "createdAt": 1,
                        });
                        db.put_message(&message)?;
                        launch_turn(db, "chat", Some(run.clone()), message, run)?;
                        let queued = db.run("run")?.unwrap();
                        assert_eq!(queued["status"], "queued");
                        assert!(queued["sessionId"].is_null());
                        assert_eq!(queued["resumeAvailable"], false);
                        assert!(
                            text(&queued["chatExecution"], "context")
                                .contains("Context to preserve")
                        );
                        let checkpoint = db.kv("run-checkpoint:run")?.unwrap();
                        assert_eq!(checkpoint["prepared"], prepared);
                        assert_eq!(checkpoint["launched"], false);
                        assert_eq!(checkpoint["workspaceInitialized"], true);
                        assert_eq!(checkpoint["freshSession"], false);
                        assert!(checkpoint["controllerRecoveries"].is_null());
                        let chat = db.get("chats", "chat")?.unwrap();
                        assert_eq!(chat["sessionRestartRequested"], false);
                        let question = db.kv("chat-question:chat:old-session")?.unwrap();
                        assert_eq!(question["status"], "cancelled");
                        assert_eq!(question["blocking"], false);

                        // A later ordinary continuation resumes the newly established session.
                        let resumed = db.patch_run("run", &json!({ "sessionId": "new-native" }))?;
                        continue_run(
                            db,
                            &chat,
                            &resumed,
                            &resumed,
                            ChatExecution {
                                message_id: "later".into(),
                                text: "Keep going".into(),
                                attachments: json!([]),
                                recovery: false,
                                context: None,
                                operation: None,
                                previous: None,
                            },
                        )?;
                        let next = db.run("run")?.unwrap();
                        assert_eq!(next["sessionId"], "new-native");
                        assert!(next["chatExecution"]["context"].is_null());
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
        }
    }

    #[test]
    fn implicit_follow_up_inherits_the_previous_engine_model_and_reasoning() {
        let template = json!({
            "snapshot": {
                "agent": { "provider": "codex", "model": "gpt-6-sol", "reasoning": "high" },
            },
        });
        let previous = json!({
            "snapshot": {
                "agent": { "provider": "opencode", "model": "opencode/ling-3.1-flash-free", "reasoning": "medium" },
            },
        });
        let mut snapshot = template.clone();
        select_agent(
            &mut snapshot,
            Some(&previous),
            &json!({ "text": "Continue" }),
        )
        .unwrap();
        assert_eq!(snapshot["snapshot"]["agent"], previous["snapshot"]["agent"]);

        let mut explicit = template;
        select_agent(
            &mut explicit,
            Some(&previous),
            &json!({ "provider": "opencode" }),
        )
        .unwrap();
        assert_eq!(explicit["snapshot"]["agent"]["model"], "");
        assert_eq!(explicit["snapshot"]["agent"]["reasoning"], "");
    }

    #[test]
    fn provider_changes_must_wait_for_the_next_turn_and_model_aliases_are_valid() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE runs(id TEXT,data TEXT);")
            .unwrap();
        let run = json!({
            "status": "running",
            "snapshot": {
                "agent": { "provider": "codex", "model": "gpt-6-sol", "reasoning": "high" },
            },
        });
        connection
            .execute("INSERT INTO runs VALUES('run',?)", [run.to_string()])
            .unwrap();
        let db = Db(&connection);
        let chat = json!({ "runId": "run" });
        let mut message = parse(
            "message",
            json!({
                "id": id(),
                "text": "Continue",
                "provider": "claude",
                "model": "opus[1m]",
                "mode": "steer",
            }),
        )
        .unwrap();
        assert_eq!(
            validate_steer(&db, &chat, &message).unwrap_err().status,
            409
        );
        message["mode"] = "queue".into();
        validate_steer(&db, &chat, &message).unwrap();
        message["mode"] = "steer".into();
        message["provider"] = "codex".into();
        message["model"] = "gpt-6-sol".into();
        validate_steer(&db, &chat, &message).unwrap();
        let unknown = json!({
            "id": id(),
            "text": "Continue",
            "provider": "unknown"
        });
        assert!(parse("message", unknown).is_err());
    }
}
