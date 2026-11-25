pub(crate) mod coding_agent;
pub(crate) mod continuation;

use crate::worker::checkpoint::RunCheckpoint;
use cairn_contracts::EventKind;

mod handoff;
pub(crate) mod question_storage;
pub(crate) mod questions;
pub(crate) mod side_questions;

pub use crate::waits as wait_for;

pub use handoff::{PLAN_TURN, execution_text, with_invoked_skills};
pub use questions::{Question, QuestionField, QuestionStatus};
pub use side_questions::side_question_prefixes;

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
pub(crate) use handoff::handoff_context;
use questions::{find_question, questions, save_question};
use rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;
pub use storage::{Conversation, Message, MessageAnswer};

pub mod engine_switch;
#[cfg(test)]
pub(crate) mod list_measurements;
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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    plan: bool,
}

/// Whether the run's current turn is a plan turn. Run grants are tied to that turn,
/// so its tools keep this restriction until the next turn replaces the grant.
pub(crate) fn planning(run: &Value) -> bool {
    run["chatExecution"]["plan"] == true
}

/// Whether the next turn of this run follows a plan turn, whose engine mode may persist.
/// A compaction in between starts no turn, so it carries the mark forward.
fn follows_plan(run: &Value) -> bool {
    planning(run) || (is_compaction(run) && run["chatExecution"]["afterPlan"] == true)
}

fn chat(db: &Db<'_>, id: &str) -> Result<Value> {
    required(db.conversation_boundary(id)?, "Chat not found")
}

pub(crate) fn conversation(db: &Db<'_>, id: &str) -> Result<Conversation> {
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

fn can_join_current_turn(db: &Db<'_>, run: &Value) -> Result<bool> {
    if !run_is_active(run) {
        return Ok(false);
    }
    let id = text(run, "id");
    if run["recoveryPending"] == true
        && RunCheckpoint::load_in(db, id)?.is_some_and(|saved| saved.settled().is_some())
    {
        return Ok(false);
    }
    Ok(!RunCheckpoint::held_deadline_elapsed_in(db, id, now())?)
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
        && a.plan == b.plan
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
    let (pending_questions, question_at) = db.pending_question_summary(&chat.id)?;
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
    let mut view = Chat {
        next_selection: None,
        can_request_new_session: crate::conversation_lifecycle::can_request_new_session(&chat, run),
        turn_returned: run
            .map(|run| turn_returned(db, text(run, "id")))
            .transpose()?
            .unwrap_or(false),
        finished_at: serde_json::from_value(run_field("finishedAt"))?,
        read_at: question_at.max(run_field("finishedAt").as_i64()),
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
        environment_expiration_cause: serde_json::from_value(run_field(
            "environmentExpirationCause",
        ))
        .unwrap_or_default(),
        environment_error: chat.environment_error,
        requested_by: chat.requested_by,
        wakeup: chat.wakeup,
        requester,
        provider,
        compaction,
        coding_agent_switch,
        error,
        pending_questions,
        pending_side_questions: 0,
        can_ask_side_question: false,
        side_question_reason: None,
        side_question_unavailable: None,
        agent_name,
        project_name,
        status,
        preview: None,
        questions: Vec::new(),
        side_questions: Vec::new(),
        messages: Vec::new(),
        run: None,
    };
    side_questions::view(db, &mut view, run)?;
    Ok(view)
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
            "Continue or resolve the last turn before compacting.".into(),
        ));
    }
    if !run["workspaceCleanedAt"].is_null() {
        return Ok(Some(
            "This workspace has been cleaned up. Start a new chat.".into(),
        ));
    }
    let (fresh_session, held_expired) = RunCheckpoint::compaction_flags_in(db, text(run, "id"))?;
    if held_expired {
        return Ok(Some(
            "Wait for the previous environment to stop before compacting.".into(),
        ));
    }
    if !run["sessionId"].is_string() || chat.session_restart_requested || fresh_session {
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
        plan: false,
        // Compacting delivers no turn: the turn after it still follows the plan turn.
        after_plan: follows_plan(&run),
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
        Some(&json!({ "chatOperation": COMPACT })),
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
    let next_selection = if chat.lifecycle == cairn_contracts::ConversationLifecycle::Active {
        crate::coding_agent_configuration::next_selection(db, &chat.agent_id, run.as_ref())?
    } else {
        None
    };
    let mut result = serde_json::to_value(conversation_view_with_run(db, chat, run.as_ref())?)?;
    if let Some(selection) = next_selection {
        result["nextSelection"] = serde_json::to_value(selection)?;
    }
    if !is_active(&result) {
        result["questions"] = json!([]);
        result["sideQuestions"] = json!([]);
        result["messages"] = json!([]);
        result["run"] = Value::Null;
        result["pendingQuestions"] = 0.into();
        result["pendingSideQuestions"] = 0.into();
        return Ok(result);
    }
    result["questions"] = serde_json::to_value(questions(db, id)?)?;
    result["sideQuestions"] = serde_json::to_value(side_questions::side_questions(db, id)?)?;
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
fn changes_running_agent(message: &Message, run: &Value) -> Result<bool> {
    let agent = &run["snapshot"]["agent"];
    let differs = |choice: &str, current: &Value| {
        !choice.is_empty() && choice != current.as_str().unwrap_or("")
    };
    let provider_changed =
        !message.provider.is_empty() && message.provider != Provider::of_run(run)?.as_str();
    // A turn without a speed runs at Standard, so asking for Standard changes nothing.
    let running_speed = agent["speed"]
        .as_str()
        .filter(|speed| !speed.is_empty())
        .unwrap_or(crate::models::STANDARD_SPEED);
    Ok(provider_changed
        || differs(&message.model, &agent["model"])
        || differs(&message.reasoning, &agent["reasoning"])
        || (!message.speed.is_empty() && message.speed != running_speed))
}

fn retained_followup_key(message: &str) -> String {
    format!("retained-followup:{message}")
}

fn steering_requested(db: &Db<'_>, message: &Message) -> Result<bool> {
    Ok(message.mode == MessageMode::Steer && db.kv(&retained_followup_key(&message.id))?.is_none())
}

fn steering_fits_turn(db: &Db<'_>, message: &Message, run: &Value) -> Result<bool> {
    Ok(steering_requested(db, message)?
        && (!message.plan || planning(run))
        && !changes_running_agent(message, run)?)
}

/// The coding agent a message's turn runs with: the one it names, else the running turn's
/// for a steering message, else the one the queued messages ahead of it, the conversation's
/// latest turn or its agent select.
fn message_provider(db: &Db<'_>, chat: &Conversation, message: &Message) -> Result<String> {
    if !message.provider.is_empty() {
        return Ok(message.provider.clone());
    }
    let run = db.run(chat.run_id.as_deref().unwrap_or(""))?;
    if steering_requested(db, message)?
        && let Some(run) = run.as_ref()
        && can_join_current_turn(db, run)?
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
    if !steering_requested(db, message)? {
        return Ok(());
    }
    let Some(run) = db.run(chat.run_id.as_deref().unwrap_or(""))? else {
        return Ok(());
    };
    if !can_join_current_turn(db, &run)? {
        return Ok(());
    }
    if changes_running_agent(message, &run)? {
        return Err(Error::conflict(
            "Queue this message to change provider, model, reasoning or speed on the next turn.",
        ));
    }
    // Steering joins the running turn with its permissions: a plan request cannot
    // restrict a turn that may already change files.
    if message.plan && !planning(&run) {
        return Err(Error::conflict(
            "Queue this message to plan on the next turn: the running turn can change files.",
        ));
    }
    Ok(())
}

/// Plan turns need a coding agent whose read-only boundary Cairn enforces.
fn require_plan_support(db: &Db<'_>, chat: &Conversation, message: &Message) -> Result<()> {
    if !message.plan {
        return Ok(());
    }
    require_plan_turns(Provider::parse(&message_provider(db, chat, message)?)?)
}

pub(crate) fn require_plan_turns(provider: Provider) -> Result<()> {
    if provider.adapter().plan_turns() {
        return Ok(());
    }
    Err(Error::bad(format!(
        "{} does not support plan turns: Cairn cannot enforce a read-only boundary for it.",
        provider.label()
    )))
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
    let undelivered = messages
        .iter()
        .filter(|message| {
            matches!(
                message.status,
                MessageStatus::Queued | MessageStatus::Sending
            )
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

/// Called inside the same manager write transaction as queue insertion. A delayed client
/// cannot approve a proposal after another writer accepted a revision or implementation.
fn require_current_proposal(
    db: &Db<'_>,
    chat: &Conversation,
    messages: &[Message],
    message: &Message,
    approval: &cairn_contracts::PlanApproval,
) -> Result<()> {
    struct Proposal {
        event_id: i64,
        kind: String,
        has_content: bool,
        request_kind: Option<String>,
        request_id: Option<String>,
    }
    let stale = || {
        Error::conflict(
            "The proposed plan changed. Refresh the conversation and review it before implementing.",
        )
    };
    if message.plan
        || message.mode != MessageMode::Queue
        || chat.revision != approval.revision
        || messages
            .iter()
            .any(|m| matches!(m.status, MessageStatus::Queued | MessageStatus::Sending))
    {
        return Err(stale());
    }
    let run_id = chat.run_id.as_deref().ok_or_else(stale)?;
    let run = db.run(run_id)?.ok_or_else(stale)?;
    if run_is_active(&run) || run["recoveryPending"] == true {
        return Err(stale());
    }
    let event: Option<Proposal> =
        db.0.query_row(
            "SELECT id,type,CASE WHEN json_type(payload,'$.item.text')='text'
         THEN length(trim(json_extract(payload,'$.item.text'),char(9)||char(10)||char(13)||' '))>0
         ELSE 0 END,json_type(payload,'$.item.request_id'),json_extract(payload,'$.item.request_id')
         FROM events WHERE run_id=?
         AND type IN ('item.started','item.updated','item.completed')
         AND json_extract(payload,'$.item.type')='proposed_plan'
         AND COALESCE(json_extract(payload,'$.item.parent_id'),'')=''
         ORDER BY id DESC LIMIT 1",
            [run_id],
            |row| {
                Ok(Proposal {
                    event_id: row.get(0)?,
                    kind: row.get(1)?,
                    has_content: row.get(2)?,
                    request_kind: row.get(3)?,
                    request_id: row.get(4)?,
                })
            },
        )
        .optional()?;
    let Proposal {
        event_id,
        kind,
        has_content,
        request_kind,
        request_id,
    } = event.ok_or_else(stale)?;
    if kind != "item.completed"
        || !has_content
        || approval.source_id != format!("{run_id}:{event_id}")
    {
        return Err(stale());
    }
    let (request_count, latest_request): (i64, Option<String>) = db.0.query_row(
        "SELECT COUNT(DISTINCT CASE
             WHEN json_type(payload,'$.messageId')='text'
               AND json_extract(payload,'$.messageId')!=''
             THEN 'message:' || json_extract(payload,'$.messageId')
             ELSE 'source:' || id
         END), (SELECT json_extract(payload,'$.messageId') FROM events
         WHERE run_id=? AND type='chat.user' ORDER BY id DESC LIMIT 1)
         FROM events WHERE run_id=? AND type='chat.user'",
        rusqlite::params![run_id, run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if let Some(request_kind) = request_kind {
        if request_kind != "text"
            || request_id.as_deref().unwrap_or("").is_empty()
            || request_id != latest_request
        {
            return Err(stale());
        }
    } else if request_count != 1 {
        // Older receipts had no request_id. Their proposal is still attributable only
        // when the run contains exactly one human request.
        return Err(stale());
    }
    let later_request: bool = db.0.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND id>? AND type='chat.user')",
        rusqlite::params![run_id, event_id],
        |row| row.get(0),
    )?;
    if later_request {
        return Err(stale());
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
    let approval: Option<cairn_contracts::PlanApproval> = values
        .get("planApproval")
        .filter(|value| !value.is_null())
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()?;
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
        // A retry must not report a message that went back to its author, or was withdrawn,
        // as accepted (ADR-0091).
        if existing.status == MessageStatus::Cancelled {
            return Err(Error::conflict(
                "This message was cancelled. Send it again as a new message.",
            ));
        }
        return Ok(serde_json::to_value(existing)?);
    }
    if let Some(approval) = approval {
        require_current_proposal(db, &chat, &messages, &values, &approval)?;
    }
    require_sendable(db, &chat, &messages, &values.id)?;
    // Questionnaire answers continue an accepted plan turn even if the feature was disabled.
    if values.plan && answer.is_none() {
        crate::chat_plan_settings::require_enabled(db)?;
    }
    validate_speed(db, &chat, &values)?;
    validate_steer(db, &chat, &values)?;
    require_plan_support(db, &chat, &values)?;
    // The first message names the conversation, unless a person already did.
    if messages.is_empty() && !crate::chat_titles::is_manual(db, &chat.id)? {
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
    let created_at = now();
    let retired_followup = if let Some(run_id) = chat.run_id.as_deref()
        && RunCheckpoint::retained_input_deadline_in(db, run_id)?
            .is_some_and(|deadline| created_at >= deadline)
    {
        RunCheckpoint::held_deadline_elapsed_in(db, run_id, created_at)?
            || db
                .run_state(run_id)?
                .is_some_and(|run| run.recovery_pending)
    } else {
        false
    };
    let expiry_owns_wait = match (&chat.wakeup, chat.run_id.as_deref()) {
        (Some(_), Some(run_id)) => {
            RunCheckpoint::expired_hold_owns_wait_in(db, run_id, created_at)?
        }
        _ => false,
    };
    if !expiry_owns_wait {
        chat.wakeup = None;
    }
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
    values.created_at = created_at;
    let saved = db.save_message(&values)?;
    if retired_followup && saved.mode == MessageMode::Steer {
        // Keep the submitted mode for idempotent retries while the queue retains
        // the original admission decision through later provider turns.
        db.set(&retained_followup_key(&saved.id), &Value::Bool(true), None)?;
    }
    crate::conversation_writers::record_message(
        db,
        chat_id,
        &saved.id,
        &crate::conversation_writers::EntryWriter::of(origin),
    )?;
    // Cairn hooks retain the writer of the turn as well as its attestation.
    crate::rules::engine::conversations::record_input(db, chat_id, &saved.id, origin)?;
    Ok(serde_json::to_value(saved)?)
}

/// Deletes a queued ordinary message. Submitted questionnaire answers are immutable.
fn withdraw(db: &Db<'_>, chat_id: &str, message: &Message) -> Result<Value> {
    if message.question_id.is_some() {
        return Err(Error::conflict(
            "A submitted answer cannot be removed. Send a new message to correct it.",
        ));
    }
    db.delete(&retained_followup_key(&message.id))?;
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

/// Parses an edited message. A speed or plan request the editor did not send stays as
/// stored: an editor predating them never turns a plan request into an ordinary turn.
pub(crate) fn parse_edit(input: Value) -> Result<Value> {
    let kept = ["speed", "plan"].map(|field| input.get(field).is_none());
    let mut values = parse_message(input)?;
    if let Some(values) = values.as_object_mut() {
        for (field, kept) in ["speed", "plan"].into_iter().zip(kept) {
            if kept {
                values.remove(field);
            }
        }
    }
    Ok(values)
}

#[cfg(test)]
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
    // A returned message is its author's draft: only their composer takes it back.
    let author = if current.status == MessageStatus::Cancelled && values.is_none() {
        db.kv(&returned_key(&current.id))?
    } else {
        None
    };
    let returned = author.is_some();
    if current.status == MessageStatus::Cancelled && !returned {
        return Err(Error::conflict(
            "This message was cancelled and can no longer be changed.",
        ));
    }
    if let Some(author) = author {
        let allowed = match origin {
            crate::native::policy::Origin::Human(account)
            | crate::native::policy::Origin::PublicAccount(account) => author == account.as_str(),
            _ => false,
        };
        if !allowed {
            return Err(Error::conflict(
                "This message went back to the person who wrote it.",
            ));
        }
        db.delete(&returned_key(&current.id))?;
    }
    if current.status != MessageStatus::Queued && !returned {
        return Err(Error::conflict("This message is already being sent."));
    }
    crate::conversation_writers::record_message_before_change(db, &current)?;
    if let Some(run_id) = chat.run_id.as_deref()
        && let Some(run) = db.run(run_id)?
        && run["chatExecution"]["messageId"] == current.id
        && Provider::of_run(&run).ok() == Some(Provider::Antigravity)
    {
        // Reusing an ID or reverting an edit does not restore the execution
        // that froze the earlier input and its writer.
        db.change_run(
            run_id,
            crate::runs::RunChanges {
                boundary: Some(json!({ "chatMessageChanged": true })),
                ..Default::default()
            },
        )?;
    }
    let Some(mut values) = values else {
        // Withdrawing a queued message is an entry of whoever withdraws it.
        crate::conversation_writers::record_entry(
            db,
            chat_id,
            &crate::conversation_writers::EntryWriter::of(origin),
        )?;
        return withdraw(db, chat_id, &current);
    };
    if current.question_id.is_some() {
        return Err(Error::conflict("A submitted answer cannot be edited."));
    }
    crate::attachments::message(db, chat_id, &mut values)?;
    // An edit without a speed, from an editor predating speeds, keeps the stored one while the
    // message still goes to Codex.
    let kept_speed = values.get("speed").is_none();
    let kept_plan = values.get("plan").is_none();
    let was_plan = current.plan;
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
    if !kept_plan {
        current.plan = next.plan;
    }
    if kept_speed && message_provider(db, &chat, &current)? != Provider::Codex.as_str() {
        current.speed.clear();
    }
    // Steering joins the running turn with the message as edited, including a kept speed.
    validate_steer(db, &chat, &current)?;
    validate_speed(db, &chat, &current)?;
    if current.plan && !was_plan {
        crate::chat_plan_settings::require_enabled(db)?;
    }
    require_plan_support(db, &chat, &current)?;
    let saved = db.save_message(&current)?;
    // An edit counts as an entry of whoever edits.
    crate::conversation_writers::record_message(
        db,
        chat_id,
        &saved.id,
        &crate::conversation_writers::EntryWriter::of(origin),
    )?;
    // Cairn hooks retain the writer of the turn as well as its attestation.
    crate::rules::engine::conversations::record_input(db, chat_id, &saved.id, origin)?;
    Ok(serde_json::to_value(saved)?)
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
    db.delete(&retained_followup_key(&message.id))?;
    crate::rules::engine::conversations::delivered_input(db, &chat_id, run_id, message_id)?;
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
        plan: message.plan,
    })?;
    db.event(run_id, &EventKind::ChatUser, text, Some(&payload))?;
    touch(&mut chat);
    db.save_conversation(&chat)?;
    Ok(())
}

/// Recheck preparation against the currently selected execution.
fn same_turn(expected: &Value, current: &Value) -> bool {
    expected["id"] == current["id"]
        && expected["status"] == current["status"]
        && expected["recoveryPending"] == current["recoveryPending"]
        && crate::waits::execution_id(expected) == crate::waits::execution_id(current)
        && expected["environmentAdmissionRevision"] == current["environmentAdmissionRevision"]
}

fn steering_run(db: &Db<'_>, expected: &Value) -> Result<Option<Value>> {
    let id = text(expected, "id");
    let current = db.run(id)?;
    let unchanged = current
        .as_ref()
        .is_some_and(|current| same_turn(expected, current));
    if !unchanged || RunCheckpoint::held_deadline_elapsed_in(db, id, now())? {
        return Ok(None);
    }
    Ok(current)
}

/// Marks compatible steering messages as sending. A message keeps its own turn
/// when a later launch cannot honor its explicit agent settings or plan mode.
fn claim_steering(db: &Db<'_>, chat_id: &str, paused: bool, run: &Value) -> Result<Vec<Value>> {
    let delivering = &run["chatExecution"]["messageId"];
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
            && message.id.as_str() != delivering.as_str().unwrap_or("")
            && steering_fits_turn(db, &message, run)?
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

/// A chat whose last turn did not succeed returns what it never delivered, unless a
/// deletion cancelled it or the user asked for a fresh session.
fn needs_attention(chat: &Conversation, run: Option<&Value>) -> bool {
    run.is_some_and(|run| run["status"] != RunStatus::Succeeded)
        && !chat.cancelled_by_deletion
        && !chat.session_restart_requested
        && chat.environment_state != Some(EnvironmentState::Expired)
}

/// What a turn that did not succeed never delivered: its own message and ordinary messages
/// written before it ended, or before Stop was pressed. Messages written since start the
/// next turn. A person's messages certainly never delivered go back to them (`returning`).
/// Everything else is `held` behind a pause, as every failure did before: messages from
/// rules and agents, which have no composer to go back to, questionnaire answers, and a turn
/// message the agent may have received. A compaction carries no message.
#[derive(Default)]
struct Unsent {
    returning: Vec<(Message, String)>,
    held: Vec<Message>,
}

/// Latest timestamp attributable to the stopped turn's input. A message at a
/// retained environment deadline already belongs to the next turn.
fn turn_end(db: &Db<'_>, run: &Value) -> Result<i64> {
    let ended = run["cancelRequestedAt"]
        .as_i64()
        .or_else(|| run["finishedAt"].as_i64())
        .unwrap_or(0);
    Ok(
        RunCheckpoint::retained_input_deadline_in(db, text(run, "id"))?
            .map_or(ended, |deadline| ended.min(deadline.saturating_sub(1))),
    )
}

/// Whether resuming the conversation should leave its stopped turn alone: the turn's
/// message went back to its author, or it is no longer waiting and a message written since
/// starts the next turn. A turn message held behind the pause is replayed first, as a
/// recovery, so the agent that may have it is told to check what it already did.
pub(crate) fn skips_replay(db: &Db<'_>, chat_id: &str, run: &Value) -> Result<bool> {
    if turn_returned(db, text(run, "id"))? {
        return Ok(true);
    }
    let turn = run["chatExecution"]["messageId"].as_str();
    let ended_at = turn_end(db, run)?;
    let written_since = db.conversation_messages(chat_id)?.iter().any(|message| {
        message.status == MessageStatus::Queued
            && message.question_id.is_none()
            && message.created_at > ended_at
            && Some(message.id.as_str()) != turn
    });
    Ok(written_since && !turn_held(db, chat_id, run)?)
}

/// The stopped turn still holds a message: its own, steering or an answer it claimed, even
/// after Stop was pressed, which the agent may have read, or a message from before it ended
/// held for a person. Resuming replays the turn first, as a recovery that tells the agent to
/// check what it did, and held messages follow it.
pub(crate) fn turn_held(db: &Db<'_>, chat_id: &str, run: &Value) -> Result<bool> {
    let turn = run["chatExecution"]["messageId"].as_str();
    let ended_at = turn_end(db, run)?;
    Ok(db.conversation_messages(chat_id)?.iter().any(|message| {
        message.status == MessageStatus::Sending
            || (message.status == MessageStatus::Queued
                && (Some(message.id.as_str()) == turn || message.created_at <= ended_at))
    }))
}

/// A turn whose agent never received its message: no attempt launched a process for this
/// turn, for example when admission or the controller refused it. Once any attempt launched
/// one, the agent may have it, whatever later attempts reported. Checkpoints saved before
/// turns were recorded only know whether the run ever launched.
fn never_received(db: &Db<'_>, run: &Value) -> Result<bool> {
    let Some(checkpoint) = RunCheckpoint::load_in(db, text(run, "id"))? else {
        return Ok(run["startedAt"].is_null());
    };
    // A launch without a chat message, such as a mission's, records an empty turn.
    let turn = run["chatExecution"]["messageId"]
        .as_str()
        .unwrap_or_default();
    Ok(match &checkpoint.launched_turn {
        Some(launched) => launched != turn,
        None => !checkpoint.launched(),
    })
}

fn unsent(db: &Db<'_>, chat_id: &str, run: &Value) -> Result<Unsent> {
    let execution = &run["chatExecution"];
    // A compaction carries no message: a person's messages written meanwhile follow it,
    // while those of rules and agents wait for a person, as after any failure.
    let operation = !execution["operation"].is_null();
    let turn = execution["messageId"].as_str();
    let ended_at = turn_end(db, run)?;
    let mut unsent = Unsent::default();
    let mut received = None;
    for message in db.conversation_messages(chat_id)? {
        let pending = matches!(
            message.status,
            MessageStatus::Queued | MessageStatus::Sending
        );
        let own = !operation && Some(message.id.as_str()) == turn;
        // Steering or an answer the stopping turn claimed after Stop belongs to it too.
        if !pending
            || (message.created_at > ended_at && !own && message.status != MessageStatus::Sending)
        {
            continue;
        }
        if operation {
            if crate::conversation_writers::person(db, chat_id, &message.id)?.is_none() {
                unsent.held.push(message);
            }
            continue;
        }
        // The turn's own message, and steering being sent to it, reached the agent only if a
        // process launched for this turn.
        let uncertain = if own || message.status == MessageStatus::Sending {
            if received.is_none() {
                received = Some(!never_received(db, run)?);
            }
            received == Some(true)
        } else {
            false
        };
        let author = if message.question_id.is_some() || uncertain {
            None
        } else {
            crate::conversation_writers::person(db, chat_id, &message.id)?
        };
        match author {
            Some(author) => unsent.returning.push((message, author)),
            None => unsent.held.push(message),
        }
    }
    Ok(unsent)
}

/// The tick settles a turn that did not succeed once: later ticks leave a person's
/// resumption alone. A run continues across turns; its end time names this one.
fn settled_key(run: &Value) -> String {
    format!("turn-settled:{}", text(run, "id"))
}

fn settled(db: &Db<'_>, run: &Value) -> Result<bool> {
    Ok(db
        .kv(&settled_key(run))?
        .is_some_and(|ended| ended == run["finishedAt"]))
}

/// Returns a person's unsent messages to them instead of replaying them, and holds the rest
/// behind a pause, queued again. The run is read again here: a manual resume since the
/// tick read it keeps its message.
fn settle_turn(db: &Db<'_>, chat_id: &str, ended: &Value) -> Result<()> {
    let Some(run) = db.run(text(ended, "id"))? else {
        return Ok(());
    };
    if run["status"] != ended["status"]
        || run["finishedAt"] != ended["finishedAt"]
        || run["chatExecution"]["messageId"] != ended["chatExecution"]["messageId"]
        || settled(db, &run)?
    {
        return Ok(());
    }
    db.set(&settled_key(&run), &run["finishedAt"], None)?;
    let turn = run["chatExecution"]["messageId"].as_str();
    let unsent = unsent(db, chat_id, &run)?;
    // A cancellation already paused the conversation when it was requested.
    let held = !unsent.held.is_empty();
    let ended_at = turn_end(db, &run)?;
    for mut message in unsent.held {
        // A message claimed after Stop stays claimed: the turn holds it, so resuming replays
        // the turn first and delivers it again.
        if message.status == MessageStatus::Sending && message.created_at <= ended_at {
            message.status = MessageStatus::Queued;
            db.save_message(&message)?;
        }
    }
    if held && !conversation(db, chat_id)?.paused {
        pause_in(db, chat_id, true)?;
    }
    for (mut message, author) in unsent.returning {
        // This turn cannot be replayed once its message went back to its author.
        if Some(message.id.as_str()) == turn {
            db.set(
                &format!("turn-returned:{}", text(&run, "id")),
                &message.id.clone().into(),
                None,
            )?;
        }
        db.set(&returned_key(&message.id), &author.into(), None)?;
        message.status = MessageStatus::Cancelled;
        db.save_message(&message)?;
    }
    Ok(())
}

pub(crate) fn returned_key(message: &str) -> String {
    format!("message-returned:{message}")
}

/// Whether a failure or stop returned this turn's message to its author: replaying the turn
/// would deliver a message that is back in their composer.
pub(crate) fn turn_returned(db: &Db<'_>, run: &str) -> Result<bool> {
    Ok(db.kv(&format!("turn-returned:{run}"))?.is_some())
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
    if continues || switched {
        // Naming the same engine still follows the conversation's selection.
        // Changed native/profile defaults are resolved atomically before launch.
        agent["model"] = if continues {
            inherited_model
        } else {
            String::new()
        }
        .into();
        agent["reasoning"] = if continues {
            inherited_reasoning
        } else {
            String::new()
        }
        .into();
        set_speed(agent, if continues { &inherited_speed } else { "" });
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
        // A turn stopped or failed before it ever launched starts afresh.
        None if chat.cancelled_by_deletion
            || (run["status"] != RunStatus::Succeeded && run["startedAt"].is_null()) =>
        {
            RunCheckpoint::default()
        }
        None => return Err(Error::conflict("Run checkpoint not found")),
    };
    let provider = Provider::of_run(snapshot)?;
    let expired = chat.environment_state == Some(EnvironmentState::Expired)
        || run["environmentState"] == "expired";
    let fresh_environment_without_session =
        run["environmentExpiredAt"].is_number() && !run["sessionId"].is_string();
    // A turn that handed the conversation over failed before its session existed: the
    // next message hands it over again rather than starting without its history.
    let unfinished_handoff = run["status"] != RunStatus::Succeeded
        && run["chatExecution"]["context"].is_string()
        && !run["sessionId"].is_string();
    let restart_requested = expired
        || fresh_environment_without_session
        || unfinished_handoff
        || chat.session_restart_requested
        || checkpoint.fresh_session == Some(true);
    if provider != Provider::of_run(run)? || restart_requested {
        if !expired && run["sessionId"].is_string() {
            checkpoint.workspace_initialized = Some(true);
        }
        // The next provider cannot resolve requests from the session being replaced.
        questions::release(db, run_id, None)?;
        checkpoint.fresh_session = Some(false);
        execution.context = Some(handoff_context(db, run_id)?);
        checkpoint.launched = Some(false);
        // The same turn relaunched with another coding agent keeps its earlier launch: the
        // first agent may already have its message.
        if checkpoint.launched_turn.as_deref() != Some(execution.message_id.as_str()) {
            checkpoint.launched_turn = None;
        }
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
        // Keep the interactive rule explicit across native versions; compaction
        // can drop earlier user messages.
        execution.text = format!(
            "{CONTINUED_MISSION} {}\n\n{}",
            crate::run_output::INTERACTIVE,
            execution.text
        );
    }
    checkpoint.completed = Some(false);
    checkpoint.output_interrupted = None;
    checkpoint.provider_failed = None;
    checkpoint.antigravity_probe_retry = None;
    checkpoint.turn_pending = Some(true);
    checkpoint.recovery_deadline_ms = None;
    checkpoint.terminal_code = Some(None);
    checkpoint.controller_recoveries = None;
    checkpoint.controller_recovery_reservation = None;
    checkpoint.provider_failure_attempt = None;
    checkpoint.acknowledge_finalization_observation();
    checkpoint.move_recovery_reservation = None;
    checkpoint.provider_retry_reservation = None;
    checkpoint.pending_paid_admission = None;
    checkpoint.controller_observation = None;
    // A new turn after one that did not succeed gives up the environment that turn retained,
    // as a manual resume does: recovery fences it before launch, with a full budget.
    if run["status"] != RunStatus::Succeeded
        && let Some(held) = checkpoint.held_environment.take()
        && checkpoint.runner_id() == Some(held.attempt.as_str())
    {
        crate::worker::jobs::unknown(
            db,
            run_id,
            &held.attempt,
            "The previous environment will be stopped before continuing; background completion is unknown.",
        )?;
    }
    if checkpoint.held_environment.is_none() {
        checkpoint.remaining_ms = Some(crate::run_limits::budget_ms(agent));
    }
    checkpoint.last_message = None;
    checkpoint.settled = None;
    checkpoint.retired_environment_deadline = None;
    checkpoint.retained_completion = None;
    checkpoint.store_in(db, run_id)?;
    let compaction = execution.operation.as_deref() == Some(COMPACT);
    let mut patch = crate::runs::RunChanges {
        snapshot: Some(serde_json::from_value(snapshot["snapshot"].clone())?),
        status: Some(RunStatus::Queued),
        error: Some(None),
        retry: Some(None),
        finished_at: Some(None),
        cancel_requested_at: Some(None),
        recovery_pending: Some(checkpoint.held_environment.is_none()),
        boundary: Some(json!({
            "chatExecution": serde_json::to_value(execution)?,
            "chatMessageDigest": null,
            "chatMessageChanged": null,
            "environmentState": if expired { json!("expired") } else { run["environmentState"].clone() },
            // The next turn's adapter announces again whether it can answer side questions.
            "sideQuestions": null,
            "sideQuestionsCompacting": null,
            "sideQuestionsClosed": null,
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
    // Retry preparation if a new turn, retirement or backend choice committed
    // while the snapshot was being built. The message stays queued.
    let current = db.run(chat.run_id.as_deref().unwrap_or(""))?;
    let same_run = match (&run, &current) {
        (Some(expected), Some(current)) => same_turn(expected, current),
        (None, None) => true,
        _ => false,
    };
    if !same_run {
        return Ok(());
    }
    let waking = message["wakeup"].is_object();
    let unchanged = if waking {
        continuation::ready(db, &chat, current.as_ref(), now())?
            && serde_json::to_value(&chat.wakeup)? == message["wakeup"]
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
    if let Some(run) = &current {
        let run_id = text(run, "id");
        // The expiry owner settles the last turn before a follow-up can use
        // this conversation again. Its poll may be behind the scheduler.
        if crate::environment_expiration::pending(db, run_id)?
            || (run["status"] == RunStatus::Succeeded
                && RunCheckpoint::held_deadline_elapsed_in(db, run_id, now())?)
        {
            return Ok(());
        }
    }
    crate::coding_agent_configuration::freeze_message(db, &mut snapshot, run.as_ref(), &message)?;
    let provider = Provider::of_run(&snapshot)?;
    provider
        .adapter()
        .validate_agent(&snapshot["snapshot"]["agent"])?;
    let execution = ChatExecution {
        message_id: text(&message, "id").to_owned(),
        text: with_invoked_skills(text(&message, "text"), &snapshot["snapshot"]["skills"]),
        attachments: serde_json::from_value(message["attachments"].clone())?,
        recovery: false,
        context: None,
        plan: message["plan"] == true,
        after_plan: run.as_ref().is_some_and(follows_plan),
        operation: None,
        previous: None,
    };
    // A wakeup resumes a turn; only a message launches one. Both record what they freeze.
    let launched_by = (!waking).then(|| execution.message_id.clone());
    let previous_turn = run.as_ref().map(|run| {
        json!({
            "status": run["status"],
            "error": run["error"],
        })
    });
    let run_id = if let Some(run) = run {
        continue_run(db, &chat, &run, &snapshot, execution)?;
        text(&run, "id").to_owned()
    } else {
        crate::native::policy::apply(&mut snapshot, &serde_json::to_value(&chat)?);
        snapshot["chatExecution"] = serde_json::to_value(execution)?;
        crate::runs::admit_prepared(db, &snapshot, None)?;
        chat.run_id = Some(text(&snapshot, "id").to_owned());
        text(&snapshot, "id").to_owned()
    };
    db.delete(&format!("turn-returned:{run_id}"))?;
    db.delete(&format!("turn-settled:{run_id}"))?;
    match &launched_by {
        Some(message) => {
            crate::conversation_writers::record_turn(db, &run_id, message)?;
            if provider == Provider::Antigravity {
                let input = required(find_message(db, chat_id, message)?, "Message not found")?;
                let digest = crate::conversation_writers::entry_digest(&input)?;
                db.change_run(
                    &run_id,
                    crate::runs::RunChanges {
                        boundary: Some(json!({ "chatMessageDigest": digest })),
                        ..Default::default()
                    },
                )?;
            }
        }
        None => crate::conversation_writers::record_relaunch(db, &run_id, &chat.id)?,
    }
    crate::rules::engine::conversations::begin_chat_turn(
        db,
        &chat.id,
        &run_id,
        launched_by.as_deref(),
        previous_turn.as_ref(),
    )?;
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
        "SELECT data FROM run_views AS runs WHERE json_extract(requested_by,'$.runId')=?",
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
    create_as_writer(
        db,
        input,
        origin,
        &crate::conversation_writers::EntryWriter::of(origin),
    )
}

/// Native authority and the writer of the entry are distinct: a rule carries its
/// saved native grant, but never becomes the person who saved it.
pub(crate) fn create_as_writer(
    db: &Db<'_>,
    input: Value,
    origin: &crate::native::policy::Origin,
    writer: &crate::conversation_writers::EntryWriter,
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
    let saved = db.save_conversation(&chat)?;
    crate::conversation_writers::record_creation(db, &saved, writer)?;
    Ok(serde_json::to_value(saved)?)
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
    let saved = serde_json::to_value(db.save_conversation(&chat)?)?;
    // A rule that cancels pauses again after this; any other pause or resumption
    // takes the conversation back from the rule's cancellation.
    Ok(saved)
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
        self.chat_from_run_live(run_id, crate::authorization::Authorization::once(authorize))
            .await
    }

    pub(crate) async fn chat_from_run_live(
        &self,
        run_id: &str,
        authorize: crate::authorization::Authorization,
    ) -> Result<Value> {
        authorize.recheck().await?;
        let origin = crate::native::policy::origin();
        if origin.known() {
            let run = self.store.run(run_id).await?;
            crate::native::policy::refresh_for(self, &run["snapshot"]["agent"]).await?;
        }

        let active = self.worker.active.lock().await;
        authorize.recheck().await?;
        if active.contains_key(run_id) {
            return Err(Error::conflict(
                "Wait for this run to finish stopping before continuing in chat.",
            ));
        }
        let run_id = run_id.to_owned();
        self.store
            .transaction(move |db| {
                authorize.authorize(db)?;
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
    pub(crate) async fn chat_compact_live(
        &self,
        id: &str,
        authorize: crate::authorization::Authorization,
    ) -> Result<Value> {
        authorize.recheck().await?;
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_chat(self, id).await?;
        }
        let id = id.to_owned();

        // A succeeded run's worker may still be saving its checkpoint: the compaction resets
        // it, so it waits for that worker to release the run, like resuming does.
        let active = self.worker.active.lock().await;
        authorize.recheck().await?;
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
        authorize.recheck().await?;
        let result = self
            .store
            .transaction(move |db| {
                authorize.authorize(db)?;
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
        let side_open = self.side_question_chats().await?;
        let expiring = crate::environment_expiration::pending_runs(self).await?;
        for chat in self.store.conversations().await? {
            // Before anything holds the conversation (trash, cleaning, a pending agent
            // switch): a stopped turn's side questions fail now.
            if side_open.contains(&chat.id) {
                let chat_id = chat.id.clone();
                let expiring = self
                    .store
                    .read(move |db| side_questions::expiring(db, &chat_id))
                    .await?;
                if expiring {
                    let chat_id = chat.id.clone();
                    self.store
                        .transaction(move |db| side_questions::expire(db, &chat_id))
                        .await?;
                }
            }
            if chat.lifecycle != cairn_contracts::ConversationLifecycle::Active
                || chat
                    .run_id
                    .as_ref()
                    .is_some_and(|run| expiring.contains(run))
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
                // A queued run has no process to read steering yet: it is claimed once one
                // runs, so a turn that never ran gives it back with its message.
                if !is_compaction(run) && run["status"] != RunStatus::Queued {
                    self.publish_steering(&chat, run, side_open.contains(&chat.id))
                        .await?;
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
            if let Some(run) = run.as_ref().filter(|run| needs_attention(&chat, Some(run))) {
                self.chat_cancel_wait(&id).await?;
                // Read first: an idle failed conversation needs no writer on each tick.
                let (chat_id, ended) = (id.clone(), run.clone());
                let settling = self
                    .store
                    .read(move |db| Ok(db.has_pending_messages(&chat_id)? && !settled(db, &ended)?))
                    .await?;
                if settling {
                    // The next tick dispatches what remains, unless a pause holds it.
                    let (chat_id, ended) = (id.clone(), run.clone());
                    self.store
                        .transaction(move |db| settle_turn(db, &chat_id, &ended))
                        .await?;
                    // No turn runs now: steering returned to its author must not wait in the
                    // inbox for the next turn's process.
                    let directory = self
                        .config
                        .data_dir
                        .join("runs")
                        .join(text(run, "id"))
                        .join("chat-input");
                    if tokio::fs::try_exists(&directory).await.unwrap_or(false) {
                        side_questions::publish_inbox(
                            &self.store,
                            text(run, "id"),
                            "",
                            &directory,
                            Vec::new(),
                        )
                        .await?;
                    }
                    continue;
                }
                if chat.paused {
                    continue;
                }
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

    /// Conversations with a side question their reply may still receive.
    async fn side_question_chats(&self) -> Result<HashSet<String>> {
        self.store.read(|db| side_questions::open_chats(db)).await
    }

    /// Writes steering messages where the running agent turn reads them.
    /// `side_open`: the conversation has side questions its reply may still receive.
    async fn publish_steering(
        &self,
        chat: &Conversation,
        run: &Value,
        side_open: bool,
    ) -> Result<()> {
        let chat_id = chat.id.clone();
        let paused = chat.paused;
        let claiming = run.clone();
        // Only a reply that announced side questions, with some to receive, reads them; a
        // later tick sees an announcement or a question made since this one read them.
        let announced = side_open && run["sideQuestions"] == true;
        let Some((mut steering, side_turn)) = self
            .store
            .transaction(move |db| {
                let Some(current) = steering_run(db, &claiming)? else {
                    return Ok(None);
                };
                let mut steering = claim_steering(db, &chat_id, paused, &current)?;
                // Side questions are not chat messages: the queue, pause and steering never
                // hold them, and the adapter answers them beside the turn. The run is read
                // again here: a launch since the tick read it withdrew its announcement.
                let mut side_turn = String::new();
                if announced {
                    steering.extend(side_questions::deliveries(db, &chat_id, &current)?);
                    side_turn = text(&current["chatExecution"], "messageId").to_owned();
                }
                Ok(Some((steering, side_turn)))
            })
            .await?
        else {
            return Ok(());
        };
        let skills = &run["snapshot"]["skills"];
        for message in &mut steering {
            let side =
                message["sideQuestion"].is_object() || message["sideQuestionCancelled"] == true;
            if side {
                continue;
            }
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
        // Nor may this write restore side questions an attempt removed meanwhile, or hand
        // one turn's side questions to the next.
        side_questions::publish_inbox(
            &self.store,
            text(run, "id"),
            &side_turn,
            &directory,
            steering,
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
        let provider = Provider::of_run(&snapshot)?;
        provider
            .adapter()
            .validate_agent(&snapshot["snapshot"]["agent"])?;
        if message["plan"] == true {
            require_plan_turns(provider)?;
        }
        let chat_id = chat.id.clone();
        self.store
            .transaction(move |db| launch_turn(db, &chat_id, run, message, snapshot))
            .await
    }
}

#[cfg(test)]
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
mod tests;
