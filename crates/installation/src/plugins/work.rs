//! Durable work for delegated connections: admission of external messages, the event
//! queue plugins consume, conversation submission and correlated replies.
use super::{
    capabilities::Context,
    connections::{AccessMode, Connection, Label, authorize, ensure_owner, external_id, label},
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

pub const EVENTS: &str = "plugin-events";
pub const DELIVERIES: &str = "plugin-deliveries";

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
) -> Result<()> {
    db.put(
        EVENTS,
        &json!({
            "id": id(),
            "connectionId": connection.id,
            "revision": connection.revision,
            "routing": connection.routing,
            "name": name,
            "payload": payload,
            "authorId": author,
            "attempts": 0,
            "createdAt": now(),
            "nextAttemptAt": now(),
        }),
    )?;
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
    // Allowlists only change with a new revision (or shrink), so this snapshot is a
    // superset: strangers are dropped without touching storage or owner authority.
    if connection.access_mode == AccessMode::Allowlist
        && !connection.user_ids.contains(&inbound.author.id)
    {
        return Ok(Admission::Denied);
    }
    ensure_owner(s, connection).await?;
    let expected = connection.clone();
    s.store
        .transaction(move |db| {
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
            enqueue(
                db,
                &current,
                &inbound.kind,
                &merge(&merge(&inbound.context, &inbound.body), &identity),
                Some(&inbound.author.id),
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
    enqueue(db, connection, "access_requested", payload, None)?;
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
    if !valid_kind
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
            Some(connection) => deliver(s, &connection, inbound).await?,
            None => Admission::Denied,
        }
    } else {
        deliver(s, &context.connection, inbound).await?
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
    if !connection.allows(&author) || event["revision"] != connection.revision {
        return Err(Error::forbidden("This person is no longer allowed."));
    }
    Ok((connection, author))
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
    let outstanding = db
        .list(DELIVERIES)?
        .iter()
        .filter(|value| value["connectionId"] == connection.id)
        .count();
    if outstanding >= 20 {
        return Err(Error::too_many_requests(
            "Connection has too much pending work.",
        ));
    }
    Ok(Counters(counters))
}

/// Files a person attached, fetched from hosts the plugin declares (at most four,
/// 10 MB each). Fetching happens before any conversation changes.
async fn download(
    s: &Service,
    manifest: &Manifest,
    files: &Value,
) -> Result<Vec<(String, Vec<u8>)>> {
    let files = files.as_array().cloned().unwrap_or_default();
    if files.len() > 4 {
        return Err(Error::bad("Attach up to four files."));
    }
    let mut downloaded = Vec::new();
    for file in files {
        let target = super::capabilities::target(s, manifest, text(&file, "url"), "https")?;
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
    let (expected, event) = (&context.connection, &context.event);
    if event["name"] != "message" {
        return Err(Error::forbidden(
            "Conversations start only from a received message.",
        ));
    }
    ensure_owner(s, expected).await?;
    let content = text(args, "text").trim().to_owned();
    let thread = text(args, "thread").to_owned();
    let shared = args["shared"] == true;
    let reply = args.get("reply").cloned().unwrap_or(Value::Null);
    if content.len() > 50_000
        || thread.chars().count() > 200
        || (shared && thread.is_empty())
        || serde_json::to_string(&reply)?.len() > 4096
    {
        return Err(Error::bad("Invalid conversation message."));
    }
    let files = download(s, &context.manifest, &args["files"]).await?;
    if content.is_empty() && files.is_empty() {
        return Err(Error::bad("Write a message or attach a file."));
    }
    let external_id = text(&event["payload"], "id").to_owned();
    let namespace = expected.routing.as_ref().map_or_else(
        || expected.id.clone(),
        |r| format!("{}:{}", expected.id, r.resource),
    );
    let dedupe = format!("plugin-submitted:{namespace}:{external_id}");

    // Resolve (or start) the conversation first, so attachments can be stored in it.
    let resolved = {
        let (expected, event, dedupe, thread) = (
            expected.clone(),
            event.clone(),
            dedupe.clone(),
            thread.clone(),
        );
        s.store
            .transaction(move |db| {
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
            && chat["cancelledByDeletion"] != true
            && chat["sessionRestartRequested"] != true
        {
            let expected = expected.clone();
            let event = event.clone();
            let resumed_chat = chat_id.clone();
            let resumed_run = run_id.to_owned();
            s.worker
                .resume_authorized(s, run_id, move |db| {
                    let (connection, author) = admit(db, &expected, &event)?;
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
        s.store_attachment(&chat_id, &attachment, &name, &bytes)
            .await?;
        attachment_ids.push(attachment);
    }
    let expected = expected.clone();
    let event = event.clone();
    let result = s
        .store
        .transaction(move |db| {
            let (connection, author) = admit(db, &expected, &event)?;
            if let Some(receipt) = db.kv(&dedupe)? {
                return Ok(receipt);
            }
            let mut chat = db.get("chats", &chat_id)?
                .ok_or_else(|| Error::not_found("Conversation no longer exists."))?;
            crate::conversation_lifecycle::require_active(&chat)?;
            chat["paused"] = json!(false);
            db.put("chats", &chat)?;
            let counters = limits(db, &connection, &author)?;
            let message_id = id();
            let message = json!({ "id": message_id, "text": content, "attachmentIds": attachment_ids });
            let principal = super::requesters::Principal::admitted(&connection, &author);
            super::requesters::capture_principal(db, &chat_id, &message, principal.as_ref(), super::requesters::Capture::New)?;
            let queued = crate::chats::send_in(
                db,
                &chat_id,
                message,
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
    let Some(owner) = owner.filter(|owner| owner["connectionId"] == connection.id) else {
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
                detail["paused"] = json!(true);
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

/// Where the admitted message stands in its conversation's run.
enum Turn {
    Waiting,
    Working {
        delivered: i64,
        run: String,
    },
    Done {
        delivered: i64,
        end: i64,
        run: String,
        failed: bool,
    },
    Stopped {
        failed: bool,
    },
}

fn turn(db: &Db<'_>, connection: &Connection, delivery: &Value) -> Result<Turn> {
    if delivery["revision"] != connection.revision || !connection.allows(text(delivery, "userId")) {
        return Err(Error::forbidden("Reply authorization was revoked."));
    }
    let chat = db
        .get("chats", text(delivery, "chatId"))?
        .ok_or_else(|| Error::not_found("Conversation no longer exists."))?;
    if !crate::conversation_lifecycle::is_active(&chat) {
        return Err(Error::forbidden("Conversation is no longer active."));
    }
    let run_id = text(&chat, "runId").to_owned();
    if run_id.is_empty() {
        return Ok(Turn::Waiting);
    }
    let delivered: Option<i64> = db.0.query_row(
        "SELECT id FROM events WHERE run_id=?1 AND type='chat.user' AND json_extract(payload,'$.messageId')=?2 ORDER BY id LIMIT 1",
        rusqlite::params![run_id, delivery["id"].as_str()], |row| row.get(0),
    ).optional()?;
    let Some(delivered) = delivered else {
        let run = db.run(&run_id)?;
        // Queued requests can fail before their message reaches the agent.
        return Ok(match run.as_ref().and_then(|run| run["status"].as_str()) {
            Some("failed" | "interrupted") => Turn::Stopped { failed: true },
            Some("cancelled") => Turn::Stopped { failed: false },
            _ => Turn::Waiting,
        });
    };
    let end: Option<(i64, String)> = db.0.query_row(
        "SELECT id, type FROM events WHERE run_id=?1 AND type IN ('turn.completed','turn.failed') AND id>?2 ORDER BY id LIMIT 1",
        rusqlite::params![run_id, delivered], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    if let Some((end, kind)) = end {
        return Ok(Turn::Done {
            delivered,
            end,
            run: run_id,
            failed: kind == "turn.failed",
        });
    }
    let run = db.run(&run_id)?;
    match run.as_ref().and_then(|run| run["status"].as_str()) {
        Some("failed" | "interrupted") => Ok(Turn::Stopped { failed: true }),
        Some("cancelled") => Ok(Turn::Stopped { failed: false }),
        _ => Ok(Turn::Working {
            delivered,
            run: run_id,
        }),
    }
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
            Turn::Done { delivered, end, run, failed } => {
                let final_text: Option<String> = db.0.query_row(
                    "SELECT json_extract(payload,'$.item.text') FROM events WHERE run_id=?1 AND id>?2 AND id<?3 AND json_extract(payload,'$.item.type')='agent_message' ORDER BY id DESC LIMIT 1",
                    rusqlite::params![run, delivered, end], |row| row.get(0),
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
                    if failed { "The agent could not finish this request. Review the conversation in Cairn." } else { "The agent finished without a text reply. Open the conversation in Cairn." }.into()
                });
                Ok(Some(Reply { text, status: if failed { "failed" } else { "completed" }, files }))
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

/// Removes a delivery and records how its exchange ended.
pub async fn finish(s: &Service, delivery: &Value, status: &'static str) -> Result<()> {
    let id = text(delivery, "id").to_owned();
    s.store
        .transaction(move |db| {
            db.remove(DELIVERIES, &id)?;
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
