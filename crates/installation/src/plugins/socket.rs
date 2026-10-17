//! A package's own WebSocket connection, e.g. a chat platform's gateway. The script
//! drives it from a dedicated transport worker: it connects, sends, schedules ticks and
//! handles every frame. The host keeps the socket, checks hosts and sizes, paces
//! reconnections and injects the instance's credential without exposing it.
use super::{
    capabilities::{self, Context},
    connections::Connection,
    registry::Package,
};
use crate::{
    error::{Error, Result},
    service::Service,
    validation::text,
};
use cairn_plugin_api::{Frame, MAX_FRAME_BYTES};
use cairn_plugin_host::{Denied, Session};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
    time::Instant,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderName, HeaderValue},
        protocol::{CloseFrame, WebSocketConfig, frame::coding::CloseCode},
    },
};
use tokio_util::sync::CancellationToken;

pub(super) mod pacing;

/// Largest message a package sends, or receives as text.
pub const MAX_MESSAGE: usize = 1_048_576;
/// Largest message read from the wire. Above `MAX_MESSAGE`, the package receives only
/// the JSON fields it asked for (`oversized`), so a large payload never drops the
/// connection.
const MAX_WIRE: usize = 16 * 1_048_576;
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const MAX_TIMERS: usize = 8;

/// What a transport worker's capability calls ask the host to do. Applied in order
/// once the running handler returns.
#[derive(Debug)]
pub enum Command {
    Connect {
        url: String,
        header: Header,
        /// Wait at least this long first, e.g. 1–5 s after an invalid session.
        after: Duration,
        /// JSON fields (`d.id`) kept from messages too large to deliver whole.
        oversized: Vec<String>,
    },
    Send(Outgoing),
    Close(u16),
    /// A named timer of the current connection: once after `at`, then every `every`.
    Timer {
        name: String,
        at: Duration,
        every: Option<Duration>,
    },
    Cancel(String),
    /// The package reported its connection usable: reconnect quickly next time.
    Healthy,
}

/// The transport worker's handle on its socket, given to its capability calls.
#[derive(Clone)]
pub struct Link {
    sender: mpsc::UnboundedSender<Command>,
    slots: Arc<Semaphore>,
}

#[derive(Debug)]
pub struct Outgoing {
    pub text: String,
    name: Option<String>,
    ticket: Option<pacing::Ticket>,
    _slot: OwnedSemaphorePermit,
}

impl Link {
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<Command>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                sender,
                slots: Arc::new(Semaphore::new(16)),
            },
            receiver,
        )
    }

    pub(crate) fn send(&self, command: Command) -> Result<()> {
        self.sender
            .send(command)
            .map_err(|_| Error::bad_gateway("The connection stopped."))
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
}

fn enabled(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null() && **value != Value::Bool(false))
}

/// `socket.*` calls. Only the transport worker holds a link.
pub(crate) async fn call(
    s: &Service,
    context: &Context,
    method: &str,
    args: &Value,
) -> Result<Value> {
    if args["auth"].get("resource").is_some() {
        return Err(Error::forbidden(
            "Resource credentials are only available to HTTP requests.",
        ));
    }
    let link = context
        .link
        .as_ref()
        .ok_or_else(|| Error::forbidden("Sockets belong to the instance's transport worker."))?;
    match method {
        "socket.connect" => {
            let url = capabilities::target(s, &context.manifest, text(args, "url"), "wss")?;
            let header = match enabled(args.get("auth")) {
                Some(auth) => capabilities::authorization(s, context, auth)
                    .await?
                    .map(|(header, prefix, secret)| (header, format!("{prefix}{secret}"))),
                None => None,
            };
            let after = args["after"].as_f64().unwrap_or(0.0);
            if !(0.0..=86_400.0).contains(&after) {
                return Err(Error::bad("Connect after 0 seconds to one day."));
            }
            let oversized = args["oversized"]
                .as_array()
                .map_or_else(Vec::new, Clone::clone);
            if oversized.len() > 16
                || !oversized.iter().all(|path| {
                    path.as_str().is_some_and(|path| {
                        !path.is_empty()
                            && path.len() <= 80
                            && path.split('.').count() <= 6
                            && path.split('.').all(|key| !key.is_empty())
                    })
                })
            {
                return Err(Error::bad("Name at most 16 fields such as d.id."));
            }
            link.send(Command::Connect {
                url,
                header,
                after: Duration::from_secs_f64(after),
                oversized: oversized
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            })?;
        }
        "socket.send" => {
            let mut message = args["text"]
                .as_str()
                .ok_or_else(|| Error::bad("Send text."))?
                .to_owned();
            if message.len() > MAX_MESSAGE {
                return Err(Error::too_large("Socket messages are limited to 1 MB."));
            }
            let pace = args["pace"].as_f64().unwrap_or(0.0);
            if !(0.0..=60_000.0).contains(&pace) {
                return Err(Error::bad("Pace sends up to 60 seconds apart."));
            }
            let mut key = format!("{}\n{}", context.manifest.id, context.connection.id);
            // `{{secret}}` marks where the host puts the credential, JSON-escaped.
            if let Some(auth) = enabled(args.get("auth")) {
                if !message.contains("{{secret}}") {
                    return Err(Error::bad("Mark the secret's place with {{secret}}."));
                }
                let (_, _, secret) = capabilities::authorization(s, context, auth)
                    .await?
                    .ok_or_else(|| Error::forbidden("This instance has no credential."))?;
                // Paced sends with one credential share a budget across instances,
                // e.g. one Discord Identify per bot token every five seconds.
                key = format!("{}\n{secret}", context.manifest.id);
                let escaped = serde_json::to_string(&secret)?;
                message = message.replace("{{secret}}", &escaped[1..escaped.len() - 1]);
            }
            let name = args["name"].as_str().map(str::to_owned);
            if name.as_ref().is_some_and(|name| !valid_name(name)) {
                return Err(Error::bad("Name sends with lowercase letters and digits."));
            }
            let budget = match args.get("budget").filter(|budget| !budget.is_null()) {
                Some(budget) => {
                    let remaining = budget["remaining"]
                        .as_u64()
                        .filter(|value| *value <= 1_000_000)
                        .ok_or_else(|| Error::bad("Give a nonnegative remaining budget."))?;
                    let reset = budget["resetAfter"]
                        .as_f64()
                        .filter(|value| (1.0..=86_400_000.0).contains(value))
                        .ok_or_else(|| {
                            Error::bad("Reset the budget within one day (milliseconds).")
                        })?;
                    Some((remaining, Duration::from_secs_f64(reset / 1000.0)))
                }
                None => None,
            };
            let slot = link
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::bad("At most 16 socket sends may be pending."))?;
            let ticket = if pace > 0.0 || budget.is_some() {
                match s.plugins.paces.reserve(
                    crate::auth::hex_digest(&key),
                    Duration::from_secs_f64(pace / 1000.0),
                    budget,
                ) {
                    Ok(ticket) => Some(ticket),
                    Err(after) => {
                        return Ok(json!({ "queued": false, "retryAfter": after.as_millis() }));
                    }
                }
            } else {
                None
            };
            link.send(Command::Send(Outgoing {
                text: message,
                name,
                ticket,
                _slot: slot,
            }))?;
            return Ok(json!({ "queued": true }));
        }
        "socket.close" => {
            let code = args["code"].as_u64().unwrap_or(1000);
            if code != 1000 && !(3000..=4999).contains(&code) {
                return Err(Error::bad("Close with 1000 or a code from 3000 to 4999."));
            }
            link.send(Command::Close(code as u16))?;
        }
        "socket.timer" => {
            let name = text(args, "name");
            if !valid_name(name) {
                return Err(Error::bad("Name timers with lowercase letters and digits."));
            }
            if args["cancel"] == true {
                link.send(Command::Cancel(name.to_owned()))?;
                return Ok(Value::Null);
            }
            let at = args["at"].as_f64().unwrap_or(-1.0);
            let every = args["every"].as_f64();
            if !(0.0..=86_400_000.0).contains(&at)
                || every.is_some_and(|every| !(100.0..=3_600_000.0).contains(&every))
            {
                return Err(Error::bad(
                    "Timers fire within a day, and repeat every 100 ms to 1 hour.",
                ));
            }
            link.send(Command::Timer {
                name: name.to_owned(),
                at: Duration::from_secs_f64(at / 1000.0),
                every: every.map(|every| Duration::from_secs_f64(every / 1000.0)),
            })?;
        }
        _ => return Err(Error::forbidden("Unknown plugin capability.")),
    }
    Ok(Value::Null)
}

/// Runs one `socket` event in the transport worker. Host refusals come back with their
/// message; `cairn.fail` stops the connection until the owner reviews it.
pub async fn deliver(
    s: &Service,
    connection: &Connection,
    session: &mut Session,
    link: &Link,
    event: Value,
) -> Result<Value> {
    let digest = super::registry::digest(session.manifest(), session.sources())?;
    super::registry::require_digest(s, &connection.plugin_id, &digest).await?;
    let context = Context {
        connection: connection.clone(),
        manifest: session.manifest().clone(),
        event: json!({ "name": "socket", "revision": connection.revision }),
        link: Some(link.clone()),
        draft: None,
        resource_grant: None,
    };
    let refused = Mutex::new(None::<Error>);
    let (context_ref, refused_ref, digest_ref) = (&context, &refused, &digest);
    let result = session
        .dispatch("socket", event, |method, args| async move {
            capabilities::approved(s, context_ref, digest_ref, &method, args)
                .await
                .map_err(|error| {
                    let code = capabilities::code(&error);
                    *refused_ref.lock().unwrap() = Some(error);
                    Denied(code)
                })
        })
        .await;
    super::registry::require_digest(s, &connection.plugin_id, &digest).await?;
    match (result, refused.into_inner().unwrap()) {
        (Ok(value), _) => Ok(value),
        (Err(message), _) if message.starts_with(cairn_plugin_api::REJECTED) => Err(
            Error::forbidden(message.trim_start_matches(cairn_plugin_api::REJECTED)),
        ),
        (Err(_), Some(error)) => Err(error),
        (Err(_), None) => Err(Error::bad_gateway(
            "The plugin failed while handling its connection.",
        )),
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A header the host adds when connecting: name and value.
type Header = Option<(String, String)>;

struct Pending {
    at: Instant,
    url: String,
    header: Header,
    oversized: Vec<String>,
}

struct State {
    socket: Option<Socket>,
    /// The connection's named timers: when each fires next, and its period.
    timers: BTreeMap<String, (Instant, Option<Duration>)>,
    outgoing: VecDeque<Outgoing>,
    connect: Option<Pending>,
    oversized: Vec<String>,
    last_attempt: Option<Instant>,
    backoff: Duration,
}

enum Wake {
    Stop,
    Queued,
    Frame(Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>),
    Timer,
    Send,
    Connect,
}

/// Only the requested fields; `*` projects each array entry without keeping large
/// sibling fields. Merge paths recursively so array entries retain their identities.
fn project(message: &str, paths: &[String]) -> Option<String> {
    fn select(value: &Value, keys: &[&str]) -> Option<Value> {
        let Some((key, rest)) = keys.split_first() else {
            return Some(value.clone());
        };
        if *key == "*" {
            return Some(Value::Array(
                value
                    .as_array()?
                    .iter()
                    .map(|value| select(value, rest).unwrap_or(Value::Null))
                    .collect(),
            ));
        }
        Some(json!({ *key: select(value.get(*key)?, rest)? }))
    }
    fn merge(target: &mut Value, value: Value) {
        match (target, value) {
            (Value::Object(target), Value::Object(value)) => {
                for (key, value) in value {
                    merge(target.entry(key).or_insert(Value::Null), value);
                }
            }
            (Value::Array(target), Value::Array(value)) => {
                for (target, value) in target.iter_mut().zip(value) {
                    merge(target, value);
                }
            }
            (target, value) if !value.is_null() => *target = value,
            _ => {}
        }
    }
    if paths.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(message).ok()?;
    let mut output = Value::Object(Map::new());
    for path in paths {
        if let Some(value) = select(&value, &path.split('.').collect::<Vec<_>>()) {
            merge(&mut output, value);
        }
    }
    serde_json::to_string(&output)
        .ok()
        .filter(|text| text.len() <= MAX_MESSAGE)
}

/// The script text is nested inside a JSON worker frame. Escaping and the envelope
/// count toward its limit too, including messages under the raw wire threshold.
fn message_event(message: &str, paths: &[String]) -> Value {
    fn fits(event: &Value) -> bool {
        serde_json::to_vec(&Frame::Event {
            name: "socket".into(),
            payload: event.clone(),
        })
        .is_ok_and(|bytes| bytes.len() < MAX_FRAME_BYTES)
    }
    if message.len() <= MAX_MESSAGE {
        let event = json!({ "type": "message", "text": message });
        if fits(&event) {
            return event;
        }
    }
    let mut event =
        json!({ "type": "message", "oversized": message.len(), "text": project(message, paths) });
    if !fits(&event) {
        event["text"] = Value::Null;
    }
    event
}

fn closed(code: Option<u16>, reason: &str, error: Option<&str>) -> Value {
    json!({
        "type": "close",
        "code": code,
        "reason": reason,
        "error": error,
    })
}

impl State {
    /// Applies the handler's commands. A failed send ends the socket like a remote close.
    async fn apply(&mut self, command: Command) -> Option<Value> {
        match command {
            Command::Connect {
                url,
                header,
                after,
                oversized,
            } => {
                // Reconnections slow down until the package reports a healthy connection.
                let paced = self.last_attempt.map_or_else(Instant::now, |last| {
                    (last + self.backoff).max(Instant::now())
                });
                self.connect = Some(Pending {
                    at: paced.max(Instant::now() + after),
                    url,
                    header,
                    oversized,
                });
            }
            Command::Send(message) => {
                if message.ticket.is_some() {
                    self.outgoing.push_back(message);
                } else {
                    return self.send(message).await;
                }
            }
            Command::Close(code) => {
                if let Some(mut socket) = self.socket.take() {
                    let frame = CloseFrame {
                        code: CloseCode::from(code),
                        reason: "".into(),
                    };
                    let _ = tokio::time::timeout(Duration::from_secs(2), socket.close(Some(frame)))
                        .await;
                }
                self.timers.clear();
                self.outgoing.clear();
            }
            Command::Timer { name, at, every } => {
                if self.timers.len() < MAX_TIMERS || self.timers.contains_key(&name) {
                    self.timers.insert(name, (Instant::now() + at, every));
                }
            }
            Command::Cancel(name) => {
                self.timers.remove(&name);
            }
            Command::Healthy => self.backoff = Duration::from_secs(1),
        }
        None
    }

    /// Timers belong to one connection: they stop when it closes.
    fn drop_socket(&mut self) {
        self.socket = None;
        self.timers.clear();
        self.outgoing.clear();
    }

    async fn shutdown(&mut self) {
        if let Some(mut socket) = self.socket.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), socket.close(None)).await;
        }
    }

    async fn send(&mut self, message: Outgoing) -> Option<Value> {
        let socket = self.socket.as_mut()?;
        let sent = tokio::time::timeout(
            Duration::from_secs(10),
            socket.send(Message::Text(message.text.into())),
        )
        .await;
        if !matches!(sent, Ok(Ok(()))) {
            self.drop_socket();
            return Some(closed(None, "", Some("interrupted")));
        }
        message
            .name
            .map(|name| json!({ "type": "sent", "name": name, "sent": true }))
    }

    async fn send_pending(&mut self) -> Option<Value> {
        let index = self
            .outgoing
            .iter()
            .enumerate()
            .min_by_key(|(_, message)| message.ticket.as_ref().unwrap().at())
            .map(|(index, _)| index)?;
        let mut message = self.outgoing.remove(index)?;
        match message.ticket.take()?.start() {
            pacing::Ready::Send(sending) => {
                let event = self.send(message).await;
                drop(sending);
                event
            }
            pacing::Ready::Wait(ticket) => {
                message.ticket = Some(ticket);
                self.outgoing.push_back(message);
                None
            }
            pacing::Ready::Expired => message.name.map(|name| {
                json!({
                    "type": "sent",
                    "name": name,
                    "sent": false,
                    "retryAfter": 0,
                })
            }),
        }
    }

    async fn open(&mut self, s: &Service) -> Value {
        let Some(Pending {
            url,
            header,
            oversized,
            ..
        }) = self.connect.take()
        else {
            return Value::Null;
        };
        self.oversized = oversized;
        self.last_attempt = Some(Instant::now());
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
        self.drop_socket();
        let url = match s.plugins.socket_origin() {
            Some(origin) => url::Url::parse(&url).map_or(url, |parsed| {
                format!(
                    "{origin}{}{}",
                    parsed.path(),
                    parsed.query().map(|q| format!("?{q}")).unwrap_or_default()
                )
            }),
            None => url,
        };
        let Ok(mut request) = url.as_str().into_client_request() else {
            return closed(None, "", Some("unavailable"));
        };
        if let Some((name, value)) = header
            && let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(&value),
            )
        {
            request.headers_mut().insert(name, value);
        }
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_WIRE))
            .max_frame_size(Some(MAX_WIRE));
        match tokio::time::timeout(
            Duration::from_secs(15),
            connect_async_with_config(request, Some(config), false),
        )
        .await
        {
            Ok(Ok((socket, _))) => {
                self.socket = Some(socket);
                json!({ "type": "open" })
            }
            Ok(Err(_)) => closed(None, "", Some("unavailable")),
            Err(_) => closed(None, "", Some("timeout")),
        }
    }
}

/// Keeps the instance's transport worker and its socket until stopped. Returns an
/// error when the worker must be replaced, or when the package or Cairn refused the
/// connection for good (403: the owner must review it).
pub async fn run(
    s: &Service,
    connection: &Connection,
    package: &Package,
    stop: &CancellationToken,
) -> Result<()> {
    let mut session = s
        .plugins
        .host
        .start(&s.plugins.executable()?, &package.manifest, &package.source)
        .await
        .map_err(Error::bad_gateway)?;
    let result = serve(s, connection, &mut session, stop).await;
    session.stop().await;
    result
}

async fn serve(
    s: &Service,
    connection: &Connection,
    session: &mut Session,
    stop: &CancellationToken,
) -> Result<()> {
    let (link, mut commands) = Link::channel();
    let mut state = State {
        socket: None,
        timers: BTreeMap::new(),
        outgoing: VecDeque::new(),
        connect: None,
        oversized: Vec::new(),
        last_attempt: None,
        backoff: Duration::from_secs(1),
    };
    let mut events = VecDeque::from([
        json!({ "type": "start", "instance": connection.id, "settings": connection.settings }),
    ]);
    loop {
        // A completion handler may immediately enqueue its next completion. Check
        // cancellation even when that FIFO never becomes empty.
        if stop.is_cancelled() {
            state.shutdown().await;
            return Ok(());
        }
        if let Some(value) = events.pop_front() {
            match deliver(s, connection, session, &link, value).await {
                Err(error) if matches!(error.status, 401 | 403) => {
                    if let Some(mut socket) = state.socket.take() {
                        let _ = socket.close(None).await;
                    }
                    return Err(error);
                }
                _ if !session.alive() => {
                    return Err(Error::bad_gateway("Plugin worker stopped."));
                }
                _ => {}
            }
            while let Ok(command) = commands.try_recv() {
                if let Some(value) = state.apply(command).await {
                    events.push_back(value);
                }
            }
            if events.len() > 128 {
                return Err(Error::bad_gateway("Plugin socket event queue is full."));
            }
        }
        let timer_at = state.timers.values().map(|(at, _)| *at).min();
        let connect_at = state.connect.as_ref().map(|pending| pending.at);
        let send_at = state
            .outgoing
            .iter()
            .filter_map(|message| message.ticket.as_ref().map(pacing::Ticket::at))
            .min();
        let wake = tokio::select! {
            () = stop.cancelled() => Wake::Stop,
            () = async {}, if !events.is_empty() => Wake::Queued,
            frame = async { state.socket.as_mut().expect("socket").next().await }, if state.socket.is_some() => Wake::Frame(frame),
            () = tokio::time::sleep_until(timer_at.unwrap_or_else(Instant::now)), if timer_at.is_some() => Wake::Timer,
            () = tokio::time::sleep_until(send_at.unwrap_or_else(Instant::now)), if send_at.is_some() => Wake::Send,
            () = tokio::time::sleep_until(connect_at.unwrap_or_else(Instant::now)), if connect_at.is_some() => Wake::Connect,
        };
        let event = match wake {
            Wake::Queued => None,
            Wake::Stop => {
                state.shutdown().await;
                return Ok(());
            }
            Wake::Timer => {
                let now = Instant::now();
                let due = state
                    .timers
                    .iter()
                    .filter(|(_, (at, _))| *at <= now)
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(name, _)| name.clone());
                due.map(|name| {
                    match state.timers.get(&name).and_then(|(_, every)| *every) {
                        Some(every) => {
                            state
                                .timers
                                .insert(name.clone(), (now + every, Some(every)));
                        }
                        None => {
                            state.timers.remove(&name);
                        }
                    }
                    json!({ "type": "tick", "name": name })
                })
            }
            Wake::Send => state.send_pending().await,
            Wake::Connect => Some(state.open(s).await),
            Wake::Frame(Some(Ok(Message::Text(message)))) => {
                Some(message_event(message.as_str(), &state.oversized))
            }
            Wake::Frame(Some(Ok(Message::Ping(bytes)))) => {
                let socket = state.socket.as_mut().expect("socket");
                if socket.send(Message::Pong(bytes)).await.is_err() {
                    state.drop_socket();
                    Some(closed(None, "", Some("interrupted")))
                } else {
                    None
                }
            }
            Wake::Frame(Some(Ok(Message::Close(frame)))) => {
                state.drop_socket();
                let (code, reason) = frame.map_or((None, String::new()), |frame| {
                    (Some(u16::from(frame.code)), frame.reason.to_string())
                });
                Some(closed(
                    code,
                    &reason.chars().take(120).collect::<String>(),
                    None,
                ))
            }
            Wake::Frame(Some(Ok(_))) => None,
            Wake::Frame(Some(Err(_)) | None) => {
                state.drop_socket();
                Some(closed(None, "", Some("interrupted")))
            }
        };
        if let Some(event) = event {
            events.push_back(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_projection_includes_json_escaping_in_the_worker_limit() {
        let payload =
            json!({ "op": 0, "data": "\\".repeat(400_000), "unused": "x".repeat(500_000) })
                .to_string();
        let paths = vec!["op".into(), "data".into()];
        assert!(project(&payload, &paths).unwrap().len() < MAX_MESSAGE);
        let event = message_event(&payload, &paths);
        let encoded = serde_json::to_vec(&Frame::Event {
            name: "socket".into(),
            payload: event.clone(),
        })
        .unwrap();
        assert!(encoded.len() < MAX_FRAME_BYTES);
        assert!(
            event["text"].is_null(),
            "the requested projection cannot fit the frame"
        );
        assert_eq!(message_event("small", &paths)["text"], "small");
    }

    #[test]
    fn wire_messages_below_one_megabyte_project_when_the_ipc_frame_would_overflow() {
        let payload = json!({ "op": 0, "data": "\\".repeat(400_000) }).to_string();
        assert!(payload.len() < MAX_MESSAGE);
        let event = message_event(&payload, &["op".into()]);
        assert_eq!(event["text"], "{\"op\":0}");
        assert_eq!(event["oversized"], payload.len());
    }
}
