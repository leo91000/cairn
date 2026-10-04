mod handoff;
mod questions;

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

fn chat_for_run(db: &Db<'_>, run_id: &str) -> Result<Option<Value>> {
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
    chat["status"] = db
        .run(text(&chat, "runId"))?
        .map_or_else(|| "idle".into(), |run| run["status"].clone());
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
fn changes_running_agent(message: &Value, run: &Value) -> bool {
    let agent = &run["snapshot"]["agent"];
    let provider = Value::from(Provider::of_run(run).as_str());
    let differs =
        |key: &str, current: &Value| !text(message, key).is_empty() && message[key] != *current;
    differs("provider", &provider)
        || differs("model", &agent["model"])
        || differs("reasoning", &agent["reasoning"])
}

fn validate_steer(db: &Db<'_>, chat: &Value, message: &Value) -> Result<()> {
    if message["mode"] != MessageMode::Steer {
        return Ok(());
    }
    let Some(run) = db.run(text(chat, "runId"))? else {
        return Ok(());
    };
    if run_is_active(&run) && changes_running_agent(message, &run) {
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

fn send(
    db: &Db<'_>,
    chat_id: &str,
    mut values: Value,
    answer: Option<(Question, Value)>,
) -> Result<Value> {
    let mut chat = chat(db, chat_id)?;
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
        chat["title"] = title(&values).into();
    }
    touch(&mut chat);
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

fn edit(db: &Db<'_>, chat_id: &str, message_id: &str, values: Option<Value>) -> Result<Value> {
    let chat = chat(db, chat_id)?;
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
fn select_agent(snapshot: &mut Value, run: Option<&Value>, message: &Value) {
    let provider = if text(message, "provider").is_empty() {
        let previous = run.map_or(
            &snapshot["snapshot"]["agent"],
            |run| &run["snapshot"]["agent"],
        );
        Provider::of_agent(previous).as_str()
    } else {
        text(message, "provider")
    }
    .to_owned();
    let switched = provider != Provider::of_run(snapshot).as_str();
    let agent = &mut snapshot["snapshot"]["agent"];
    if switched {
        agent["model"] = "".into();
        agent["reasoning"] = "".into();
    }
    agent["provider"] = provider.into();
    if !text(message, "model").is_empty() {
        agent["model"] = message["model"].clone();
        agent["reasoning"] = text(message, "reasoning").into();
    }
    if !text(message, "reasoning").is_empty() {
        agent["reasoning"] = message["reasoning"].clone();
    }
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
    let provider = Provider::of_run(snapshot);
    if provider != Provider::of_run(run) || checkpoint["freshSession"] == true {
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
    let current = find_message(db, chat_id, text(&message, "id"))?;
    let unchanged = current.is_some_and(|current| {
        current["status"] == MessageStatus::Queued && same_content(&current, &message)
    });
    if !is_active(&chat) || chat["paused"] == true || !unchanged {
        return Ok(());
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
        snapshot["chatExecution"] = serde_json::to_value(execution)?;
        db.add_run(&snapshot, None)?;
        chat["runId"] = snapshot["id"].clone();
    }
    chat["cancelledByDeletion"] = false.into();
    chat["sessionRestartRequested"] = false.into();
    db.put("chats", &chat)?;
    message["status"] = MessageStatus::Sending.into();
    db.put_message(&message)?;
    Ok(())
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
        let input = parse_as::<ChatInput>("chat", input)?;
        self.store
            .transaction(move |db| {
                let agent = required(db.get("agents", &input.agent_id)?, "Agent not found")?;
                let created_at = now();
                let chat = serde_json::to_value(NewChat {
                    id: id(),
                    title: "New chat",
                    agent_id: input.agent_id,
                    project_id: input.project_id,
                    run_id: None,
                    paused: false,
                    created_at,
                    updated_at: created_at,
                })?;
                task_projects(&agent, &chat, &db.list("projects")?)?;
                db.put("chats", &chat)
            })
            .await
    }

    pub async fn chat_send(&self, id: &str, input: Value) -> Result<Value> {
        let values = parse("message", input)?;
        let id = id.to_owned();
        let result = self
            .store
            .transaction(move |db| send(db, &id, values, None))
            .await?;
        self.worker.notify();
        Ok(result)
    }

    pub async fn chat_edit(&self, id: &str, message: &str, input: Option<Value>) -> Result<Value> {
        let (id, message) = (id.to_owned(), message.to_owned());
        let values = input
            .map(|mut input| {
                input["id"] = message.clone().into();
                parse("message", input)
            })
            .transpose()?;
        let result = self
            .store
            .transaction(move |db| edit(db, &id, &message, values))
            .await?;
        self.worker.notify();
        Ok(result)
    }

    pub async fn chat_pause(&self, id: &str, paused: bool) -> Result<Value> {
        let id = id.to_owned();
        let result = self
            .store
            .write(move |db| {
                let mut chat = chat(db, &id)?;
                require_active(&chat)?;
                if chat["lastActivityAt"].is_null() {
                    chat["lastActivityAt"] = chat["updatedAt"].clone();
                }
                chat["paused"] = paused.into();
                chat["updatedAt"] = now().into();
                db.put("chats", &chat)
            })
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
                self.publish_steering(&chat, run).await?;
                continue;
            }
            let busy = run.as_ref().is_some_and(|run| {
                active.contains(text(run, "id")) || run["recoveryPending"] == true
            });
            if chat["paused"] == true || busy {
                continue;
            }
            let id = text(&chat, "id").to_owned();
            if needs_attention(&chat, run.as_ref()) {
                self.chat_pause(&id, true).await?;
                continue;
            }
            let chat_id = id.clone();
            let Some(message) = self
                .store
                .transaction(move |db| next_queued(db, &chat_id))
                .await?
            else {
                continue;
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
        let paused = chat["paused"] == true;
        let delivering = run["chatExecution"]["messageId"].clone();
        let mut steering = self
            .store
            .transaction(move |db| claim_steering(db, &chat_id, paused, &delivering))
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
        select_agent(&mut snapshot, run.as_ref(), &message);
        crate::claude::validate_agent(&snapshot["snapshot"]["agent"])?;
        let chat_id = text(&chat, "id").to_owned();
        self.store
            .transaction(move |db| launch_turn(db, &chat_id, run, message, snapshot))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
