//! Durable replay and live delivery share one cursor. Notifications are hints;
//! SQLite is authoritative, so a slow subscriber never buffers or blocks writes.
use crate::{
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
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

const PAGE_EVENTS: i64 = 100;
const PAGE_BYTES: usize = 256 * 1024;
const MAX_HISTORY_VERSION: usize = 200;
/// Every commit wakes every stream. The list stream rebuilds the conversation list at
/// most this often, so an agent writing output does not rebuild it per event.
const LIST_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone)]
struct Scope {
    chat: bool,
    id: String,
    /// The subscriber's Cairn account, whose read marks the conversation list carries.
    account: String,
    /// A conversation's stream also carries the list, so a browser tab reading a
    /// conversation keeps a single stream within its account's stream capacity.
    with_list: bool,
}

impl Scope {
    fn new(kind: &str, id: &str, account: &str) -> Self {
        Self {
            chat: kind == "chats",
            id: id.to_owned(),
            account: account.to_owned(),
            with_list: false,
        }
    }

    fn single_chat(&self) -> bool {
        self.chat && !self.id.is_empty()
    }

    /// The conversation list stream.
    fn list(&self) -> bool {
        self.chat && self.id.is_empty()
    }

    fn carries_list(&self) -> bool {
        self.list() || (self.single_chat() && self.with_list)
    }
}

struct CachedList {
    revision: u64,
    built_at: Instant,
    chats: Arc<Vec<Value>>,
}

struct ListMetrics {
    since: Instant,
    rebuilds: u64,
    cache_hits: u64,
    coalesced: u64,
    rebuild_us: u64,
    max_rebuild_us: u64,
    pages: u64,
    queue_us: u64,
    read_us: u64,
    max_queue_us: u64,
    max_read_us: u64,
}

impl Default for ListMetrics {
    fn default() -> Self {
        Self {
            since: Instant::now(),
            rebuilds: 0,
            cache_hits: 0,
            coalesced: 0,
            rebuild_us: 0,
            max_rebuild_us: 0,
            pages: 0,
            queue_us: 0,
            read_us: 0,
            max_queue_us: 0,
            max_read_us: 0,
        }
    }
}

impl ListMetrics {
    fn report(&mut self) {
        let elapsed = self.since.elapsed();
        if elapsed < Duration::from_secs(30) {
            return;
        }
        tracing::info!(target: "cairn_performance", operation = "conversation_reads",
            interval_ms = elapsed.as_millis() as u64, pages = self.pages,
            list_rebuilds = self.rebuilds, list_cache_hits = self.cache_hits,
            list_coalesced = self.coalesced, list_build_us = self.rebuild_us,
            max_list_build_us = self.max_rebuild_us, queue_us = self.queue_us,
            read_us = self.read_us, max_queue_us = self.max_queue_us,
            max_read_us = self.max_read_us);
        *self = Self::default();
    }
}

/// Account-scoped lists are shared across tabs. The async gate is taken before
/// enqueueing a SQLite job: concurrent rebuilds never occupy all reader actors
/// waiting on one synchronous lock. Chat events still bypass list pacing.
#[derive(Default)]
pub struct ConversationLists {
    cache: std::sync::Mutex<HashMap<String, CachedList>>,
    gates: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    metrics: std::sync::Mutex<ListMetrics>,
}

impl ConversationLists {
    fn invalidate(&self, account: &str) {
        self.cache.lock().unwrap().remove(account);
    }

    fn gate(&self, account: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.gates
            .lock()
            .unwrap()
            .entry(account.to_owned())
            .or_default()
            .clone()
    }

    /// A newer revision may reuse the list for a quarter second, independently
    /// of chat-event delivery. Its deadline schedules a refresh even if no more
    /// writes arrive. Same-revision lists need no periodic rebuild.
    fn get(
        &self,
        db: &Db<'_>,
        scope: &Scope,
        revision: u64,
    ) -> Result<(Arc<Vec<Value>>, Option<Instant>)> {
        if let Some(cached) = self.cache.lock().unwrap().get(&scope.account) {
            let refresh_at = cached.built_at + LIST_INTERVAL;
            let same_revision = cached.revision == revision;
            if same_revision || Instant::now() < refresh_at {
                let mut metrics = self.metrics.lock().unwrap();
                metrics.cache_hits += 1;
                metrics.coalesced += u64::from(!same_revision);
                return Ok((cached.chats.clone(), (!same_revision).then_some(refresh_at)));
            }
        }
        let started = Instant::now();
        let chats = Arc::new(conversations(db, scope)?);
        let micros = started.elapsed().as_micros() as u64;
        self.cache.lock().unwrap().insert(
            scope.account.clone(),
            CachedList {
                revision,
                built_at: Instant::now(),
                chats: chats.clone(),
            },
        );
        let mut metrics = self.metrics.lock().unwrap();
        metrics.rebuilds += 1;
        metrics.rebuild_us += micros;
        metrics.max_rebuild_us = metrics.max_rebuild_us.max(micros);
        Ok((chats, None))
    }

    fn record_page(&self, queue: Duration, read: Duration) {
        let mut metrics = self.metrics.lock().unwrap();
        let queue = queue.as_micros() as u64;
        let read = read.as_micros() as u64;
        metrics.pages += 1;
        metrics.queue_us += queue;
        metrics.read_us += read;
        metrics.max_queue_us = metrics.max_queue_us.max(queue);
        metrics.max_read_us = metrics.max_read_us.max(read);
        metrics.report();
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
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_chats"
    )]
    chats: Option<Arc<Vec<Value>>>,
}

fn serialize_chats<S: serde::Serializer>(
    chats: &Option<Arc<Vec<Value>>>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    chats.as_deref().serialize(serializer)
}

struct Page {
    state: State,
    events: Vec<crate::store::Event>,
    reset: bool,
    more: bool,
    history: String,
    oldest: Option<i64>,
    has_older: bool,
    list_refresh_at: Option<Instant>,
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

use cairn_contracts::HistoryPage;

#[cfg(test)]
mod read_tests;

struct Request {
    after: i64,
    expected: Option<String>,
    window: bool,
    /// The viewer follows the latest output, so a long backlog may become the recent window.
    recent: bool,
    /// The connection's first page: a reader resuming from its saved cursor.
    resume: bool,
}

/// Oldest and newest retained event IDs of a run, 0 when it has none.
pub(crate) fn event_bounds(db: &Db<'_>, run: &str) -> Result<(i64, i64)> {
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

fn read_page(
    db: &Db<'_>,
    scope: &Scope,
    request: Request,
    lists: &ConversationLists,
    revision: u64,
    list_permit: Option<tokio::sync::OwnedMutexGuard<()>>,
) -> Result<Page> {
    let Request {
        after,
        expected,
        window,
        recent,
        resume,
    } = request;
    let (chats, list_refresh_at) = if scope.carries_list() {
        let (chats, refresh_at) = lists.get(db, scope, revision)?;
        (Some(chats), refresh_at)
    } else {
        (None, None)
    };
    // Only list construction is shared. A slow message, run or event read in
    // this conversation must not hold up another tab's durable event delivery.
    drop(list_permit);
    let chat = if scope.single_chat() {
        crate::chats::public_stream_detail(db, &scope.id, window)?
    } else {
        Value::Null
    };
    let run = scoped_run(db, scope, &chat)?;
    let run_id = text(&run, "id");
    let (first, max) = event_bounds(db, run_id)?;
    // Events are append-only and IDs are AUTOINCREMENT. The retained first
    // ID changes on pruning; a new run has a new UUID. No full-history hash.
    let history = format!("v1:{run_id}:{first}");
    let mut reset = after > max
        || (after > 0 && after < first)
        || expected.as_ref().is_some_and(|value| *value != history);
    // Stop reading rows at the byte budget, rather than allocating an
    // entire page of large historical outputs for every subscriber.
    let start = if reset { 0 } else { after };
    let mut tail = window && (after == 0 || reset);
    let (mut events, mut has_older) = if tail {
        db.live_events_between(run_id, 0, i64::MAX)?
    } else {
        (
            db.event_batch(run_id, start, PAGE_EVENTS, PAGE_BYTES)?,
            false,
        )
    };

    // A backlog longer than one page is mostly text snapshots that later snapshots
    // replaced. When the newest window reaches back to the cursor, it is the whole
    // catch-up: the reader keeps its history and only receives what it lacks.
    let long_backlog = !tail && events.last().is_some_and(|event| event.id < max);
    if window && resume && long_backlog {
        let (missing, gap) = db.live_events_between(run_id, start, i64::MAX)?;
        if !gap {
            events = missing;
        } else if recent {
            // A viewer at the latest output skips the rest: it reopens on the recent
            // window, and older messages load from the window's boundary.
            (events, has_older) = db.live_events_between(run_id, 0, i64::MAX)?;
            reset = true;
            tail = true;
        }
    }
    let oldest = tail.then(|| events.first().map_or(0, |e| e.id));
    let cursor = events.last().map_or(start, |e| e.id);
    let state = State {
        artifacts: artifacts(db, run_id)?,
        run,
        chat,
        cache_revision: db
            .kv("conversation-cache-revision")?
            .unwrap_or_else(|| "initial".into()),
        chats,
    };
    Ok(Page {
        state,
        events,
        reset,
        more: cursor < max,
        history,
        oldest,
        has_older,
        list_refresh_at,
    })
}

fn conversations(db: &Db<'_>, scope: &Scope) -> Result<Vec<Value>> {
    let mut chats = crate::chats::list(db)?;
    crate::reads::Reads::load(db, &scope.account)?.chats(db, &mut chats)?;
    Ok(chats)
}

async fn page(s: &Service, scope: Scope, request: Request) -> Result<Page> {
    let started = Instant::now();
    let lists = s.conversation_lists.clone();
    let gate = scope.carries_list().then(|| lists.gate(&scope.account));
    let permit = match gate {
        Some(gate) => Some(gate.lock_owned().await),
        None => None,
    };
    // Read before the snapshot starts, so the snapshot includes this revision.
    let revision = s.store.revision();
    s.store
        .read(move |db| {
            let queue = started.elapsed();
            let read = Instant::now();
            // A single SQLite snapshot covers metadata and the event boundary.
            let tx = rusqlite::Transaction::new_unchecked(
                db.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let result = read_page(&Db(&tx), &scope, request, &lists, revision, permit);
            lists.record_page(queue, read.elapsed());
            result
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
    let account = crate::reads::account(&input)?;
    if kind == "chats" {
        crate::reads::begin(&s, account).await?;
    }
    let scope = Scope {
        with_list: input.query.get("list").is_some_and(|v| v == "1"),
        ..Scope::new(kind, id, account)
    };
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
    let recent = window && input.query.get("recent").is_some_and(|v| v == "1");
    let request = Request {
        after,
        expected,
        window,
        recent,
        resume: true,
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
        deltas: crate::live_text::TextDeltas::default(),
        read_at: Instant::now(),
        list_refresh_at: None,
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
    deltas: crate::live_text::TextDeltas,
    /// When the last page was read, to pace rebuilding the conversation list.
    read_at: Instant,
    list_refresh_at: Option<Instant>,
}

impl Subscription {
    async fn read(&mut self, after: i64, expected: Option<String>) -> Result<Page> {
        self.read_at = Instant::now();
        let request = Request {
            after,
            expected,
            window: self.window,
            recent: false,
            resume: false,
        };
        page(&self.s, self.scope.clone(), request).await
    }

    async fn next(&mut self) -> Result<Option<Event>> {
        loop {
            if self.s.shutdown.is_cancelled() {
                return Ok(None);
            }
            if self.scope.list() && self.pending.is_none() {
                tokio::select! {
                    () = self.s.shutdown.cancelled() => return Ok(None),
                    () = tokio::time::sleep_until((self.read_at + LIST_INTERVAL).into()) => {},
                }
            }
            // Mark observed changes before reading the next durable page.
            self.changes.borrow_and_update();
            let opening = self.pending.is_some();
            if let Some((event, has_backlog)) = self.poll().await? {
                // Coalesce live updates without delaying the opening page or
                // throttling pages already stored for replay.
                if !opening && !has_backlog {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                return Ok(Some(event));
            }
            tokio::select! {
                () = self.s.shutdown.cancelled() => return Ok(None),
                result = self.changes.changed() => if result.is_err() { return Ok(None); },
                // Recover writes by an external maintenance process; normal
                // delivery is notification driven. Transport owns revocation.
                () = tokio::time::sleep_until(self.list_refresh_at.unwrap_or_else(|| Instant::now() + Duration::from_secs(15)).into()) => {
                    if self.list_refresh_at.is_none() && self.scope.carries_list() {
                        self.s.conversation_lists.invalidate(&self.scope.account);
                    }
                },
            }
        }
    }

    /// Reads the next page; returns a batch when it carries anything new.
    async fn poll(&mut self) -> Result<Option<(Event, bool)>> {
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
        self.list_refresh_at = current.list_refresh_at;
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
        let has_backlog = batch.more;
        let event = Event::default()
            .event("batch")
            .id(self.cursor.to_string())
            .json_data(batch)
            .map_err(Error::internal)?;
        Ok(Some((event, has_backlog)))
    }
}

/// Backwards pages use full persisted snapshots, never connection-local deltas.
/// They need only the run and its events, not the metadata a stream carries.
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
    let scope = Scope::new(kind, id, "");
    let page = s
        .store
        .read(move |db| {
            let tx = rusqlite::Transaction::new_unchecked(
                db.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let db = Db(&tx);
            let run = if scope.chat {
                crate::chats::history_run(&db, &scope.id)?
            } else {
                text(&scoped_run(&db, &scope, &Value::Null)?, "id").to_owned()
            };
            history_page(&db, &run, before, expected.as_deref().unwrap())
        })
        .await?;
    Ok(serde_json::to_value(page)?)
}

/// Both the UI's backwards pages and the agent capability read the same snapshot.
pub(crate) fn history_page(
    db: &Db<'_>,
    run: &str,
    before: i64,
    expected: &str,
) -> Result<HistoryPage> {
    let (first, _) = event_bounds(db, run)?;
    let history = format!("v1:{run}:{first}");
    if expected != history {
        return Err(Error::conflict(
            "History changed. Reconnect before loading older messages.",
        ));
    }
    let (events, has_older) = db.events_before(run, before)?;
    Ok(HistoryPage {
        oldest: events.first().map_or(before, |event| event.id),
        events,
        history,
        has_older,
    })
}
