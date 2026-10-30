pub(crate) mod coding_agent;
pub(crate) mod continuation;

use crate::worker::checkpoint::RunCheckpoint;
use cairn_contracts::EventKind;

mod handoff;
pub(crate) mod question_migration;
pub(crate) mod question_storage;
pub(crate) mod questions;

pub use crate::waits as wait_for;

pub use handoff::{execution_text, with_invoked_skills};
pub use questions::{Question, QuestionField, QuestionStatus};

use crate::{
    config::{id, now},
    conversation_lifecycle::{is_active, require_active},
    error::{Error, Result, required},
    provider::Provider,
    run_status::RunStatus,
    service::{Service, task_projects},
    store::Db,
    validation::text,
};
use handoff::handoff_context;
use questions::{find_question, questions, save_question};
use rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;
pub use storage::{Conversation, Message, MessageAnswer};

pub mod engine_switch;
pub(crate) mod migration;
#[cfg(test)]
mod read_tests;
pub(crate) mod storage;

pub use cairn_contracts::{MessageMode, MessageStatus};

const MAX_UNDELIVERED: usize = 20;
const TITLE_CHARS: usize = 90;
const PRIVATE_ANSWER_TEXT: &str = "Answered a private question.";

const CONTINUED_MISSION: &str = concat!(
    "This mission now continues as a chat; the unattended rule given earlier ",
    "no longer applies."
);

use cairn_contracts::{ChatExecution, ConversationEnvironmentState as EnvironmentState};

/// The `chatExecution.operation` of a context compaction.
pub(crate) const COMPACT: &str = "compact";

pub(crate) fn is_compaction(run: &Value) -> bool {
    run["chatExecution"]["operation"] == COMPACT
}

/// A compaction is not a reply: once it succeeds, the conversation keeps its last reply and
/// its outcome, and does not become newly finished. Resuming an interrupted compaction clears
/// the run's outcome, so it comes from what the compaction saved. Failures and stops still show
/// why it ended.
pub(crate) fn keep_reply_after_compaction(
    run: &Value,
    patch: &mut crate::runs::RunChanges,
) -> Result<()> {
    if !is_compaction(run) {
        return Ok(());
    }
    let previous = &run["chatExecution"]["previous"];
    patch.summary = Some(serde_json::from_value(previous["summary"].clone())?);
    patch.finished_at = Some(serde_json::from_value(previous["finishedAt"].clone())?);
    patch.boundary.get_or_insert_with(|| json!({}))["outcome"] = previous["outcome"].clone();
    Ok(())
}

/// Payload of the `chat.user` event recorded when a message reaches the agent.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UserMessageEvent<'a> {
    message_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    question_id: Option<&'a str>,
    text: &'a str,
    attachments: &'a [cairn_contracts::ChatAttachment],
    created_at: i64,
}

fn chat(db: &Db<'_>, id: &str) -> Result<Value> {
    required(db.conversation_boundary(id)?, "Chat not found")
}

fn conversation(db: &Db<'_>, id: &str) -> Result<Conversation> {
    required(db.conversation(id)?, "Chat not found")
}

pub(crate) fn chat_for_run(db: &Db<'_>, run_id: &str) -> Result<Option<Value>> {
    db.conversation_for_run(run_id)?
        .map(serde_json::to_value)
        .transpose()
        .map_err(Into::into)
}

fn require_live(chat: &Conversation) -> Result<()> {
    if chat.lifecycle != cairn_contracts::ConversationLifecycle::Active {
        return Err(Error::conflict("This conversation has been deleted."));
    }
    Ok(())
}

fn authorize_conversation(
    db: &Db<'_>,
    chat: &mut Conversation,
    origin: &crate::native::policy::Origin,
) -> Result<()> {
    let mut boundary = serde_json::to_value(&*chat)?;
    crate::native::policy::authorize_chat(db, &mut boundary, origin)?;
    *chat = serde_json::from_value(boundary)?;
    Ok(())
}

fn find_message(db: &Db<'_>, chat: &str, id: &str) -> Result<Option<Message>> {
    db.conversation_message(chat, id)
}

fn run_is_active(run: &Value) -> bool {
    RunStatus::of(run).is_some_and(RunStatus::is_active)
}

fn touch(chat: &mut Conversation) {
    chat.updated_at = now();
    chat.last_activity_at = Some(chat.updated_at);
}

/// Same text, agent settings and attachments.
fn same_content(a: &Message, b: &Message) -> bool {
    a.text == b.text
        && a.provider == b.provider
        && a.model == b.model
        && a.reasoning == b.reasoning
        && a.speed == b.speed
        && a.attachments == b.attachments
}

// Other domains still accept a JSON projection; current conversation decisions
// and views use the named state and public contract directly.
pub(crate) fn view(db: &Db<'_>, chat: Value) -> Result<Value> {
    Ok(serde_json::to_value(conversation_view(
        db,
        serde_json::from_value(chat)?,
    )?)?)
}

fn conversation_view(db: &Db<'_>, chat: Conversation) -> Result<cairn_contracts::Chat> {
    let run = db.conversation_run(chat.run_id.as_deref().unwrap_or(""))?;
    conversation_view_with_run(db, chat, run.as_ref())
}

fn conversation_view_with_run(
    db: &Db<'_>,
    chat: Conversation,
    run: Option<&Value>,
) -> Result<cairn_contracts::Chat> {
    use cairn_contracts::{Chat, ChatRequester, ConversationLifecycle, ConversationStatus};
    let active = chat.lifecycle == ConversationLifecycle::Active;
    let pending_questions = questions(db, &chat.id)?
        .iter()
        .filter(|question| question.status == QuestionStatus::Pending)
        .count() as u32;
    let agent_name =
        db.0.query_row(
            "SELECT name FROM agents WHERE id=?",
            [&chat.agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_else(|| {
            chat.agent_name
                .clone()
                .unwrap_or_else(|| "Deleted agent".into())
        });
    let project_name = chat
        .project_id
        .as_ref()
        .map(|id| {
            Ok::<_, Error>(
                db.0.query_row("SELECT name FROM projects WHERE id=?", [id], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?
                .unwrap_or_else(|| {
                    chat.project_name
                        .clone()
                        .unwrap_or_else(|| "Deleted project".into())
                }),
            )
        })
        .transpose()?;
    let pending_delegation = run.is_none()
        && chat.requested_by.is_some()
        && db.conversation_messages(&chat.id)?.iter().any(|message| {
            matches!(
                message.status,
                MessageStatus::Queued | MessageStatus::Sending
            )
        });
    let status = if pending_delegation {
        ConversationStatus::Queued
    } else {
        run.as_ref()
            .map(|run| serde_json::from_value(run["status"].clone()))
            .transpose()?
            .unwrap_or(ConversationStatus::Idle)
    };
    let requester = chat
        .requested_by
        .as_ref()
        .map(|requester| {
            Ok::<_, Error>(
                db.conversation(&requester.chat_id)?
                    .filter(|requester| requester.lifecycle == ConversationLifecycle::Active)
                    .map(|requester| ChatRequester {
                        chat_id: requester.id,
                        title: requester.title,
                    }),
            )
        })
        .transpose()?
        .flatten();
    let compaction = Some(compaction_state(db, &chat, run)?);
    let coding_agent_switch = coding_agent::progress(db, &chat.id)?;
    let error = if active {
        db.kv(&format!("chat-error:{}", chat.id))?
            .map(serde_json::from_value)
            .transpose()?
    } else {
        None
    };
    // Run-owned fields remain at A5/A10's explicit projection boundary.
    let run_field = |field: &str| run.as_ref().map_or(Value::Null, |run| run[field].clone());
    let provider = run
        .as_ref()
        .and_then(|run| Provider::of_run(run).ok())
        .map(|provider| serde_json::from_value(Value::from(provider.as_str())))
        .transpose()?;
    Ok(Chat {
        can_request_new_session: crate::conversation_lifecycle::can_request_new_session(&chat, run),
        finished_at: serde_json::from_value(run_field("finishedAt"))?,
        context_usage: serde_json::from_value(run_field("contextUsage"))?,
        account_wait_reason: serde_json::from_value(run_field("accountWaitReason"))?,
        account_required: serde_json::from_value(run_field("accountRequired"))?,
        suspended_by_exclusive: serde_json::from_value(run_field("suspendedByExclusive"))?,
        id: chat.id,
        slug: chat.slug,
        revision: chat.revision,
        title: chat.title,
        agent_id: chat.agent_id,
        project_id: chat.project_id,
        run_id: chat.run_id,
        paused: chat.paused,
        lifecycle: chat.lifecycle,
        created_at: chat.created_at,
        updated_at: chat.updated_at,
        last_activity_at: chat.last_activity_at,
        trashed_at: chat.trashed_at,
        purge_at: chat.purge_at,
        restored_at: chat.restored_at,
        session_restart_requested: chat.session_restart_requested,
        unread: false,
        lifecycle_error: chat.lifecycle_error,
        environment_state: chat.environment_state,
        environment_expired_at: chat.environment_expired_at,
        environment_error: chat.environment_error,
        requested_by: chat.requested_by,
        wakeup: chat.wakeup,
        requester,
        provider,
        compaction,
        coding_agent_switch,
        error,
        pending_questions,
        agent_name,
        project_name,
        status,
        preview: None,
        questions: Vec::new(),
        messages: Vec::new(),
        run: None,
    })
}

/// Whether the conversation's coding agent can compact its context, whether it is
/// compacting now, and why a compaction cannot start when it cannot.
fn compaction_state(
    db: &Db<'_>,
    chat: &Conversation,
    run: Option<&Value>,
) -> Result<cairn_contracts::ChatCompaction> {
    let provider = run.and_then(|run| Provider::of_run(run).ok());
    let supported = provider.is_some_and(Provider::compacts_context);
    let running = run.is_some_and(|run| run_is_active(run) && is_compaction(run));
    let blocker = if running {
        Some("The conversation is already compacting its context.".to_owned())
    } else {
        compaction_blocker(db, chat, run)?
    };
    Ok(cairn_contracts::ChatCompaction {
        supported,
        running,
        available: blocker.is_none(),
        reason: blocker,
    })
}

/// Why the conversation cannot compact its native context now, if it cannot.
fn compaction_blocker(
    db: &Db<'_>,
    chat: &Conversation,
    run: Option<&Value>,
) -> Result<Option<String>> {
    if chat.lifecycle != cairn_contracts::ConversationLifecycle::Active {
        return Ok(Some("This conversation has been deleted.".into()));
    }
    if chat.environment_state.is_some() {
        return Ok(Some(
            "The working environment expired. Send a new message to start a fresh session.".into(),
        ));
    }
    let Some(run) = run else {
        return Ok(Some("Send a first message before compacting.".into()));
    };
    // The view of a run that names no coding agent still opens: it just cannot compact.
    let Ok(provider) = Provider::of_run(run) else {
        return Ok(Some(
            "This conversation's coding agent does not support context compaction.".into(),
        ));
    };
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
    let fresh_session =
        chat.session_restart_requested || RunCheckpoint::fresh_session_in(db, text(run, "id"))?;
    if !run["sessionId"].is_string() || fresh_session {
        return Ok(Some(
            "This conversation has no saved native session to compact.".into(),
        ));
    }
    if chat.wakeup.is_some() {
        return Ok(Some(
            "Compact after the scheduled continuation resumes.".into(),
        ));
    }
    let undelivered: bool = db.0.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE chat_id=?
         AND status IN ('queued','sending'))",
        [&chat.id],
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
    let mut chat = conversation(db, chat_id)?;
    authorize_conversation(db, &mut chat, origin)?;
    require_live(&chat)?;
    let run = db.run(chat.run_id.as_deref().unwrap_or(""))?;
    if run
        .as_ref()
        .is_some_and(|run| run_is_active(run) && is_compaction(run))
    {
        // A retried request after a lost response keeps the compaction already started.
        return view(db, serde_json::to_value(chat)?);
    }
    if let Some(reason) = compaction_blocker(db, &chat, run.as_ref())? {
        return Err(Error::conflict(reason));
    }
    let run = run.unwrap();
    let execution = ChatExecution {
        message_id: format!("compact:{}", id()),
        text: String::new(),
        attachments: Vec::new(),
        recovery: false,
        context: None,
        operation: Some(COMPACT.into()),
        previous: Some(serde_json::from_value(json!({
            "summary": run["summary"],
            "finishedAt": run["finishedAt"],
            "outcome": run["outcome"],
        }))?),
    };
    continue_run(db, &chat, &run, &run, execution)?;
    db.event(
        text(&run, "id"),
        &EventKind::Status,
        "Compacting the conversation context",
        None,
    )?;
    let saved = db.save_conversation(&chat)?;
    Ok(serde_json::to_value(conversation_view(db, saved)?)?)
}

/// Active conversations. Deleted ones are skipped before their view is built.
pub(crate) fn list(db: &Db<'_>) -> Result<Vec<Value>> {
    db.conversations()?
        .into_iter()
        .filter(|chat| chat.lifecycle == cairn_contracts::ConversationLifecycle::Active)
        .map(|chat| Ok(serde_json::to_value(conversation_view(db, chat)?)?))
        .collect()
}

pub(crate) fn list_all(db: &Db<'_>) -> Result<Vec<Value>> {
    db.conversations()?
        .into_iter()
        .map(|chat| Ok(serde_json::to_value(conversation_view(db, chat)?)?))
        .collect()
}

pub(crate) fn detail(db: &Db<'_>, id: &str) -> Result<Value> {
    detail_messages(db, id, false)
}

fn detail_messages(db: &Db<'_>, id: &str, undelivered: bool) -> Result<Value> {
    let chat = conversation(db, id)?;
    let run = db.run(chat.run_id.as_deref().unwrap_or(""))?;
    let mut result = serde_json::to_value(conversation_view_with_run(db, chat, run.as_ref())?)?;
    if !is_active(&result) {
        result["questions"] = json!([]);
        result["messages"] = json!([]);
        result["run"] = Value::Null;
        result["pendingQuestions"] = 0.into();
        return Ok(result);
    }
    result["questions"] = serde_json::to_value(questions(db, id)?)?;
    result["messages"] = if undelivered {
        db.undelivered_messages(id)?
    } else {
        db.messages(id)?
    }
    .into();
    result["run"] = run.unwrap_or(Value::Null);
    Ok(result)
}

/// One redacted read for every transport. Answers stay in A8's delivery storage;
/// public history uses their structured text, with private replies masked.
pub(crate) fn public_detail(db: &Db<'_>, id: &str) -> Result<Value> {
    public_stream_detail(db, id, false)
}

/// A window already carries delivered input in its event history. Filter it in
/// SQL, before decoding message bodies and loading their attachments.
pub(crate) fn public_stream_detail(db: &Db<'_>, id: &str, window: bool) -> Result<Value> {
    if db.0.is_autocommit() {
        let transaction =
            rusqlite::Transaction::new_unchecked(db.0, rusqlite::TransactionBehavior::Deferred)?;
        let result = public_stream_detail(&Db(&transaction), id, window)?;
        transaction.commit()?;
        return Ok(result);
    }
    let mut result = detail_messages(db, id, window)?;
    let private = db.private_answer_messages(id)?;
    if let Some(messages) = result["messages"].as_array_mut() {
        for message in messages {
            if message["id"]
                .as_str()
                .is_some_and(|id| private.contains(id))
            {
                message["text"] = "Private answer".into();
            }
            if let Some(fields) = message.as_object_mut() {
                fields.remove("answers");
            }
        }
    }
    Ok(result)
}

/// The run whose history an active conversation shows, without the rest of its detail.
pub(crate) fn history_run(db: &Db<'_>, id: &str) -> Result<String> {
    let chat = conversation(db, id)?;
    let run = chat.run_id.as_deref().unwrap_or("");
    Ok(
        if chat.lifecycle == cairn_contracts::ConversationLifecycle::Active && db.has_run(run)? {
            run.to_owned()
        } else {
            String::new()
        },
    )
}

/// Steering joins the running turn, whose provider, model, reasoning and speed are fixed.
fn changes_running_agent(message: &Value, run: &Value) -> Result<bool> {
    let agent = &run["snapshot"]["agent"];
    let provider = Value::from(Provider::of_run(run)?.as_str());
    let differs = |key: &str, current: &Value| {
        !text(message, key).is_empty() && text(message, key) != current.as_str().unwrap_or("")
    };
    // A turn without a speed runs at Standard, so asking for Standard changes nothing.
    let speed = |value: &str| {
        if value.is_empty() {
            crate::models::STANDARD_SPEED.to_owned()
        } else {
            value.to_owned()
        }
    };
    let changes_speed = !text(message, "speed").is_empty()
        && speed(text(message, "speed")) != speed(text(agent, "speed"));
    Ok(differs("provider", &provider)
        || differs("model", &agent["model"])
        || differs("reasoning", &agent["reasoning"])
        || changes_speed)
}

/// The coding agent a message's turn runs with: the one it names, else the running turn's
/// for a steering message, else the one the queued messages ahead of it, the conversation's
/// latest turn or its agent select.
fn message_provider(db: &Db<'_>, chat: &Conversation, message: &Message) -> Result<String> {
    if !message.provider.is_empty() {
        return Ok(message.provider.clone());
    }
    let run = db.run(chat.run_id.as_deref().unwrap_or(""))?;
    if message.mode == MessageMode::Steer
        && let Some(run) = run.as_ref().filter(|run| run_is_active(run))
    {
        return Ok(Provider::of_run(run)?.as_str().to_owned());
    }

    let ahead = db
        .conversation_messages(&chat.id)?
        .into_iter()
        .take_while(|queued| queued.id != message.id)
        .filter(|queued| {
            queued.status == MessageStatus::Queued || queued.status == MessageStatus::Sending
        })
        .filter(|queued| !queued.provider.is_empty())
        .last();
    if let Some(queued) = ahead {
        return Ok(queued.provider.clone());
    }

    if let Some(run) = run {
        return Ok(Provider::of_run(&run)?.as_str().to_owned());
    }
    let agent = required(
        db.agent_snapshot(&chat.agent_id)?,
        "This agent is no longer available.",
    )?;
    Ok(Provider::of_agent(&agent)?.as_str().to_owned())
}

/// Only Codex offers a speed choice; a turn with another coding agent cannot ask for one.
fn validate_speed(db: &Db<'_>, chat: &Conversation, message: &Message) -> Result<()> {
    if message.speed.is_empty() {
        return Ok(());
    }
    crate::models::validate_speed(&message.speed)?;
    if message_provider(db, chat, message)? != Provider::Codex.as_str() {
        return Err(Error::bad(
            "Speed applies only to Codex. Remove it or choose Codex for this message.",
        ));
    }
    Ok(())
}

fn validate_steer(db: &Db<'_>, chat: &Conversation, message: &Message) -> Result<()> {
    if message.mode != MessageMode::Steer {
        return Ok(());
    }
    let Some(run) = db.run(chat.run_id.as_deref().unwrap_or(""))? else {
        return Ok(());
    };
    if run_is_active(&run) && changes_running_agent(&serde_json::to_value(message)?, &run)? {
        return Err(Error::conflict(
            "Queue this message to change provider, model, reasoning or speed on the next turn.",
        ));
    }
    Ok(())
}

/// A new chat is named after its first message, or its first attachment.
fn title(message: &Message) -> String {
    let source = if message.text.is_empty() {
        message.attachments.first().map_or("", |a| a.name.as_str())
    } else {
        message.text.as_str()
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

fn require_sendable(
    db: &Db<'_>,
    chat: &Conversation,
    messages: &[Message],
    id: &str,
) -> Result<()> {
    crate::conversation_lifecycle::require_environment_state_ready(chat.environment_state)?;
    let undelivered = messages
        .iter()
        .filter(|message| message.status != MessageStatus::Delivered)
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
        db.agent_snapshot(&chat.agent_id)?,
        "This agent is no longer available.",
    )?;
    crate::nodes::require_node(&agent)?;
    task_projects(&agent, &serde_json::to_value(chat)?, &db.projects()?)?;
    let cleaned = db
        .run(chat.run_id.as_deref().unwrap_or(""))?
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
    let mut chat = conversation(db, chat_id)?;
    authorize_conversation(db, &mut chat, origin)?;
    require_live(&chat)?;
    crate::attachments::message(db, chat_id, &mut values)?;
    let mut values: Message = serde_json::from_value(values)?;
    let messages = db.conversation_messages(chat_id)?;
    // Retried submissions are idempotent; a reused identifier with other content is not.
    if let Some(existing) = messages.iter().find(|m| m.id == values.id) {
        let same_answer = answer
            .as_ref()
            .is_none_or(|(question, _)| existing.question_id.as_deref() == Some(&question.id));
        if !same_content(existing, &values) || !same_answer {
            return Err(Error::conflict(
                "This message identifier has already been used.",
            ));
        }
        return Ok(serde_json::to_value(existing)?);
    }
    require_sendable(db, &chat, &messages, &values.id)?;
    validate_speed(db, &chat, &values)?;
    validate_steer(db, &chat, &values)?;
    if messages.is_empty() {
        let private_answer = answer
            .as_ref()
            .is_some_and(|(question, _)| question.is_private());
        chat.title = if private_answer {
            PRIVATE_ANSWER_TEXT.into()
        } else {
            title(&values)
        };
    }
    touch(&mut chat);
    chat.wakeup = None;
    db.save_conversation(&chat)?;
    if let Some((mut question, answers)) = answer {
        question.status = QuestionStatus::Answering;
        question.message_id = Some(values.id.clone());
        save_question(db, &question)?;
        values.question_id = Some(question.id);
        values.answers = Some(serde_json::from_value(answers)?);
    }
    values.chat_id = chat_id.to_owned();
    values.status = MessageStatus::Queued;
    values.created_at = now();
    Ok(serde_json::to_value(db.save_message(&values)?)?)
}

/// Deletes a queued ordinary message. Submitted questionnaire answers are immutable.
fn withdraw(db: &Db<'_>, chat_id: &str, message: &Message) -> Result<Value> {
    if message.question_id.is_some() {
        return Err(Error::conflict(
            "A submitted answer cannot be removed. Send a new message to correct it.",
        ));
    }
    db.0.execute(
        "DELETE FROM chat_messages WHERE chat_id=? AND id=?",
        rusqlite::params![chat_id, &message.id],
    )?;
    Ok(json!({ "deleted": true }))
}

/// Decode and validate the Rust-owned message contract at every transport boundary.
pub(crate) fn parse_message(input: Value) -> Result<Value> {
    let mut input: cairn_contracts::ChatMessageInput =
        serde_json::from_value(input).map_err(|e| Error::bad(e.to_string()))?;
    input.validate().map_err(Error::bad)?;
    Ok(serde_json::to_value(input)?)
}

/// Parses an edited message. A speed the editor did not send stays as stored.
pub(crate) fn parse_edit(input: Value) -> Result<Value> {
    let sends_speed = input.get("speed").is_some();
    let mut values = parse_message(input)?;
    if !sends_speed && let Some(values) = values.as_object_mut() {
        values.remove("speed");
    }
    Ok(values)
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
    let mut chat = conversation(db, chat_id)?;
    if values.is_some() {
        authorize_conversation(db, &mut chat, origin)?;
        db.save_conversation(&chat)?;
    }
    require_live(&chat)?;
    let mut current = required(find_message(db, chat_id, message_id)?, "Message not found")?;
    if current.status != MessageStatus::Queued {
        return Err(Error::conflict("This message is already being sent."));
    }
    let Some(mut values) = values else {
        return withdraw(db, chat_id, &current);
    };
    if current.question_id.is_some() {
        return Err(Error::conflict("A submitted answer cannot be edited."));
    }
    crate::attachments::message(db, chat_id, &mut values)?;
    // An edit without a speed, from an editor predating speeds, keeps the stored one while the
    // message still goes to Codex.
    let kept_speed = values.get("speed").is_none();
    let next: cairn_contracts::ChatMessage = serde_json::from_value(values)?;
    current.text = next.text;
    current.provider = next.provider;
    current.model = next.model;
    current.reasoning = next.reasoning;
    current.mode = next.mode;
    current.attachments = next.attachments;
    if !kept_speed {
        current.speed = next.speed;
    }
    if kept_speed && message_provider(db, &chat, &current)? != Provider::Codex.as_str() {
        current.speed.clear();
    }
    // Steering joins the running turn with the message as edited, including a kept speed.
    validate_steer(db, &chat, &current)?;
    validate_speed(db, &chat, &current)?;
    Ok(serde_json::to_value(db.save_message(&current)?)?)
}

fn acknowledge(db: &Db<'_>, run_id: &str, message_id: &str) -> Result<()> {
    let Some(mut chat) = db.conversation_for_run(run_id)? else {
        return Ok(());
    };
    let chat_id = chat.id.clone();
    let Some(mut message) = find_message(db, &chat_id, message_id)? else {
        return Ok(());
    };
    let settled =
        message.status == MessageStatus::Delivered || message.status == MessageStatus::Cancelled;
    if settled || chat.lifecycle != cairn_contracts::ConversationLifecycle::Active {
        return Ok(());
    }
    message.status = MessageStatus::Delivered;
    db.save_message(&message)?;
    let private = db.private_answer_message(message_id)?;
    if let Some(question_id) = message.question_id.as_deref()
        && let Some(mut question) = find_question(db, &chat_id, question_id)?
    {
        question.blocking = false;
        question.status = QuestionStatus::Answered;
        save_question(db, &question)?;
    }
    let text = if private {
        PRIVATE_ANSWER_TEXT
    } else {
        &message.text
    };
    let payload = serde_json::to_value(UserMessageEvent {
        message_id,
        question_id: message.question_id.as_deref(),
        text,
        attachments: &message.attachments,
        created_at: message.created_at,
    })?;
    db.event(run_id, &EventKind::ChatUser, text, Some(&payload))?;
    touch(&mut chat);
    db.save_conversation(&chat)?;
    Ok(())
}

/// Marks steering messages as sending so the running turn picks them up.
fn claim_steering(
    db: &Db<'_>,
    chat_id: &str,
    paused: bool,
    delivering: &Value,
) -> Result<Vec<Value>> {
    if conversation(db, chat_id)
        .is_ok_and(|chat| chat.lifecycle != cairn_contracts::ConversationLifecycle::Active)
    {
        return Ok(Vec::new());
    }
    let mut steering = Vec::new();
    for mut message in db.conversation_messages(chat_id)? {
        let pending =
            message.status == MessageStatus::Queued || message.status == MessageStatus::Sending;
        // Answers to questions still flow while the chat is paused.
        let deliverable = !paused || message.question_id.is_some();
        if deliverable
            && pending
            && message.mode == MessageMode::Steer
            && message.id.as_str() != delivering.as_str().unwrap_or("")
        {
            message.status = MessageStatus::Sending;
            db.save_message(&message)?;
            // Common questionnaire replies are delivered as structured text;
            // the persisted answer map is not needed by any native adapter.
            message.answers = None;
            steering.push(serde_json::to_value(message)?);
        }
    }
    Ok(steering)
}

/// Requeues messages left sending by an interrupted turn and returns the next one.
fn next_queued(db: &Db<'_>, chat_id: &str) -> Result<Option<Value>> {
    let paused = conversation(db, chat_id)?.paused;
    let mut messages = db.conversation_messages(chat_id)?;
    for message in &mut messages {
        if message.status == MessageStatus::Sending {
            message.status = MessageStatus::Queued;
            db.save_message(message)?;
        }
    }
    messages
        .into_iter()
        .find(|message| {
            message.status == MessageStatus::Queued && (!paused || message.question_id.is_some())
        })
        .map(serde_json::to_value)
        .transpose()
        .map_err(Into::into)
}

/// A chat whose last turn did not succeed waits for the user, unless a deletion
/// cancelled it or the user asked for a fresh session.
fn needs_attention(chat: &Conversation, run: Option<&Value>) -> bool {
    run.is_some_and(|run| run["status"] != RunStatus::Succeeded)
        && !chat.cancelled_by_deletion
        && !chat.session_restart_requested
        && chat.environment_state != Some(EnvironmentState::Expired)
}

/// Applies the provider, model, reasoning and speed chosen for this message. The speed is
/// independent of the model: it carries over model changes, not a change of coding agent.
pub(crate) fn select_agent(
    snapshot: &mut Value,
    run: Option<&Value>,
    message: &Value,
) -> Result<()> {
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
    let inherited_speed = text(previous, "speed").to_owned();
    let continues = provider == Provider::of_agent(previous)?.as_str();
    let execution_backend = run.map(|run| {
        run["snapshot"]["agent"]["access"]["backend"]
            .as_str()
            .unwrap_or("firecracker")
            .to_owned()
    });

    let switched = provider != Provider::of_run(snapshot)?.as_str();
    let agent = &mut snapshot["snapshot"]["agent"];
    if agent["access"]["backend"] == "auto"
        && let Some(backend) = execution_backend
    {
        agent["access"]["backend"] = backend.into();
    }
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
        set_speed(
            agent,
            if implicit_provider {
                &inherited_speed
            } else {
                ""
            },
        );
    }
    // Naming the coding agent of the latest turn keeps that turn's speed, as both clients
    // show it, rather than a default the agent's profile gained since.
    if !implicit_provider && continues {
        set_speed(agent, &inherited_speed);
    }
    if !text(message, "model").is_empty() {
        agent["model"] = message["model"].clone();
        agent["reasoning"] = text(message, "reasoning").into();
    }
    if !text(message, "reasoning").is_empty() {
        agent["reasoning"] = message["reasoning"].clone();
    }
    if !text(message, "speed").is_empty() {
        set_speed(agent, text(message, "speed"));
    }
    // Another coding agent's run has no speed to keep, even one inherited from its profile.
    if provider != Provider::Codex.as_str() {
        set_speed(agent, "");
    }
    agent["provider"] = provider.into();
    Ok(())
}

/// Snapshots without a speed, including every earlier one, run at Standard.
fn set_speed(agent: &mut Value, speed: &str) {
    if !speed.is_empty() {
        agent["speed"] = speed.into();
    } else if let Some(agent) = agent.as_object_mut() {
        agent.remove("speed");
    }
}

/// Reuses the chat's run for the next turn, handing the transcript to a fresh
/// native session when the provider changed or a new session was requested.
fn continue_run(
    db: &Db<'_>,
    chat: &Conversation,
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
    let mut checkpoint = match RunCheckpoint::load_in(db, run_id)? {
        Some(value) => value,
        None if chat.cancelled_by_deletion => RunCheckpoint::default(),
        None => return Err(Error::conflict("Run checkpoint not found")),
    };
    let provider = Provider::of_run(snapshot)?;
    crate::conversation_lifecycle::require_environment_state_ready(chat.environment_state)?;
    let expired = chat.environment_state == Some(EnvironmentState::Expired);
    let restart_requested =
        expired || chat.session_restart_requested || checkpoint.fresh_session == Some(true);
    if provider != Provider::of_run(run)? || restart_requested {
        if !expired && run["sessionId"].is_string() {
            checkpoint.workspace_initialized = Some(true);
        }
        // The next provider cannot resolve requests from the session being replaced.
        questions::release(db, run_id, None)?;
        checkpoint.fresh_session = Some(false);
        execution.context = Some(handoff_context(db, run_id)?);
        checkpoint.launched = Some(false);
        checkpoint.controller_recoveries = None;
        db.change_run(
            run_id,
            crate::runs::RunChanges {
                session_id: Some(None),
                resume_available: Some(false),
                // The fresh session starts with an empty context.
                boundary: Some(json!({ "contextUsage": null })),
                ..Default::default()
            },
        )?;
        let status = if expired {
            "Starting a fresh environment using the preserved conversation history".to_owned()
        } else {
            format!(
                "Continuing with {} · conversation context and workspace preserved",
                provider.label()
            )
        };
        db.event(run_id, &EventKind::Status, &status, None)?;
    } else if provider == Provider::Codex && resumes_mission_thread(run) {
        // Codex keeps the developer instructions a thread started with, so the
        // mission's thread would stay unattended. Every turn repeats the rule
        // because compaction can drop earlier user messages.
        execution.text = format!(
            "{CONTINUED_MISSION} {}\n\n{}",
            crate::run_output::INTERACTIVE,
            execution.text
        );
    }
    checkpoint.completed = Some(false);
    checkpoint.remaining_ms = Some(crate::run_limits::budget_ms(agent));
    checkpoint.last_message = None;
    checkpoint.settled = None;
    checkpoint.store_in(db, run_id)?;
    let compaction = execution.operation.as_deref() == Some(COMPACT);
    let mut patch = crate::runs::RunChanges {
        snapshot: Some(serde_json::from_value(snapshot["snapshot"].clone())?),
        status: Some(RunStatus::Queued),
        error: Some(None),
        finished_at: Some(None),
        cancel_requested_at: Some(None),
        recovery_pending: Some(true),
        boundary: Some(json!({
            "chatExecution": serde_json::to_value(execution)?,
            "environmentState": null,
        })),
        ..Default::default()
    };
    // A compaction answers no message: the last reply and its outcome stay current.
    if !compaction {
        patch.summary = Some(String::new());
        patch.boundary.as_mut().unwrap()["outcome"] = Value::Null;
    }
    db.change_run(run_id, patch)?;
    Ok(())
}

/// A mission continued in chat that still resumes the thread its mission started.
fn resumes_mission_thread(run: &Value) -> bool {
    run["sessionId"].is_string() && run["sessionId"] == run["sourceSessionId"]
}

/// Starts the agent turn for `message` unless the chat or message changed meanwhile.
fn launch_turn(
    db: &Db<'_>,
    chat_id: &str,
    run: Option<Value>,
    mut message: Value,
    mut snapshot: Value,
) -> Result<()> {
    let mut chat = conversation(db, chat_id)?;
    // Preparation can overlap a merge. Retry with the chat's current profile
    // rather than publishing a snapshot of the removed source.
    if chat.agent_id != snapshot["snapshot"]["agent"]["id"] {
        return Ok(());
    }
    let waking = message["wakeup"].is_object();
    let unchanged = if waking {
        let current = db.run(chat.run_id.as_deref().unwrap_or(""))?;
        continuation::ready(db, &chat, current.as_ref(), now())?
            && serde_json::to_value(&chat.wakeup)? == message["wakeup"]
            && current.as_ref().map(|run| &run["id"]) == run.as_ref().map(|run| &run["id"])
            && (!message["wakeup"]["condition"].is_object()
                || current.as_ref().is_some_and(|run| {
                    wait_for::observation(db, &chat.id, run, now())
                        .is_ok_and(|observation| observation.as_ref() == message.get("observation"))
                }))
    } else {
        find_message(db, chat_id, text(&message, "id"))?.is_some_and(|current| {
            current.status == MessageStatus::Queued
                && serde_json::from_value::<Message>(message.clone())
                    .is_ok_and(|message| same_content(&current, &message))
        })
    };
    let held_by_pause = chat.paused && !message["questionId"].is_string();
    if chat.lifecycle != cairn_contracts::ConversationLifecycle::Active
        || held_by_pause
        || !unchanged
    {
        return Ok(());
    }
    crate::conversation_lifecycle::require_environment_state_ready(chat.environment_state)?;
    let execution = ChatExecution {
        message_id: text(&message, "id").to_owned(),
        text: with_invoked_skills(text(&message, "text"), &snapshot["snapshot"]["skills"]),
        attachments: serde_json::from_value(message["attachments"].clone())?,
        recovery: false,
        context: None,
        operation: None,
        previous: None,
    };
    if let Some(run) = run {
        continue_run(db, &chat, &run, &snapshot, execution)?;
    } else {
        crate::native::policy::apply(&mut snapshot, &serde_json::to_value(&chat)?);
        snapshot["chatExecution"] = serde_json::to_value(execution)?;
        crate::runs::admit_prepared(db, &snapshot, None)?;
        chat.run_id = Some(text(&snapshot, "id").to_owned());
    }
    chat.cancelled_by_deletion = false;
    chat.session_restart_requested = false;
    chat.environment_state = None;
    chat.wakeup = None;
    db.save_conversation(&chat)?;
    if waking {
        let mut observed_wakeup = message["wakeup"].clone();
        if message["observation"].is_object() {
            observed_wakeup["observation"] = message["observation"].clone();
        }
        db.event(
            chat.run_id.as_deref().unwrap_or(""),
            &EventKind::ChatWakeup,
            "Resuming after scheduled wait",
            Some(&observed_wakeup),
        )?;
        return Ok(());
    }
    message["status"] = MessageStatus::Sending.into();
    db.put_message(&message)?;
    Ok(())
}

/// Transfers a completed mission's conversation to a chat without replaying it.
fn from_run(db: &Db<'_>, run_id: &str, origin: &crate::native::policy::Origin) -> Result<Value> {
    crate::conversation_lifecycle::require_active_run(db, run_id)?;
    if let Some(chat) = db.conversation_for_run(run_id)? {
        return Ok(serde_json::to_value(chat)?);
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
        RunCheckpoint::load_in(db, run_id)?,
        "Run checkpoint not found",
    )?;
    if checkpoint.prepared().is_none() || text(&run, "sessionId").is_empty() {
        return Err(Error::conflict(
            "This mission has no saved conversation available to continue.",
        ));
    }

    crate::native::policy::authorize_run_in(db, run_id, origin)?;
    let run = required(db.run(run_id)?, "Run not found")?;
    let agent = required(
        db.agent_snapshot(text(&run["snapshot"]["agent"], "id"))?,
        "Agent not found",
    )?;
    if !crate::service::covers(&agent, &run["snapshot"]["agent"]) {
        return Err(Error::conflict(
            "Agent access was reduced. Start a new chat with the updated permissions.",
        ));
    }

    let created_at = now();
    let mut chat = json!({
        "id": id(),
        "title": run["snapshot"]["task"]["name"],
        "agentId": agent["id"],
        "projectId": run["projectId"],
        "runId": run_id,
        "paused": false,
        "createdAt": created_at,
        "updatedAt": created_at,
        "lastActivityAt": created_at,
    });
    crate::native::policy::apply(&mut chat, &run);
    task_projects(&agent, &chat, &db.projects()?)?;

    let chat = db.save_conversation(&serde_json::from_value(chat)?)?;
    db.change_run(
        run_id,
        crate::runs::RunChanges {
            task_id: Some(chat.id.clone()),
            trigger: Some(("chat").to_owned()),
            boundary: Some(json!({
                "sourceTaskId": run["taskId"],
                "sourceTaskPrompt": run["snapshot"]["task"]["prompt"],
                "sourceSessionId": run["sessionId"],
            })),
            ..Default::default()
        },
    )?;
    // Work already delegated by this run still returns to its new conversation.
    let dependents = db.json_rows(
        "SELECT data FROM runs WHERE json_extract(requested_by,'$.runId')=?",
        [run_id],
    )?;
    for dependent in dependents {
        db.change_run(
            text(&dependent, "id"),
            crate::runs::RunChanges {
                requested_by: Some(Some(cairn_contracts::RunRequester {
                    run_id: run_id.to_owned(),
                    chat_id: Some(chat.id.clone()),
                    task_id: None,
                })),
                ..Default::default()
            },
        )?;
    }
    db.restore_chat_summaries(Some(run_id))?;
    db.audit(
        "run.converted_to_chat",
        &json!({ "runId": run_id, "chatId": chat.id }),
    )?;
    Ok(serde_json::to_value(chat)?)
}

pub(crate) fn create_as(
    db: &Db<'_>,
    input: Value,
    origin: &crate::native::policy::Origin,
) -> Result<Value> {
    let input: cairn_contracts::CreateChatInput =
        serde_json::from_value(input).map_err(|e| Error::bad(e.to_string()))?;
    let agent = required(
        db.agent_snapshot(&input.agent_id.to_string())?,
        "Agent not found",
    )?;
    crate::native::policy::authorize_origin(db, &agent, origin)?;
    let created_at = now();
    let mut chat = json!({
        "id": id(),
        "title": "New chat",
        "agentId": input.agent_id,
        "projectId": input.project_id,
        "runId": null,
        "paused": false,
        "createdAt": created_at,
        "updatedAt": created_at,
        "lastActivityAt": created_at,
    });
    crate::native::policy::authorize_chat(db, &mut chat, origin)?;
    task_projects(&agent, &chat, &db.projects()?)?;
    let chat: Conversation = serde_json::from_value(chat)?;
    Ok(serde_json::to_value(db.save_conversation(&chat)?)?)
}

pub(crate) fn send_as_in(
    db: &Db<'_>,
    chat: &str,
    input: Value,
    origin: &crate::native::policy::Origin,
) -> Result<Value> {
    send(db, chat, parse_message(input)?, None, origin)
}

pub(crate) fn pause_in(db: &Db<'_>, id: &str, paused: bool) -> Result<Value> {
    let mut chat = conversation(db, id)?;
    require_live(&chat)?;
    if chat.last_activity_at.is_none() {
        chat.last_activity_at = Some(chat.updated_at);
    }
    chat.paused = paused;
    chat.updated_at = now();
    Ok(serde_json::to_value(db.save_conversation(&chat)?)?)
}

/// Preparation happens outside the writer. A merge may have transferred the
/// conversation since it was read; that stale attempt must simply retry next tick.
fn preparation_failed(db: &Db<'_>, prepared: &Conversation, error: Error) -> Result<()> {
    let id = &prepared.id;
    if conversation(db, id)?.agent_id != prepared.agent_id {
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
            crate::native::policy::refresh_for(self, &run["snapshot"]["agent"]).await?;
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
        let values = parse_message(input)?;
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
                parse_edit(input)
            })
            .transpose()?;
        let result = self
            .store
            .transaction(move |db| edit_as(db, &id, &message, values, &origin))
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

        // A succeeded run's worker may still be saving its checkpoint: the compaction resets
        // it, so it waits for that worker to release the run, like resuming does.
        let active = self.worker.active.lock().await;
        let run_id = self
            .store
            .read({
                let id = id.clone();
                move |db| Ok(conversation(db, &id)?.run_id.unwrap_or_default())
            })
            .await?;
        if active.contains_key(&run_id) {
            return Err(Error::conflict(
                "Wait for the current turn to finish before compacting.",
            ));
        }
        let result = self
            .store
            .transaction(move |db| {
                authorize(db)?;
                compact(db, &id, &origin)
            })
            .await?;
        drop(active);
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
        self.expire_attachment_uploads().await?;
        for chat in self.store.conversations().await? {
            if chat.lifecycle != cairn_contracts::ConversationLifecycle::Active
                || chat.environment_state == Some(EnvironmentState::Cleaning)
            {
                continue;
            }
            if self
                .store
                .read({
                    let id = chat.id.clone();
                    move |db| db.engine_switch(&id)
                })
                .await?
                .is_some_and(|value| value.pending)
            {
                continue;
            }
            let run = match chat.run_id.as_deref() {
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
            let id = chat.id.clone();
            if needs_attention(&chat, run.as_ref()) {
                self.chat_cancel_wait(&id).await?;
                if chat.paused {
                    continue;
                }
                let chat_id = id.clone();
                self.store
                    .transaction(move |db| next_queued(db, &chat_id).map(|_| ()))
                    .await?;
                self.chat_pause(&id, true).await?;
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
                    if chat.paused {
                        continue;
                    }
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
    async fn publish_steering(&self, chat: &Conversation, run: &Value) -> Result<()> {
        let chat_id = chat.id.clone();
        let paused = chat.paused;
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
        // Publish a bounded prefix. Unacknowledged messages remain pending and
        // join a later batch once the adapter has received this one.
        let mut encoded_bytes = 2;
        for (index, message) in steering.iter().enumerate() {
            encoded_bytes += serde_json::to_vec(message)?.len() + usize::from(index != 0);
            if encoded_bytes > crate::chat_inbox::ADAPTER_LIMIT {
                steering.truncate(index);
                break;
            }
        }
        let directory = self
            .config
            .data_dir
            .join("runs")
            .join(chat.run_id.as_deref().unwrap_or(""))
            .join("chat-input");
        crate::skills::private_dir(&directory).await?;
        crate::skills::atomic_write(
            &directory.join("messages.json"),
            &serde_json::to_vec(&steering)?,
        )
        .await
    }

    async fn chat_prepare(
        &self,
        chat: Conversation,
        run: Option<Value>,
        message: Value,
    ) -> Result<()> {
        let prompt = if message["questionId"].is_string() {
            // The structured response is carried by chatExecution, not by a
            // mission prompt constrained to the ordinary composer budget.
            "Continue from the questionnaire response."
        } else if text(&message, "text").is_empty() {
            "Review the attached files."
        } else {
            text(&message, "text")
        };
        let mut settings: cairn_contracts::MissionSettings = serde_json::from_value(json!({
            "name": chat.title,
            "prompt": prompt,
            "agentId": chat.agent_id,
            "projectId": chat.project_id,
            "worktree": true,
        }))?;
        settings.validate().map_err(Error::bad)?;
        let mut task = serde_json::to_value(settings)?;
        task["id"] = chat.id.clone().into();
        task["createdAt"] = chat.created_at.into();
        task["nextRun"] = Value::Null;
        let mut snapshot = self.snapshot(task, "chat").await?;
        snapshot["snapshot"]["delegation"] = crate::delegation::role(
            &serde_json::to_value(&chat)?,
            &snapshot["snapshot"]["agent"],
        );
        select_agent(&mut snapshot, run.as_ref(), &message)?;
        Provider::of_run(&snapshot)?
            .adapter()
            .validate_agent(&snapshot["snapshot"]["agent"])?;
        let chat_id = chat.id.clone();
        self.store
            .transaction(move |db| launch_turn(db, &chat_id, run, message, snapshot))
            .await
    }
}

#[cfg(test)]
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_chat(value: &Value) -> Conversation {
        let mut value = value.clone();
        value
            .as_object_mut()
            .unwrap()
            .entry("id")
            .or_insert_with(|| "chat".into());
        serde_json::from_value(value).unwrap()
    }

    fn validate_steer(db: &Db<'_>, chat: &Value, message: &Value) -> Result<()> {
        super::validate_steer(
            db,
            &fixture_chat(chat),
            &serde_json::from_value(message.clone())?,
        )
    }

    fn validate_speed(db: &Db<'_>, chat: &Value, message: &Value) -> Result<()> {
        super::validate_speed(
            db,
            &fixture_chat(chat),
            &serde_json::from_value(message.clone())?,
        )
    }

    fn preparation_failed(db: &Db<'_>, chat: &Value, error: Error) -> Result<()> {
        super::preparation_failed(db, &fixture_chat(chat), error)
    }

    fn withdraw(db: &Db<'_>, chat: &str, message: &Value) -> Result<Value> {
        super::withdraw(db, chat, &serde_json::from_value(message.clone())?)
    }

    fn continue_run(
        db: &Db<'_>,
        chat: &Value,
        run: &Value,
        snapshot: &Value,
        execution: ChatExecution,
    ) -> Result<()> {
        super::continue_run(db, &fixture_chat(chat), run, snapshot, execution)
    }

    #[tokio::test]
    async fn purging_question_markers_during_a_public_read_never_exposes_its_answer() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.save_conversation_boundary(&json!({ "id": "chat" }))?;
                let question: Question = serde_json::from_value(json!({
                    "id": "private-question",
                    "chatId": "chat",
                    "runId": "run",
                    "blocking": false,
                    "status": "answering",
                    "fields": [{ "id": "field", "title": "Private fixture", "secret": true }],
                }))?;
                save_question(db, &question)?;
                db.put_message(&json!({
                    "id": "reply",
                    "chatId": "chat",
                    "text": "private-fixture-answer",
                    "questionId": question.id,
                    "answers": { "field": { "selected": [], "comment": "private-fixture-answer" } },
                }))?;
                Ok(())
            })
            .await
            .unwrap();
        let path = root.path().join("manager.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        // The view lets a second WAL connection purge the marker at the exact
        // point the reader captures the message, without production test hooks.
        connection
            .execute_batch(
                "ALTER TABLE chat_messages RENAME TO retained_messages;
CREATE VIEW chat_messages AS SELECT rowid AS rowid,id,chat_id,revision,purge_marker(text) AS text,
provider,model,reasoning,speed,mode,status,created_at,question_id FROM retained_messages;",
            )
            .unwrap();
        connection
            .create_scalar_function(
                "purge_marker",
                1,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                move |context| {
                    let text: String = context.get(0)?;
                    rusqlite::Connection::open(&path)?.execute(
                        "DELETE FROM chat_questions WHERE chat_id='chat' AND id='private-question'",
                        [],
                    )?;
                    Ok(text)
                },
            )
            .unwrap();
        let result = public_detail(&Db(&connection), "chat").unwrap();
        assert_eq!(result["messages"][0]["text"], "Private answer");
        assert!(!result.to_string().contains("private-fixture-answer"));
        assert!(connection.is_autocommit());
        assert!(
            Db(&connection)
                .question("chat", "private-question")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn automatic_profiles_keep_the_conversations_environment_on_follow_up_turns() {
        for backend in ["firecracker", "native"] {
            for message in [json!({}), json!({ "provider": "claude" })] {
                let previous = json!({ "snapshot": { "agent": { "provider": "codex", "access": { "backend": backend } } } });
                let mut next = json!({ "snapshot": { "agent": { "provider": "codex", "access": { "backend": "auto" } } } });
                select_agent(&mut next, Some(&previous), &message).unwrap();
                assert_eq!(next["snapshot"]["agent"]["access"]["backend"], backend);
            }
        }
    }

    #[tokio::test]
    async fn a_merge_during_preparation_keeps_queued_work_unpaused() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store.transaction(|db| {
            let stale = json!({ "id": "chat", "agentId": "source", "paused": false });
            let transferred = json!({ "id": "chat", "agentId": "target", "paused": false });
            let transferred = db.save_conversation_boundary(&transferred)?;
            let queued = json!({ "id": "message", "chatId": "chat", "createdAt": 1, "status": "queued" });
            let queued = db.put_message(&queued)?;
            // The stale agent lookup failed before launch_turn's identity guard.
            preparation_failed(db, &stale, Error::not_found("Record not found"))?;
            assert_eq!(db.conversation_boundary("chat")?.unwrap(), transferred);
            assert!(db.kv("chat-error:chat")?.is_none());
            assert_eq!(db.messages("chat")?, vec![queued]);
            // A real error on the current profile still pauses and reports normally.
            preparation_failed(db, &transferred, Error::bad("Current preparation failed"))?;
            assert_eq!(db.conversation_boundary("chat")?.unwrap()["paused"], true);
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
                    ("codex", json!({ "provider": "codex" })),
                ] {
                    let run = json!({
                        "id": id,
                        "taskId": id,
                        "createdAt": 0,
                        "status": "succeeded",
                        "snapshot": { "agent": agent },
                    });
                    db.add_run_fixture(&run, None)?;
                    db.save_conversation_boundary(
                        &json!({ "id": id, "runId": id, "agentId": "agent" }),
                    )?;
                }
                db.save_conversation_boundary(
                    &json!({ "id": "new", "runId": null, "agentId": "agent" }),
                )?;

                let chats = list(db)?;
                let provider = |id: &str| {
                    chats.iter().find(|chat| chat["id"] == id).unwrap()["provider"].clone()
                };
                assert_eq!(provider("switched"), "claude");
                assert_eq!(provider("codex"), "codex");
                assert!(provider("new").is_null());

                let runs = db.runs(None, None, 10, 0, false)?;
                let provider =
                    |id: &str| runs.iter().find(|run| run["id"] == id).unwrap()["provider"].clone();
                assert_eq!(provider("switched"), "claude");
                assert_eq!(provider("codex"), "codex");
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
                db.add_run_fixture(&run, None)?;
                db.save_checkpoint_boundary("run", &json!({ "completed": true, "launched": true }))?;
                db.save_conversation_boundary(&json!({
                        "id": "chat",
                        "runId": "run",
                        "agentId": "agent",
                        "nativeProvenanceUnknown": false,
                    }),
                )?;

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
                assert_eq!(
                    compacting["chatExecution"]["previous"],
                    json!({ "summary": "Last reply", "finishedAt": 500, "outcome": outcome })
                );
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
                    db.add_run_fixture(
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
                    db.save_conversation_boundary(&json!({
                            "id": id,
                            "runId": id,
                            "agentId": "agent",
                            "nativeProvenanceUnknown": false,
                        }),
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

                db.save_conversation_boundary(&json!({ "id": "empty", "runId": null, "agentId": "agent" }),
                )?;
                let state = view(db, chat(db, "empty")?)?["compaction"].clone();
                assert_eq!(state["supported"], false);
                assert!(text(&state, "reason").contains("first message"));

                // A run that names no coding agent still shows its conversation.
                db.add_run_fixture(
                    &json!({ "id": "unnamed", "taskId": "unnamed", "createdAt": 0, "status": "succeeded" }),
                    None,
                )?;
                db.save_conversation_boundary(&json!({
                        "id": "unnamed",
                        "runId": "unnamed",
                        "agentId": "agent",
                        "nativeProvenanceUnknown": false,
                    }),
                )?;
                let state = view(db, chat(db, "unnamed")?)?["compaction"].clone();
                assert_eq!(state["supported"], false);
                assert_eq!(state["available"], false);
                assert!(text(&state, "reason").contains("does not support"));
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn continuation_consumes_chat_and_checkpoint_fresh_session_requests_once() {
        use crate::store::Store;

        for checkpoint_restart in [false, true] {
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
                        let run = db.add_run_fixture(&run, None)?;
                        db.event("run", &EventKind::ChatUser, "Context to preserve", None)?;
                        let chat = json!({
                            "id": "chat",
                            "runId": "run",
                            "sessionRestartRequested": !checkpoint_restart,
                            "agentId": crate::config::MAIN_AGENT_ID,
                        });
                        db.save_conversation_boundary(&chat)?;
                        db.save_question(&serde_json::from_value(json!({
                            "id": "old-session", "chatId":"chat", "runId":"run",
                            "source":"codex", "status":"pending", "blocking":true,
                            "fields":[{"id":"field","title":"Question from previous session"}],
                        }))?)?;
                        let mut prepared =
                            json!({ "isolated": isolated, "workspace": "preserved" });
                        if native {
                            prepared["backend"] = "native".into();
                        }
                        db.save_checkpoint_boundary(
                            "run",
                            &json!({
                                "freshSession": checkpoint_restart,
                                "completed": true,
                                "launched": true,
                                "prepared": prepared,
                                "controllerRecoveries": 2,
                            }),
                        )?;
                        let message = json!({
                            "id": "next",
                            "chatId": "chat",
                            "text": "Continue",
                            "status": "queued",
                            "createdAt": 1,
                        });
                        let message = db.put_message(&message)?;
                        launch_turn(db, "chat", Some(run.clone()), message, run)?;
                        let queued = db.run("run")?.unwrap();
                        assert_eq!(queued["status"], "queued");
                        assert!(queued["sessionId"].is_null());
                        assert_eq!(queued["resumeAvailable"], false);
                        assert!(
                            text(&queued["chatExecution"], "context")
                                .contains("Context to preserve")
                        );
                        let checkpoint = db.checkpoint_boundary("run")?.unwrap();
                        assert_eq!(checkpoint["prepared"], prepared);
                        assert_eq!(checkpoint["launched"], false);
                        assert_eq!(checkpoint["workspaceInitialized"], true);
                        assert_eq!(checkpoint["freshSession"], false);
                        assert!(checkpoint["controllerRecoveries"].is_null());
                        let chat = db.conversation_boundary("chat")?.unwrap();
                        assert_eq!(chat["sessionRestartRequested"], false);
                        let question =
                            serde_json::to_value(db.question("chat", "old-session")?.unwrap())?;
                        assert_eq!(question["status"], "cancelled");
                        assert_eq!(question["blocking"], false);

                        // A later ordinary continuation resumes the newly established session.
                        let _resumed = db.change_run(
                            "run",
                            crate::runs::RunChanges {
                                session_id: Some(Some("new-native".into())),
                                ..Default::default()
                            },
                        )?;
                        let resumed = db.run("run")?.unwrap();
                        continue_run(
                            db,
                            &chat,
                            &resumed,
                            &resumed,
                            ChatExecution {
                                message_id: "later".into(),
                                text: "Keep going".into(),
                                attachments: Vec::new(),
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

    #[tokio::test]
    async fn codex_threads_of_missions_continued_in_chat_receive_the_interactive_rule() {
        // Provider, thread the mission started, and whether each turn carries the rule.
        for (provider, source, expected) in [
            ("codex", Some("thread"), true),
            ("codex", Some("earlier-thread"), false),
            ("codex", None, false),
            ("claude", Some("thread"), false),
        ] {
            let root = tempfile::tempdir().unwrap();
            let store = crate::store::Store::open(root.path()).unwrap();
            store
                .transaction(move |db| {
                    let mut run = json!({
                        "id": "run",
                        "taskId": "chat",
                        "createdAt": 0,
                        "status": "succeeded",
                        "trigger": "chat",
                        "sessionId": "thread",
                        "snapshot": { "agent": { "provider": provider } },
                    });
                    if let Some(source) = source {
                        run["sourceTaskPrompt"] = "Check the deployment.".into();
                        run["sourceSessionId"] = source.into();
                    }
                    let run = db.add_run_fixture(&run, None)?;
                    db.save_checkpoint_boundary("run", &json!({ "completed": true }))?;
                    let chat = json!({ "id": "chat", "runId": "run" });
                    db.save_conversation_boundary(&chat)?;
                    // Compaction can drop earlier messages, so every turn repeats it.
                    for message in ["Now fix it", "Thanks"] {
                        let execution = ChatExecution {
                            message_id: message.into(),
                            text: message.into(),
                            attachments: Vec::new(),
                            recovery: false,
                            context: None,
                            operation: None,
                            previous: None,
                        };
                        continue_run(db, &chat, &run, &run, execution)?;
                        let continued = db.run("run")?.unwrap();
                        let turn = text(&continued["chatExecution"], "text");
                        assert_eq!(
                            turn.contains(crate::run_output::INTERACTIVE),
                            expected,
                            "{provider} {source:?}"
                        );
                        assert_eq!(turn.starts_with(CONTINUED_MISSION), expected);
                        assert!(turn.ends_with(message));
                        assert_eq!(continued["sessionId"], "thread");
                    }
                    Ok(())
                })
                .await
                .unwrap();
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
        let root = tempfile::tempdir().unwrap();
        let _store = crate::store::Store::open(root.path()).unwrap();
        let connection = rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
        let run = json!({
            "status": "running",
            "snapshot": {
                "agent": { "provider": "codex", "model": "gpt-6-sol", "reasoning": "high" },
            },
        });
        let db = Db(&connection);
        let mut run = run;
        run["id"] = "run".into();
        run["taskId"] = "chat".into();
        db.add_run_fixture(&run, None).unwrap();
        let chat = json!({ "runId": "run" });
        let mut message = parse_message(json!({
            "id": id(),
            "text": "Continue",
            "provider": "claude",
            "model": "opus[1m]",
            "mode": "steer",
        }))
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

        // The running turn keeps its speed; the run has none, so only an explicit one differs.
        message["speed"] = "priority".into();
        assert_eq!(
            validate_steer(&db, &chat, &message).unwrap_err().status,
            409
        );
        message["speed"] = "".into();
        validate_steer(&db, &chat, &message).unwrap();
        // Asking for Standard does not change a turn without a speed: it runs at Standard.
        message["speed"] = "standard".into();
        validate_steer(&db, &chat, &message).unwrap();
        message["speed"] = "".into();

        let unknown = json!({
            "id": id(),
            "text": "Continue",
            "provider": "unknown"
        });
        assert!(parse_message(unknown).is_err());
    }

    #[test]
    fn speed_is_independent_of_the_model_but_belongs_to_codex() {
        let codex = |speed: Option<&str>| {
            let mut agent =
                json!({ "provider": "codex", "model": "gpt-6-sol", "reasoning": "high" });
            if let Some(speed) = speed {
                agent["speed"] = speed.into();
            }
            json!({ "snapshot": { "agent": agent } })
        };
        let select = |previous: &Value, message: Value| {
            let mut snapshot = codex(None);
            select_agent(&mut snapshot, Some(previous), &message).unwrap();
            snapshot["snapshot"]["agent"].clone()
        };

        // A model change resets the reasoning level, not the speed.
        let previous = codex(Some("priority"));
        let agent = select(&previous, json!({ "model": "gpt-6-astra" }));
        assert_eq!(agent["model"], "gpt-6-astra");
        assert_eq!(agent["reasoning"], "");
        assert_eq!(agent["speed"], "priority");

        // Reasoning changes keep it too, and an explicit speed replaces it.
        let agent = select(&previous, json!({ "reasoning": "low" }));
        assert_eq!(agent["speed"], "priority");
        let agent = select(&previous, json!({ "speed": "standard" }));
        assert_eq!(agent["speed"], "standard");
        assert_eq!(agent["reasoning"], "high");

        // Earlier runs and messages without a speed keep none.
        let agent = select(&codex(None), json!({ "text": "Continue" }));
        assert!(agent.get("speed").is_none());

        // Naming the conversation's coding agent keeps its latest turn's speed, even after the
        // agent's profile gained another default.
        let mut faster_profile = codex(Some("priority"));
        let continued = json!({ "provider": "codex", "model": "gpt-6-sol", "reasoning": "high" });
        select_agent(&mut faster_profile, Some(&codex(None)), &continued).unwrap();
        assert!(faster_profile["snapshot"]["agent"].get("speed").is_none());
        let mut standard_profile = codex(None);
        select_agent(&mut standard_profile, Some(&previous), &continued).unwrap();
        assert_eq!(standard_profile["snapshot"]["agent"]["speed"], "priority");

        // Another coding agent has no speed, and returning to Codex starts from its default.
        let agent = select(&previous, json!({ "provider": "claude" }));
        assert!(agent.get("speed").is_none());
        let claude = json!({ "snapshot": { "agent": { "provider": "claude", "model": "opus", "speed": "priority" } } });
        let agent = select(&claude, json!({ "text": "Continue" }));
        assert!(agent.get("speed").is_none());
        let agent = select(&claude, json!({ "provider": "codex" }));
        assert!(agent.get("speed").is_none());
        let agent = select(&claude, json!({ "provider": "codex", "speed": "priority" }));
        assert_eq!(agent["speed"], "priority");
    }

    #[tokio::test]
    async fn a_speed_is_refused_for_turns_another_coding_agent_runs() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let codex = cairn_contracts::AgentId(uuid::Uuid::new_v4());
                let claude = cairn_contracts::AgentId(uuid::Uuid::new_v4());
                for (agent_id, provider) in [(codex, "codex"), (claude, "claude")] {
                    let settings = serde_json::from_value(json!({
                        "name": provider,
                        "provider": provider,
                        "access": cairn_contracts::AccessPolicy::default(),
                        "exclusiveNodes": [],
                        "plugins": [],
                    }))?;
                    db.save_agent(&crate::agents::new_agent(settings, Some(agent_id)))?;
                }
                let run = json!({
                    "id": "claude-run",
                    "taskId": "switched",
                    "createdAt": 0,
                    "status": "succeeded",
                    "snapshot": { "agent": { "provider": "claude" } },
                });
                db.add_run_fixture(&run, None)?;
                let new_codex = json!({ "id": "new-codex", "runId": null, "agentId": codex });
                let new_claude = json!({ "id": "new-claude", "runId": null, "agentId": claude });
                let switched = json!({ "id": "switched", "runId": "claude-run", "agentId": codex });
                db.save_conversation_boundary(&switched)?;
                let fast = |provider: &str| json!({ "id": id(), "provider": provider, "speed": "priority" });
                let refused = |chat: &Value, message: &Value| {
                    validate_speed(db, chat, message).unwrap_err().status == 400
                };

                // Without a provider, the message runs with the conversation's coding agent.
                validate_speed(db, &new_codex, &fast(""))?;
                assert!(refused(&new_claude, &fast("")));
                assert!(refused(&switched, &fast("")));
                validate_speed(db, &switched, &fast("codex"))?;
                assert!(refused(&new_codex, &fast("opencode")));
                validate_speed(db, &new_claude, &json!({ "id": id(), "speed": "" }))?;
                // Codex's name for Standard is not a speed choice.
                assert!(refused(&new_codex, &json!({ "id": id(), "speed": "default" })));

                // A queued message switching coding agent decides for the ones after it.
                let mut ahead = json!({
                    "id": "ahead",
                    "chatId": "switched",
                    "createdAt": 1,
                    "status": "queued",
                    "provider": "codex",
                });
                db.put_message(&ahead)?;
                validate_speed(db, &switched, &fast(""))?;
                ahead["status"] = "delivered".into();
                db.put_message(&ahead)?;
                assert!(refused(&switched, &fast("")));

                // A steering message joins the running turn, whatever the queue switches to next.
                let running = json!({
                    "id": "codex-run",
                    "taskId": "steering",
                    "createdAt": 0,
                    "status": "running",
                    "snapshot": { "agent": { "provider": "codex", "speed": "priority" } },
                });
                db.add_run_fixture(&running, None)?;
                let steering = json!({ "id": "steering", "runId": "codex-run", "agentId": codex });
                db.save_conversation_boundary(&steering)?;
                let switching = json!({
                    "id": "switching",
                    "chatId": "steering",
                    "createdAt": 1,
                    "status": "queued",
                    "provider": "claude",
                });
                db.put_message(&switching)?;
                let mut steer = fast("");
                steer["mode"] = "steer".into();
                validate_speed(db, &steering, &steer)?;
                steer["mode"] = "queue".into();
                assert!(refused(&steering, &steer));
                Ok(())
            })
            .await
            .unwrap();
    }
}
