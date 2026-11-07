//! The persisted timeline projection: an index materialized from the run's events
//! in the transaction that records each event. Events remain the reference; the
//! projection can always be rebuilt from them and stores no copy of their text.
use crate::{
    error::Result,
    store::{Db, Event as StoredEvent},
};
use cairn_contracts::{ChatAttachment, EventKind, TimelineContent, TimelineEntry};
use reducer::{Card, Cards, Event, State};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

pub(crate) mod migration;
pub(crate) mod reducer;
#[cfg(test)]
mod tests;

pub(crate) use reducer::{covers, reply_snapshot};

/// Cards are keyed by run; positions and revisions are event sequences of that run.
/// The reducer checkpoint is the minimal state needed to apply the next event.
pub(crate) const SCHEMA: &str = "CREATE TABLE timeline_entries(
 run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
 id TEXT NOT NULL CHECK(length(id) BETWEEN 1 AND 1024),
 position INTEGER NOT NULL CHECK(position BETWEEN 1 AND 9007199254740991),
 revision INTEGER NOT NULL CHECK(revision BETWEEN position AND 9007199254740991),
 source INTEGER NOT NULL CHECK(source BETWEEN position AND revision),
 turn INTEGER NOT NULL CHECK(turn BETWEEN 0 AND 9007199254740991),
 attempt INTEGER NOT NULL CHECK(attempt BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
 kind TEXT NOT NULL CHECK(kind IN ('userMessage','agentMessage','notice')),
 content TEXT NOT NULL CHECK(json_valid(content) AND json_type(content)='object'),
 PRIMARY KEY(run_id,id)
) WITHOUT ROWID;
CREATE INDEX timeline_entries_position ON timeline_entries(run_id,position);
CREATE INDEX timeline_entries_revision ON timeline_entries(run_id,revision);
CREATE TABLE timeline_state(
 run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
 state TEXT NOT NULL CHECK(json_valid(state) AND json_type(state)='object')
) WITHOUT ROWID;";

struct StoredCards<'a> {
    db: &'a Db<'a>,
    run: &'a str,
}

impl Cards for StoredCards<'_> {
    fn card(&mut self, id: &str) -> Result<Option<Card>> {
        self.db
            .0
            .prepare_cached(
                "SELECT id,position,revision,source,turn,attempt,created_at,updated_at,kind,content
                 FROM timeline_entries WHERE run_id=? AND id=?",
            )?
            .query_row(params![self.run, id], card_row)
            .optional()?
            .map(|(card, content)| card_with(card, &content))
            .transpose()
    }

    fn reason(&mut self, event_id: i64) -> Result<Option<String>> {
        let Some(stored) = self
            .db
            .event_batch(self.run, event_id - 1, 1, 256 * 1024)?
            .into_iter()
            .find(|event| event.id == event_id)
        else {
            return Ok(None);
        };
        let payload = stored
            .payload
            .as_ref()
            .map(|value| serde_json::from_str(value.get()))
            .transpose()?;
        Ok(reducer::failure_reason(&Event {
            id: stored.id,
            created_at: stored.created_at,
            kind: &stored.kind,
            text: &stored.text,
            payload: payload.as_ref(),
        }))
    }
}

/// A card and its stored kind and content, decoded once the kind is known.
type CardRow = (Card, (String, String));

fn card_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CardRow> {
    Ok((
        Card {
            id: row.get(0)?,
            position: row.get(1)?,
            revision: row.get(2)?,
            source: row.get(3)?,
            turn: row.get(4)?,
            attempt: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
            content: TimelineContent::Notice {
                notice: Box::default(),
            },
        },
        (row.get(8)?, row.get(9)?),
    ))
}

/// What a reply card stores; its text stays in the source event.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredReply {
    item_id: String,
    state: cairn_contracts::TimelineState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    via: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    echo: bool,
}

/// Content is stored without its union tag, which the `kind` column carries: a
/// tagged union would be buffered whole before decoding on every read and write.
fn card_with(mut card: Card, (kind, content): &(String, String)) -> Result<Card> {
    card.content = match kind.as_str() {
        "userMessage" => TimelineContent::UserMessage {
            plan: false,
            text: String::new(),
            message_id: None,
            question_id: None,
            sent_at: 0,
            attachments: Vec::new(),
        },
        "agentMessage" => {
            let reply: StoredReply = serde_json::from_str(content)?;
            TimelineContent::AgentMessage {
                item_id: reply.item_id,
                text: String::new(),
                state: reply.state,
                via: reply.via,
                echo: reply.echo,
            }
        }
        _ => TimelineContent::Notice {
            notice: serde_json::from_str(content)?,
        },
    };
    Ok(card)
}

fn stored_content(content: &TimelineContent) -> Result<String> {
    Ok(match content {
        TimelineContent::UserMessage { .. } => "{}".to_owned(),
        TimelineContent::AgentMessage {
            item_id,
            state,
            via,
            echo,
            ..
        } => serde_json::to_string(&StoredReply {
            item_id: item_id.clone(),
            state: *state,
            via: via.clone(),
            echo: *echo,
        })?,
        TimelineContent::Notice { notice } => {
            let mut stored = notice.clone();
            stored.tasks.clear();
            stored.task_ids.clear();
            if let Some(account) = &mut stored.account {
                account.agent.clear();
                account.name.clear();
            }
            if let Some(retry) = &mut stored.engine_retry
                && let Some(reason) = &mut retry.reason
            {
                reason.clear();
            }
            if let Some(limit) = &mut stored.usage_limit {
                limit.status.clear();
                if let Some(window) = &mut limit.window {
                    window.clear();
                }
            }
            if let Some(level) = &mut stored.level {
                level.clear();
            }
            serde_json::to_string(&stored)?
        }
    })
}

fn content_kind(content: &TimelineContent) -> &'static str {
    match content {
        TimelineContent::UserMessage { .. } => "userMessage",
        TimelineContent::AgentMessage { .. } => "agentMessage",
        TimelineContent::Notice { .. } => "notice",
    }
}

pub(crate) fn save_card(db: &Db<'_>, run: &str, card: &Card) -> Result<()> {
    db.0.prepare_cached(
        "INSERT INTO timeline_entries(run_id,id,position,revision,source,turn,attempt,created_at,updated_at,kind,content)
         VALUES(?,?,?,?,?,?,?,?,?,?,?)
         ON CONFLICT(run_id,id) DO UPDATE SET revision=excluded.revision,source=excluded.source,
           updated_at=excluded.updated_at,content=excluded.content",
    )?
    .execute(params![
        run,
        card.id,
        card.position,
        card.revision,
        card.source,
        card.turn,
        card.attempt,
        card.created_at,
        card.updated_at,
        content_kind(&card.content),
        stored_content(&card.content)?,
    ])?;
    Ok(())
}

fn state(db: &Db<'_>, run: &str) -> Result<State> {
    let state: Option<String> =
        db.0.prepare_cached("SELECT state FROM timeline_state WHERE run_id=?")?
            .query_row([run], |row| row.get(0))
            .optional()?;
    Ok(state
        .map(|state| serde_json::from_str(&state))
        .transpose()?
        .unwrap_or_default())
}

pub(crate) fn save_state(db: &Db<'_>, run: &str, state: &State) -> Result<()> {
    db.0.prepare_cached(
        "INSERT INTO timeline_state(run_id,state) VALUES(?,?)
         ON CONFLICT(run_id) DO UPDATE SET state=excluded.state",
    )?
    .execute(params![run, serde_json::to_string(state)?])?;
    Ok(())
}

/// Applies a newly recorded event inside the writer's transaction.
pub(crate) fn record(db: &Db<'_>, run: &str, event: Event<'_>) -> Result<()> {
    let mut current = state(db, run)?;
    let before = current.clone();
    let cards = reducer::reduce(&mut current, event, StoredCards { db, run })?;
    for card in &cards {
        save_card(db, run, card)?;
    }
    if current != before {
        save_state(db, run, &current)?;
    }
    Ok(())
}

/// Cards whose source event was pruned can no longer be read; the history that
/// referenced them is gone too, so the stream resets its readers.
pub(crate) fn prune(db: &Db<'_>, run: &str) -> Result<()> {
    db.0.prepare_cached(
        "DELETE FROM timeline_entries WHERE run_id=?1
           AND source<COALESCE((SELECT MIN(id) FROM events WHERE run_id=?1),9007199254740991)",
    )?
    .execute([run])?;
    Ok(())
}

/// A source event, as the reader hydrates its card.
struct Source {
    created_at: i64,
    kind: EventKind,
    text: String,
    payload: Option<Value>,
}

const SELECT: &str = "SELECT t.id,t.position,t.revision,t.source,t.turn,t.attempt,t.created_at,t.updated_at,t.kind,t.content,
 e.created_at,e.type,e.text,e.payload FROM timeline_entries t JOIN events e ON e.id=t.source AND e.run_id=t.run_id";

fn entries(
    db: &Db<'_>,
    run: &str,
    filter: &str,
    parameters: impl rusqlite::Params,
) -> Result<Vec<TimelineEntry>> {
    Ok(entries_bounded(db, run, filter, parameters, None, false, false)?.0)
}

/// Stops decoding before accumulating an unbounded page. A single oversized card
/// is returned so the caller can replace it with a preview and advance its cursor.
fn entries_bounded(
    db: &Db<'_>,
    run: &str,
    filter: &str,
    parameters: impl rusqlite::Params,
    budget: Option<usize>,
    keep_positions: bool,
    revision_index: bool,
) -> Result<(Vec<TimelineEntry>, bool)> {
    let select = if revision_index {
        SELECT.replace(
            "FROM timeline_entries t",
            "FROM timeline_entries t INDEXED BY timeline_entries_revision",
        )
    } else {
        SELECT.to_owned()
    };
    let sql = format!("{select} WHERE {filter}");
    let mut stmt = db.0.prepare_cached(&sql)?;
    let rows = stmt.query_map(parameters, |row| {
        let (card, content) = card_row(row)?;
        Ok((
            card,
            content,
            Source {
                created_at: row.get(10)?,
                kind: EventKind::from_adapter(&row.get::<_, String>(11)?),
                text: row.get(12)?,
                payload: None,
            },
            row.get::<_, Option<String>>(13)?,
        ))
    })?;
    let mut entries = Vec::new();
    let mut bytes = 0_usize;
    for row in rows {
        let (card, content, mut source, payload) = row?;
        let card = card_with(card, &content)?;
        // A reply reads only its item's text; process output only the event's text.
        let entry = if let TimelineContent::AgentMessage { .. } = card.content {
            hydrate_reply(card, &source.text, payload.as_deref(), run)
        } else {
            if reads_payload(&card.content) {
                source.payload = payload.and_then(|payload| serde_json::from_str(&payload).ok());
            }
            hydrate(card, &source, run)
        };
        if let Some(budget) = budget {
            // Leave room for the surrounding change or page variant as well.
            let size = crate::validation::encoded_size(&entry)?.saturating_add(32);
            if !entries.is_empty()
                && bytes.saturating_add(size) > budget
                && (!keep_positions
                    || entries
                        .last()
                        .is_none_or(|last: &TimelineEntry| last.position != entry.position))
            {
                return Ok((entries, true));
            }
            bytes = bytes.saturating_add(size);
        }
        entries.push(entry);
    }
    Ok((entries, false))
}

fn reads_payload(content: &TimelineContent) -> bool {
    match content {
        TimelineContent::Notice { notice } => {
            (notice.detail.is_some() && notice.code != cairn_contracts::NoticeCode::Diagnostic)
                || notice.quota.is_some()
                || notice.code == cairn_contracts::NoticeCode::BackgroundTasks
                || notice
                    .engine_retry
                    .as_ref()
                    .is_some_and(|retry| retry.reason.is_some())
                || notice.usage_limit.is_some()
                || notice.level.is_some()
        }
        _ => true,
    }
}

/// A reply's text, decoding only its item's text rather than the whole snapshot.
fn hydrate_reply(card: Card, text: &str, payload: Option<&str>, run: &str) -> TimelineEntry {
    #[derive(serde::Deserialize)]
    struct Item {
        text: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Payload {
        item: Option<Item>,
    }
    let reply = payload
        .and_then(|payload| serde_json::from_str::<Payload>(payload).ok())
        .and_then(|payload| payload.item)
        .and_then(|item| item.text)
        .filter(|reply| !reply.is_empty())
        .unwrap_or_else(|| text.to_owned());
    let TimelineContent::AgentMessage {
        item_id,
        state,
        via,
        echo,
        ..
    } = card.content
    else {
        unreachable!("a reply card")
    };
    TimelineEntry {
        id: card.id,
        run_id: run.to_owned(),
        position: card.position,
        revision: card.revision,
        source: card.source,
        turn: card.turn,
        attempt: card.attempt,
        created_at: card.created_at,
        updated_at: card.updated_at,
        content: TimelineContent::AgentMessage {
            item_id,
            text: reply,
            state,
            via,
            echo,
        },
    }
}

/// Reads the card's text and details from its source event.
fn hydrate(card: Card, source: &Source, run: &str) -> TimelineEntry {
    let event = Event {
        id: card.source,
        created_at: source.created_at,
        kind: &source.kind,
        text: &source.text,
        payload: source.payload.as_ref(),
    };
    let content = match card.content {
        TimelineContent::UserMessage { .. } => {
            let payload = source.payload.as_ref().unwrap_or(&Value::Null);
            let string = |key: &str| {
                payload[key]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            };
            let sent_at = payload["createdAt"]
                .as_i64()
                .filter(|sent| *sent > 0 && *sent <= source.created_at)
                .unwrap_or(source.created_at);
            TimelineContent::UserMessage {
                plan: payload["plan"].as_bool().unwrap_or(false),
                text: reducer::message_text(&event),
                message_id: string("messageId"),
                question_id: string("questionId"),
                sent_at,
                attachments: serde_json::from_value::<Vec<ChatAttachment>>(
                    payload["attachments"].clone(),
                )
                .unwrap_or_default(),
            }
        }
        TimelineContent::AgentMessage {
            item_id,
            state,
            via,
            echo,
            ..
        } => TimelineContent::AgentMessage {
            item_id,
            text: reducer::message_text(&event),
            state,
            via,
            echo,
        },
        TimelineContent::Notice { mut notice } => {
            reducer::source_notice_fields(&mut notice, &event);
            if notice.detail.is_some() {
                notice.detail = reducer::notice_detail(&event, notice.code);
            }
            if notice.status.is_some() {
                notice.status = Some(source.text.clone());
            }
            if let Some(quota) = &mut notice.quota {
                quota.message = reducer::quota_failure(&event).unwrap_or_default();
            }
            TimelineContent::Notice { notice }
        }
    };
    TimelineEntry {
        id: card.id,
        run_id: run.to_owned(),
        position: card.position,
        revision: card.revision,
        source: card.source,
        turn: card.turn,
        attempt: card.attempt,
        created_at: card.created_at,
        updated_at: card.updated_at,
        content,
    }
}

/// A bounded part of the changes in an event range, ordered by revision and ID.
/// A concurrent change past the upper bound is delivered by the next event batch.
pub fn changed_chunk(
    db: &Db<'_>,
    run: &str,
    after: i64,
    upto: i64,
    revision: i64,
    id: &str,
) -> Result<(Vec<TimelineEntry>, bool)> {
    entries_bounded(
        db,
        run,
        "t.run_id=?1 AND t.revision>?2 AND t.revision<=?3 AND (t.revision>?4 OR (t.revision=?4 AND t.id>?5)) ORDER BY t.revision,t.id",
        params![run, after, upto, revision.max(after), id],
        Some(256 * 1024 - 1024),
        false,
        true,
    )
}

/// The current wait may precede an opening window after many background events.
pub(crate) fn active_wait(db: &Db<'_>, run: &str) -> Result<Option<TimelineEntry>> {
    let current = state(db, run)?;
    match current.active_wait_card() {
        Some(id) => entry(db, run, id),
        None => Ok(None),
    }
}

/// Read one interpreted card, including its hydrated content.
pub(crate) fn entry(db: &Db<'_>, run: &str, id: &str) -> Result<Option<TimelineEntry>> {
    Ok(
        entries(db, run, "t.run_id=?1 AND t.id=?2", params![run, id])?
            .into_iter()
            .next(),
    )
}

/// Rehydrate the referenced snapshot from its immutable source event. Later
/// revisions of the same card do not change the bytes returned for this snapshot.
pub(crate) fn snapshot_from_source(
    db: &Db<'_>,
    run: &str,
    snapshot: TimelineEntry,
) -> Result<TimelineEntry> {
    use crate::error::{Error, required};
    if snapshot.run_id != run
        || snapshot.source < snapshot.position
        || snapshot.source > snapshot.revision
    {
        return Err(Error::bad("Invalid timeline snapshot."));
    }
    let valid: bool = db.0.query_row(
        "SELECT EXISTS(SELECT 1 FROM timeline_entries WHERE run_id=?1 AND id=?2 AND position=?3 AND revision>=?4)",
        params![run, snapshot.id, snapshot.position, snapshot.revision],
        |row| row.get(0),
    )?;
    if !valid {
        return Err(Error::conflict("Timeline snapshot is no longer retained."));
    }
    let event = required(
        db.event_page(run, snapshot.source - 1, 1)?
            .into_iter()
            .find(|event| event.id == snapshot.source),
        "Timeline source is no longer retained",
    )?;
    let payload = event
        .payload
        .as_ref()
        .and_then(|value| serde_json::from_str::<Value>(value.get()).ok());
    let matches = match &snapshot.content {
        TimelineContent::UserMessage { .. } => {
            event.kind == EventKind::ChatUser && snapshot.id == format!("user:{}", event.id)
        }
        TimelineContent::AgentMessage { .. } => {
            matches!(
                event.kind,
                EventKind::ItemStarted | EventKind::ItemUpdated | EventKind::ItemCompleted
            ) && payload.as_ref().is_some_and(|payload| {
                let item = &payload["item"];
                item["type"] == "agent_message"
                    && snapshot.id
                        == reducer::reply_id(
                            snapshot.attempt,
                            snapshot.turn,
                            event.id,
                            item["id"].as_str().unwrap_or(""),
                        )
            })
        }
        TimelineContent::Notice { .. } => snapshot.id == format!("notice:{}", event.id),
    };
    if !matches {
        return Err(Error::bad("Timeline source does not match the card."));
    }
    let mut card = Card {
        id: snapshot.id,
        position: snapshot.position,
        revision: snapshot.revision,
        source: snapshot.source,
        turn: snapshot.turn,
        attempt: snapshot.attempt,
        created_at: snapshot.created_at,
        updated_at: snapshot.updated_at,
        content: snapshot.content,
    };
    if let TimelineContent::AgentMessage { item_id, .. } = &mut card.content {
        *item_id = payload
            .as_ref()
            .and_then(|value| value["item"]["id"].as_str())
            .unwrap_or("")
            .to_owned();
    }
    if matches!(card.content, TimelineContent::AgentMessage { .. }) {
        return Ok(hydrate_reply(
            card,
            &event.text,
            event.payload.as_ref().map(|value| value.get()),
            run,
        ));
    }
    Ok(hydrate(
        card,
        &Source {
            created_at: event.created_at,
            kind: event.kind,
            text: event.text,
            payload,
        },
        run,
    ))
}

pub(crate) fn changes_after(
    db: &Db<'_>,
    run: &str,
    revision: i64,
    id: Option<&str>,
    limit: i64,
) -> Result<(Vec<TimelineEntry>, bool)> {
    entries_bounded(
        db,
        run,
        "t.run_id=?1 AND (t.revision>?2 OR (t.revision=?2 AND ?3 IS NOT NULL AND t.id>?3)) ORDER BY t.revision,t.id LIMIT ?4",
        params![run, revision, id, limit],
        Some(256 * 1024 - 1024),
        false,
        true,
    )
}

/// The cards of a window that starts at `oldest`, including a card that began
/// before it and changed within it, such as a reply still streaming.
#[cfg(test)]
fn window(db: &Db<'_>, run: &str, oldest: i64) -> Result<Vec<TimelineEntry>> {
    // Two index ranges: an OR of both columns would scan every card of the run.
    let changed_earlier = SELECT.replace(
        "FROM timeline_entries t",
        "FROM timeline_entries t INDEXED BY timeline_entries_revision",
    ) + " WHERE t.run_id=?1 AND t.revision>=?2 AND t.position<?2";
    entries(
        db,
        run,
        &format!("t.run_id=?1 AND t.position>=?2 UNION ALL {changed_earlier} ORDER BY 2"),
        params![run, oldest],
    )
}

/// A bounded part of an opening window. Cards changed after an older position
/// use revision order; the remaining cards use position order. Both paths are indexed.
pub fn window_chunk(
    db: &Db<'_>,
    run: &str,
    oldest: i64,
    upto: i64,
    position: i64,
    id: &str,
    earlier: bool,
) -> Result<(Vec<TimelineEntry>, bool)> {
    let filter = if earlier {
        "t.run_id=?1 AND t.revision>=?2 AND t.position<?2 AND t.revision<=?3 AND (t.revision>?4 OR (t.revision=?4 AND t.id>?5)) ORDER BY t.revision,t.id"
    } else {
        "t.run_id=?1 AND t.position>=?2 AND t.revision<=?3 AND (t.position>?4 OR (t.position=?4 AND t.id>?5)) ORDER BY t.position,t.id"
    };
    // SQLite uses the pagination cursor as the index seek bound. Clamp it to
    // the window so the first page does not scan every older card in the run.
    let cursor = position.max(oldest.saturating_sub(1));
    entries_bounded(
        db,
        run,
        filter,
        params![run, oldest, upto, cursor, id],
        Some(256 * 1024 - 1024),
        false,
        earlier,
    )
}

/// Selects a bounded newest suffix of a backwards event page without splitting
/// cards born at the same event; an individual event's cards may exceed the budget.
pub(crate) fn between_bounded(
    db: &Db<'_>,
    run: &str,
    oldest: i64,
    before: i64,
) -> Result<(Vec<TimelineEntry>, bool)> {
    let (mut cards, more) = entries_bounded(
        db,
        run,
        "t.run_id=?1 AND t.position>=?2 AND t.position<?3 ORDER BY t.position DESC,t.id DESC",
        params![run, oldest, before],
        Some(256 * 1024 - 1024),
        true,
        false,
    )?;
    cards.reverse();
    Ok((cards, more))
}

/// A tool page: the newest cards before a position, newest last.
pub(crate) fn page(
    db: &Db<'_>,
    run: &str,
    before: i64,
    limit: i64,
) -> Result<(Vec<TimelineEntry>, bool)> {
    let (mut cards, limited) = entries_bounded(
        db,
        run,
        "t.run_id=?1 AND t.position<?2 ORDER BY t.position DESC LIMIT ?3",
        params![run, before, limit + 1],
        Some(256 * 1024 - 1024),
        false,
        false,
    )?;
    let more = limited || cards.len() > limit as usize;
    cards.truncate(limit as usize);
    cards.reverse();
    Ok((cards, more))
}

/// Replays a run's events from the start with the same reducer as the writer.
#[derive(Default)]
pub(crate) struct Replay {
    pub state: State,
    cards: std::collections::BTreeMap<String, Card>,
    order: Vec<String>,
    reasons: std::collections::BTreeMap<i64, String>,
}

impl Replay {
    pub fn apply(&mut self, stored: &StoredEvent) -> Result<()> {
        let payload = stored
            .payload
            .as_ref()
            .map(|payload| serde_json::from_str::<Value>(payload.get()))
            .transpose()?;
        let event = Event {
            id: stored.id,
            created_at: stored.created_at,
            kind: &stored.kind,
            text: &stored.text,
            payload: payload.as_ref(),
        };
        if let Some(reason) = reducer::failure_reason(&event) {
            self.reasons.insert(event.id, reason);
        }
        let cards = ReplayCards {
            cards: &mut self.cards,
            reasons: &self.reasons,
        };
        for card in reducer::reduce(&mut self.state, event, cards)? {
            if !self.cards.contains_key(&card.id) {
                self.order.push(card.id.clone());
            }
            self.cards.insert(card.id.clone(), card);
        }
        let retained = self.state.reason_sources();
        self.reasons.retain(|id, _| retained.contains(id));
        Ok(())
    }

    /// The cards in creation order, with the reducer's checkpoint.
    pub fn finish(mut self) -> (State, Vec<Card>) {
        let cards = self
            .order
            .into_iter()
            .filter_map(|id| self.cards.remove(&id))
            .collect();
        (self.state, cards)
    }
}

struct ReplayCards<'a> {
    cards: &'a mut std::collections::BTreeMap<String, Card>,
    reasons: &'a std::collections::BTreeMap<i64, String>,
}

impl Cards for ReplayCards<'_> {
    fn card(&mut self, id: &str) -> Result<Option<Card>> {
        Ok(self.cards.get(id).cloned())
    }

    fn reason(&mut self, event_id: i64) -> Result<Option<String>> {
        Ok(self.reasons.get(&event_id).cloned())
    }
}

/// Whether a stored event is a reply snapshot, and whether the timeline presents
/// it. Only an item's type is decoded, never its content.
pub(crate) fn presentation(event: &StoredEvent) -> (bool, bool) {
    #[derive(serde::Deserialize)]
    struct Item<'a> {
        #[serde(borrow, rename = "type")]
        kind: Option<std::borrow::Cow<'a, str>>,
    }
    #[derive(serde::Deserialize)]
    struct Payload<'a> {
        #[serde(borrow)]
        item: Option<Item<'a>>,
    }
    if !matches!(
        event.kind,
        EventKind::ItemStarted | EventKind::ItemUpdated | EventKind::ItemCompleted
    ) {
        return (false, covers(&event.kind, None));
    }
    let kind = event
        .payload
        .as_ref()
        .and_then(|payload| serde_json::from_str::<Payload<'_>>(payload.get()).ok())
        .and_then(|payload| payload.item)
        .and_then(|item| item.kind);
    let payload = kind.map(|kind| serde_json::json!({"item": {"type": kind}}));
    (
        reply_snapshot(&event.kind, payload.as_ref()),
        covers(&event.kind, payload.as_ref()),
    )
}
