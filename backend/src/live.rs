//! Durable replay and live delivery share one cursor. Notifications are hints;
//! SQLite is authoritative, so a slow subscriber never buffers or blocks writes.
use crate::{
    auth::InstallationIdentity,
    error::{Error, Result},
    http::Input,
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use axum::response::{
    IntoResponse, Response,
    sse::{Event, KeepAlive, Sse},
};
use serde::Serialize;
use serde_json::Value;
use std::{sync::Arc, time::Duration};

const PAGE_EVENTS: i64 = 100;
const PAGE_BYTES: usize = 256 * 1024;
const MAX_HISTORY_VERSION: usize = 200;

#[derive(Clone)]
struct Scope {
    chat: bool,
    id: String,
}

impl Scope {
    fn new(kind: &str, id: &str) -> Self {
        Self {
            chat: kind == "chats",
            id: id.to_owned(),
        }
    }

    fn single_chat(&self) -> bool {
        self.chat && !self.id.is_empty()
    }
}

/// Metadata sent alongside events whenever it changes.
#[derive(Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct State {
    run: Value,
    chat: Value,
    artifacts: Vec<Value>,
    cache_revision: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    chats: Option<Vec<Value>>,
}

struct Page {
    state: State,
    events: Vec<crate::store::Event>,
    reset: bool,
    more: bool,
    history: String,
    oldest: Option<i64>,
    has_older: bool,
}

/// One SSE `batch` event.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Batch {
    events: Vec<Value>,
    reset: bool,
    more: bool,
    history: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    oldest: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_older: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<State>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryPage {
    events: Vec<crate::store::Event>,
    history: String,
    oldest: Option<i64>,
    has_older: bool,
}

struct Request {
    after: i64,
    expected: Option<String>,
    window: bool,
    before: Option<i64>,
}

/// Oldest and newest retained event IDs of a run, 0 when it has none.
fn event_bounds(db: &Db<'_>, run: &str) -> Result<(i64, i64)> {
    Ok(db.0.query_row(
        "SELECT COALESCE((SELECT id FROM events WHERE run_id=?1 ORDER BY id LIMIT 1),0),
                COALESCE((SELECT id FROM events WHERE run_id=?1 ORDER BY id DESC LIMIT 1),0)",
        [run],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

fn artifacts(db: &Db<'_>, run: &str) -> Result<Vec<Value>> {
    if run.is_empty() {
        return Ok(Vec::new());
    }
    let mut artifacts: Vec<_> = db
        .keys(&format!("artifact:{run}:"))?
        .into_iter()
        .map(|(_, v)| v)
        .collect();
    artifacts.sort_by_key(|v| v["createdAt"].as_i64().unwrap_or(0));
    Ok(artifacts)
}

fn scoped_run(db: &Db<'_>, scope: &Scope, chat: &Value) -> Result<Value> {
    if scope.chat {
        return Ok(chat["run"].clone());
    }
    if crate::conversation_lifecycle::require_active_run(db, &scope.id).is_err() {
        return Ok(Value::Null);
    }
    crate::error::required(db.run(&scope.id)?, "Run not found")
}

fn read_page(db: &Db<'_>, scope: &Scope, request: Request) -> Result<Page> {
    let Request {
        after,
        expected,
        window,
        before,
    } = request;
    let chat = if scope.single_chat() {
        crate::chats::detail(db, &scope.id)?
    } else {
        Value::Null
    };
    let run = scoped_run(db, scope, &chat)?;
    let run_id = text(&run, "id");
    let (first, max) = event_bounds(db, run_id)?;
    // Events are append-only and IDs are AUTOINCREMENT. The retained first
    // ID changes on pruning; a new run has a new UUID. No full-history hash.
    let history = format!("v1:{run_id}:{first}");
    let reset = after > max
        || (after > 0 && after < first)
        || expected.as_ref().is_some_and(|value| *value != history);
    if before.is_some() && reset {
        return Err(Error::conflict(
            "History changed. Reconnect before loading older messages.",
        ));
    }
    // Stop reading rows at the byte budget, rather than allocating an
    // entire page of large historical outputs for every subscriber.
    let start = if reset { 0 } else { after };
    let tail = before.is_some() || (window && (after == 0 || reset));
    let (events, has_older) = if tail {
        db.events_before(run_id, before.unwrap_or(i64::MAX))?
    } else {
        (
            db.event_batch(run_id, start, PAGE_EVENTS, PAGE_BYTES)?,
            false,
        )
    };
    let oldest = tail.then(|| events.first().map_or(before.unwrap_or(0), |e| e.id));
    let cursor = events.last().map_or(start, |e| e.id);
    let mut state = State {
        artifacts: artifacts(db, run_id)?,
        run,
        chat,
        cache_revision: db
            .kv("conversation-cache-revision")?
            .unwrap_or_else(|| "initial".into()),
        chats: scope.chat.then(|| crate::chats::list(db)).transpose()?,
    };
    // Delivered messages already live in the event history. Do not resend
    // the entire conversation as metadata on every paginated stream update.
    if window
        && scope.single_chat()
        && let Some(messages) = state.chat["messages"].as_array_mut()
    {
        messages.retain(|m| m["status"] != crate::chats::MessageStatus::Delivered);
    }
    Ok(Page {
        state,
        events,
        reset,
        more: cursor < max,
        history,
        oldest,
        has_older,
    })
}

async fn page(s: &Service, scope: Scope, request: Request) -> Result<Page> {
    s.store
        .read(move |db| {
            // A single SQLite snapshot covers metadata and the event boundary.
            let tx = rusqlite::Transaction::new_unchecked(
                db.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            read_page(&Db(&tx), &scope, request)
        })
        .await
}

fn cursor(input: &Input) -> Result<i64> {
    let Some(value) = input.headers.get("last-event-id") else {
        return input.number("after", 0, 0, i64::MAX);
    };
    value
        .to_str()
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .ok_or_else(|| Error::bad("Invalid event cursor."))
}

pub async fn http(s: Arc<Service>, kind: &str, id: &str, input: Input) -> Result<Response> {
    if input.method != "GET" {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    if !id.is_empty() {
        uuid(id)?;
    }
    let after = cursor(&input)?;
    let identity = input
        .identity
        .ok_or_else(|| Error::unauthorized("Please sign in."))?;
    let scope = Scope::new(kind, id);
    // Subscribe before reading: commits during replay remain observable.
    let changes = s.store.subscribe();
    let expected = input.query.get("history").cloned();
    if expected
        .as_ref()
        .is_some_and(|v| v.len() > MAX_HISTORY_VERSION)
    {
        return Err(Error::bad("Invalid history version."));
    }
    let window = input.query.get("window").is_some_and(|v| v == "1");
    let request = Request {
        after,
        expected,
        window,
        before: None,
    };
    let first = page(&s, scope.clone(), request).await?;
    let subscription = Subscription {
        s,
        scope,
        window,
        changes,
        cursor: after,
        pending: Some(first),
        previous: None,
        history: None,
        identity,
        deltas: crate::live_text::TextDeltas::default(),
    };
    let stream = futures_util::stream::try_unfold(subscription, |mut subscription| async move {
        let event = subscription.next().await?;
        Ok::<_, Error>(event.map(|event| (event, subscription)))
    });
    Ok((
        [("x-accel-buffering", "no")],
        Sse::new(stream).keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(10))
                .event(Event::default().event("ping").data("{}")),
        ),
    )
        .into_response())
}

struct Subscription {
    s: Arc<Service>,
    scope: Scope,
    window: bool,
    changes: tokio::sync::watch::Receiver<u64>,
    cursor: i64,
    pending: Option<Page>,
    previous: Option<State>,
    history: Option<String>,
    identity: InstallationIdentity,
    deltas: crate::live_text::TextDeltas,
}

impl Subscription {
    async fn read(&self, after: i64, expected: Option<String>) -> Result<Page> {
        let request = Request {
            after,
            expected,
            window: self.window,
            before: None,
        };
        page(&self.s, self.scope.clone(), request).await
    }

    async fn next(&mut self) -> Result<Option<Event>> {
        loop {
            if self.s.shutdown.is_cancelled() {
                return Ok(None);
            }
            // Mark observed changes before reading auth. Otherwise a logout
            // between the auth check and the page read can be consumed unseen.
            self.changes.borrow_and_update();
            if !self.identity.is_active(&self.s.auth).await? {
                return Ok(None);
            }
            if let Some(event) = self.poll().await? {
                // Coalesce rapid commits without an unbounded per-client queue.
                tokio::time::sleep(Duration::from_millis(25)).await;
                return Ok(Some(event));
            }
            tokio::select! {
                () = self.s.shutdown.cancelled() => return Ok(None),
                result = self.changes.changed() => if result.is_err() { return Ok(None); },
                // Revalidate session expiry and recover writes by an external
                // maintenance process; normal delivery is notification driven.
                () = tokio::time::sleep(Duration::from_secs(15)) => {},
            }
        }
    }

    /// Reads the next page; returns a batch when it carries anything new.
    async fn poll(&mut self) -> Result<Option<Event>> {
        let mut current = match self.pending.take() {
            Some(first) => first,
            None => self.read(self.cursor, self.history.clone()).await?,
        };
        let run_changed = self
            .previous
            .as_ref()
            .is_some_and(|previous| previous.run["id"] != current.state.run["id"]);
        if run_changed {
            current = self.read(0, None).await?;
        }
        let reset = current.reset || run_changed;
        if reset {
            self.cursor = 0;
        }
        self.history = Some(current.history.clone());
        let changed = self.previous.as_ref() != Some(&current.state);
        let has_events = !current.events.is_empty();
        self.cursor = current.events.last().map_or(self.cursor, |e| e.id);
        let batch = Batch {
            events: self.deltas.encode(current.events, reset)?,
            reset,
            more: current.more,
            history: current.history,
            oldest: current.oldest,
            has_older: current.oldest.map(|_| current.has_older),
            state: changed.then(|| current.state.clone()),
        };
        self.previous = Some(current.state);
        if !(changed || has_events || reset) {
            return Ok(None);
        }
        let event = Event::default()
            .event("batch")
            .id(self.cursor.to_string())
            .json_data(batch)
            .map_err(Error::internal)?;
        Ok(Some(event))
    }
}

/// Backwards pages use full persisted snapshots, never connection-local deltas.
pub async fn history(s: &Service, kind: &str, id: &str, input: &Input) -> Result<Value> {
    uuid(id)?;
    let before = input.number("before", i64::MAX, 1, i64::MAX)?;
    let expected = input.query.get("history").cloned();
    if expected
        .as_ref()
        .is_none_or(|v| v.len() > MAX_HISTORY_VERSION)
    {
        return Err(Error::bad("A history revision is required."));
    }
    let request = Request {
        after: 0,
        expected,
        window: true,
        before: Some(before),
    };
    let page = page(s, Scope::new(kind, id), request).await?;
    Ok(serde_json::to_value(HistoryPage {
        events: page.events,
        history: page.history,
        oldest: page.oldest,
        has_older: page.has_older,
    })?)
}
