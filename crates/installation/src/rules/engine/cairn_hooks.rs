//! The hooks Cairn emits for conversations bound to a key (ADR-0075, « Hooks de
//! Cairn »): reply ready, question asked, progress and end.
//!
//! Cairn follows each bound conversation's run through its recorded events. A hook
//! reaches the rules of the key's creator that listen to it, while the creator keeps
//! access to the conversation's agent, as one triggering per rule, pinned to the rule's
//! version like a plugin event. Each hook carries the origin of its turn and the depth
//! of the chain it belongs to, which only Cairn records.
use super::conversations::{MAX_CHAIN_DEPTH, values};
use super::*;
use crate::{run_status::RunStatus, validation::text};
use cairn_contracts::CairnHookKind;

/// About how often a working turn reports a short step.
const PROGRESS_INTERVAL: i64 = 8_000;
/// Boundary events one pass reads per conversation.
const EVENTS: i64 = 200;
/// A reply's text kept in the hook, so that formulas can read it whole.
const MAX_REPLY_BYTES: usize = 128 * 1024;
/// Deliverables reported with one reply.
const MAX_DELIVERABLES: i64 = 10;
/// Question creations loaded in one observation. Receipts, rather than this batch,
/// remember every question already reported for the lifetime of its definition.
const QUESTIONS_PER_PASS: usize = 100;

/// Record creation in the questionnaire's transaction, independently of its later
/// status. The run's event position orders it against turns without a wall clock
/// or an additional run-log/UI event.
pub(crate) fn record_question(db: &Db<'_>, question: &cairn_contracts::ChatQuestion) -> Result<()> {
    db.0.execute(
        "INSERT OR IGNORE INTO hook_rule_question_order(question_id,event_cursor) \
         SELECT ?1,coalesce((SELECT max(id) FROM events WHERE run_id=?2),0) \
         WHERE EXISTS(SELECT 1 FROM hook_rule_conversations WHERE chat_id=?3 AND key IS NOT NULL)",
        params![question.id, question.run_id, question.chat_id],
    )?;
    Ok(())
}

struct AskedQuestion {
    definition: cairn_contracts::ChatQuestion,
    after_event: i64,
}

/// Who wrote a delivered entry, and how deep in a chain it stands.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Input {
    origin: Json,
    depth: u32,
}

/// Where Cairn stands in a bound conversation's current turn.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Watch {
    open: bool,
    /// Admission tracks terminal failures immediately; progress starts only
    /// after delivery, a provider turn start or a real wakeup.
    started_work: bool,
    /// Manual context maintenance is not a reply or a new user turn. Retain the
    /// marker through retries and restarts until real input opens the next turn.
    compacting: bool,
    origin: Json,
    depth: u32,
    /// The event that opened the turn.
    started: i64,
    /// The turn opened at its provider's notification, before its input was delivered.
    /// Codex may notify the turn before Cairn records the message that starts it.
    awaiting_input: bool,
    /// A failure that a retry may still continue.
    failed: Option<i64>,
    error: Option<String>,
    #[serde(skip)]
    asked: Vec<String>,
    progress_at: i64,
    /// The last progress hook, while a rule has yet to handle it.
    progress_event: Option<String>,
}

/// A bound conversation as the watcher reads it.
struct Bound {
    chat: String,
    creator: String,
    key: String,
    agent: String,
    run: Option<String>,
    cursor: i64,
    watch: Watch,
}

/// A hook to emit, with the origin and chain depth of the turn it belongs to.
struct Emission {
    hook: CairnHookKind,
    fields: Json,
    origin: Json,
    depth: u32,
}

struct Advanced {
    run: String,
    cursor: i64,
    watch: Watch,
    emissions: Vec<Emission>,
}

fn load(db: &Db<'_>, chat: &str) -> Result<Option<Bound>> {
    Ok(db
        .0
        .prepare_cached(
            "SELECT chat_id,creator_account_id,key,agent_id,run_id,cursor,watch FROM hook_rule_conversations \
             WHERE chat_id=? AND key IS NOT NULL",
        )?
        .query_row([chat], |row| {
            let watch: String = row.get(6)?;
            Ok(Bound {
                chat: row.get(0)?,
                creator: row.get(1)?,
                key: row.get(2)?,
                agent: row.get(3)?,
                run: row.get(4)?,
                cursor: row.get(5)?,
                watch: serde_json::from_str(&watch).unwrap_or_default(),
            })
        })
        .optional()?)
}

/// A failure's message as the `ended` hook reports it.
fn failure(message: &Json) -> Option<String> {
    message
        .as_str()
        .filter(|message| !message.is_empty())
        .map(|message| message.chars().take(1000).collect())
}

/// Who wrote a delivered message, when Cairn recorded it.
fn recorded_input(db: &Db<'_>, message: &str) -> Result<Option<Input>> {
    Ok(db
        .0
        .prepare_cached("SELECT origin,depth FROM hook_rule_turn_origins WHERE message_id=?")?
        .query_row([message], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
        })
        .optional()?
        .map(|(origin, depth)| Input {
            origin: serde_json::from_str(&origin).unwrap_or(Json::Null),
            depth,
        }))
}

/// Whether some rule of the creator listens to this Cairn hook.
fn listened(db: &Db<'_>, creator: &str, hook: CairnHookKind) -> Result<bool> {
    Ok(!listeners(db, creator, hook)?.is_empty())
}

fn listeners(db: &Db<'_>, creator: &str, hook: CairnHookKind) -> Result<Vec<String>> {
    Ok(db
        .0
        .prepare_cached(
            "SELECT r.id FROM hook_rules r JOIN hook_rule_versions v ON v.rule_id=r.id AND v.version=r.version \
             WHERE r.creator_account_id=?1 AND json_extract(v.definition,'$.trigger.type')='cairn' \
             AND json_extract(v.definition,'$.trigger.hook')=?2 ORDER BY r.id",
        )?
        .query_map(params![creator, hook.key()], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The turn's final answer or proposed approach, and its deliverables. A Plan
/// proposal takes precedence over surrounding commentary. Retries use their last attempt.
fn reply(db: &Db<'_>, run: &str, started: i64, end: i64) -> Result<Json> {
    let answered: i64 = db.0.query_row(
        "SELECT coalesce(max(id),?2) FROM events WHERE run_id=?1 AND type='retry.scheduled' AND id>?2 AND id<?3",
        params![run, started, end],
        |row| row.get(0),
    )?;
    let text: Option<String> = db
        .0
        .query_row(
            "SELECT json_extract(payload,'$.item.text') FROM events WHERE run_id=?1 AND id>?2 AND id<?3 \
             AND json_extract(payload,'$.item.type') IN ('agent_message','proposed_plan') \
             AND coalesce(json_extract(payload,'$.item.parent_id'),'')='' \
             AND json_type(payload,'$.item.text')='text' AND length(json_extract(payload,'$.item.text'))>0 \
             ORDER BY (json_extract(payload,'$.item.type')='proposed_plan') DESC,id DESC LIMIT 1",
            params![run, answered, end],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let mut text = text.unwrap_or_default();
    let truncated = text.len() > MAX_REPLY_BYTES;
    if truncated {
        let mut cut = MAX_REPLY_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    let deliverables = db
        .0
        .prepare_cached(
            "SELECT payload FROM events WHERE run_id=?1 AND type='artifact' AND id>?2 AND id<?3 ORDER BY id LIMIT ?4",
        )?
        .query_map(params![run, started, end, MAX_DELIVERABLES], |row| {
            row.get::<_, String>(0)
        })?
        .filter_map(|payload| {
            payload
                .ok()
                .and_then(|payload| serde_json::from_str::<Json>(&payload).ok())
        })
        .map(|artifact| {
            json!({
                "id": artifact["id"],
                "name": artifact["name"],
                "title": artifact["title"],
                "mediaType": artifact["mediaType"],
                "size": artifact["size"],
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "text": text, "truncated": truncated, "deliverables": deliverables }))
}

/// A short step, without detail, as the plugin delegation reported it.
fn step(db: &Db<'_>, run: &str, started: i64) -> Result<String> {
    let latest: Option<String> = db
        .0
        .query_row(
            "SELECT payload FROM events WHERE run_id=?1 AND id>?2 AND type IN ('item.started','item.completed') \
             ORDER BY id DESC LIMIT 1",
            params![run, started],
            |row| row.get(0),
        )
        .optional()?;
    let item = latest
        .and_then(|payload| serde_json::from_str::<Json>(&payload).ok())
        .unwrap_or(Json::Null);
    Ok(match item["item"]["type"].as_str() {
        Some("command_execution") => "Running a command",
        Some("file_change") => "Editing files",
        Some("web_search") => "Searching the web",
        Some("mcp_tool_call" | "collab_tool_call") => "Using a tool",
        Some("todo_list") => "Planning",
        Some("agent_message") => "Writing the answer",
        _ => "Thinking",
    }
    .into())
}

fn question(question: &cairn_contracts::ChatQuestion) -> Json {
    let fields: Vec<Json> = question
        .fields
        .iter()
        .map(|field| {
            json!({
                "id": field.id,
                "title": field.title,
                "mode": field.mode,
                "options": field.options.iter().map(|option| json!({ "id": option.id, "label": option.label })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let text = question
        .fields
        .iter()
        .map(|field| {
            let options = field
                .options
                .iter()
                .map(|option| format!("- {}", option.label))
                .collect::<Vec<_>>();
            if options.is_empty() {
                field.title.clone()
            } else {
                format!("{}\n{}", field.title, options.join("\n"))
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    json!({
        "questionId": question.id,
        "title": question.fields.first().map(|field| field.title.clone()).unwrap_or_default(),
        "text": text,
        "fields": fields,
    })
}

fn progress_pending(db: &Db<'_>, event: Option<&str>) -> Result<bool> {
    let Some(event) = event else {
        return Ok(false);
    };
    Ok(db.0.query_row(
        "SELECT EXISTS(SELECT 1 FROM hook_rule_triggerings WHERE event_id=?1 AND status IN ('running','waiting','retrying')) \
         OR EXISTS(SELECT 1 FROM hook_rule_admissions WHERE event_id=?1)",
        [event],
        |row| row.get(0),
    )?)
}

impl Watch {
    fn emit(&self, emissions: &mut Vec<Emission>, hook: CairnHookKind, fields: Json) {
        emissions.push(Emission {
            hook,
            fields,
            origin: self.origin.clone(),
            depth: self.depth,
        });
    }

    /// Opens a turn with its input, or as a resumption of the chain it belongs to.
    fn open(&mut self, event: i64, at: i64, input: Option<Input>) {
        self.awaiting_input = input.is_none();
        let input = input.unwrap_or_else(|| Input {
            origin: json!({ "kind": "resume" }),
            depth: self.depth,
        });
        self.open = true;
        self.started_work = true;
        self.origin = input.origin;
        self.depth = input.depth;
        self.started = event;
        self.failed = None;
        self.error = None;
        self.progress_at = at;
    }

    fn begin_work(&mut self, at: i64) {
        if !self.started_work {
            self.progress_at = at;
        }
        self.started_work = true;
    }

    /// Delivered input starts a turn, or joins the working one. The first input of a
    /// turn opened before its delivery is the one that started it.
    fn deliver(&mut self, event: i64, at: i64, input: Input, emissions: &mut Vec<Emission>) {
        if self.open && self.failed.is_some() {
            // Writing again after a failure no retry continued starts a new turn.
            self.end(emissions, "failed");
        }
        if !self.open {
            self.open(event, at, Some(input));
        } else if self.awaiting_input {
            self.awaiting_input = false;
            self.origin = input.origin;
            self.depth = input.depth;
        } else {
            // Input that joins a working turn extends its chain.
            self.depth = self.depth.max(input.depth);
        }
        self.begin_work(at);
    }

    /// Reports creations before the next event boundary, even if the question
    /// has since been answered or withdrawn.
    fn ask(
        &mut self,
        emissions: &mut Vec<Emission>,
        questions: &[AskedQuestion],
        until: Option<i64>,
    ) {
        for question in questions {
            if self.asked.contains(&question.definition.id)
                || until.is_some_and(|until| question.after_event >= until)
            {
                continue;
            }
            self.emit(
                emissions,
                CairnHookKind::QuestionAsked,
                self::question(&question.definition),
            );
            self.asked.push(question.definition.id.clone());
        }
    }

    fn end(&mut self, emissions: &mut Vec<Emission>, outcome: &str) {
        let mut fields = json!({ "outcome": outcome });
        if outcome == "failed"
            && let Some(error) = &self.error
        {
            fields["error"] = error.clone().into();
        }
        self.emit(emissions, CairnHookKind::Ended, fields);
        self.open = false;
        self.started_work = false;
        self.awaiting_input = false;
        self.failed = None;
        self.error = None;
    }
}

/// One transactionally ordered page of question creations not yet reported.
fn pending_questions(
    db: &Db<'_>,
    chat: &str,
    run: &str,
    at: i64,
    until: Option<i64>,
    offset: usize,
    limit: usize,
) -> Result<Vec<(String, i64)>> {
    Ok(db.0.prepare_cached(
        "SELECT q.id,o.event_cursor FROM hook_rule_question_order o JOIN chat_questions q ON q.id=o.question_id \
         WHERE q.chat_id=?1 AND q.run_id=?2 \
         AND NOT EXISTS(SELECT 1 FROM question_import_metadata m WHERE m.question_id=q.id AND m.expires<=?3) \
         AND NOT EXISTS(SELECT 1 FROM hook_rule_reported_questions r WHERE r.question_id=q.id) \
         AND (?4 IS NULL OR o.event_cursor<?4) \
         ORDER BY o.event_cursor,q.id LIMIT ?6 OFFSET ?5",
    )?.query_map(params![chat,run,at,until,offset as i64,limit as i64], |row|Ok((row.get(0)?,row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Reads what happened in a bound conversation since the last pass. Pure over the
/// database, so the write transaction computes it again before applying it.
fn advance(db: &Db<'_>, chat: &str, at: i64) -> Result<Option<Advanced>> {
    let Some(bound) = load(db, chat)? else {
        return Ok(None);
    };
    let Some(conversation) = db.conversation(chat)? else {
        return Ok(None);
    };
    if conversation.lifecycle != cairn_contracts::ConversationLifecycle::Active
        || conversation.agent_id != bound.agent
    {
        return Ok(None);
    }
    let Some(run) = conversation.run_id.clone() else {
        return Ok(None);
    };
    let same_run = bound.run.as_deref() == Some(run.as_str());
    let mut watch = if same_run {
        bound.watch.clone()
    } else {
        // Another run starts afresh, in the chain the conversation stands in.
        Watch {
            depth: bound.watch.depth,
            ..Watch::default()
        }
    };
    let mut cursor = if same_run { bound.cursor } else { 0 };
    let mut emissions = Vec::new();
    let mut questions = Vec::new();
    for (id, after_event) in pending_questions(db, chat, &run, at, None, 0, QUESTIONS_PER_PASS)? {
        if let Some(question) = db.question(chat, &id)? {
            questions.push(AskedQuestion {
                definition: question,
                after_event,
            });
        }
    }

    // Delivered input and wakeups open turns whatever the provider notifies: Claude's
    // adapter reports no turn start.
    let events = db
        .0
        .prepare_cached(
            "SELECT id,type,text,payload FROM events WHERE run_id=?1 AND id>?2 AND (type IN \
             ('chat.user','chat.wakeup','turn.started','turn.completed','turn.failed','retry.scheduled') \
             OR (type='status' AND (text IN ('failed','cancelled','interrupted') \
             OR json_extract(payload,'$.chatOperation')='compact' \
             OR json_type(payload,'$.hookRuleTurn')='object' \
             OR json_type(payload,'$.hookRuleDepth')='integer'))) ORDER BY id LIMIT ?3",
        )?
        .query_map(params![run, cursor, EVENTS], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut complete = (events.len() as i64) < EVENTS;
    for (id, kind, text, payload) in events {
        let payload: Json = payload
            .and_then(|payload| serde_json::from_str(&payload).ok())
            .unwrap_or(Json::Null);
        // Drain older pages before any boundary, including a new user input.
        watch.ask(&mut emissions, &questions, Some(id));
        let included = questions.iter().filter(|q| q.after_event < id).count();
        // The page is ordered: a later page cannot precede this boundary
        // unless every element of this full page precedes it already.
        if included == QUESTIONS_PER_PASS
            && !pending_questions(db, chat, &run, at, Some(id), included, 1)?.is_empty()
        {
            complete = false;
            break;
        }
        if kind == "status" && payload["chatOperation"] == crate::chats::COMPACT {
            watch.compacting = true;
            watch.open = false;
            watch.started_work = false;
            watch.awaiting_input = false;
            watch.failed = None;
        }
        if watch.compacting {
            if matches!(kind.as_str(), "chat.user" | "chat.wakeup")
                || kind == "status" && payload["hookRuleTurn"].is_object()
            {
                watch.compacting = false;
            } else {
                cursor = id;
                continue;
            }
        }
        match kind.as_str() {
            "status" if payload["hookRuleTurn"].is_object() => {
                if watch.open {
                    let previous = &payload["hookRulePreviousTurn"];
                    let outcome = match previous["status"].as_str() {
                        Some("failed" | "interrupted") => Some("failed"),
                        Some("cancelled") => Some("cancelled"),
                        Some("succeeded") => Some("completed"),
                        _ => None,
                    };
                    if let Some(outcome) = outcome {
                        if outcome == "completed" {
                            watch.emit(
                                &mut emissions,
                                CairnHookKind::ReplyReady,
                                reply(db, &run, watch.started, id)?,
                            );
                        } else if outcome == "failed" {
                            watch.error = failure(&previous["error"]);
                        }
                        watch.end(&mut emissions, outcome);
                    }
                }
                let input: Input = serde_json::from_value(payload["hookRuleTurn"].clone())?;
                if input.depth > MAX_CHAIN_DEPTH {
                    return Err(Error::bad("Invalid admitted turn depth."));
                }
                watch.open(id, at, Some(input));
                watch.started_work = false;
            }
            "status" if payload["hookRuleDepth"].is_number() => {
                if let Some(depth) = payload["hookRuleDepth"]
                    .as_u64()
                    .and_then(|depth| u32::try_from(depth).ok())
                    .filter(|depth| *depth <= MAX_CHAIN_DEPTH)
                {
                    watch.depth = watch.depth.max(depth);
                }
            }
            "chat.user" => {
                let message = payload["messageId"].as_str().unwrap_or_default();
                let input = recorded_input(db, message)?.unwrap_or_else(|| Input {
                    // Input Cairn did not attribute stays in the conversation's chain.
                    origin: json!({ "kind": "unknown" }),
                    depth: watch.depth,
                });
                watch.deliver(id, at, input, &mut emissions);
            }
            "chat.wakeup" => {
                if watch.failed.is_some() {
                    watch.end(&mut emissions, "failed");
                }
                if !watch.open {
                    // A wakeup resumes the chain without new input.
                    let resume = Input {
                        origin: json!({ "kind": "resume" }),
                        depth: watch.depth,
                    };
                    watch.open(id, at, Some(resume));
                }
                watch.begin_work(at);
            }
            "turn.started" => {
                if watch.failed.is_some() {
                    watch.end(&mut emissions, "failed");
                }
                if !watch.open {
                    watch.open(id, at, None);
                }
                watch.begin_work(at);
            }
            "retry.scheduled" => {
                watch.failed = None;
                watch.error = None;
            }
            "turn.failed" if watch.open => {
                watch.failed = Some(id);
                watch.error =
                    failure(&payload["error"]["message"]).or_else(|| failure(&text.into()));
            }
            "turn.completed" if watch.open => {
                let reply = reply(db, &run, watch.started, id)?;
                watch.emit(&mut emissions, CairnHookKind::ReplyReady, reply);
                watch.end(&mut emissions, "completed");
            }
            "status" if watch.open || watch.failed.is_some() => {
                let outcome = if text == "cancelled" {
                    "cancelled"
                } else {
                    // The terminal cause overrides a transient provider error.
                    // Keep it on the event so replay cannot borrow a later turn's error.
                    watch.error = failure(&payload["error"]["message"]).or(watch.error.take());
                    "failed"
                };
                watch.end(&mut emissions, outcome);
            }
            _ => {}
        }
        cursor = id;
    }
    if complete {
        // Later events are not boundaries: the next pass need not read them again.
        let last: i64 = db.0.query_row(
            "SELECT coalesce(max(id),?2) FROM events WHERE run_id=?1",
            params![run, cursor],
            |row| row.get(0),
        )?;
        cursor = cursor.max(last);
    }

    // Once every event is read and the run no longer works, a turn still open ends as
    // its run did: a failure no retry continued, or a run stopped or failed without its
    // turn reporting it, such as a queued wakeup cancelled before it started. A run that
    // succeeded still reports its turn's end.
    let state = db.run(&run)?.unwrap_or(Json::Null);
    let status = RunStatus::of(&state);
    let working = status.is_some_and(RunStatus::is_active) || state["recoveryPending"] == true;
    if complete && watch.open && !working {
        let outcome = match status {
            Some(RunStatus::Cancelled) => Some("cancelled"),
            Some(RunStatus::Failed | RunStatus::Interrupted) => Some("failed"),
            _ if watch.failed.is_some() => Some("failed"),
            _ => None,
        };
        if let Some(outcome) = outcome {
            watch.ask(&mut emissions, &questions, None);
            if outcome == "failed"
                && matches!(status, Some(RunStatus::Failed | RunStatus::Interrupted))
            {
                watch.error = failure(&state["error"]).or(watch.error.take());
            }
            watch.end(&mut emissions, outcome);
        }
    }

    // Creation remains reportable after the question or its turn ended.
    watch.ask(
        &mut emissions,
        &questions,
        (!complete).then_some(cursor.saturating_add(1)),
    );
    if watch.open
        && watch.started_work
        && working
        && at - watch.progress_at >= PROGRESS_INTERVAL
        && listened(db, &bound.creator, CairnHookKind::Progress)?
        && !progress_pending(db, watch.progress_event.as_deref())?
    {
        let step = json!({ "step": step(db, &run, watch.started)? });
        watch.emit(&mut emissions, CairnHookKind::Progress, step);
        watch.progress_at = at;
    }

    if same_run && cursor == bound.cursor && watch == bound.watch && emissions.is_empty() {
        return Ok(None);
    }
    Ok(Some(Advanced {
        run,
        cursor,
        watch,
        emissions,
    }))
}

fn payload_fields(hook: CairnHookKind) -> Vec<String> {
    super::super::catalog::cairn_payload(hook)["properties"]
        .as_object()
        .map(|fields| fields.keys().cloned().collect())
        .unwrap_or_default()
}

/// Copies one Cairn hook into a triggering for every rule of the key's creator that
/// listens to it and may receive it. Returns the event when some rule received it.
fn fanout(
    db: &Db<'_>,
    bound: &Bound,
    hook: CairnHookKind,
    payload: &Json,
    depth: u32,
) -> Result<Option<String>> {
    let rules = listeners(db, &bound.creator, hook)?;
    if rules.is_empty() || depth > MAX_CHAIN_DEPTH {
        return Ok(None);
    }
    let event = id();
    let record = json!({
        "id": event,
        "hook": hook.key(),
        "instanceId": null,
        "payload": payload,
        "receivedAt": now(),
        "payloadFields": payload_fields(hook),
        "cairn": { "conversationId": bound.chat, "depth": depth },
    });
    let recorded = record.to_string();
    let recorded_bytes = recorded.len() + 256;
    // A hook over the recorded-data budget, such as a long escaped reply with large
    // remembered values, is refused whole: each rule that would receive it counts it as
    // not admitted, without its payload.
    let oversized = recorded_bytes > MAX_RECORDED_BYTES - ERROR_RESERVE;
    if oversized {
        tracing::warn!(chat = %bound.chat, hook = hook.key(), "A Cairn hook exceeds the recorded-data budget");
    }
    let mut delivered = false;
    for rule in rules {
        let listener = match eligible_listener(db, &rule) {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(rule = %rule, error = %error, "Rule listener is unavailable");
                continue;
            }
        };
        let Some((row, version)) = listener else {
            continue;
        };
        // A rule receives the hooks of its creator's keys while they keep access to
        // the conversation's agent.
        let creator = super::super::validation::Creator {
            account: row.creator_account_id.clone(),
            owner: row.creator_owner,
        };
        if !creator.uses_agent(db, &bound.agent)? {
            continue;
        }
        if oversized {
            db.record_admission_refusal(&rule, &event)?;
            continue;
        }
        if db.refuse_admission(&rule, &event, recorded.len())? {
            continue;
        }
        db.0.execute(
            "INSERT OR IGNORE INTO hook_rule_receipts(rule_id,event_id) VALUES(?1,?2)",
            params![rule, event],
        )?;
        db.0.prepare_cached(
            "INSERT OR IGNORE INTO hook_rule_triggerings( \
             id,rule_id,version,event_id,event,status,cursor,pending,in_flight, \
             actions,tool_calls,counted_at,verify,verify_since,next_at,lease_until, \
             error,created_at,updated_at,finished_at,recorded_bytes) \
             VALUES(?1,?2,?3,?4,?5,'running',?6,NULL,0,0,0,NULL,0,NULL,NULL,NULL,NULL,?7,?7,NULL,?8)",
        )?
        .execute(params![
            id(),
            rule,
            version.version,
            event,
            recorded,
            serde_json::to_string(&Cursor::default())?,
            now(),
            recorded_bytes as i64
        ])?;
        delivered = true;
    }
    Ok(delivered.then_some(event))
}

fn apply(db: &Db<'_>, chat: &str, at: i64) -> Result<bool> {
    let Some(Advanced {
        run,
        cursor,
        mut watch,
        emissions,
    }) = advance(db, chat, at)?
    else {
        return Ok(false);
    };
    let bound = required(load(db, chat)?, "Bound conversation not found")?;
    let remembered = values(db, chat)?;
    let mut delivered = false;
    for Emission {
        hook,
        fields,
        origin,
        depth,
    } in emissions
    {
        let mut payload = json!({
            "key": bound.key,
            "conversationId": bound.chat,
            "agentId": bound.agent,
            "origin": origin,
            "values": remembered,
        });
        for (name, value) in fields.as_object().into_iter().flatten() {
            payload[name] = value.clone();
        }
        let event = fanout(db, &bound, hook, &payload, depth)?;
        if hook == CairnHookKind::QuestionAsked {
            db.0.execute(
                "INSERT OR IGNORE INTO hook_rule_reported_questions(question_id) VALUES(?)",
                [text(&fields, "questionId")],
            )?;
        }
        if hook == CairnHookKind::Progress {
            watch.progress_event.clone_from(&event);
        }
        delivered |= event.is_some();
    }
    db.0.execute(
        "UPDATE hook_rule_conversations SET run_id=?2,cursor=?3,watch=?4 WHERE chat_id=?1",
        params![chat, run, cursor, serde_json::to_string(&watch)?],
    )?;
    Ok(delivered)
}

/// Follows every bound conversation with something new, and emits its hooks. Reads
/// first, so that an idle installation never takes the writer.
pub(super) async fn observe(s: &Service) -> Result<bool> {
    let at = now();
    let candidates = s
        .store
        .read(move |db| {
            let chats = db
                .0
                .prepare_cached(
                    "SELECT k.chat_id FROM hook_rule_conversations k JOIN conversations c ON c.id=k.chat_id \
                     WHERE k.key IS NOT NULL AND c.lifecycle='active' AND c.run_id IS NOT NULL \
                     AND c.agent_id=k.agent_id AND (k.run_id IS NOT c.run_id \
                     OR json_extract(k.watch,'$.open')=1 OR json_extract(k.watch,'$.failed') IS NOT NULL \
                     OR EXISTS(SELECT 1 FROM events e WHERE e.run_id=c.run_id AND e.id>k.cursor) \
                     OR EXISTS(SELECT 1 FROM hook_rule_question_order o JOIN chat_questions q ON q.id=o.question_id \
                       WHERE q.chat_id=k.chat_id AND q.run_id=c.run_id \
                       AND NOT EXISTS(SELECT 1 FROM question_import_metadata m \
                         WHERE m.question_id=q.id AND m.expires<=?1) \
                       AND NOT EXISTS(SELECT 1 FROM hook_rule_reported_questions r WHERE r.question_id=q.id))) \
                     ORDER BY k.chat_id",
                )?
                .query_map([at], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut due = Vec::new();
            for chat in chats {
                if advance(db, &chat, at)?.is_some() {
                    due.push(chat);
                }
            }
            Ok(due)
        })
        .await?;
    let mut delivered = false;
    for chat in candidates {
        let applied = s
            .store
            .transaction({
                let chat = chat.clone();
                move |db| apply(db, &chat, now())
            })
            .await;
        match applied {
            Ok(applied) => delivered |= applied,
            Err(error) => {
                tracing::warn!(chat = %chat, error = %error, "Cairn hooks could not be emitted");
            }
        }
    }
    Ok(delivered)
}
