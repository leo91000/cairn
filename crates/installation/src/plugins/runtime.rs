//! One supervisor per enabled connection: a long-lived plugin worker, the plugin's
//! native transport if any, and a pump that delivers queued events, due timers and
//! completed replies to the script.
use super::{
    capabilities::{self, Context},
    connections::{self, Connection, ensure_owner, status},
    discord,
    registry::{self, Package},
    work::{self, DELIVERIES, EVENTS},
};
use crate::{
    config::now,
    error::{Error, Result},
    service::Service,
    validation::text,
};
use cairn_plugin_host::Session;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

const ATTEMPTS: u64 = 5;

pub async fn maintain(s: Arc<Service>) {
    let mut active: HashMap<String, (String, CancellationToken, tokio::task::JoinHandle<()>)> =
        HashMap::new();
    let mut timer = tokio::time::interval(Duration::from_secs(2));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            () = s.plugins.changed.notified() => {},
            _ = timer.tick() => {},
        }
        let Ok(values) = s.store.list(connections::KIND).await else {
            continue;
        };
        let mut wanted = HashMap::new();
        for connection in values
            .into_iter()
            .filter_map(|value| serde_json::from_value::<Connection>(value).ok())
            .filter(|connection| connection.enabled)
        {
            // A new revision or a different approved package restarts the worker.
            let package = registry::active(&s, &connection.plugin_id).await;
            let key = format!(
                "{}:{}",
                connection.revision,
                package
                    .as_ref()
                    .map_or("unavailable", |package| package.digest.as_str())
            );
            wanted.insert(connection.id.clone(), (key, connection, package));
        }
        let stale = active
            .iter()
            .filter(|(id, (key, _, _))| wanted.get(*id).is_none_or(|(wanted, _, _)| wanted != key))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in stale {
            if let Some((_, stop, task)) = active.remove(&id) {
                stop.cancel();
                let _ = task.await;
            }
        }
        for (id, (key, connection, package)) in wanted {
            if active.contains_key(&id) {
                continue;
            }
            let stop = s.shutdown.child_token();
            let task = tokio::spawn(run(s.clone(), connection, package, stop.clone()));
            active.insert(id, (key, stop, task));
        }
    }
    for (_, (_, stop, task)) in active {
        stop.cancel();
        let _ = task.await;
    }
}

async fn run(
    s: Arc<Service>,
    connection: Connection,
    package: Result<Package>,
    stop: CancellationToken,
) {
    let Ok(package) = package else {
        status(&s, &connection, "needs_review").await;
        return;
    };
    let mut retry = Duration::from_secs(5);
    loop {
        status(&s, &connection, "connecting").await;
        let result = tokio::select! {
            () = stop.cancelled() => return,
            result = serve(&s, &connection, &package, &stop) => result,
        };
        if let Err(error) = result {
            let permanent = matches!(error.status, 401 | 403);
            status(
                &s,
                &connection,
                if permanent {
                    "needs_review"
                } else {
                    "disconnected"
                },
            )
            .await;
            if permanent {
                return;
            }
        }
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(retry) => {},
        }
        retry = (retry * 2).min(Duration::from_secs(60));
    }
}

async fn serve(
    s: &Service,
    connection: &Connection,
    package: &Package,
    stop: &CancellationToken,
) -> Result<()> {
    ensure_owner(s, connection).await?;
    let mut session = s
        .plugins
        .host
        .start(&s.plugins.executable()?, &package.manifest, &package.source)
        .await
        .map_err(Error::bad_gateway)?;
    if let Err(error) = begin(s, connection, &mut session).await {
        session.stop().await;
        return Err(error);
    }
    let transport = async {
        if package.manifest.permits_capability("transport.discord") {
            discord::transport(s, connection, stop).await
        } else {
            status(s, connection, "connected").await;
            stop.cancelled().await;
            Ok(())
        }
    };
    let events = async {
        let mut owner_check = tokio::time::Instant::now();
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = stop.cancelled() => return Ok(()),
                _ = timer.tick() => {},
            }
            if owner_check.elapsed() >= Duration::from_secs(30) {
                ensure_owner(s, connection).await?;
                owner_check = tokio::time::Instant::now();
            }
            pump(s, connection, &mut session).await?;
        }
    };
    let result = tokio::try_join!(transport, events).map(|_| ());
    session.stop().await;
    result
}

/// Sent once to every new worker so packages can prepare, e.g. schedule timers.
pub async fn begin(s: &Service, connection: &Connection, session: &mut Session) -> Result<()> {
    let event = json!({ "name": "start", "revision": connection.revision });
    dispatch(
        s,
        connection,
        session,
        &event,
        "start",
        json!({ "connectionId": connection.id }),
    )
    .await
    .map(|_| ())
    .map_err(Error::bad_gateway)
}

/// Delivers everything due for this connection once. Returns an error when the
/// worker must be replaced; individual handler failures are retried with backoff.
pub async fn pump(s: &Service, connection: &Connection, session: &mut Session) -> Result<()> {
    release_timers(s, connection).await?;
    let mut events = owned(s, EVENTS, connection).await?;
    events.sort_by_key(|value| value["createdAt"].as_i64().unwrap_or(0));
    for mut event in events {
        if event["revision"] != connection.revision {
            remove(s, EVENTS, &event).await?;
            continue;
        }
        let name = text(&event, "name").to_owned();
        let result = dispatch(
            s,
            connection,
            session,
            &event,
            &name,
            event["payload"].clone(),
        )
        .await;
        match result {
            Ok(_) => remove(s, EVENTS, &event).await?,
            Err(_) => {
                if retry_later(s, EVENTS, &mut event).await? {
                    audit_failure(s, connection, &name).await?;
                }
            }
        }
        if !session.alive() {
            return Err(Error::bad_gateway("Plugin worker stopped."));
        }
    }
    for mut delivery in owned(s, DELIVERIES, connection).await? {
        if delivery["revision"] != connection.revision
            || now() - delivery["createdAt"].as_i64().unwrap_or(0) > 86_400_000
        {
            work::finish(s, &delivery, "failed").await?;
            continue;
        }
        let Some(text) = work::reply(s, connection, &delivery).await? else {
            continue;
        };
        let payload = json!({
            "deliveryId": delivery["id"],
            "text": text,
            "reply": delivery["reply"],
            "state": delivery["state"],
        });
        let event = json!({ "name": "reply", "revision": connection.revision });
        match dispatch(s, connection, session, &event, "reply", payload).await {
            // A plugin asks to be called again later, e.g. after a platform rate limit.
            Ok(value) if value["retryAfter"].is_number() => {
                let seconds = value["retryAfter"]
                    .as_f64()
                    .unwrap_or(5.0)
                    .clamp(1.0, 300.0);
                delivery["nextAttemptAt"] = json!(now() + (seconds * 1000.0) as i64);
                delivery["state"] = value["state"].clone();
                s.store.put(DELIVERIES, delivery).await?;
                break;
            }
            Ok(_) => work::finish(s, &delivery, "replied").await?,
            Err(_) => {
                if retry_later(s, DELIVERIES, &mut delivery).await? {
                    work::finish(s, &delivery, "failed").await?;
                }
            }
        }
        if !session.alive() {
            return Err(Error::bad_gateway("Plugin worker stopped."));
        }
    }
    Ok(())
}

async fn dispatch(
    s: &Service,
    connection: &Connection,
    session: &mut Session,
    event: &Value,
    name: &str,
    payload: Value,
) -> std::result::Result<Value, String> {
    let context = Context {
        connection: connection.clone(),
        manifest: session.manifest().clone(),
        event: event.clone(),
    };
    let context = &context;
    session
        .dispatch(name, payload, |method, args| async move {
            capabilities::call(s, context, &method, args).await
        })
        .await
}

async fn owned(s: &Service, kind: &str, connection: &Connection) -> Result<Vec<Value>> {
    let time = now();
    Ok(s.store
        .list(kind)
        .await?
        .into_iter()
        .filter(|value| value["connectionId"] == connection.id)
        .filter(|value| value["nextAttemptAt"].as_i64().unwrap_or(0) <= time)
        .collect())
}

async fn remove(s: &Service, kind: &'static str, value: &Value) -> Result<()> {
    let id = text(value, "id").to_owned();
    s.store.transaction(move |db| db.remove(kind, &id)).await
}

/// Schedules another attempt; returns true when attempts are exhausted and the
/// record was given up (events are removed; deliveries are finished by the caller).
async fn retry_later(s: &Service, kind: &'static str, value: &mut Value) -> Result<bool> {
    let attempts = value["attempts"].as_u64().unwrap_or(0) + 1;
    if attempts >= ATTEMPTS {
        if kind == EVENTS {
            remove(s, kind, value).await?;
        }
        return Ok(true);
    }
    value["attempts"] = json!(attempts);
    value["nextAttemptAt"] = json!(now() + 5000 * attempts as i64);
    s.store.put(kind, value.clone()).await?;
    Ok(false)
}

async fn audit_failure(s: &Service, connection: &Connection, name: &str) -> Result<()> {
    let detail = json!({ "connectionId": connection.id, "event": name });
    s.store
        .transaction(move |db| db.audit("plugin.event.failed", &detail))
        .await
}

/// Moves due timers into the event queue in one transaction.
async fn release_timers(s: &Service, connection: &Connection) -> Result<()> {
    let connection = connection.clone();
    s.store
        .transaction(move |db| {
            for timer in db.list("plugin-timers")? {
                if timer["connectionId"] != connection.id
                    || timer["at"].as_i64().unwrap_or(0) > now()
                {
                    continue;
                }
                db.remove("plugin-timers", text(&timer, "id"))?;
                if timer["revision"] == connection.revision {
                    work::enqueue(
                        db,
                        &connection,
                        "timer",
                        &json!({ "id": timer["id"], "name": timer["name"], "payload": timer["payload"] }),
                        None,
                    )?;
                }
            }
            Ok(())
        })
        .await
}
