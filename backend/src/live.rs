//! Durable replay and live delivery share one cursor. Notifications are hints;
//! SQLite is authoritative, so a slow subscriber never buffers or blocks writes.
use crate::{
    error::{Error, Result},
    http::{Input, cookie},
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use axum::response::{
    IntoResponse, Response,
    sse::{Event, KeepAlive, Sse},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

#[derive(Clone)]
struct Scope {
    chat: bool,
    id: String,
}

struct Page {
    state: Value,
    events: Vec<crate::store::Event>,
    reset: bool,
    more: bool,
    history: String,
    oldest: Option<i64>,
    has_older: bool,
}

async fn page(
    s: &Service,
    scope: Scope,
    after: i64,
    expected: Option<String>,
    window: bool,
    before: Option<i64>,
) -> Result<Page> {
    s.store
        .read(move |db| {
            // A single SQLite snapshot covers metadata and the event boundary.
            let tx = rusqlite::Transaction::new_unchecked(
                db.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let db = Db(&tx);
            let chat = if scope.chat && !scope.id.is_empty() {
                crate::chats::detail(&db, &scope.id)?
            } else {
                Value::Null
            };
            let run = if scope.chat {
                chat["run"].clone()
            } else {
                if crate::conversation_lifecycle::require_active_run(&db, &scope.id).is_err() {
                    Value::Null
                } else {
                    crate::error::required(db.run(&scope.id)?, "Run not found")?
                }
            };
            let run_id = text(&run, "id");
            let (first, max): (i64, i64) = db.0.query_row(
                "SELECT COALESCE((SELECT id FROM events WHERE run_id=?1 ORDER BY id LIMIT \
                1),0),COALESCE((SELECT id FROM events WHERE run_id=?1 ORDER BY id DESC LIMIT 1),0)",
                [run_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            // Events are append-only and IDs are AUTOINCREMENT. The retained first
            // ID changes on pruning; a new run has a new UUID. No full-history hash.
            let history = format!("v1:{run_id}:{first}");
            let reset = after > max
                || (after > 0 && after < first)
                || expected.as_ref().is_some_and(|value| *value != history);
            // Stop reading rows at the byte budget, rather than allocating an
            // entire page of large historical outputs for every subscriber.
            if before.is_some() && reset {
                return Err(Error::new(
                    409,
                    "History changed. Reconnect before loading older messages.",
                ));
            }
            let tail = before.is_some() || (window && (after == 0 || reset));
            let (events, has_older) = if tail {
                db.events_before(run_id, before.unwrap_or(i64::MAX))?
            } else {
                (
                    db.event_batch(run_id, if reset { 0 } else { after }, 100, 256 * 1024)?,
                    false,
                )
            };
            let oldest = tail.then(|| events.first().map_or(before.unwrap_or(0), |e| e.id));
            let cursor = events
                .last()
                .map_or(if reset { 0 } else { after }, |e| e.id);
            let mut artifacts: Vec<_> = if run_id.is_empty() {
                Vec::new()
            } else {
                db.keys(&format!("artifact:{run_id}:"))?
                    .into_iter()
                    .map(|(_, v)| v)
                    .collect()
            };
            artifacts.sort_by_key(|v| v["createdAt"].as_i64().unwrap_or(0));
            let mut state = json!({
                "run": run,
                "chat": chat,
                "artifacts": artifacts,
                "cacheRevision": db
                    .kv("conversation-cache-revision")?
                    .unwrap_or("initial".into()),
            });
            if scope.chat {
                state["chats"] = crate::chats::list(&db)?.into();
            }
            // Delivered messages already live in the event history. Do not resend
            // the entire conversation as metadata on every paginated stream update.
            if window
                && scope.chat
                && !scope.id.is_empty()
                && let Some(messages) = state["chat"]["messages"].as_array_mut()
            {
                messages.retain(|m| m["status"] != "delivered");
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
        })
        .await
}

pub async fn http(s: Arc<Service>, kind: &str, id: &str, input: Input) -> Result<Response> {
    if input.method != "GET" {
        return Err(Error::new(405, "Method not allowed."));
    }
    if !id.is_empty() {
        uuid(id)?;
    }
    let after = match input.headers.get("last-event-id") {
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v >= 0)
            .ok_or_else(|| Error::bad("Invalid event cursor."))?,
        None => input.number("after", 0, 0, i64::MAX)?,
    };
    let scope = Scope {
        chat: kind == "chats",
        id: id.to_owned(),
    };
    // Subscribe before reading: commits during replay remain observable.
    let changes = s.store.subscribe();
    let expected = input.query.get("history").cloned();
    if expected.as_ref().is_some_and(|v| v.len() > 200) {
        return Err(Error::bad("Invalid history version."));
    }
    let window = input.query.get("window").is_some_and(|v| v == "1");
    let first = page(&s, scope.clone(), after, expected, window, None).await?;
    let session = cookie(&input.headers);
    let subscription = Subscription {
        s,
        scope,
        window,
        changes,
        cursor: after,
        pending: Some(first),
        previous: Value::Null,
        history: None,
        session,
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
    previous: Value,
    history: Option<String>,
    session: String,
    deltas: crate::live_text::TextDeltas,
}

impl Subscription {
    async fn next(&mut self) -> Result<Option<Event>> {
        loop {
            if self.s.shutdown.is_cancelled() {
                return Ok(None);
            }
            // Mark observed changes before reading auth. Otherwise a logout
            // between the auth check and the page read can be consumed unseen.
            self.changes.borrow_and_update();
            if self.s.auth.read(&self.session).await?.is_none() {
                return Ok(None);
            }
            let mut current = match self.pending.take() {
                Some(first) => first,
                None => {
                    page(
                        &self.s,
                        self.scope.clone(),
                        self.cursor,
                        self.history.clone(),
                        self.window,
                        None,
                    )
                    .await?
                }
            };
            let run_changed = !self.previous.is_null()
                && self.previous["run"]["id"] != current.state["run"]["id"];
            if run_changed {
                current = page(&self.s, self.scope.clone(), 0, None, self.window, None).await?;
            }
            let reset = current.reset || run_changed;
            if reset {
                self.cursor = 0;
            }
            self.history = Some(current.history.clone());
            let changed = self.previous != current.state;
            let has_events = !current.events.is_empty();
            self.cursor = current.events.last().map_or(self.cursor, |e| e.id);
            let mut data = json!({
                "events": self.deltas.encode(current.events, reset)?,
                "reset": reset,
                "more": current.more,
                "history": current.history,
            });
            if let Some(oldest) = current.oldest {
                data["oldest"] = oldest.into();
                data["hasOlder"] = current.has_older.into();
            }
            if changed {
                data["state"] = current.state.clone();
            }
            self.previous = current.state;
            if changed || has_events || reset {
                let event = Event::default()
                    .event("batch")
                    .id(self.cursor.to_string())
                    .json_data(data)
                    .map_err(Error::internal)?;
                // Coalesce rapid commits without an unbounded per-client queue.
                tokio::time::sleep(Duration::from_millis(25)).await;
                return Ok(Some(event));
            }
            tokio::select! {
                _ = self.s.shutdown.cancelled() => return Ok(None),
                result = self.changes.changed() => {
                    if result.is_err() {
                        return Ok(None);
                    }
                }
                // Revalidate session expiry and recover writes by an external
                // maintenance process; normal delivery is notification driven.
                _ = tokio::time::sleep(Duration::from_secs(15)) => {}
            }
        }
    }
}

/// Backwards pages use full persisted snapshots, never connection-local deltas.
pub async fn history(s: &Service, kind: &str, id: &str, input: &Input) -> Result<Value> {
    uuid(id)?;
    let before = input.number("before", i64::MAX, 1, i64::MAX)?;
    let expected = input.query.get("history").cloned();
    if expected.as_ref().is_none_or(|v| v.len() > 200) {
        return Err(Error::bad("A history revision is required."));
    }
    let page = page(
        s,
        Scope {
            chat: kind == "chats",
            id: id.to_owned(),
        },
        0,
        expected,
        true,
        Some(before),
    )
    .await?;
    Ok(json!({
        "events": page.events,
        "history": page.history,
        "oldest": page.oldest,
        "hasOlder": page.has_older,
    }))
}
