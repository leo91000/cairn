//! One supervisor per enabled connection: a long-lived plugin worker, a second one
//! driving the package's socket if it has one, and a pump that delivers queued
//! events, due timers and completed replies to the script.
use super::{
    capabilities::{self, Context},
    connections::{self, Connection, ensure_owner, status, status_with},
    registry::{self, Package},
    work::{self, DELIVERIES, EVENTS},
};
use crate::{
    config::now,
    error::{Error, Result},
    service::Service,
    validation::text,
};
use cairn_plugin_host::{InvocationError, Session};
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
            if matches!(error.status, 401 | 403) {
                status_with(&s, &connection, "needs_review", Some(&error.message)).await;
                return;
            }
            status(&s, &connection, "disconnected").await;
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
    if super::program_installation::needs_preparation(s, package) {
        super::program_installation::prepare(s, package, None).await?;
    }
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
    // Packages with their own connection report its state; others are ready now.
    let transport = async {
        if package.manifest.permits_capability("socket") {
            super::socket::run(s, connection, package, stop).await
        } else if package.manifest.permits_capability("inbound") {
            stop.cancelled().await;
            Ok(())
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
        json!({ "connectionId": connection.id, "settings": connection.settings }),
    )
    .await
    .map(|_| ())
    // Package start refusals remain retryable (502), unlike socket start's
    // explicit 403/needs_review policy. Preserve its existing diagnostic text.
    .map_err(|error| match error {
        InvocationError::Rejected(message) => Error::bad_gateway(format!("rejected: {message}")),
        InvocationError::Failed(message) => Error::bad_gateway(message),
    })
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
        // Connection-wide revocation still ends the session. A person revoked or a
        // conversation deleted affects only that delivery, not every other user.
        let expected = connection.clone();
        s.store
            .read(move |db| connections::authorize(db, &expected))
            .await?;
        let reply = match work::reply(s, connection, &delivery).await {
            Ok(reply) => reply,
            Err(error) if matches!(error.status, 403 | 404) => {
                work::finish(s, &delivery, "failed").await?;
                continue;
            }
            Err(error) => return Err(error),
        };
        let Some(reply) = reply else {
            if let Err(error) = follow(s, connection, session, &mut delivery).await {
                if matches!(error.status, 403 | 404) {
                    work::finish(s, &delivery, "failed").await?;
                } else {
                    return Err(error);
                }
            }
            if !session.alive() {
                return Err(Error::bad_gateway("Plugin worker stopped."));
            }
            continue;
        };
        let payload = json!({
            "deliveryId": delivery["id"],
            "conversation": delivery["chatId"],
            "userId": delivery["userId"],
            "thread": delivery["thread"],
            "text": reply.text,
            "status": reply.status,
            "files": reply.files,
            "reply": delivery["reply"],
            "state": delivery["state"],
        });
        // Uploads are limited to this reply's deliverables.
        let event = json!({
            "name": "reply",
            "revision": connection.revision,
            "files": reply.files,
            "routing": delivery["routing"],
        });
        let outcome = match reply.status {
            "completed" => "replied",
            "cancelled" => "stopped",
            _ => "failed",
        };
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
            Ok(_) => work::finish(s, &delivery, outcome).await?,
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

/// While the agent works: each new question once, and progress every eight seconds
/// (long enough for a platform's typing indicator to stay visible).
async fn follow(
    s: &Service,
    connection: &Connection,
    session: &mut Session,
    delivery: &mut Value,
) -> Result<()> {
    let mut changed = false;
    for question in work::questions(s, connection, delivery).await? {
        let asked = delivery["asked"]
            .as_array()
            .is_some_and(|asked| asked.contains(&question["id"]));
        if asked
            || delivery["questionRetryAt"][text(&question, "id")]
                .as_i64()
                .unwrap_or(0)
                > now()
        {
            continue;
        }
        let payload = json!({
            "deliveryId": delivery["id"],
            "conversation": delivery["chatId"],
            "userId": delivery["userId"],
            "thread": delivery["thread"],
            "question": question,
            "reply": delivery["reply"],
        });
        let event = json!({ "name": "question", "revision": connection.revision, "routing": delivery["routing"] });
        match dispatch(s, connection, session, &event, "question", payload).await {
            Ok(value) if !value["retryAfter"].is_number() => {
                s.store
                    .set(
                        &work::question_seen_key(connection, delivery, &question),
                        json!(true),
                        Some(now() + 30 * 86_400_000),
                    )
                    .await?;
                match delivery["asked"].as_array_mut() {
                    Some(asked) => asked.push(question["id"].clone()),
                    None => delivery["asked"] = json!([question["id"]]),
                }
            }
            result => {
                let seconds = result
                    .ok()
                    .and_then(|value| value["retryAfter"].as_f64())
                    .unwrap_or(5.0)
                    .clamp(1.0, 300.0);
                if !delivery["questionRetryAt"].is_object() {
                    delivery["questionRetryAt"] = json!({});
                }
                delivery["questionRetryAt"][text(&question, "id")] =
                    json!(now() + (seconds * 1000.0) as i64);
            }
        }
        changed = true;
    }
    if now() - delivery["progressAt"].as_i64().unwrap_or(0) >= 8000
        && let Some(progress) = work::progress(s, connection, delivery).await?
    {
        let payload = json!({
            "deliveryId": delivery["id"],
            "conversation": delivery["chatId"],
            "userId": delivery["userId"],
            "thread": delivery["thread"],
            "state": progress["state"],
            "step": progress["step"],
            "reply": delivery["reply"],
        });
        let event = json!({ "name": "progress", "revision": connection.revision, "routing": delivery["routing"] });
        let _ = dispatch(s, connection, session, &event, "progress", payload).await;
        delivery["progressAt"] = json!(now());
        changed = true;
    }
    if changed {
        s.store.put(DELIVERIES, delivery.clone()).await?;
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
) -> std::result::Result<Value, InvocationError> {
    let digest = registry::digest(session.manifest(), session.sources())
        .map_err(|e| InvocationError::Failed(e.message))?;
    registry::require_digest(s, &connection.plugin_id, &digest)
        .await
        .map_err(|e| InvocationError::Failed(e.message))?;
    let connection = super::routes::bound(s, connection, &event["routing"])
        .await
        .map_err(|e| InvocationError::Failed(e.message))?;
    if let Some(author) = event["authorId"].as_str() {
        super::routes::require_person(&connection, author)
            .map_err(|e| InvocationError::Failed(e.message))?;
    }
    let context = Context {
        connection,
        manifest: session.manifest().clone(),
        event: event.clone(),
        link: None,
        draft: None,
        resource_grant: None,
    };
    let context = &context;
    let digest_ref = &digest;
    let result = session
        .dispatch(name, payload, |method, args| async move {
            capabilities::approved(s, context, digest_ref, &method, args)
                .await
                .map_err(|error| cairn_plugin_host::Denied(capabilities::code(&error)))
        })
        .await?;
    registry::require_digest(s, &context.manifest.id, &digest)
        .await
        .map_err(|e| InvocationError::Failed(e.message))?;
    Ok(result)
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
    s.store
        .transaction(move |db| {
            if kind == EVENTS {
                work::remove_event(db, &id)
            } else {
                db.remove(kind, &id)
            }
        })
        .await
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
