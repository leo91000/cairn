use super::*;

pub(super) fn acknowledge(db: &Db<'_>, run_id: &str, message_id: &str) -> Result<()> {
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
pub(super) fn same_turn(expected: &Value, current: &Value) -> bool {
    expected["id"] == current["id"]
        && expected["status"] == current["status"]
        && expected["recoveryPending"] == current["recoveryPending"]
        && crate::waits::execution_id(expected) == crate::waits::execution_id(current)
        && expected["environmentAdmissionRevision"] == current["environmentAdmissionRevision"]
}

pub(super) fn steering_run(db: &Db<'_>, expected: &Value) -> Result<Option<Value>> {
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
pub(super) fn claim_steering(
    db: &Db<'_>,
    chat_id: &str,
    paused: bool,
    run: &Value,
) -> Result<Vec<Value>> {
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
pub(super) fn next_queued(db: &Db<'_>, chat_id: &str) -> Result<Option<Value>> {
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
pub(super) fn needs_attention(chat: &Conversation, run: Option<&Value>) -> bool {
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
pub(super) struct Unsent {
    pub(super) returning: Vec<(Message, String)>,
    pub(super) held: Vec<Message>,
    cancelled: Vec<Message>,
}

/// Latest timestamp attributable to the stopped turn's input. A message at a
/// retained environment deadline already belongs to the next turn.
pub(super) fn turn_end(db: &Db<'_>, run: &Value) -> Result<i64> {
    let ended = [
        run["cancelRequestedAt"].as_i64(),
        run["finishedAt"].as_i64(),
    ]
    .into_iter()
    .flatten()
    .min()
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
    if turn_returned(db, text(run, "id"))? || RunCheckpoint::antigravity_replay_refused_in(db, run)?
    {
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
pub(super) fn never_received(db: &Db<'_>, run: &Value) -> Result<bool> {
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

pub(super) fn unsent(db: &Db<'_>, chat_id: &str, run: &Value) -> Result<Unsent> {
    let execution = &run["chatExecution"];
    // A compaction carries no message: a person's messages written meanwhile follow it,
    // while those of rules and agents wait for a person, as after any failure.
    let operation = !execution["operation"].is_null();
    let turn = execution["messageId"].as_str();
    let ended_at = turn_end(db, run)?;
    let mut unsent = Unsent::default();
    let replay_refused = RunCheckpoint::antigravity_replay_refused_in(db, run)?;
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
        let author = if message.question_id.is_some() || (uncertain && !replay_refused) {
            None
        } else {
            crate::conversation_writers::person(db, chat_id, &message.id)?
        };
        match author {
            Some(author) => unsent.returning.push((message, author)),
            None if replay_refused && (own || uncertain) => unsent.cancelled.push(message),
            None => unsent.held.push(message),
        }
    }
    Ok(unsent)
}

/// The tick settles a turn that did not succeed once: later ticks leave a person's
/// resumption alone. A run continues across turns; its end time names this one.
pub(super) fn settled_key(run: &Value) -> String {
    format!("turn-settled:{}", text(run, "id"))
}

pub(super) fn settled(db: &Db<'_>, run: &Value) -> Result<bool> {
    Ok(db
        .kv(&settled_key(run))?
        .is_some_and(|ended| ended == run["finishedAt"]))
}

/// Returns a person's unsent messages to them instead of replaying them, and holds the rest
/// behind a pause, queued again. The run is read again here: a manual resume since the
/// tick read it keeps its message.
pub(super) fn settle_turn(db: &Db<'_>, chat_id: &str, ended: &Value) -> Result<()> {
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
    for mut message in unsent.cancelled {
        message.status = MessageStatus::Cancelled;
        db.save_message(&message)?;
        if let Some(question_id) = message.question_id.as_deref()
            && let Some(mut question) = find_question(db, chat_id, question_id)?
            && question.status == QuestionStatus::Answering
            && question.message_id.as_deref() == Some(message.id.as_str())
        {
            question.status = QuestionStatus::Cancelled;
            question.blocking = false;
            save_question(db, &question)?;
        }
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
pub(super) fn set_speed(agent: &mut Value, speed: &str) {
    if !speed.is_empty() {
        agent["speed"] = speed.into();
    } else if let Some(agent) = agent.as_object_mut() {
        agent.remove("speed");
    }
}

/// Reuses the chat's run for the next turn, handing the transcript to a fresh
/// native session when the provider changed or a new session was requested.
pub(super) fn continue_run(
    db: &Db<'_>,
    chat: &Conversation,
    run: &Value,
    snapshot: &Value,
    mut execution: ChatExecution,
    wakeup_id: Option<&str>,
) -> Result<()> {
    require_workspace_not_cleaned(run)?;
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
    if checkpoint.antigravity_replay_refused(Some(&execution.message_id)) {
        return Err(Error::conflict(crate::antigravity::FRESH_INPUT_MESSAGE));
    }
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
    let new_input_without_antigravity_session = provider == Provider::Antigravity
        && run["sessionId"].is_null()
        && run["chatExecution"]["messageId"]
            .as_str()
            .is_some_and(|previous| {
                previous != execution.message_id
                    && checkpoint.antigravity_replay_refused(Some(previous))
            });
    let restart_requested = expired
        || fresh_environment_without_session
        || unfinished_handoff
        || new_input_without_antigravity_session
        || chat.session_restart_requested
        || checkpoint.fresh_session == Some(true);
    if provider != Provider::of_run(run)? || restart_requested {
        if !expired && (run["sessionId"].is_string() || new_input_without_antigravity_session) {
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
    let migrated_wakeup_id = checkpoint.migrated_mission_wakeup_id.take();
    let resumes_migrated_wait = wakeup_id.is_some() && wakeup_id == migrated_wakeup_id.as_deref();
    if checkpoint.held_environment.is_none() && !resumes_migrated_wait {
        checkpoint.remaining_ms = Some(crate::run_limits::budget_ms(agent));
    }
    checkpoint.last_message = None;
    checkpoint.settled = None;
    checkpoint.retired_environment_deadline = None;
    checkpoint.retained_completion = None;
    checkpoint.store_in(db, run_id)?;
    if wakeup_id.is_none() && chat.wakeup.is_some() {
        // New input ends the parked occurrence before reusing its run.
        super::continuation::stop_for_run(db, run_id)?;
    }
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
            "sourceTaskInitialTurn": null,
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
pub(super) fn resumes_mission_thread(run: &Value) -> bool {
    run["sessionId"].is_string() && run["sessionId"] == run["sourceSessionId"]
}

/// Starts the agent turn for `message` unless the chat or message changed meanwhile.
pub(super) fn launch_turn(
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
            || RunCheckpoint::awaiting_native_retirement_in(db, run_id)?
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
    let message_id = text(&message, "id");
    let input = text(&message, "text");
    let scheduled_brief = run.is_none()
        && crate::rules::engine::conversations::origin_rule_for_chat(db, chat_id)?
            .is_some_and(|origin| origin.schedule)
        && crate::conversation_writers::person(db, chat_id, message_id)?.is_none();
    // A scheduled brief is text for the model. A leading slash otherwise
    // becomes an engine command when it is the first input in Claude Code.
    let input = if scheduled_brief {
        format!("Task brief:\n{input}")
    } else {
        input.to_owned()
    };
    let execution = ChatExecution {
        message_id: message_id.to_owned(),
        text: with_invoked_skills(&input, &snapshot["snapshot"]["skills"]),
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
        continue_run(
            db,
            &chat,
            &run,
            &snapshot,
            execution,
            message["wakeup"]["id"].as_str(),
        )?;
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
    db.delete(&format!("chat-error:{}", chat.id))?;
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
