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
        TimelineContent::Notice { notice } => serde_json::to_string(notice)?,
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
    let sql = format!("{SELECT} WHERE {filter}");
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
    rows.map(|row| {
        let (card, content, mut source, payload) = row?;
        let card = card_with(card, &content)?;
        // A reply reads only its item's text; process output only the event's text.
        if let TimelineContent::AgentMessage { .. } = card.content {
            return Ok(hydrate_reply(card, &source.text, payload.as_deref(), run));
        }
        if reads_payload(&card.content) {
            source.payload = payload.and_then(|payload| serde_json::from_str(&payload).ok());
        }
        Ok(hydrate(card, &source, run))
    })
    .collect()
}

fn reads_payload(content: &TimelineContent) -> bool {
    match content {
        TimelineContent::Notice { notice } => {
            (notice.detail.is_some() && notice.code != cairn_contracts::NoticeCode::Diagnostic)
                || notice.quota.is_some()
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

/// Cards a live batch carries: those changed by its events, in position order.
pub fn changed(db: &Db<'_>, run: &str, after: i64, upto: i64) -> Result<Vec<TimelineEntry>> {
    if upto <= after {
        return Ok(Vec::new());
    }
    entries(
        db,
        run,
        "t.run_id=?1 AND t.revision>?2 AND t.revision<=?3 ORDER BY t.position",
        params![run, after, upto],
    )
}

/// The cards of a window that starts at `oldest`, including a card that began
/// before it and changed within it, such as a reply still streaming.
pub fn window(db: &Db<'_>, run: &str, oldest: i64) -> Result<Vec<TimelineEntry>> {
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

/// The cards that first appeared in a backwards page `[oldest, before)`.
pub(crate) fn between(
    db: &Db<'_>,
    run: &str,
    oldest: i64,
    before: i64,
) -> Result<Vec<TimelineEntry>> {
    entries(
        db,
        run,
        "t.run_id=?1 AND t.position>=?2 AND t.position<?3 ORDER BY t.position",
        params![run, oldest, before],
    )
}

/// A tool page: the newest cards before a position, newest last.
pub(crate) fn page(
    db: &Db<'_>,
    run: &str,
    before: i64,
    limit: i64,
) -> Result<(Vec<TimelineEntry>, bool)> {
    let mut cards = entries(
        db,
        run,
        "t.run_id=?1 AND t.position<?2 ORDER BY t.position DESC LIMIT ?3",
        params![run, before, limit + 1],
    )?;
    let more = cards.len() > limit as usize;
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
        for card in reducer::reduce(&mut self.state, event, &mut self.cards)? {
            if !self.cards.contains_key(&card.id) {
                self.order.push(card.id.clone());
            }
            self.cards.insert(card.id.clone(), card);
        }
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

impl Cards for &mut std::collections::BTreeMap<String, Card> {
    fn card(&mut self, id: &str) -> Result<Option<Card>> {
        Ok(self.get(id).cloned())
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
