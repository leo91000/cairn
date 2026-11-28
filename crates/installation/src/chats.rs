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
    let origin_rule = crate::rules::engine::conversations::origin_rule_for_chat(db, &chat.id)?;
    let person_participated = db.0.query_row(
        "SELECT EXISTS(SELECT 1 FROM conversation_entry_writers WHERE chat_id=? AND writer='person')",
        [&chat.id],
        |row| row.get::<_, bool>(0),
    )?;
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
        origin_rule,
        person_participated,
        outcome: run
            .as_ref()
            .and_then(|run| run.get("outcome"))
            .filter(|value| !value.is_null())
            .cloned(),
        snoozed_until: None,
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
         ELSE 0 END,CASE WHEN json_type(payload,'$.item.request_id')='text'
         THEN json_extract(payload,'$.item.request_id') END
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
                    request_id: row.get(3)?,
                })
            },
        )
        .optional()?;
    let Proposal {
        event_id,
        kind,
        has_content,
        request_id,
    } = event.ok_or_else(stale)?;
    if kind != "item.completed"
        || !has_content
        || approval.source_id != format!("{run_id}:{event_id}")
    {
        return Err(stale());
    }
    let request_id = request_id.filter(|id| !id.is_empty()).ok_or_else(|| {
        Error::conflict(
            "This proposal has no authored request ID. Ask for a fresh Plan proposal before approving.",
        )
    })?;
    let latest_request: Option<(i64, Option<String>)> =
        db.0.query_row(
            "SELECT id,CASE WHEN json_type(payload,'$.messageId')='text'
             THEN json_extract(payload,'$.messageId') END
             FROM events WHERE run_id=? AND type='chat.user' ORDER BY id DESC LIMIT 1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if !latest_request.is_some_and(|(source, latest)| {
        source < event_id && latest.as_deref() == Some(request_id.as_str())
    }) {
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

mod service;
mod turns;

use turns::*;
pub(crate) use turns::{returned_key, select_agent, skips_replay, turn_held, turn_returned};

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

#[cfg(test)]
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
mod tests;
