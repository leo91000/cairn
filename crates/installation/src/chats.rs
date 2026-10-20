mod handoff;
pub(crate) mod questions;
pub mod sleep;
pub mod wait_for;

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
    // When the agent's latest turn ended, for what the Fil shows as unread.
    chat["finishedAt"] = run.map_or(Value::Null, |run| run["finishedAt"].clone());
    Ok(chat)
}

pub(crate) fn list(db: &Db<'_>) -> Result<Vec<Value>> {
    Ok(list_all(db)?.into_iter().filter(is_active).collect())
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
        .filter(|message| {
            message["status"] == MessageStatus::Queued
                || message["status"] == MessageStatus::Sending
        })
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
        db.delete(&crate::plugins::turns::key(chat_id, message_id))?;
        return withdraw(db, chat_id, &current);
    };
    if current["questionId"].is_string() {
        return Err(Error::conflict("A submitted answer cannot be edited."));
    }
    crate::attachments::message(db, chat_id, &mut values)?;
    validate_steer(db, &chat, &values)?;
    crate::store::merge(&mut current, &values);
    db.delete(&crate::plugins::turns::key(chat_id, message_id))?;
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

/// A source refusal cancels just this control and reopens its still-current
/// question. Temporary authority outages leave the input queued for a later tick.
fn steering_ready(
    db: &Db<'_>,
    chat: &str,
    run: &Value,
    message: &Value,
    source: &Result<Option<crate::plugins::requesters::SteeringSource>>,
) -> Result<bool> {
    let allowed = crate::plugins::requesters::steering_authorized(db, chat, run, message, source);
    match allowed {
        Ok(true) => return Ok(true),
        Err(error) if error.status == 503 => return Ok(false),
        Err(error) if !crate::plugins::requesters::ExecutionSource::is_revoked(&error) => {
            return Err(error);
        }
        _ => {}
    }
    let mut refused = message.clone();
    refused["status"] = MessageStatus::Cancelled.into();
    db.put_message(&refused)?;
    if let Some(id) = message["questionId"].as_str()
        && let Some(mut question) = find_question(db, chat, id)?
        && question.run_id == text(run, "id")
        && question.turn_id == text(&run["chatExecution"], "messageId")
        && question.message_id.as_deref() == message["id"].as_str()
        && question.status == QuestionStatus::Answering
    {
        question.status = QuestionStatus::Pending;
        question.message_id = None;
        save_question(db, &question)?;
    }
    db.audit(
        "chat.steering.refused",
        &json!({
            "chatId": chat,
            "runId": run["id"],
            "messageId": message["id"],
        }),
    )?;
    Ok(false)
}

/// Marks steering messages as sending so the running turn picks them up.
fn claim_steering(
    db: &Db<'_>,
    chat_id: &str,
    delivering: &Value,
    run_id: &str,
    source: &Result<Option<crate::plugins::requesters::SteeringSource>>,
) -> Result<Vec<Value>> {
    let Some(current_chat) = db.get("chats", chat_id)? else {
        return Ok(Vec::new());
    };
    if !is_active(&current_chat) || current_chat["runId"] != run_id {
        return Ok(Vec::new());
    }
    let Some(run) = db.run(run_id)? else {
        return Ok(Vec::new());
    };
    if !run_is_active(&run) || run["chatExecution"]["messageId"] != *delivering {
        return Ok(Vec::new());
    }
    let paused = current_chat["paused"] == true;
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
            if !steering_ready(db, chat_id, &run, &message, source)? {
                continue;
            }
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

/// Abandon a settled turn without replaying its original prompt. Called under
/// the worker's preparation and active guards, with the next requester rechecked.
pub(crate) fn request_next_turn_in(db: &Db<'_>, chat_id: &str, run_id: &str) -> Result<bool> {
    let mut chat = chat(db, chat_id)?;
    require_active(&chat)?;
    let run = required(db.run(run_id)?, "Run not found")?;
    if chat["paused"] == true
        || chat["runId"] != run_id
        || !RunStatus::of(&run).is_some_and(RunStatus::is_resumable)
    {
        return Ok(false);
    }
    let checkpoint_key = format!("run-checkpoint:{run_id}");
    let mut checkpoint = db.kv(&checkpoint_key)?.unwrap_or_else(|| json!({}));
    if run["recoveryPending"] == true {
        return Ok(false);
    }
    // completed records agent success, not process settlement. The worker guards
    // exclude a live execution; continue_run schedules native fencing of the saved
    // process/runner before the next request may prepare or launch its workspace.
    if !run["workspaceCleanedAt"].is_null() {
        return Err(Error::conflict(
            "The workspace was cleaned up. Start a new conversation.",
        ));
    }
    let old_message = text(&run["chatExecution"], "messageId");
    db.set(
        &crate::plugins::turns::abandoned_key(chat_id, old_message),
        &json!({
            "runId": run_id,
            "failed": run["status"] != RunStatus::Cancelled,
        }),
        None,
    )?;
    let mut answers = HashSet::new();
    for mut question in questions(db, chat_id)? {
        if question.run_id == run_id && question.turn_id == old_message && question.is_open() {
            if let Some(message) = &question.message_id {
                answers.insert(message.clone());
            }
            question.status = QuestionStatus::Cancelled;
            question.blocking = false;
            save_question(db, &question)?;
        }
    }
    for mut message in db.messages(chat_id)? {
        let pending = message["status"] == MessageStatus::Queued
            || message["status"] == MessageStatus::Sending;
        if !pending {
            continue;
        }
        if message["id"] == old_message || answers.contains(text(&message, "id")) {
            message["status"] = MessageStatus::Cancelled.into();
        } else if message["mode"] == MessageMode::Steer {
            message["mode"] = MessageMode::Queue.into();
        }
        db.put_message(&message)?;
    }
    checkpoint["freshSession"] = true.into();
    checkpoint["launched"] = false.into();
    db.set(&checkpoint_key, &checkpoint, None)?;
    db.patch_run(
        run_id,
        &json!({
            "sessionId": null,
            "resumeAvailable": false,
            "retry": null,
        }),
    )?;
    chat["sessionRestartRequested"] = true.into();
    chat["wakeup"] = Value::Null;
    db.put("chats", &chat)?;
    Ok(true)
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
    if switched {
        // An implicit follow-up keeps the previous engine's qualified selection,
        // even when the saved agent template still names a different engine.
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
                "resumeAvailable": false
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
    db.patch_run(
        run_id,
        &json!({
            "snapshot": snapshot["snapshot"],
            "status": RunStatus::Queued,
            "summary": "",
            "error": null,
            "outcome": null,
            "finishedAt": null,
            "cancelRequestedAt": null,
            "recoveryPending": true,
            "chatExecution": serde_json::to_value(execution)?,
        }),
    )?;
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
    let waking = message["wakeup"].is_object();
    let unchanged = if waking {
        let current = db.run(text(&chat, "runId"))?;
        sleep::ready(db, &chat, current.as_ref(), now())?
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
    if waking {
        let previous = run
            .as_ref()
            .ok_or_else(|| Error::conflict("The wakeup's run was removed."))?;
        crate::plugins::requesters::capture_wakeup(db, chat_id, previous, &message)?;
    }
    if let Some(crate::plugins::requesters::Principal::External(person)) =
        crate::plugins::requesters::recorded(db, chat_id, text(&message, "id"))?
    {
        crate::plugins::requesters::external_in(db, &person)?;
    }
    let execution = ChatExecution {
        message_id: message["id"].clone(),
        text: with_invoked_skills(text(&message, "text"), &snapshot["snapshot"]["skills"]),
        attachments: message["attachments"].clone(),
        recovery: false,
        context: None,
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
    db.delete(&crate::plugins::turns::key(chat_id, text(&message, "id")))?;
    db.put_message(&message)?;
    Ok(())
}

pub(crate) fn create_in(db: &Db<'_>, input: Value) -> Result<Value> {
    create_as(db, input, &crate::native::policy::Origin::default())
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

/// Capture the host-verified requester in the same transaction as the message.
/// Management commands supply no personal author.
/// Native host authority is separate and never supplies personal OAuth proof.
pub(crate) fn send_in(
    db: &Db<'_>,
    chat: &str,
    input: Value,
    requester: Option<&crate::plugins::requesters::Principal>,
    origin: &crate::native::policy::Origin,
) -> Result<Value> {
    let message = parse("message", input)?;
    crate::plugins::requesters::capture_principal(
        db,
        chat,
        &message,
        requester,
        crate::plugins::requesters::Capture::New,
    )?;
    send(db, chat, message, None, origin)
}

pub(crate) fn edit_in(
    db: &Db<'_>,
    chat: &str,
    message: &str,
    input: Option<Value>,
    origin: &crate::native::policy::Origin,
) -> Result<Value> {
    let values = input
        .map(|mut input| {
            input["id"] = message.into();
            parse("message", input)
        })
        .transpose()?;
    let attribution = values
        .as_ref()
        .cloned()
        .unwrap_or_else(|| json!({"id": message}));
    crate::plugins::requesters::capture(
        db,
        chat,
        &attribution,
        None,
        crate::plugins::requesters::Capture::Edit,
    )?;
    edit_as(db, chat, message, values, origin)
}

pub(crate) fn pause_in(db: &Db<'_>, id: &str, paused: bool) -> Result<Value> {
    set_pause_in(db, id, paused, true)
}

fn set_pause_in(db: &Db<'_>, id: &str, paused: bool, by_user: bool) -> Result<Value> {
    let mut chat = chat(db, id)?;
    require_active(&chat)?;
    if chat["lastActivityAt"].is_null() {
        chat["lastActivityAt"] = chat["updatedAt"].clone();
    }
    if by_user {
        chat["pausedByUser"] = paused.into();
    }
    chat["paused"] = (paused || chat["pausedByUser"] == true).into();
    chat["updatedAt"] = now().into();
    db.put("chats", &chat)
}

impl Service {
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
        self.chat_send_as(id, input, None).await
    }

    pub async fn chat_send_as(
        &self,
        id: &str,
        input: Value,
        identity: Option<&crate::auth::InstallationIdentity>,
    ) -> Result<Value> {
        let values = parse("message", input)?;
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_chat(self, id).await?;
        }

        let actor = match identity {
            Some(identity) => {
                Some(crate::plugins::requesters::message_actor(self, &identity.account_id).await?)
            }
            None => None,
        };

        let id = id.to_owned();
        let result = self
            .store
            .transaction(move |db| {
                crate::plugins::requesters::capture(
                    db,
                    &id,
                    &values,
                    actor.as_ref(),
                    crate::plugins::requesters::Capture::New,
                )?;
                send(db, &id, values, None, &origin)
            })
            .await?;
        self.worker.notify();
        Ok(result)
    }

    pub async fn chat_edit(&self, id: &str, message: &str, input: Option<Value>) -> Result<Value> {
        self.chat_edit_as(id, message, input, None).await
    }

    pub async fn chat_edit_as(
        &self,
        id: &str,
        message: &str,
        input: Option<Value>,
        identity: Option<&crate::auth::InstallationIdentity>,
    ) -> Result<Value> {
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_chat(self, id).await?;
        }

        let actor = match identity {
            Some(identity) if input.is_some() => {
                Some(crate::plugins::requesters::message_actor(self, &identity.account_id).await?)
            }
            _ => None,
        };
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
                let attribution = values
                    .as_ref()
                    .cloned()
                    .unwrap_or_else(|| json!({"id": message}));
                crate::plugins::requesters::capture(
                    db,
                    &id,
                    &attribution,
                    actor.as_ref(),
                    crate::plugins::requesters::Capture::Edit,
                )?;
                edit_as(db, &id, &message, values, &origin)
            })
            .await?;
        self.worker.notify();
        Ok(result)
    }

    pub async fn chat_pause(&self, id: &str, paused: bool) -> Result<Value> {
        self.set_chat_pause(id, paused, false).await
    }

    pub(crate) async fn chat_pause_by_user(&self, id: &str, paused: bool) -> Result<Value> {
        self.set_chat_pause(id, paused, true).await
    }

    async fn set_chat_pause(&self, id: &str, paused: bool, by_user: bool) -> Result<Value> {
        let id = id.to_owned();
        let result = self
            .store
            .write(move |db| set_pause_in(db, &id, paused, by_user))
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
        for mut chat in self.store.list("chats").await? {
            if !is_active(&chat) {
                continue;
            }
            let run = match chat["runId"].as_str() {
                Some(run) => Some(self.store.run(run).await?),
                None => None,
            };
            if let Some(run) = run.as_ref().filter(|run| run_is_active(run)) {
                self.publish_steering(&chat, run).await?;
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
                let advance = match crate::plugins::turns::advance(
                    self,
                    &chat,
                    run.as_ref().unwrap(),
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        self.chat_pause(&id, true).await?;
                        let shown = if error.status < 500 {
                            error.message
                        } else {
                            "Unable to start a new turn.".into()
                        };
                        self.store
                            .set(&format!("chat-error:{id}"), shown.into(), None)
                            .await?;
                        continue;
                    }
                };
                match advance {
                    crate::plugins::turns::Advance::Waiting => continue,
                    crate::plugins::turns::Advance::Ready => {
                        chat = self.get("chats", &id).await?;
                    }
                    crate::plugins::turns::Advance::NoRequest => {
                        self.chat_cancel_sleep(&id).await?;
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
                }
            }
            if chat["paused"] == true {
                continue;
            }
            // A scheduled wait continues the current task before queued follow-ups.
            let (waiting_chat, waiting_run) = (chat.clone(), run.clone());
            let waiting = self
                .store
                .read(move |db| sleep::holds_queue(db, &waiting_chat, waiting_run.as_ref()))
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
                        .read(move |db| sleep::message(db, &chat, run.as_ref(), now()))
                        .await?
                    else {
                        continue;
                    };
                    message
                }
            };
            if let Err(error) = self.chat_prepare(chat, run, message).await {
                self.chat_pause(&id, true).await?;
                let shown = if error.status < 500 {
                    error.message
                } else {
                    "Unable to prepare this conversation.".into()
                };
                self.store
                    .set(&format!("chat-error:{id}"), shown.into(), None)
                    .await?;
            }
        }
        Ok(())
    }

    /// Writes steering messages where the running agent turn reads them.
    async fn publish_steering(&self, chat: &Value, run: &Value) -> Result<()> {
        let chat_id = text(chat, "id").to_owned();
        let delivering = run["chatExecution"]["messageId"].clone();
        let run_id = text(run, "id").to_owned();
        let source = crate::plugins::requesters::SteeringSource::capture(self, run).await;
        let admitted = source.clone();
        let mut steering = self
            .store
            .transaction(move |db| claim_steering(db, &chat_id, &delivering, &run_id, &admitted))
            .await?;
        let skills = &run["snapshot"]["skills"];
        for message in &mut steering {
            self.prepare_chat_files(text(run, "id"), &message["attachments"])
                .await?;
            message["text"] = with_invoked_skills(text(message, "text"), skills).into();
        }
        let source = match source {
            Ok(Some(source)) => source.recheck(self).await.map(|()| Some(source)),
            value => value,
        };
        let (chat_id, expected_run) = (text(chat, "id").to_owned(), run.clone());
        let steering = self
            .store
            .transaction(move |db| {
                let Some(chat) = db.get("chats", &chat_id)? else {
                    return Ok(Vec::new());
                };
                let Some(run) = db.run(text(&expected_run, "id"))? else {
                    return Ok(Vec::new());
                };
                if !is_active(&chat)
                    || chat["runId"] != expected_run["id"]
                    || !run_is_active(&run)
                    || run["chatExecution"]["messageId"]
                        != expected_run["chatExecution"]["messageId"]
                {
                    return Ok(Vec::new());
                }
                let mut ready = Vec::new();
                for message in steering {
                    if message["questionCancelled"] == true {
                        ready.push(message);
                    } else if let Some(current) = find_message(db, &chat_id, text(&message, "id"))?
                        && current["status"] == MessageStatus::Sending
                        && steering_ready(db, &chat_id, &run, &current, &source)?
                    {
                        ready.push(message);
                    }
                }
                Ok(ready)
            })
            .await?;
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
