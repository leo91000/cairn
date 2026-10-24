//! Durable work for delegated connections: admission of external messages, the event
//! queue plugins consume, conversation submission and correlated replies.
use super::{
    capabilities::Context,
    connections::{AccessMode, Connection, Label, authorize, ensure_owner, external_id, label},
    registry,
};
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use cairn_plugin_api::Manifest;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::sync::Arc;

pub const EVENTS: &str = "plugin-events";
pub const DELIVERIES: &str = "plugin-deliveries";
pub const CHECKPOINTS: &str = "plugin-delivery-checkpoints";

/// Remove the native event and its idempotency receipts atomically. Consent
/// remains valid independently of whether its URL delivery has finished.
pub(crate) fn remove_event(db: &Db<'_>, event: &str) -> Result<()> {
    super::oauth::remove_event_receipts_in(db, event)?;
    db.remove(EVENTS, event)
}

/// Event names the host sends itself; packages can't admit messages under them.
pub const RESERVED: &[&str] = &[
    "start",
    "socket",
    "timer",
    "reply",
    "progress",
    "question",
    "access_requested",
    "view",
    "action",
    "test",
    "form",
    "command",
    "tool",
    "webhook",
];

/// A message a transport received for a connection, already scoped by the transport.
pub struct Inbound {
    /// The event plugins receive: `message`, or e.g. `interaction` for buttons.
    pub kind: String,
    /// An optional capacity preflight for a message that will submit new work.
    /// Control messages omit it; submission always enforces native limits.
    pub starts_work: bool,
    /// The platform's message ID, used for deduplication and replies.
    pub id: String,
    pub author: super::connections::Label,
    /// Routing information plugins need to answer (channel, bot ID…). Never content.
    pub context: Value,
    /// What the person wrote, stored only for allowed people.
    pub body: Value,
}

fn merge(base: &Value, extra: &Value) -> Value {
    let mut value = base.clone();
    if let (Some(target), Some(source)) = (value.as_object_mut(), extra.as_object()) {
        for (key, item) in source {
            target.insert(key.clone(), item.clone());
        }
    }
    value
}

pub(crate) fn enqueue(
    db: &Db<'_>,
    connection: &Connection,
    name: &str,
    payload: &Value,
    author: Option<&str>,
    starts_work: bool,
) -> Result<()> {
    let routing = match author {
        Some(author) => super::routes::admit_person(db, connection, author)?,
        None => connection.routing.clone(),
    };
    let mut event = json!({
        "id": id(),
        "connectionId": connection.id,
        "revision": connection.revision,
        "routing": routing,
        "name": name,
        "payload": payload,
        "authorId": author,
        "attempts": 0,
        "createdAt": now(),
        "nextAttemptAt": now(),
    });
    if starts_work {
        event["startsWork"] = json!(true);
    }
    db.put(EVENTS, &event)?;
    Ok(())
}

/// Fast durable ingress. Messages from people who aren't allowed are discarded before
/// their content is stored; approval mode records only who asked.
#[derive(Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Admission {
    Accepted,
    Duplicate,
    Denied,
    Requested,
    Busy,
}

pub async fn accept(s: &Service, connection: &Connection, inbound: Inbound) -> Result<bool> {
    Ok(deliver(s, connection, inbound).await? == Admission::Accepted)
}

/// An HTTP or socket transport must only acknowledge temporary saturation as a retry.
pub async fn deliver(s: &Service, connection: &Connection, inbound: Inbound) -> Result<Admission> {
    deliver_approved(s, connection, inbound, None).await
}

async fn deliver_approved(
    s: &Service,
    connection: &Connection,
    inbound: Inbound,
    digest: Option<&str>,
) -> Result<Admission> {
    // Allowlists only change with a new revision (or shrink), so this snapshot is a
    // superset: strangers are dropped without touching storage or owner authority.
    if connection.access_mode == AccessMode::Allowlist
        && !connection.user_ids.contains(&inbound.author.id)
    {
        return Ok(Admission::Denied);
    }
    let package = registry::approved(s, &connection.plugin_id).await?;
    let expected_digest = digest.or_else(|| {
        connection
            .routing
            .as_ref()
            .map(|route| route.digest.as_str())
    });
    if expected_digest.is_some_and(|digest| digest != package.package.digest) {
        return Err(Error::forbidden(
            "The plugin worker's package approval changed.",
        ));
    }
    ensure_owner(s, connection).await?;
    registry::require_digest(s, &connection.plugin_id, &package.package.digest).await?;
    let approval = package.snapshot();
    let expected = connection.clone();
    s.store
        .transaction(move |db| {
            // Approval and admission commit together, including after slow
            // membership checks or a wait for the store transaction.
            approval.recheck_in(db)?;
            let current = authorize(db, &expected)?;
            let author = json!({ "id": inbound.author.id, "name": label(&inbound.author.name) });
            // The host's identity fields always win over the package's own.
            let mut identity = json!({ "id": inbound.id, "author": author });
            if let Some(routing) = &current.routing {
                identity["resource"] = json!(routing.resource);
                identity["route"] = json!(routing.id);
                identity["context"] = inbound.context.clone();
            }
            let base = merge(&inbound.context, &identity);
            if !current.allows(&inbound.author.id) {
                if current.access_mode == AccessMode::Requests && inbound.kind == "message" {
                    return request(db, &current, &inbound.author, &base);
                }
                return Ok(Admission::Denied);
            }
            let namespace = current.routing.as_ref().map_or_else(
                || current.id.clone(),
                |r| format!("{}:{}", current.id, r.resource),
            );
            let dedupe = format!("plugin-seen:{namespace}:{}", inbound.id);
            if db.kv(&dedupe)?.is_some() {
                return Ok(Admission::Duplicate);
            }
            let pending = db
                .list(EVENTS)?
                .iter()
                .filter(|value| value["connectionId"] == current.id)
                .count();
            if pending >= 50 {
                return Ok(Admission::Busy);
            }
            if inbound.starts_work {
                match limits(db, &current, &inbound.author.id) {
                    Err(error) if error.status == 429 => return Ok(Admission::Busy),
                    Err(error) => return Err(error),
                    Ok(_) => {}
                }
            }
            enqueue(
                db,
                &current,
                &inbound.kind,
                &merge(&merge(&inbound.context, &inbound.body), &identity),
                Some(&inbound.author.id),
                inbound.starts_work,
            )?;
            db.set(&dedupe, &json!(true), Some(now() + 30 * 86_400_000))?;
            Ok(Admission::Accepted)
        })
        .await
}

/// One request per person. The plugin hears about it once, without message content.
fn request(
    db: &Db<'_>,
    connection: &Connection,
    author: &super::connections::Label,
    payload: &Value,
) -> Result<Admission> {
    let requests = db.list("plugin-requests")?;
    if let Some(mut existing) = requests
        .iter()
        .find(|value| {
            value["connectionId"] == connection.id
                && value["userId"] == author.id
                && value["routeId"].as_str() == connection.routing.as_ref().map(|r| r.id.as_str())
        })
        .cloned()
    {
        if existing["status"] == "pending" {
            existing["mentions"] = json!(existing["mentions"].as_u64().unwrap_or(1) + 1);
            existing["updatedAt"] = json!(now());
            db.put("plugin-requests", &existing)?;
        }
        return Ok(if existing["status"] == "pending" {
            Admission::Requested
        } else {
            Admission::Denied
        });
    }
    if requests
        .iter()
        .filter(|value| value["connectionId"] == connection.id)
        .count()
        >= 50
    {
        return Ok(Admission::Busy);
    }
    if db
        .list(EVENTS)?
        .iter()
        .filter(|value| value["connectionId"] == connection.id)
        .count()
        >= 50
    {
        return Ok(Admission::Busy);
    }
    db.put(
        "plugin-requests",
        &json!({
            "id": id(),
            "connectionId": connection.id,
            "routeId": connection.routing.as_ref().map(|r| &r.id),
            "userId": author.id,
            "name": label(&author.name),
            "status": "pending",
            "mentions": 1,
            "createdAt": now(),
            "updatedAt": now(),
        }),
    )?;
    enqueue(db, connection, "access_requested", payload, None, false)?;
    db.audit(
        "plugin.access.requested",
        &json!({ "connectionId": connection.id }),
    )?;
    Ok(Admission::Requested)
}

/// Events that act for a person: what they wrote or a button they pressed, admitted
/// through `inbound.accept`.
fn personal(event: &Value) -> bool {
    event["authorId"].is_string()
        && event["name"]
            .as_str()
            .is_some_and(|name| !RESERVED.contains(&name))
}

fn size(value: &Value) -> usize {
    serde_json::to_string(value).map_or(usize::MAX, |json| json.len())
}

/// `inbound.accept`: a message the package received through its own connection
/// (a socket, or polling with timers). Admission rules apply before anything is kept.
/// Returns whether it was queued for the package.
pub(crate) async fn inbound(
    s: &Service,
    context: &Context,
    args: &Value,
    detailed: bool,
    digest: Option<&str>,
) -> Result<Value> {
    let kind = match text(args, "kind") {
        "" => "message",
        kind => kind,
    };
    let valid_kind = kind.len() <= 40
        && kind.starts_with(|c: char| c.is_ascii_lowercase())
        && kind
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        && !RESERVED.contains(&kind);
    let id = text(args, "id");
    let author = &args["author"];
    let extra = args.get("context").cloned().unwrap_or_else(|| json!({}));
    let body = args.get("body").cloned().unwrap_or_else(|| json!({}));
    let starts_work = args["startsWork"].as_bool().unwrap_or(false);
    if args
        .get("startsWork")
        .is_some_and(|value| !value.is_boolean())
        || (starts_work && kind != "message")
        || !valid_kind
        || id.is_empty()
        || id.len() > 200
        || id.chars().any(char::is_control)
        || !external_id(text(author, "id"))
        || !extra.is_object()
        || !body.is_object()
        || size(&extra) > 8192
        || size(&body) > 262_144
    {
        return Err(Error::bad("Invalid inbound message."));
    }
    let inbound = Inbound {
        kind: kind.to_owned(),
        starts_work,
        id: id.to_owned(),
        author: Label {
            id: text(author, "id").to_owned(),
            name: label(text(author, "name")),
        },
        context: extra,
        body,
    };
    let by_resource = context
        .manifest
        .resources
        .iter()
        .any(|resource| resource.delegation);
    let result = if by_resource || args.get("resource").is_some() {
        let resource = args["resource"]
            .as_str()
            .ok_or_else(|| Error::bad("Name the resource that received this event."))?;
        match super::routes::select(s, &context.connection, resource, &inbound.context).await? {
            Some(connection) => deliver_approved(s, &connection, inbound, digest).await?,
            None => Admission::Denied,
        }
    } else {
        deliver_approved(s, &context.connection, inbound, digest).await?
    };
    Ok(if detailed {
        json!({ "status": result })
    } else {
        json!(result == Admission::Accepted)
    })
}

fn binding(connection: &Connection, author: &str, thread: &str, shared: bool) -> String {
    let thread = crate::auth::hex_digest(thread);
    let namespace = connection.delegation_key();
    if shared {
        // A shared thread (e.g. a Discord thread) is one conversation for everyone
        // allowed on this connection who writes in it.
        format!("plugin-chat:{namespace}:thread:{thread}")
    } else {
        // Otherwise the author is part of the key: a plugin chooses threads, never people.
        format!("plugin-chat:{namespace}:{author}:{thread}")
    }
}

fn admit(db: &Db<'_>, expected: &Connection, event: &Value) -> Result<(Connection, String)> {
    let connection = authorize(db, expected)?;
    let author = text(event, "authorId").to_owned();
    super::routes::require_person(&connection, &author)?;
    if !connection.allows(&author) || event["revision"] != connection.revision {
        return Err(Error::forbidden("This person is no longer allowed."));
    }
    Ok((connection, author))
}

/// Queued messages can outlive the capacity available at admission. Check the
/// current native policy before invoking package effects. Submitted events must
/// still reach their idempotent receipt even if their own delivery fills the queue.
pub(crate) async fn submission_ready(
    s: &Service,
    expected: &Connection,
    event: &Value,
) -> Result<bool> {
    let expected = expected.clone();
    let event = event.clone();
    s.store
        .read(move |db| {
            let (connection, author) = admit(db, &expected, &event)?;
            let namespace = connection.routing.as_ref().map_or_else(
                || connection.id.clone(),
                |r| format!("{}:{}", connection.id, r.resource),
            );
            let receipt = format!(
                "plugin-submitted:{namespace}:{}",
                text(&event["payload"], "id")
            );
            if db.kv(&receipt)?.is_some() {
                return Ok(true);
            }
            match limits(db, &connection, &author) {
                Err(error) if error.status == 429 => Ok(false),
                Err(error) => Err(error),
                Ok(_) => Ok(true),
            }
        })
        .await
}

/// Hourly counters to increment when the submission is accepted.
struct Counters(Vec<(String, u64)>);

impl Counters {
    fn increment(&self, db: &Db<'_>) -> Result<()> {
        let hour = now() / 3_600_000;
        for (key, count) in &self.0 {
            db.set(key, &json!(count + 1), Some((hour + 1) * 3_600_000))?;
        }
        Ok(())
    }
}

fn limits(db: &Db<'_>, connection: &Connection, author: &str) -> Result<Counters> {
    let hour = now() / 3_600_000;
    let used = |key: &str| -> Result<u64> {
        Ok(db.kv(key)?.and_then(|value| value.as_u64()).unwrap_or(0))
    };
    let rate_key = format!("plugin-rate:{}:{hour}", connection.delegation_key());
    let count = used(&rate_key)?;
    if count >= u64::from(connection.max_messages_per_hour) {
        return Err(Error::too_many_requests(
            "Connection message limit reached.",
        ));
    }
    let mut counters = vec![(rate_key, count)];
    // When anyone can write, nobody may use up the whole connection alone.
    if connection.access_mode == AccessMode::Everyone {
        let key = format!(
            "plugin-rate:{}:{author}:{hour}",
            connection.delegation_key()
        );
        let personal = used(&key)?;
        if personal >= u64::from(connection.max_per_person_per_hour) {
            return Err(Error::too_many_requests("Personal message limit reached."));
        }
        counters.push((key, personal));
    }
    pending_capacity(db, &connection.id)?;
    Ok(Counters(counters))
}

fn pending_capacity(db: &Db<'_>, connection: &str) -> Result<()> {
    let outstanding = db
        .list(DELIVERIES)?
        .iter()
        .filter(|value| value["connectionId"] == connection)
        .count();
    if outstanding >= 20 {
        return Err(Error::too_many_requests(
            "Connection has too much pending work.",
        ));
    }
    Ok(())
}

/// Reserve forwarding before a native wait is committed. Each continuation has
/// its own native input ID and reply checkpoint, with the original source scope.
pub(crate) fn reserve_continuation(
    db: &Db<'_>,
    chat: &Value,
    run: &Value,
    wakeup: &Value,
) -> Result<()> {
    let source = text(&run["chatExecution"], "messageId");
    let Some(mut delivery) = db.get(DELIVERIES, source)? else {
        if matches!(
            super::requesters::recorded(db, text(chat, "id"), source)?,
            Some(super::requesters::Principal::External(_))
        ) {
            return Err(Error::conflict(
                "This external turn's reply already settled. Start a new request before waiting.",
            ));
        }
        return Ok(());
    };
    if delivery["chatId"] != chat["id"] || wakeup["messageId"] != source {
        return Err(Error::conflict("The continuation's source turn changed."));
    }
    let previous = text(&chat["wakeup"], "id");
    if let Some(replaced) = db.get(DELIVERIES, previous)?
        && replaced["continuationOf"] == source
        && replaced["chatId"] == chat["id"]
        && replaced["connectionId"] == delivery["connectionId"]
    {
        if db.get(CHECKPOINTS, &checkpoint_key(previous))?.is_some() {
            return Err(Error::conflict(
                "The previous continuation already began its reply.",
            ));
        }
        remove_delivery(db, &replaced)?;
    }
    pending_capacity(db, text(&delivery, "connectionId"))?;
    let fields = delivery.as_object_mut().unwrap();
    for field in ["questionRetryAt", "progressAt", "state"] {
        fields.remove(field);
    }
    delivery["id"] = wakeup["id"].clone();
    delivery["continuationOf"] = json!(source);
    delivery["asked"] = json!([]);
    delivery["attempts"] = json!(0);
    delivery["createdAt"] = json!(now());
    delivery["nextAttemptAt"] = json!(now());
    db.put(DELIVERIES, &delivery)?;
    for mut activity in db.list("plugin-activity")? {
        if activity["chatId"] == chat["id"]
            && (activity["deliveryId"] == source || activity["deliveryId"] == previous)
        {
            activity["deliveryId"] = wakeup["id"].clone();
            activity["updatedAt"] = json!(now());
            db.put("plugin-activity", &activity)?;
        }
    }
    Ok(())
}

/// Files a person attached, fetched from hosts the plugin declares (at most four,
/// 10 MB each). Fetching happens before any conversation changes.
async fn download<F, Fut>(
    s: &Service,
    manifest: &Manifest,
    files: &Value,
    check: F,
) -> Result<Vec<(String, Vec<u8>)>>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let files = files.as_array().cloned().unwrap_or_default();
    if files.len() > 4 {
        return Err(Error::bad("Attach up to four files."));
    }
    let mut downloaded = Vec::new();
    for file in files {
        let target = super::capabilities::target(s, manifest, text(&file, "url"), "https")?;
        check().await?;
        let response = s
            .plugins
            .http
            .get(target)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| Error::bad_gateway("The attachment is unavailable."))?;
        if !response.status().is_success()
            || response.content_length().unwrap_or(0) > crate::attachments::MAX_FILE as u64
        {
            return Err(Error::bad(
                "The attachment is unavailable or larger than 10 MB.",
            ));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
            let chunk = chunk.map_err(|_| Error::bad_gateway("The attachment was interrupted."))?;
            if bytes.len() + chunk.len() > crate::attachments::MAX_FILE {
                return Err(Error::too_large("Attachments must be 10 MB or smaller."));
            }
            bytes.extend_from_slice(&chunk);
        }
        let name = label(text(&file, "name"));
        downloaded.push((
            if name.is_empty() {
                "attachment".into()
            } else {
                name
            },
            bytes,
        ));
    }
    Ok(downloaded)
}

/// `conversations.submit`: only while handling a `message` event, on behalf of that
/// event's author. Identity and routing come from ingress, never from script arguments.
pub async fn submit(s: &Service, context: &Context, args: &Value) -> Result<Value> {
    submit_approved(s, context, args, None).await
}

async fn submission_authority(
    s: &Service,
    context: &Context,
    package: &registry::ApprovedPackage,
) -> Result<()> {
    ensure_owner(s, &context.connection).await?;
    registry::require_digest(s, &context.manifest.id, &package.package.digest).await?;
    let approval = package.snapshot();
    let expected = context.connection.clone();
    let event = context.event.clone();
    s.store
        .transaction(move |db| {
            approval.recheck_in(db)?;
            admit(db, &expected, &event).map(|_| ())
        })
        .await
}

pub(crate) async fn submit_approved(
    s: &Service,
    context: &Context,
    args: &Value,
    digest: Option<&str>,
) -> Result<Value> {
    let (expected, event) = (&context.connection, &context.event);
    if event["name"] != "message" {
        return Err(Error::forbidden(
            "Conversations start only from a received message.",
        ));
    }
    let package = Arc::new(registry::approved(s, &context.manifest.id).await?);
    if digest.is_some_and(|digest| digest != package.package.digest) {
        return Err(Error::forbidden(
            "The plugin worker's package approval changed.",
        ));
    }
    ensure_owner(s, expected).await?;
    let content = text(args, "text").trim().to_owned();
    let thread = text(args, "thread").to_owned();
    let shared = args["shared"] == true;
    let new_turn = args["newTurn"] == true;
    if args.get("newTurn").is_some_and(|value| !value.is_boolean())
        || (new_turn && expected.routing.is_none())
    {
        return Err(Error::bad(
            "A new turn requires a boolean option and resource-routed admission.",
        ));
    }
    let reply = args.get("reply").cloned().unwrap_or(Value::Null);
    if content.len() > 50_000
        || thread.chars().count() > 200
        || (shared && thread.is_empty())
        || serde_json::to_string(&reply)?.len() > 4096
    {
        return Err(Error::bad("Invalid conversation message."));
    }
    let files = download(s, &context.manifest, &args["files"], || {
        submission_authority(s, context, &package)
    })
    .await?;
    if content.is_empty() && files.is_empty() {
        return Err(Error::bad("Write a message or attach a file."));
    }
    let external_id = text(&event["payload"], "id").to_owned();
    let namespace = expected.routing.as_ref().map_or_else(
        || expected.id.clone(),
        |r| format!("{}:{}", expected.id, r.resource),
    );
    let dedupe = format!("plugin-submitted:{namespace}:{external_id}");

    submission_authority(s, context, &package).await?;

    // Resolve (or start) the conversation first, so attachments can be stored in it.
    let resolved = {
        let (expected, event, dedupe, thread) = (
            expected.clone(),
            event.clone(),
            dedupe.clone(),
            thread.clone(),
        );
        let approval = package.clone();
        s.store
            .transaction(move |db| {
                approval.recheck_in(db)?;
                let (connection, author) = admit(db, &expected, &event)?;
                if let Some(receipt) = db.kv(&dedupe)? {
                    return Ok(Err(receipt));
                }
                limits(db, &connection, &author)?;
                let binding_key = binding(&connection, &author, &thread, shared);
                let bound = db.kv(&binding_key)?.and_then(|value| value.as_str().map(str::to_owned));
                let usable = bound
                    .as_deref()
                    .and_then(|id| db.get("chats", id).ok().flatten())
                    .filter(|chat| {
                        crate::conversation_lifecycle::is_active(chat)
                            && chat["agentId"] == connection.agent_id
                            && chat["projectId"]
                                == serde_json::to_value(&connection.project_id).unwrap_or(Value::Null)
                    });
                let usable = match usable {
                    Some(chat) => {
                        let previous = db.run(text(&chat, "runId"))?;
                        let agent = db.get("agents", &connection.agent_id)?
                            .ok_or_else(|| Error::forbidden("Agent no longer exists."))?;
                        let compatible = previous.as_ref().is_none_or(|run| {
                            crate::service::covers(&agent, &run["snapshot"]["agent"])
                        });
                        compatible.then_some(chat)
                    }
                    None => None,
                };
                let chat = match usable {
                    Some(chat) => chat,
                    None => crate::chats::create_in(
                        db,
                        json!({ "agentId": connection.agent_id, "projectId": connection.project_id }),
                    )?,
                };
                let chat_id = text(&chat, "id").to_owned();
                db.set(&binding_key, &json!(chat_id), None)?;
                let owner_key = format!("plugin-conversation:{chat_id}");
                let started_by = db
                    .kv(&owner_key)?
                    .and_then(|owner| owner["startedBy"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| author.clone());
                db.set(
                    &owner_key,
                    &json!({
                        "connectionId": connection.id,
                        "thread": thread,
                        "startedBy": started_by,
                        "routing": connection.routing,
                    }),
                    None,
                )?;
                Ok(Ok(chat_id))
            })
            .await?
    };
    let chat_id = match resolved {
        Ok(chat_id) => chat_id,
        Err(receipt) => return Ok(receipt),
    };
    // Follow Cairn's resume path for a stopped run; merely clearing `paused` would
    // make chat_tick pause it again. Resume also restores the checkpoint budget.
    let chat = s.get("chats", &chat_id).await?;
    if let Some(run_id) = chat["runId"].as_str() {
        let run = s.store.run(run_id).await?;
        if crate::run_status::RunStatus::of(&run)
            .is_some_and(crate::run_status::RunStatus::is_resumable)
            && !new_turn
            && chat["pausedByUser"] != true
            && chat["cancelledByDeletion"] != true
            && chat["sessionRestartRequested"] != true
        {
            let expected = expected.clone();
            let event = event.clone();
            let resumed_chat = chat_id.clone();
            let resumed_run = run_id.to_owned();
            let resumed_thread = thread.clone();
            let approval = package.clone();
            submission_authority(s, context, &package).await?;
            s.worker
                .resume_authorized(s, run_id, move |db| {
                    approval.recheck_in(db)?;
                    let (connection, author) = admit(db, &expected, &event)?;
                    require_binding(db, &binding(&connection, &author, &resumed_thread, shared), &resumed_chat)?;
                    if let Some(principal) = super::requesters::Principal::admitted(&connection, &author) {
                        let chat = db.get("chats", &resumed_chat)?
                            .ok_or_else(|| Error::not_found("Conversation no longer exists."))?;
                        if chat["runId"] != resumed_run {
                            return Err(Error::conflict("The conversation's execution changed."));
                        }
                        let run = db.run(text(&chat, "runId"))?
                            .ok_or_else(|| Error::not_found("Run no longer exists."))?;
                        let previous = super::requesters::recorded(db, &resumed_chat, text(&run["chatExecution"], "messageId"))?;
                        if !previous.is_some_and(|previous| previous.same_authority(&principal)) {
                            return Err(Error::conflict("Only the previous request's author may resume its stopped execution. Start a new turn instead."));
                        }
                    }
                    Ok(())
                })
                .await?;
        }
    }
    let mut attachment_ids = Vec::new();
    for (name, bytes) in files {
        let attachment = id();
        let approval = package.clone();
        let expected = expected.clone();
        let event = event.clone();
        let thread = thread.clone();
        let attached_chat = chat_id.clone();
        s.store_attachment_checked(
            &chat_id,
            &attachment,
            &name,
            &bytes,
            || submission_authority(s, context, &package),
            move |db| {
                approval.recheck_in(db)?;
                let (connection, author) = admit(db, &expected, &event)?;
                require_binding(
                    db,
                    &binding(&connection, &author, &thread, shared),
                    &attached_chat,
                )?;
                let chat = db
                    .get("chats", &attached_chat)?
                    .ok_or_else(|| Error::not_found("Conversation no longer exists."))?;
                crate::conversation_lifecycle::require_active(&chat)
            },
        )
        .await?;
        attachment_ids.push(attachment);
    }
    submission_authority(s, context, &package).await?;
    let expected = expected.clone();
    let event = event.clone();
    let approval = package.clone();
    let result = s
        .store
        .transaction(move |db| {
            approval.recheck_in(db)?;
            let (connection, author) = admit(db, &expected, &event)?;
            if let Some(receipt) = db.kv(&dedupe)? {
                return Ok(receipt);
            }
            let mut chat = db.get("chats", &chat_id)?
                .ok_or_else(|| Error::not_found("Conversation no longer exists."))?;
            crate::conversation_lifecycle::require_active(&chat)?;
            chat["paused"] = json!(chat["pausedByUser"] == true);
            db.put("chats", &chat)?;
            require_binding(db, &binding(&connection, &author, &thread, shared), &chat_id)?;
            let counters = limits(db, &connection, &author)?;
            let message_id = id();
            let message = json!({ "id": message_id, "text": content, "attachmentIds": attachment_ids });
            let principal = super::requesters::Principal::admitted(&connection, &author);
            if new_turn {
                // Native admission must already know this request queues behind
                // the current task, including its scheduled continuations.
                let key = super::turns::key(&chat_id, &message_id);
                let binding = binding(&connection, &author, &thread, shared);
                db.set(&key, &json!({ "binding": binding }), None)?;
            }
            let queued = crate::chats::send_in(
                db,
                &chat_id,
                message,
                principal.as_ref(),
                &crate::native::policy::Origin::default(),
            )?;
            counters.increment(db)?;
            let receipt = json!({ "chatId": chat_id, "messageId": queued["id"] });
            db.set(&dedupe, &receipt, Some(now() + 30 * 86_400_000))?;
            db.put(
                DELIVERIES,
                &json!({
                    "id": message_id,
                    "connectionId": connection.id,
                    "revision": connection.revision,
                    "routing": connection.routing,
                    "chatId": chat_id,
                    "thread": thread,
                    "userId": author,
                    "reply": reply,
                    "asked": [],
                    "attempts": 0,
                    "createdAt": now(),
                    "nextAttemptAt": now(),
                }),
            )?;
            let preview = if content.is_empty() { "(attachment)".to_owned() } else { content.chars().take(200).collect() };
            db.put(
                "plugin-activity",
                &json!({
                    // Record IDs are unique across kinds; link to the delivery separately.
                    "id": id(),
                    "deliveryId": message_id,
                    "connectionId": connection.id,
                    "chatId": chat_id,
                    "routeId": connection.routing.as_ref().map(|r| &r.id),
                    "userId": author,
                    "name": event["payload"]["author"]["name"],
                    "preview": preview,
                    "status": "working",
                    "createdAt": now(),
                    "updatedAt": now(),
                }),
            )?;
            let mut recent = db
                .list("plugin-activity")?
                .into_iter()
                .filter(|value| value["connectionId"] == connection.id)
                .collect::<Vec<_>>();
            recent.sort_by_key(|value| -value["createdAt"].as_i64().unwrap_or(0));
            for stale in recent.iter().skip(30) {
                db.remove("plugin-activity", text(stale, "id"))?;
            }
            db.audit(
                "plugin.message.admitted",
                &json!({ "connectionId": connection.id, "chatId": chat_id, "messageId": message_id }),
            )?;
            Ok(receipt)
        })
        .await?;
    s.worker.notify();
    Ok(result)
}

/// A conversation this connection started, named by ID or by its shared thread.
fn conversation(
    db: &Db<'_>,
    connection: &Connection,
    author: &str,
    args: &Value,
) -> Result<String> {
    let chat = match args["conversation"].as_str() {
        Some(chat) => chat.to_owned(),
        None => db
            .kv(&binding(
                connection,
                author,
                text(args, "thread"),
                args["shared"] != false,
            ))?
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| Error::not_found("Conversation not found."))?,
    };
    let owner = db.kv(&format!("plugin-conversation:{chat}"))?;
    let Some(owner) = owner
        .filter(|owner| owner["connectionId"] == connection.id && owner["retiredAt"].is_null())
    else {
        return Err(Error::not_found("Conversation not found."));
    };
    if let Some(principal) = super::requesters::Principal::admitted(connection, author) {
        let detail = db
            .get("chats", &chat)?
            .ok_or_else(|| Error::not_found("Conversation not found."))?;
        let run = db
            .run(text(&detail, "runId"))?
            .ok_or_else(|| Error::conflict("This conversation has no current execution."))?;
        let delivery = text(args, "delivery");
        let current = super::requesters::recorded(db, &chat, delivery)?;
        if delivery.is_empty()
            || run["chatExecution"]["messageId"] != delivery
            || !crate::run_status::RunStatus::of(&run)
                .is_some_and(crate::run_status::RunStatus::is_active)
            || !run["cancelRequestedAt"].is_null()
            || !current.is_some_and(|current| current.same_authority(&principal))
        {
            return Err(Error::forbidden(
                "Only the author of the current execution can control it.",
            ));
        }
        return Ok(chat);
    }
    // When anyone can write, only listed people and whoever started the conversation
    // may answer for it or stop it.
    if connection.access_mode == AccessMode::Everyone
        && !connection.user_ids.iter().any(|id| id == author)
        && owner["startedBy"] != author
    {
        return Err(Error::forbidden(
            "Only the person who started this conversation can do that.",
        ));
    }
    Ok(chat)
}

/// Checks whether the event's author may control this conversation before a plugin
/// accumulates partial answers. Final answer/cancel effects recheck independently.
pub async fn check(s: &Service, context: &Context, args: &Value) -> Result<Value> {
    if !personal(&context.event) {
        return Err(Error::forbidden("Only a person can control the agent."));
    }
    ensure_owner(s, &context.connection).await?;
    let (expected, event, input) = (
        context.connection.clone(),
        context.event.clone(),
        args.clone(),
    );
    s.store
        .read(move |db| {
            let (connection, author) = admit(db, &expected, &event)?;
            let chat = conversation(db, &connection, &author, &input)?;
            Ok(json!({ "conversation": chat }))
        })
        .await
}

/// `conversations.answer`: answers a pending agent question for a person who is allowed
/// on this connection, in a conversation this connection started.
pub async fn answer(s: &Service, context: &Context, args: &Value) -> Result<Value> {
    if !personal(&context.event) {
        return Err(Error::forbidden("Only a person can answer the agent."));
    }
    ensure_owner(s, &context.connection).await?;
    let (expected, event, input) = (
        context.connection.clone(),
        context.event.clone(),
        args.clone(),
    );
    let (chat, principal) = s
        .store
        .read(move |db| {
            let (connection, author) = admit(db, &expected, &event)?;
            let chat = conversation(db, &connection, &author, &input)?;
            Ok((
                chat,
                super::requesters::Principal::admitted(&connection, &author),
            ))
        })
        .await?;
    let question = text(args, "question");
    let mut answers = serde_json::Map::new();
    for (field, value) in args["answers"].as_object().into_iter().flatten() {
        let value = value.as_str().unwrap_or("");
        if field.chars().count() > 200 || value.trim().is_empty() || value.chars().count() > 10_000
        {
            return Err(Error::bad("Invalid answer."));
        }
        answers.insert(field.clone(), json!([value]));
    }
    let (expected, event, input, chat_id) = (
        context.connection.clone(),
        context.event.clone(),
        args.clone(),
        chat.clone(),
    );
    let answered = s
        .question_answer_authorized(
            &chat,
            question,
            json!({ "id": id(), "answers": answers }),
            principal,
            false,
            move |db, question| {
                let (connection, author) = admit(db, &expected, &event)?;
                if conversation(db, &connection, &author, &input)? != chat_id {
                    return Err(Error::forbidden("The controlled conversation changed."));
                }
                if connection.routing.is_some() {
                    let chat = db
                        .get("chats", &chat_id)?
                        .ok_or_else(|| Error::not_found("Conversation not found."))?;
                    if question.run_id != text(&chat, "runId")
                        || question.turn_id != text(&input, "delivery")
                    {
                        return Err(Error::forbidden(
                            "This question belongs to another execution.",
                        ));
                    }
                }
                Ok(())
            },
        )
        .await?;
    Ok(json!({ "status": answered["status"] }))
}

/// `conversations.cancel`: stops the agent's current work, like Stop in Cairn. The next
/// message from the platform resumes the conversation.
pub async fn cancel(s: &Service, context: &Context, args: &Value) -> Result<Value> {
    if !personal(&context.event) {
        return Err(Error::forbidden("Only a person can stop the agent."));
    }
    ensure_owner(s, &context.connection).await?;
    let (expected, event, input) = (
        context.connection.clone(),
        context.event.clone(),
        args.clone(),
    );
    let chat = s
        .store
        .read(move |db| {
            let (connection, author) = admit(db, &expected, &event)?;
            conversation(db, &connection, &author, &input)
        })
        .await?;
    let detail = s.get("chats", &chat).await?;
    if context.connection.routing.is_some() {
        let run = text(&detail, "runId").to_owned();
        let expected_run = run.clone();
        let (expected, event, input, chat_id) = (
            context.connection.clone(),
            context.event.clone(),
            args.clone(),
            chat.clone(),
        );
        s.worker
            .cancel_authorized(s, &run, move |db| {
                let (connection, author) = admit(db, &expected, &event)?;
                if conversation(db, &connection, &author, &input)? != chat_id {
                    return Err(Error::forbidden("The controlled conversation changed."));
                }
                let mut detail = db
                    .get("chats", &chat_id)?
                    .ok_or_else(|| Error::not_found("Conversation not found."))?;
                if detail["runId"] != expected_run {
                    return Err(Error::conflict("The conversation's execution changed."));
                }
                let run = db
                    .run(&expected_run)?
                    .ok_or_else(|| Error::not_found("Execution no longer exists."))?;
                let keep_paused =
                    detail["paused"] == true || !super::turns::pending(db, &detail, &run)?;
                detail["paused"] = json!(keep_paused);
                db.put("chats", &detail)?;
                Ok(())
            })
            .await?;
        return Ok(json!({ "stopped": true, "conversation": chat }));
    }
    s.chat_pause(&chat, true).await?;
    if let Some(run) = detail["runId"].as_str()
        && crate::run_status::RunStatus::of(&s.store.run(run).await?)
            .is_some_and(crate::run_status::RunStatus::is_active)
        // The run may finish between the check and the request; that is still stopped.
        && let Err(error) = s.worker.cancel(s, run).await
        && error.status != 409
    {
        return Err(error);
    }
    Ok(json!({ "stopped": true, "conversation": chat }))
}

/// Retire a settled source binding, preserving its Cairn history. The next source
/// message creates its own conversation; retrying this event never detaches it.
pub async fn reset(s: &Service, context: &Context, args: &Value) -> Result<Value> {
    if !personal(&context.event) || context.connection.routing.is_none() {
        return Err(Error::forbidden(
            "Only an admitted resource person can reset a thread.",
        ));
    }
    let chat = text(args, "conversation").to_owned();
    crate::validation::uuid(&chat)?;
    let thread = text(args, "thread").to_owned();
    if thread.is_empty()
        || thread.chars().count() > 200
        || args.get("shared").is_some_and(|v| !v.is_boolean())
    {
        return Err(Error::bad(
            "Name the source thread and its expected conversation.",
        ));
    }
    let shared = args["shared"] != false;
    ensure_owner(s, &context.connection).await?;
    let (expected, event, expected_chat) = (
        context.connection.clone(),
        context.event.clone(),
        chat.clone(),
    );
    let receipt_key = format!("plugin-reset:{chat}:{}", text(&event, "id"));
    let lookup_key = receipt_key.clone();
    let initial = s
        .store
        .read(move |db| {
            admit(db, &expected, &event)?;
            if let Some(receipt) = db.kv(&lookup_key)? {
                return Ok(Err(receipt));
            }
            let chat = db
                .get("chats", &expected_chat)?
                .ok_or_else(|| Error::not_found("Conversation not found."))?;
            Ok(Ok(text(&chat, "runId").to_owned()))
        })
        .await?;
    let run = match initial {
        Ok(run) => run,
        Err(receipt) => return Ok(receipt),
    };
    let preparation = s.worker.run_preparation_lock(&run).await;
    let _preparation = preparation
        .try_lock_owned()
        .map_err(|_| Error::conflict("Wait for this conversation to finish preparing."))?;
    let active = s
        .worker
        .active
        .try_lock()
        .map_err(|_| Error::conflict("Wait for this conversation to finish stopping."))?;
    if active.contains_key(&run) {
        return Err(Error::conflict(
            "Wait for this conversation to finish stopping.",
        ));
    }
    let (expected, event) = (context.connection.clone(), context.event.clone());
    s.store
        .transaction(move |db| {
            let (connection, author) = admit(db, &expected, &event)?;
            if let Some(receipt) = db.kv(&receipt_key)? {
                return Ok(receipt);
            }
            let binding_key = binding(&connection, &author, &thread, shared);
            require_binding(db, &binding_key, &chat)?;
            let detail = db
                .get("chats", &chat)?
                .ok_or_else(|| Error::not_found("Conversation not found."))?;
            crate::conversation_lifecycle::require_active(&detail)?;
            if detail["runId"].as_str().unwrap_or("") != run {
                return Err(Error::conflict("The conversation's execution changed."));
            }
            let pending = db
                .messages(&chat)?
                .iter()
                .any(|message| message["status"] == "queued" || message["status"] == "sending");
            let delivering = db
                .list(DELIVERIES)?
                .iter()
                .any(|delivery| delivery["chatId"] == chat);
            let execution = if run.is_empty() { None } else { db.run(&run)? };
            let unsettled = execution.as_ref().is_some_and(|run| {
                crate::run_status::RunStatus::of(run)
                    .is_none_or(crate::run_status::RunStatus::is_active)
                    || run["recoveryPending"] == true
            });
            if pending || delivering || unsettled || detail["wakeup"].is_object() {
                return Err(Error::conflict(
                    "Finish or stop the current work and wait for its reply before resetting.",
                ));
            }
            let owner_key = format!("plugin-conversation:{chat}");
            let mut owner = db
                .kv(&owner_key)?
                .filter(|owner| owner["connectionId"] == connection.id && owner["thread"] == thread)
                .ok_or_else(|| Error::not_found("Conversation not found."))?;
            owner["retiredAt"] = json!(now());
            db.set(&owner_key, &owner, None)?;
            db.delete(&binding_key)?;
            let receipt = json!({ "reset": true, "conversation": chat });
            db.set(&receipt_key, &receipt, Some(now() + 30 * 86_400_000))?;
            db.audit(
                "plugin.conversation.reset",
                &json!({
                    "connectionId": connection.id, "chatId": chat, "userId": author,
                }),
            )?;
            Ok(receipt)
        })
        .await
}

/// Source authority is tied to this exact binding, even across slow submission I/O.
fn require_binding(db: &Db<'_>, key: &str, chat: &str) -> Result<()> {
    let owner = db.kv(&format!("plugin-conversation:{chat}"))?;
    if db.kv(key)?.as_ref() != Some(&json!(chat))
        || owner.is_none_or(|owner| !owner["retiredAt"].is_null())
    {
        return Err(Error::conflict("This source thread has been reset."));
    }
    Ok(())
}

/// Where the admitted message stands in its conversation's run.
enum Turn {
    Waiting,
    Working {
        delivered: i64,
        run: String,
    },
    /// The turn ended at `end`: `completed`, `failed`, or `cancelled` when Cairn stopped it.
    Done {
        delivered: i64,
        end: i64,
        run: String,
        status: &'static str,
    },
    Stopped {
        failed: bool,
    },
}

/// How long an account wait lasts before a failed turn answers the person.
const ACCOUNT_WAIT_REPLY_MS: i64 = 2 * 60_000;

fn turn(db: &Db<'_>, connection: &Connection, delivery: &Value) -> Result<Turn> {
    super::routes::require_person(connection, text(delivery, "userId"))?;
    if delivery["revision"] != connection.revision || !connection.allows(text(delivery, "userId")) {
        return Err(Error::forbidden("Reply authorization was revoked."));
    }
    let chat = db
        .get("chats", text(delivery, "chatId"))?
        .ok_or_else(|| Error::not_found("Conversation no longer exists."))?;
    let owner = db.kv(&format!("plugin-conversation:{}", text(&chat, "id")))?;
    if !crate::conversation_lifecycle::is_active(&chat)
        || owner.is_none_or(|owner| !owner["retiredAt"].is_null())
    {
        return Err(Error::forbidden("Conversation is no longer active."));
    }
    if let Some(ended) = db.kv(&super::turns::abandoned_key(
        text(&chat, "id"),
        text(delivery, "id"),
    ))? {
        // The run may now be serving another request. Use only the predecessor's
        // range captured at advancement; never search for a future terminator.
        let run = text(&ended, "runId");
        let reply = &ended["reply"];
        let status = match reply["status"].as_str() {
            Some("completed") => Some("completed"),
            Some("failed") => Some("failed"),
            Some("cancelled") => Some("cancelled"),
            None if reply["status"].is_null() => reply["failed"]
                .as_bool()
                .map(|failed| if failed { "failed" } else { "completed" }),
            _ => None,
        };
        let bounds = (reply["delivered"].as_i64(), reply["end"].as_i64(), status);
        if let (Some(delivered), Some(end), Some(status)) = bounds
            && delivered > 0
            && end > delivered
            && !run.is_empty()
        {
            return Ok(Turn::Done {
                delivered,
                end,
                run: run.to_owned(),
                status,
            });
        }
        return Ok(Turn::Stopped {
            failed: ended["failed"] == true,
        });
    }
    // An independent queued request has not started yet. The preceding run's
    // terminal status must not cancel its future reply delivery.
    if db
        .kv(&super::turns::key(text(&chat, "id"), text(delivery, "id")))?
        .is_some()
    {
        return Ok(Turn::Waiting);
    }
    let run_id = text(&chat, "runId").to_owned();
    if run_id.is_empty() {
        return Ok(Turn::Waiting);
    }
    let delivered: Option<i64> = db.0.query_row(
        "SELECT id FROM events WHERE run_id=?1 AND type IN ('chat.user','chat.wakeup.delivered') AND json_extract(payload,'$.messageId')=?2 ORDER BY id LIMIT 1",
        rusqlite::params![run_id, delivery["id"].as_str()], |row| row.get(0),
    ).optional()?;
    let Some(delivered) = delivered else {
        let run = db.run(&run_id)?;
        if delivery["continuationOf"].is_string()
            && chat["wakeup"]["id"] != delivery["id"]
            && run
                .as_ref()
                .is_none_or(|run| run["chatExecution"]["messageId"] != delivery["id"])
        {
            return Err(Error::forbidden(
                "The scheduled continuation was cancelled or replaced.",
            ));
        }
        // Queued requests can fail before their message reaches the agent.
        return Ok(match run.as_ref().and_then(|run| run["status"].as_str()) {
            Some("failed" | "interrupted") => Turn::Stopped { failed: true },
            Some("cancelled") => Turn::Stopped { failed: false },
            _ => Turn::Waiting,
        });
    };
    let run = db.run(&run_id)?;
    if let Some((end, status)) =
        finished_turn(db, &run_id, text(delivery, "id"), delivered, run.as_ref())?
    {
        return Ok(Turn::Done {
            delivered,
            end,
            run: run_id,
            status,
        });
    }

    match run.as_ref().and_then(|run| run["status"].as_str()) {
        Some("failed" | "interrupted") => Ok(Turn::Stopped { failed: true }),
        Some("cancelled") => Ok(Turn::Stopped { failed: false }),
        _ => Ok(Turn::Working {
            delivered,
            run: run_id,
        }),
    }
}

/// Select a terminal boundary before another request can reuse this run.
/// Live replies and frozen predecessor ranges share retry/account/stop semantics.
pub(crate) fn finished_turn(
    db: &Db<'_>,
    run_id: &str,
    message: &str,
    delivered: i64,
    run: Option<&Value>,
) -> Result<Option<(i64, &'static str)>> {
    let mut statement = db.0.prepare(
        "SELECT id, CASE WHEN type<>'status' THEN type WHEN text='cancelled' THEN 'cancelled' ELSE 'stopped' END FROM events WHERE run_id=?1 AND id>?2 AND (type IN ('turn.started','turn.completed','turn.failed','retry.scheduled') OR (type='status' AND text IN ('failed','cancelled','interrupted'))) ORDER BY id",
    )?;
    let history = statement
        .query_map(rusqlite::params![run_id, delivered], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // A failed turn is final only once Cairn stops working on its message: an automatic
    // retry, another account or a recovered controller continues it, and a later turn answers.
    // A scheduled retry says so until the run stops; otherwise the run still works on this
    // message.
    let current = run.is_some_and(|run| run["chatExecution"]["messageId"] == message);
    let active = run
        .and_then(crate::run_status::RunStatus::of)
        .is_some_and(crate::run_status::RunStatus::is_active);
    // Waiting for an account can last hours: once it lasts, the failure answers the person
    // meanwhile. A quick switch to another account, recovery and capacity waits continue the
    // message; a wait for the person to reconnect an account answers at once. These are the
    // waits clients show as account waits.
    let reason = run
        .map(|run| text(run, "accountWaitReason"))
        .unwrap_or_default();
    let account_wait = reason.starts_with("Usage exhausted.")
        || (reason.starts_with("Waiting for a ")
            && reason.contains(" account with available usage."));
    let since: Option<i64> = if account_wait {
        db.0.query_row(
            "SELECT created_at FROM events WHERE run_id=?1 AND id>?2 AND type='status' AND (text=?3 OR (?4 AND text LIKE 'Usage exhausted.%')) ORDER BY id DESC LIMIT 1",
            rusqlite::params![run_id, delivered, reason, reason.starts_with("Usage exhausted.")], |row| row.get(0),
        ).optional()?
    } else {
        None
    };
    let waiting_for_account = run.is_some_and(|run| !run["accountRequired"].is_null())
        || (account_wait && since.is_some_and(|since| now() - since >= ACCOUNT_WAIT_REPLY_MS));
    let turn_end = |kind: &str| matches!(kind, "turn.completed" | "turn.failed");
    let stopped = |kind: &str| match kind {
        "cancelled" => Some("cancelled"),
        "stopped" => Some("failed"),
        _ => None,
    };
    for (index, (end, kind)) in history.iter().enumerate() {
        // A turn Cairn stops reports no failure: the run's status ends this message.
        if let Some(status) = stopped(kind) {
            return Ok(Some((*end, status)));
        }
        if !turn_end(kind) {
            continue;
        }
        let failed = kind == "turn.failed";
        // An older adapter repeats the failure of the same turn before Cairn schedules its
        // retry. The retry continues this message until the run stops before another turn ends.
        let mut rest = history[index + 1..]
            .iter()
            .skip_while(|(_, next)| next == "turn.failed");
        let retried = rest
            .next()
            .is_some_and(|(_, next)| next == "retry.scheduled");
        let stop = retried
            .then(|| rest.find(|(_, next)| stopped(next).is_some() || turn_end(next)))
            .flatten()
            .and_then(|(stop, next)| stopped(next).map(|status| (*stop, status)));
        // The run stopped during the retried attempt: the message ends there, with what that
        // attempt said.
        if failed && let Some((stop, status)) = stop {
            return Ok(Some((stop, status)));
        }
        let later = history[index + 1..].iter().any(|(_, next)| turn_end(next));
        // While the run waits for an account on this message, its failure answers.
        let waiting = waiting_for_account && current && !later;
        let continued = failed && !waiting && (retried || (current && (later || active)));
        if !continued {
            return Ok(Some((*end, if failed { "failed" } else { "completed" })));
        }
    }

    Ok(None)
}

/// The agent's final answer for the admitted message, with deliverables published
/// during that turn. Never exports tools, reasoning or unrelated turns.
#[derive(Debug)]
pub struct Reply {
    pub text: String,
    pub status: &'static str,
    pub files: Vec<Value>,
}

pub async fn reply(s: &Service, expected: &Connection, delivery: &Value) -> Result<Option<Reply>> {
    let expected = super::routes::bound(s, expected, &delivery["routing"]).await?;
    ensure_owner(s, &expected).await?;
    let delivery = delivery.clone();
    s.store.read(move |db| {
        let connection = authorize(db, &expected)?;
        match turn(db, &connection, &delivery)? {
            Turn::Waiting | Turn::Working { .. } => Ok(None),
            Turn::Stopped { failed: false } => Ok(Some(Reply { text: "Stopped. Write again to continue.".into(), status: "cancelled", files: Vec::new() })),
            Turn::Stopped { failed: true } => Ok(Some(Reply { text: "The agent could not finish this request. Review the conversation in Cairn.".into(), status: "failed", files: Vec::new() })),
            Turn::Done { delivered, end, run, status } => {
                // A retried turn answers with its last attempt, not with what a failed one said.
                let answered: i64 = db.0.query_row(
                    "SELECT COALESCE(MAX(id), ?2) FROM events WHERE run_id=?1 AND type='retry.scheduled' AND id>?2 AND id<?3",
                    rusqlite::params![run, delivered, end], |row| row.get(0),
                )?;
                let final_text: Option<String> = db.0.query_row(
                    "SELECT json_extract(payload,'$.item.text') FROM events WHERE run_id=?1 AND id>?2 AND id<?3 AND json_extract(payload,'$.item.type')='agent_message' ORDER BY id DESC LIMIT 1",
                    rusqlite::params![run, answered, end], |row| row.get(0),
                ).optional()?.flatten();
                let mut statement = db.0.prepare(
                    "SELECT payload FROM events WHERE run_id=?1 AND type='artifact' AND id>?2 AND id<?3 ORDER BY id LIMIT 10",
                )?;
                let files = statement
                    .query_map(rusqlite::params![run, delivered, end], |row| row.get::<_, String>(0))?
                    .filter_map(|payload| payload.ok().and_then(|payload| serde_json::from_str::<Value>(&payload).ok()))
                    .map(|artifact| json!({
                        "id": artifact["id"],
                        "name": artifact["name"],
                        "title": artifact["title"],
                        "mediaType": artifact["mediaType"],
                        "size": artifact["size"],
                    }))
                    .collect();
                let text = final_text.filter(|value| !value.is_empty()).unwrap_or_else(|| {
                    match status {
                        "cancelled" => "Stopped. Write again to continue.",
                        "failed" => "The agent could not finish this request. Review the conversation in Cairn.",
                        _ => "The agent finished without a text reply. Open the conversation in Cairn.",
                    }.into()
                });
                Ok(Some(Reply { text, status, files }))
            }
        }
    }).await
}

/// What the agent is doing for this message right now, if it is still working.
pub async fn progress(
    s: &Service,
    expected: &Connection,
    delivery: &Value,
) -> Result<Option<Value>> {
    let expected = super::routes::bound(s, expected, &delivery["routing"]).await?;
    let delivery = delivery.clone();
    s.store
        .read(move |db| {
            let connection = authorize(db, &expected)?;
            let (delivered, run) = match turn(db, &connection, &delivery)? {
                Turn::Waiting => return Ok(Some(json!({ "state": "queued" }))),
                Turn::Working { delivered, run } => (delivered, run),
                _ => return Ok(None),
            };
            let latest: Option<String> = db.0.query_row(
                "SELECT payload FROM events WHERE run_id=?1 AND id>?2 AND type IN ('item.started','item.completed') ORDER BY id DESC LIMIT 1",
                rusqlite::params![run, delivered], |row| row.get(0),
            ).optional()?;
            let item = latest.and_then(|payload| serde_json::from_str::<Value>(&payload).ok()).unwrap_or(Value::Null);
            Ok(Some(json!({ "state": "working", "step": step(&item["item"]) })))
        })
        .await
}

/// A short, non-sensitive description of the current step. Never includes output.
fn step(item: &Value) -> String {
    match item["type"].as_str() {
        Some("command_execution") => "Running a command".into(),
        Some("file_change") => "Editing files".into(),
        Some("web_search") => "Searching the web".into(),
        Some("mcp_tool_call" | "collab_tool_call") => "Using a tool".into(),
        Some("todo_list") => "Planning".into(),
        Some("agent_message") => "Writing the answer".into(),
        _ => "Thinking".into(),
    }
}

pub(crate) fn question_seen_key(
    connection: &Connection,
    delivery: &Value,
    question: &Value,
) -> String {
    format!(
        "plugin-question-seen:{}:{}:{}",
        connection.id,
        text(delivery, "id"),
        text(question, "id"),
    )
}

/// Questions the agent is waiting on in this conversation.
pub async fn questions(s: &Service, expected: &Connection, delivery: &Value) -> Result<Vec<Value>> {
    let expected = super::routes::bound(s, expected, &delivery["routing"]).await?;
    let delivery = delivery.clone();
    let prefix = crate::chats::questions::question_prefix(text(&delivery, "chatId"));
    s.store
        .read(move |db| {
            let connection = authorize(db, &expected)?;
            let Turn::Working { run, .. } = turn(db, &connection, &delivery)? else {
                return Ok(Vec::new());
            };
            let execution = db.run(&run)?.unwrap_or(Value::Null);
            if execution["chatExecution"]["messageId"] != delivery["id"] {
                return Ok(Vec::new());
            }
            Ok(db
                .keys(&prefix)?
                .into_iter()
                .map(|(_, question)| question)
                .filter(|question| question["status"] == "pending" && question["runId"] == run)
                .filter(|question| question["turnId"] == delivery["id"])
                .filter_map(|question| {
                    match db.kv(&question_seen_key(&connection, &delivery, &question)) {
                        Ok(None) => Some(Ok(question)),
                        Ok(Some(_)) => None,
                        Err(error) => Some(Err(error)),
                    }
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .map(|question| {
                    json!({
                        "id": question["id"],
                        "blocking": question["blocking"],
                        "fields": question["fields"],
                    })
                })
                .collect())
        })
        .await
}

/// Durable reply state belongs to one host-selected delivery, not general plugin
/// storage. It survives a handler failure and is removed with that delivery.
pub(crate) async fn checkpoint(
    s: &Service,
    context: &Context,
    args: &Value,
    digest: Option<&str>,
) -> Result<Value> {
    if context.event["name"] != "reply" {
        return Err(Error::forbidden(
            "Only a reply can checkpoint its delivery.",
        ));
    }
    let delivery = text(&context.event, "id").to_owned();
    crate::validation::uuid(&delivery)?;
    let state = args.get("state").cloned().unwrap_or(Value::Null);
    if serde_json::to_vec(&state)?.len() > 65_536 {
        return Err(Error::too_large(
            "Delivery checkpoints are limited to 64 KB.",
        ));
    }
    let package = registry::approved(s, &context.connection.plugin_id).await?;
    if digest.is_some_and(|expected| expected != package.package.digest) {
        return Err(Error::forbidden("The reply package approval changed."));
    }
    let approval = package.snapshot();
    let expected = context.connection.clone();
    let author = context.event["authorId"].clone();
    let routing = serde_json::to_value(&expected.routing)?;
    s.store
        .transaction(move |db| {
            approval.recheck_in(db)?;
            let connection = authorize(db, &expected)?;
            if !connection.allows(author.as_str().unwrap_or("")) {
                return Err(Error::forbidden("Reply authorization was revoked."));
            }
            let current = db
                .get(DELIVERIES, &delivery)?
                .ok_or_else(|| Error::forbidden("The delivery is no longer pending."))?;
            if current["connectionId"] != expected.id
                || current["revision"] != expected.revision
                || current["routing"] != routing
                || current["userId"] != author
            {
                return Err(Error::forbidden("The reply's authority changed."));
            }
            db.put(
                CHECKPOINTS,
                &json!({
                    "id": checkpoint_key(&delivery), "connectionId": expected.id,
                    "deliveryId": delivery, "state": state,
                }),
            )?;
            Ok(Value::Null)
        })
        .await
}

pub(crate) fn checkpoint_key(delivery: &str) -> String {
    format!("delivery-checkpoint:{delivery}")
}

pub(crate) async fn checkpoint_state(s: &Service, delivery: &Value) -> Result<Value> {
    Ok(s.store
        .get(CHECKPOINTS, &checkpoint_key(text(delivery, "id")))
        .await?
        .map_or(Value::Null, |record| record["state"].clone()))
}

/// Removes a delivery and records how its exchange ended.
pub async fn finish(s: &Service, delivery: &Value, status: &'static str) -> Result<()> {
    let id = text(delivery, "id").to_owned();
    let delivery = delivery.clone();
    s.store
        .transaction(move |db| {
            remove_delivery(db, &delivery)?;
            for mut value in db.list("plugin-activity")? {
                if value["deliveryId"] == id {
                    value["status"] = json!(status);
                    value["updatedAt"] = json!(now());
                    db.put("plugin-activity", &value)?;
                }
            }
            Ok(())
        })
        .await
}

pub(crate) fn remove_delivery(db: &Db<'_>, delivery: &Value) -> Result<()> {
    let id = text(delivery, "id");
    let connection = text(delivery, "connectionId");
    super::storage::forget_delivery(db, connection, id)?;
    for (key, _) in db.keys(&format!("plugin-question-seen:{connection}:{id}:"))? {
        db.delete(&key)?;
    }
    db.remove(DELIVERIES, id)?;
    db.remove(CHECKPOINTS, &checkpoint_key(id))
}
