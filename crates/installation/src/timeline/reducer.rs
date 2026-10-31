//! The pure timeline reducer: one event in, the cards it creates or changes out.
//! It decides what the clients used to fold themselves: successive reply snapshots,
//! one card per failure, Cairn's retries, reconnects and the engine's own retries.
//! State is the minimal checkpoint persisted beside the cards; card text is never
//! kept here, it stays in the source event.
use crate::{error::Result, validation::text};
use cairn_contracts::{
    AccountNotice, EngineRetry, EventKind, FailureOrigin, NoticeCode, QuotaFailure, SpeedNotice,
    TimelineContent, TimelineNotice, TimelineRetry, TimelineState, TokenUsage, UsageLimit,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

/// A stored event, as the writer records it.
#[derive(Clone, Copy)]
pub(crate) struct Event<'a> {
    pub id: i64,
    pub created_at: i64,
    pub kind: &'a EventKind,
    pub text: &'a str,
    pub payload: Option<&'a Value>,
}

static EMPTY: LazyLock<Value> = LazyLock::new(|| Value::Object(serde_json::Map::default()));

impl Event<'_> {
    /// Stderr can itself look like a protocol event; it is always process output.
    fn data(&self) -> &Value {
        match (self.kind, self.payload) {
            (EventKind::Diagnostic, _) | (_, None) => &EMPTY,
            (_, Some(payload)) => payload,
        }
    }

    fn item(&self) -> &Value {
        &self.data()["item"]
    }

    fn item_event(&self) -> bool {
        matches!(
            self.kind,
            EventKind::ItemStarted | EventKind::ItemUpdated | EventKind::ItemCompleted
        )
    }

    fn status(&self, values: &[&str]) -> bool {
        *self.kind == EventKind::Status && values.contains(&self.text)
    }

    fn attempt_marker(&self) -> bool {
        *self.kind == EventKind::Status && self.data()["attempt"] == true
    }

    /// The event's own data as written, the source a connection pattern is matched against.
    fn raw(&self) -> String {
        let data = self.data();
        if data.as_object().is_some_and(|data| !data.is_empty()) {
            serde_json::to_string_pretty(data).unwrap_or_default()
        } else {
            self.text.to_owned()
        }
    }
}

/// The timeline presents this event: clients render it only through its card.
pub(crate) fn covers(kind: &EventKind, payload: Option<&Value>) -> bool {
    match kind {
        EventKind::ChatUser
        | EventKind::Status
        | EventKind::Diagnostic
        | EventKind::RetryScheduled
        | EventKind::ThreadStarted
        | EventKind::TurnStarted
        | EventKind::TurnCompleted
        | EventKind::TurnFailed
        | EventKind::TurnWaiting
        | EventKind::EngineNotice
        | EventKind::EngineRetry
        | EventKind::ProviderLimit
        | EventKind::RunnerFailed
        | EventKind::Error => true,
        EventKind::ItemStarted | EventKind::ItemUpdated | EventKind::ItemCompleted => {
            matches!(
                payload.map(|payload| &payload["item"]["type"]),
                Some(Value::String(kind)) if kind == "agent_message" || kind == "error"
            )
        }
        _ => false,
    }
}

/// A reply snapshot, which a later snapshot of the same reply replaces on the wire.
pub(crate) fn reply_snapshot(kind: &EventKind, payload: Option<&Value>) -> bool {
    matches!(
        kind,
        EventKind::ItemStarted | EventKind::ItemUpdated | EventKind::ItemCompleted
    ) && payload.is_some_and(|payload| payload["item"]["type"] == "agent_message")
}

/// One timeline card as stored. Text and details stay in its source event and
/// are read with it; `detail: Some("")` marks a notice whose detail is read so.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Card {
    pub id: String,
    pub position: i64,
    pub revision: i64,
    pub source: i64,
    pub turn: i64,
    pub attempt: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub content: TimelineContent,
}

impl Card {
    fn notice(&mut self) -> Option<&mut TimelineNotice> {
        match &mut self.content {
            TimelineContent::Notice { notice } => Some(notice.as_mut()),
            _ => None,
        }
    }
}

/// The cards an event may change, beyond the ones it creates.
pub(crate) trait Cards {
    fn card(&mut self, id: &str) -> Result<Option<Card>>;

    fn reason(&mut self, event_id: i64) -> Result<Option<String>>;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reason {
    digest: String,
    source: i64,
}

impl Reason {
    fn new(message: &str, source: i64) -> Self {
        Self {
            digest: crate::auth::hex_digest(message),
            source,
        }
    }
}

fn remember_reason(reasons: &mut Vec<Reason>, reason: Reason) {
    if let Some(existing) = reasons
        .iter_mut()
        .find(|existing| existing.digest == reason.digest)
    {
        existing.source = reason.source;
    } else {
        remember(reasons, reason);
    }
}

/// A failed attempt's reasons, its card and how often Cairn retried its chain.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Failure {
    reasons: Vec<Reason>,
    notice: Option<String>,
    retries: Option<u32>,
    /// Its turn or run failed, beyond a reported error.
    ended: bool,
    /// Cairn scheduled a retry that has not started yet.
    pending: bool,
}

/// The reducer's checkpoint. Replaying from it continues exactly like a full replay.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct State {
    turn: i64,
    attempt: i64,
    /// The provider of the current attempt's replies, when the engine routes among several.
    via: Option<String>,
    /// Replies still streaming.
    replies: Vec<String>,
    /// This turn's reconnect notices, until work continues.
    connections: Vec<String>,
    /// The engine's latest retry, until work continues.
    engine_retries: Vec<String>,
    /// The provider's usage limit, until it no longer applies.
    limits: Vec<String>,
    failure: Option<Failure>,
    /// The previous attempt's failure, while Cairn retries it.
    retried: Option<Failure>,
    /// Reasons the run log already gave in this attempt; a final status does not repeat them.
    reported: Vec<String>,
    /// This turn's short replies and the failures and usage limits they may repeat.
    echoes: Echoes,
    /// The current usage-limit failure, until the turn or execution moves on.
    quota: Option<CurrentQuota>,
}

impl State {
    pub(crate) fn reason_sources(&self) -> Vec<i64> {
        self.failure
            .iter()
            .chain(self.retried.iter())
            .flat_map(|failure| failure.reasons.iter().map(|reason| reason.source))
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Echoes {
    exhausted: bool,
    /// Fingerprints of the usage-limit reports and failure reasons given in this turn.
    causes: Vec<u64>,
    failures: Vec<u64>,
    /// The latest replies short enough to repeat a reason, by card.
    replies: Vec<Reply>,
}

/// What a reply's text needs for echo detection; the text itself stays in its event.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Reply {
    card: String,
    text: u64,
    trimmed: u64,
    usage_limit: bool,
}

impl Reply {
    fn new(card: String, text: &str) -> Self {
        Self {
            card,
            text: fingerprint(text),
            trimmed: fingerprint(text.trim()),
            usage_limit: crate::run_output::exhausted_message(text),
        }
    }
}

impl Echoes {
    /// An agent may also say its failure as a reply, as Claude Code does for API errors.
    fn repeats(&self, reply: &Reply) -> bool {
        (self.exhausted && (reply.usage_limit || self.causes.contains(&reply.text)))
            || self.failures.contains(&reply.trimmed)
    }
}

/// A stable 64-bit FNV-1a fingerprint: checkpoints outlive a toolchain's hasher.
fn fingerprint(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Checkpoint lists keep their latest entries: a turn rarely needs more.
const RECENT: usize = 32;

fn remember<T: PartialEq>(list: &mut Vec<T>, value: T) {
    if !list.contains(&value) {
        list.push(value);
    }
    if list.len() > RECENT {
        list.remove(0);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct CurrentQuota {
    card: String,
    digest: String,
    resets_at: Option<i64>,
}

const TERMINAL: &[&str] = &["succeeded", "failed", "cancelled", "interrupted"];

// Adapters report a stop that Cairn requested as a failed turn.
const STOP_ECHOES: &[&str] = &[
    "Conversation stopped.",
    "Claude execution stopped.",
    "OpenCode execution stopped.",
];

const INTERRUPTED: &str = "Conversation interrupted.";
/// Reasons are compared, never shown, from the state; bound what a checkpoint keeps.
const REASON_BYTES: usize = 4096;

static CONNECTION_SOURCE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)websocket|reconnecting\.\.\.|reconnecting\s+\d+/").unwrap()
});

static CONNECTION_FAILURE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)503|reconnect|falling back|connection.*(?:closed|failed)").unwrap()
});

static ACCOUNT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^Using (Codex|Claude Code) account: (.+)$").unwrap());

/// A connection the provider is restoring, as the engine or its stderr reports it.
fn connection_text(raw: &str) -> bool {
    CONNECTION_SOURCE.is_match(raw) && CONNECTION_FAILURE.is_match(raw)
}

/// The reason a failure event gives, or None when the event reports no failure.
pub(crate) fn failure_reason(event: &Event<'_>) -> Option<String> {
    let data = event.data();
    let failure = matches!(event.kind, EventKind::TurnFailed | EventKind::RunnerFailed)
        || (*event.kind == EventKind::Error && data["willRetry"] != true)
        || event.status(&["failed"]);
    if !failure {
        return None;
    }
    let reason = [text(&data["error"], "message"), text(data, "message")]
        .into_iter()
        .find(|reason| !reason.is_empty())
        .unwrap_or(if *event.kind == EventKind::Status {
            ""
        } else {
            event.text
        });
    Some(reason.to_owned())
}

/// The engine's report that the account's usage limit stopped the turn.
pub(crate) fn quota_failure(event: &Event<'_>) -> Option<String> {
    if !matches!(event.kind, EventKind::TurnFailed | EventKind::Error) {
        return None;
    }
    let data = event.data();
    if data["willRetry"] == true {
        return None;
    }
    let error = if *event.kind == EventKind::TurnFailed {
        &data["error"]
    } else {
        data
    };
    let message = match text(error, "message") {
        "" => event.text,
        message => message,
    };
    let exhausted = error["code"] == "usage_limit_reached"
        || error["codexErrorInfo"] == "usageLimitExceeded"
        || crate::run_output::exhausted_message(message);
    exhausted.then(|| {
        if message.is_empty() {
            "Usage limit reached".to_owned()
        } else {
            message.to_owned()
        }
    })
}

/// Whether a reason repeats one already given; a run's recorded error may be truncated.
fn same_reason(given: &str, reason: &str) -> bool {
    given == reason || (!reason.is_empty() && given.starts_with(reason))
}

fn bounded(reason: &str) -> String {
    let mut end = reason.len().min(REASON_BYTES);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_owned()
}

fn count(value: &Value) -> Option<u32> {
    value.as_u64().and_then(|value| u32::try_from(value).ok())
}

/// The detail a notice reads from its source event: the work's own words.
pub(crate) fn notice_detail(event: &Event<'_>, code: NoticeCode) -> Option<String> {
    let data = event.data();
    let detail = match code {
        NoticeCode::Failure => failure_reason(event).unwrap_or_default(),
        NoticeCode::Error | NoticeCode::Connection => [
            text(event.item(), "message"),
            text(data, "message"),
            text(&data["error"], "message"),
        ]
        .into_iter()
        .find(|message| !message.is_empty())
        .unwrap_or(event.text)
        .to_owned(),
        NoticeCode::Diagnostic => event.text.to_owned(),
        NoticeCode::EngineNotice => text(data, "message").to_owned(),
        NoticeCode::RunStatus => text(&data["error"], "message").to_owned(),
        _ => String::new(),
    };
    (!detail.is_empty()).then_some(detail)
}

/// Source-owned notice fields, reconstructed identically during replay and hydration.
pub(crate) fn source_notice_fields(notice: &mut TimelineNotice, event: &Event<'_>) {
    let data = event.data();
    if notice.code == NoticeCode::BackgroundTasks {
        notice.tasks = data["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|task| text(task, "description").trim().to_owned())
            .filter(|task| !task.is_empty())
            .collect();
    }
    if let Some(account) = &mut notice.account
        && let Some(found) = ACCOUNT.captures(event.text)
    {
        account.agent = found[1].to_owned();
        account.name = found[2].trim().to_owned();
    }
    if let Some(retry) = &mut notice.engine_retry
        && retry.reason.is_some()
    {
        retry.reason = data["reason"].as_str().map(str::to_owned);
    }
    if let Some(limit) = &mut notice.usage_limit {
        limit.status = text(data, "status").to_owned();
        if limit.window.is_some() {
            limit.window = data["window"].as_str().map(str::to_owned);
        }
    }
    if notice.level.is_some() {
        notice.level = data["level"].as_str().map(str::to_owned);
    }
}

/// The text a reply or message reads from its source event.
pub(crate) fn message_text(event: &Event<'_>) -> String {
    match event.kind {
        EventKind::ChatUser if event.text == "Answered a private question." => {
            event.text.to_owned()
        }
        EventKind::ChatUser => event
            .payload
            .and_then(|payload| payload["text"].as_str())
            .unwrap_or(event.text)
            .to_owned(),
        _ => {
            let reply = text(event.item(), "text");
            if reply.is_empty() { event.text } else { reply }.to_owned()
        }
    }
}

pub(crate) fn reply_id(attempt: i64, turn: i64, event: i64, item_id: &str) -> String {
    if item_id.is_empty() {
        return format!("message:{event}");
    }
    let plain = format!("message:{attempt}:{turn}:{item_id}");
    if plain.chars().count() <= 1024 {
        plain
    } else {
        // A distinct namespace prevents a long ID from colliding with a short ID.
        format!(
            "message-hash:{attempt}:{turn}:{}",
            crate::auth::hex_digest(item_id)
        )
    }
}

struct Reducer<'a, C> {
    state: &'a mut State,
    cards: C,
    /// Cards this event touched, with their stored version when it existed.
    changed: Vec<(Option<Card>, Card)>,
    event: Event<'a>,
}

/// Applies one event and returns every card it created or changed, in creation order.
/// Each returned card's revision is this event.
pub(crate) fn reduce(state: &mut State, event: Event<'_>, cards: impl Cards) -> Result<Vec<Card>> {
    let mut reducer = Reducer {
        state,
        cards,
        changed: Vec::new(),
        event,
    };
    reducer.apply()?;
    Ok(reducer
        .changed
        .into_iter()
        .filter(|(stored, card)| stored.as_ref() != Some(card))
        .map(|(_, mut card)| {
            card.revision = event.id;
            card.updated_at = event.created_at;
            card
        })
        .collect())
}

impl<C: Cards> Reducer<'_, C> {
    fn card(&mut self, id: &str) -> Result<Option<&mut Card>> {
        if let Some(index) = self.changed.iter().position(|(_, card)| card.id == id) {
            return Ok(Some(&mut self.changed[index].1));
        }
        let Some(card) = self.cards.card(id)? else {
            return Ok(None);
        };
        self.changed.push((Some(card.clone()), card));
        Ok(self.changed.last_mut().map(|(_, card)| card))
    }

    /// Changes a notice this event did not create; the change advances its revision.
    fn update(&mut self, id: &str, change: impl FnOnce(&mut TimelineNotice)) -> Result<()> {
        if let Some(notice) = self.card(id)?.and_then(Card::notice) {
            change(notice);
        }
        Ok(())
    }

    fn hide(&mut self, ids: Vec<String>) -> Result<()> {
        for id in ids {
            self.update(&id, |notice| notice.prominent = false)?;
        }
        Ok(())
    }

    fn create(&mut self, id: String, content: TimelineContent) -> &mut Card {
        let Event {
            id: event,
            created_at,
            ..
        } = self.event;
        self.changed.push((
            None,
            Card {
                id,
                position: event,
                revision: event,
                source: event,
                turn: self.state.turn,
                attempt: self.state.attempt,
                created_at,
                updated_at: created_at,
                content,
            },
        ));
        &mut self.changed.last_mut().unwrap().1
    }

    /// Work continued after this turn's reconnects.
    fn recovered(&mut self) -> Result<()> {
        for id in std::mem::take(&mut self.state.connections) {
            self.update(&id, |notice| {
                notice.state = TimelineState::Info;
                notice.recovered = true;
                notice.prominent = false;
            })?;
        }
        Ok(())
    }

    fn retries_recovered(&mut self) -> Result<()> {
        for id in std::mem::take(&mut self.state.engine_retries) {
            self.update(&id, |notice| {
                notice.recovered = true;
                notice.prominent = false;
            })?;
        }
        Ok(())
    }

    /// A reply that stopped streaming, finished or not, stays as written.
    fn settle_replies(&mut self) -> Result<()> {
        for id in std::mem::take(&mut self.state.replies) {
            if let Some(card) = self.card(&id)?
                && let TimelineContent::AgentMessage { state, .. } = &mut card.content
            {
                *state = TimelineState::Done;
            }
        }
        Ok(())
    }

    /// The engine's retries and usage limit belong to the execution that reported them.
    fn end_engine_reports(&mut self) -> Result<()> {
        let ended = [
            std::mem::take(&mut self.state.engine_retries),
            std::mem::take(&mut self.state.limits),
        ]
        .concat();
        self.hide(ended)
    }

    fn apply(&mut self) -> Result<()> {
        let event = self.event;
        let kind = event.kind;
        let data = event.data();
        let item = event.item();
        let item_type = text(item, "type");

        if matches!(kind, EventKind::TurnCompleted | EventKind::ChatUser) {
            self.state.retried = None;
        }
        if matches!(kind, EventKind::TurnStarted | EventKind::ChatUser) || event.attempt_marker() {
            self.state.echoes = Echoes::default();
        }
        let moves_on = matches!(
            kind,
            EventKind::ChatUser | EventKind::TurnStarted | EventKind::TurnCompleted
        ) || event.status(&["queued", "succeeded", "cancelled", "interrupted"]);
        if moves_on && let Some(current) = self.state.quota.take() {
            self.update(&current.card, |notice| {
                if let Some(quota) = &mut notice.quota {
                    quota.current = false;
                }
            })?;
        }
        if *kind == EventKind::ChatUser {
            // A new request after a failed turn gets its own card, even without an attempt marker.
            if self
                .state
                .failure
                .as_ref()
                .is_some_and(|failure| failure.ended && !failure.pending)
            {
                self.state.failure = None;
            }
            self.create(
                format!("user:{}", event.id),
                TimelineContent::UserMessage {
                    text: String::new(),
                    message_id: None,
                    question_id: None,
                    sent_at: 0,
                    attachments: Vec::new(),
                },
            );
            return Ok(());
        }

        // An attempt may name the provider of its replies; the next attempt ends that label.
        if event.attempt_marker() {
            self.state.via = None;
            self.state.attempt += 1;
        }
        let provider = text(&data["route"], "providerName");
        if *kind == EventKind::Status && !provider.is_empty() {
            self.state.via = Some(provider.to_owned());
        }

        let ended = matches!(
            kind,
            EventKind::TurnCompleted | EventKind::TurnFailed | EventKind::RunnerFailed
        ) || event.status(TERMINAL);
        if ended || *kind == EventKind::TurnStarted || event.attempt_marker() {
            self.settle_replies()?;
        }
        if *kind == EventKind::TurnStarted {
            // Item IDs may repeat in a later turn: a new turn's replies start afresh.
            self.state.connections.clear();
            self.state.engine_retries.clear();
            self.state.limits.clear();
            self.state.turn += 1;
        }

        let reason = failure_reason(&event);
        if *kind == EventKind::TurnFailed {
            // The failure says why the turn ended; its reconnect attempts add nothing.
            let connections = std::mem::take(&mut self.state.connections);
            if reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty() && reason != INTERRUPTED)
            {
                self.hide(connections)?;
            }
            // A limit that stopped the turn shows as the conversation's usage banner.
            self.end_engine_reports()?;
        }
        if *kind == EventKind::RunnerFailed || event.status(TERMINAL) || event.attempt_marker() {
            self.end_engine_reports()?;
        }

        // A run that ends, or is stopped while it waits, ends its retry chain.
        if event.status(TERMINAL) {
            self.state.retried = None;
            if let Some(failure) = &mut self.state.failure {
                let pending = std::mem::take(&mut failure.pending);
                // The announced retry will not run: the card reports the failure again.
                if pending && let Some(id) = failure.notice.clone() {
                    self.update(&id, |notice| {
                        notice.retry = None;
                        notice.state = TimelineState::Error;
                    })?;
                }
            }
        }
        if *kind == EventKind::TurnStarted || event.attempt_marker() {
            if let Some(previous) = self.state.failure.take().filter(|f| f.pending) {
                self.state.retried = Some(previous);
            }
            self.state.reported.clear();
        }

        let error_item = event.item_event() && item_type == "error";
        let connection_source =
            matches!(kind, EventKind::Error | EventKind::Diagnostic) || error_item;
        // One repeating the attempt's failure, such as the final error after the last retry,
        // folds into it rather than reporting a reconnect.
        let repeats_failure = *kind == EventKind::Error
            && self.state.failure.as_ref().is_some_and(|failure| {
                let digest =
                    crate::auth::hex_digest(&bounded(reason.as_deref().unwrap_or_default()));
                failure.reasons.iter().any(|reason| reason.digest == digest)
            });
        let connection = connection_source && !repeats_failure && connection_text(&event.raw());
        let failure_message = (!connection).then_some(reason.clone()).flatten();

        if matches!(kind, EventKind::TurnCompleted)
            || (item_type == "command_execution"
                && *kind == EventKind::ItemCompleted
                && item["exit_code"] == 0
                && item["status"] != "failed")
        {
            self.recovered()?;
        }
        // Work continues once the agent's own reply streams or its tools start. Claude Code
        // reports a request it gave up on as a completed message, which the turn's failure
        // follows; a subagent's progress, or its resumption, says nothing about the request.
        let own_work = text(item, "parent_id").is_empty()
            && ((*kind == EventKind::ItemStarted
                && !(item_type == "subagent" && item["resumed"] == true))
                || (*kind == EventKind::ItemUpdated && item_type != "subagent"));
        if *kind == EventKind::TurnCompleted || own_work {
            self.retries_recovered()?;
        }

        if event.item_event() && item_type == "agent_message" {
            self.recovered()?;
            return self.reply(item);
        }
        if !covers(kind, event.payload) {
            return Ok(());
        }

        let mut notice = self.notice(&event, connection);
        let id = format!("notice:{}", event.id);

        // The run log keeps every lifecycle event; a conversation shows one card per failure.
        if let Some(message) = &failure_message {
            let message = bounded(message);
            let digest = crate::auth::hex_digest(&message);
            let stopped = *kind != EventKind::Status && STOP_ECHOES.contains(&message.as_str());
            let shown = self.state.failure.as_ref();
            let mut repeated = shown.is_some_and(|shown| {
                message.is_empty()
                    || shown.reasons.iter().any(|reason| reason.digest == digest)
                    || (message == INTERRUPTED && shown.ended)
            });
            if !repeated && *kind == EventKind::Status && !message.is_empty() {
                let sources = shown
                    .map(|shown| {
                        shown
                            .reasons
                            .iter()
                            .map(|reason| reason.source)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for source in sources {
                    if self
                        .cards
                        .reason(source)?
                        .is_some_and(|given| same_reason(&bounded(&given), &message))
                    {
                        repeated = true;
                        break;
                    }
                }
            }
            if stopped {
                // The terminal status that follows says Cairn stopped it.
                notice.prominent = false;
            } else if let Some(shown) = self.state.failure.as_mut().filter(|_| repeated) {
                remember_reason(&mut shown.reasons, Reason::new(&message, event.id));
                shown.ended |= *kind != EventKind::Error;
                notice.prominent = false;
            } else {
                let failure = self.state.failure.get_or_insert_with(Failure::default);
                remember_reason(&mut failure.reasons, Reason::new(&message, event.id));
                failure.notice = Some(id.clone());
                failure.ended |= *kind != EventKind::Error;
                if let Some(previous) = self.state.retried.take()
                    && let Some(card) = previous.notice.clone()
                    && previous
                        .reasons
                        .iter()
                        .any(|reason| reason.digest == digest)
                {
                    // The next attempt failed the same way: its card replaces the previous one.
                    self.hide(vec![card])?;
                    let failure = self.state.failure.as_mut().unwrap();
                    failure.retries = previous.retries;
                    notice.retried = previous.retries;
                }
            }
        }
        if *kind == EventKind::RetryScheduled
            && let Some(card) = self
                .state
                .failure
                .as_ref()
                .and_then(|failure| failure.notice.clone())
        {
            // Cairn retries a temporary failure itself: its card says so instead of a dead end.
            let failure = self.state.failure.as_mut().unwrap();
            let attempt = notice
                .retry
                .as_ref()
                .map_or(failure.retries.unwrap_or(0) + 1, |retry| retry.attempt);
            failure.retries = Some(attempt);
            failure.pending = true;
            let limit = notice.retry.as_ref().and_then(|retry| retry.limit);
            self.update(&card, |card| {
                card.retry = Some(TimelineRetry { attempt, limit });
                card.state = TimelineState::Info;
            })?;
            notice.prominent = false;
        }
        if let Some(reason) = &reason {
            // A final status does not repeat a reason the run log already gave.
            let reason = crate::auth::hex_digest(&bounded(reason));
            if *kind == EventKind::Status && self.state.reported.contains(&reason) {
                notice.detail = None;
            }
            remember(&mut self.state.reported, reason);
        }

        // Usage-limit bookkeeping and replies repeating a failure stay in the run log.
        if let Some(reason) = reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty() && *reason != INTERRUPTED)
        {
            remember(&mut self.state.echoes.failures, fingerprint(reason));
            self.mark_echoes()?;
        }
        if let Some(cause) = quota_failure(&event) {
            let cause = bounded(&cause);
            self.state.echoes.exhausted = true;
            remember(&mut self.state.echoes.causes, fingerprint(&cause));
            self.mark_echoes()?;
            notice.prominent = false;
            // The runner repeats the engine's failure without its reset.
            let previous = self.state.quota.take();
            let resets_at = data["error"]["resetsAt"].as_i64().or_else(|| {
                previous
                    .as_ref()
                    .filter(|previous| previous.digest == crate::auth::hex_digest(&cause))
                    .and_then(|previous| previous.resets_at)
            });
            if let Some(previous) = previous {
                self.update(&previous.card, |notice| {
                    if let Some(quota) = &mut notice.quota {
                        quota.current = false;
                    }
                })?;
            }
            notice.quota = Some(QuotaFailure {
                message: String::new(),
                resets_at,
                current: true,
            });
            self.state.quota = Some(CurrentQuota {
                card: id.clone(),
                digest: crate::auth::hex_digest(&cause),
                resets_at,
            });
        }
        if self.state.echoes.exhausted && event.status(&["failed"]) {
            notice.prominent = false;
        }

        match notice.code {
            NoticeCode::Connection => {
                // The turn's reconnect attempts share one card, showing the latest.
                let previous = std::mem::replace(&mut self.state.connections, vec![id.clone()]);
                self.hide(previous)?;
            }
            NoticeCode::EngineRetry => {
                let previous = std::mem::replace(&mut self.state.engine_retries, vec![id.clone()]);
                self.hide(previous)?;
            }
            NoticeCode::UsageLimit => {
                let applies = notice
                    .usage_limit
                    .as_ref()
                    .is_some_and(|limit| limit.status != "allowed");
                let next = if applies {
                    vec![id.clone()]
                } else {
                    Vec::new()
                };
                let previous = std::mem::replace(&mut self.state.limits, next);
                self.hide(previous)?;
            }
            _ => {}
        }
        self.create(
            id,
            TimelineContent::Notice {
                notice: Box::new(notice),
            },
        );
        Ok(())
    }

    /// Marks this turn's replies that repeat a failure or usage limit reported since.
    fn mark_echoes(&mut self) -> Result<()> {
        let repeated = self
            .state
            .echoes
            .replies
            .iter()
            .filter(|reply| self.state.echoes.repeats(reply))
            .map(|reply| reply.card.clone())
            .collect::<Vec<_>>();
        for id in repeated {
            if let Some(card) = self.card(&id)?
                && let TimelineContent::AgentMessage { echo, .. } = &mut card.content
            {
                *echo = true;
            }
        }
        Ok(())
    }

    fn reply(&mut self, item: &Value) -> Result<()> {
        let event = self.event;
        let item_id = text(item, "id");
        let id = reply_id(self.state.attempt, self.state.turn, event.id, item_id);
        let state = if *event.kind == EventKind::ItemCompleted {
            TimelineState::Done
        } else {
            TimelineState::Running
        };
        let text = message_text(&event);
        let fingerprinted = Reply::new(id.clone(), &text);
        let repeats = self.state.echoes.repeats(&fingerprinted);
        if let Some(card) = self.card(&id)? {
            card.source = event.id;
            if let TimelineContent::AgentMessage {
                state: current,
                echo,
                ..
            } = &mut card.content
            {
                *current = state;
                *echo = repeats;
            }
        } else {
            let via = self.state.via.clone();
            self.create(
                id.clone(),
                TimelineContent::AgentMessage {
                    item_id: item_id.to_owned(),
                    text: String::new(),
                    state,
                    via,
                    echo: repeats,
                },
            );
        }
        // A completed reply is remembered for a failure reported after it; streaming
        // snapshots do not rewrite the checkpoint for every fragment.
        if state == TimelineState::Done {
            let replies = &mut self.state.echoes.replies;
            replies.retain(|reply| reply.card != id);
            remember(replies, fingerprinted);
            self.state.replies.retain(|reply| *reply != id);
        } else if !self.state.replies.contains(&id) {
            self.state.replies.push(id);
        }
        Ok(())
    }

    /// The card a covered lifecycle, error or retry event reports on its own.
    fn notice(&self, event: &Event<'_>, connection: bool) -> TimelineNotice {
        let data = event.data();
        let kind = event.kind;
        let mut notice = TimelineNotice {
            state: TimelineState::Done,
            ..Default::default()
        };
        let failed = matches!(
            kind,
            EventKind::TurnFailed | EventKind::RunnerFailed | EventKind::Error
        ) || event.status(&["failed"])
            || text(event.item(), "type") == "error";
        notice.code = match kind {
            EventKind::ThreadStarted => NoticeCode::SessionStarted,
            EventKind::TurnStarted => NoticeCode::TurnStarted,
            EventKind::TurnCompleted => {
                let usage = &data["usage"];
                if let (Some(input_tokens), Some(output_tokens)) = (
                    usage["input_tokens"].as_u64(),
                    usage["output_tokens"].as_u64(),
                ) {
                    notice.usage = Some(TokenUsage {
                        input_tokens,
                        output_tokens,
                    });
                }
                NoticeCode::TurnCompleted
            }
            EventKind::TurnWaiting => {
                notice.state = TimelineState::Info;
                NoticeCode::BackgroundTasks
            }
            EventKind::TurnFailed => {
                notice.origin = Some(FailureOrigin::Turn);
                NoticeCode::Failure
            }
            EventKind::RunnerFailed => {
                notice.origin = Some(FailureOrigin::Runner);
                NoticeCode::Failure
            }
            EventKind::Error if data["willRetry"] != true => {
                notice.origin = Some(FailureOrigin::Engine);
                NoticeCode::Failure
            }
            EventKind::Error => NoticeCode::Error,
            EventKind::Status if event.text == "failed" => {
                notice.origin = Some(FailureOrigin::Run);
                NoticeCode::Failure
            }
            EventKind::Status => {
                let speed = &data["speed"];
                notice.speed = if speed["unconfirmed"] == true {
                    Some(SpeedNotice::Unconfirmed)
                } else if speed["confirmed"] == true {
                    Some(SpeedNotice::Confirmed)
                } else if speed["applied"] == "standard" {
                    Some(SpeedNotice::Standard)
                } else {
                    None
                };
                if notice.speed.is_some() {
                    notice.state = TimelineState::Info;
                    notice.prominent = true;
                    NoticeCode::Speed
                } else if ACCOUNT.is_match(event.text) {
                    notice.account = Some(AccountNotice {
                        agent: String::new(),
                        name: String::new(),
                    });
                    NoticeCode::AccountSelected
                } else {
                    let stopped = event.status(&["cancelled", "interrupted"]);
                    if stopped {
                        notice.state = TimelineState::Stopped;
                        notice.prominent = true;
                    }
                    notice.status = Some(String::new());
                    NoticeCode::RunStatus
                }
            }
            EventKind::RetryScheduled => {
                let retry = &data["retry"];
                notice.state = TimelineState::Info;
                notice.retry = count(&retry["attempt"]).map(|attempt| TimelineRetry {
                    attempt,
                    limit: count(&retry["limit"]),
                });
                NoticeCode::RetryScheduled
            }
            EventKind::EngineRetry => {
                notice.state = TimelineState::Info;
                notice.prominent = true;
                notice.engine_retry = Some(EngineRetry {
                    attempt: count(&data["attempt"]),
                    max_retries: count(&data["maxRetries"]).filter(|limit| *limit > 0),
                    reason: data["reason"].as_str().map(|_| String::new()),
                    http_status: count(&data["status"]).filter(|status| *status > 0),
                    delay_ms: data["delayMs"].as_u64(),
                    no_response: data["noResponse"].is_object(),
                });
                NoticeCode::EngineRetry
            }
            EventKind::ProviderLimit => {
                let status = text(data, "status");
                notice.state = TimelineState::Info;
                notice.prominent = status != "allowed";
                notice.usage_limit = Some(UsageLimit {
                    status: String::new(),
                    window: data["window"].as_str().map(|_| String::new()),
                    blocked: data["blocked"] == true,
                    resets_at: data["resetsAt"].as_i64().filter(|reset| *reset >= 0),
                });
                NoticeCode::UsageLimit
            }
            EventKind::EngineNotice => {
                let level = text(data, "level");
                notice.state = TimelineState::Info;
                notice.prominent =
                    matches!(level, "warning" | "suggestion") || data["stopsTurn"] == true;
                notice.level = (!level.is_empty()).then(String::new);
                NoticeCode::EngineNotice
            }
            EventKind::Diagnostic => {
                notice.state = TimelineState::Info;
                NoticeCode::Diagnostic
            }
            _ => NoticeCode::Error,
        };
        if failed {
            notice.state = TimelineState::Error;
            notice.prominent = true;
        }
        if connection {
            notice.code = NoticeCode::Connection;
            notice.origin = None;
            notice.state = TimelineState::Error;
            notice.prominent = true;
        }
        if notice_detail(event, notice.code).is_some() {
            notice.detail = Some(String::new());
        }
        source_notice_fields(&mut notice, event);
        notice
    }
}
