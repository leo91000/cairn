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
use cairn_plugin_host::{Denied, Session};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Mutex, time::Duration};
use tokio::{net::TcpStream, sync::mpsc, time::Instant};
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

/// Largest frame a package sends or receives.
pub const MAX_MESSAGE: usize = 1_048_576;
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// What a transport worker's capability calls ask the host to do. Applied in order
/// once the running handler returns.
pub enum Command {
    Connect {
        url: String,
        header: Header,
    },
    Send(String),
    Close(u16),
    Every {
        every: Duration,
        first: Duration,
    },
    /// The package reported its connection usable: reconnect quickly next time.
    Healthy,
}

/// The transport worker's handle on its socket, given to its capability calls.
#[derive(Clone)]
pub struct Link(mpsc::UnboundedSender<Command>);

impl Link {
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<Command>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self(sender), receiver)
    }

    pub(crate) fn send(&self, command: Command) -> Result<()> {
        self.0
            .send(command)
            .map_err(|_| Error::bad_gateway("The connection stopped."))
    }
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
            link.send(Command::Connect { url, header })?;
        }
        "socket.send" => {
            let mut message = args["text"]
                .as_str()
                .ok_or_else(|| Error::bad("Send text."))?
                .to_owned();
            if message.len() > MAX_MESSAGE {
                return Err(Error::too_large("Socket messages are limited to 1 MB."));
            }
            // `{{secret}}` marks where the host puts the credential, JSON-escaped.
            if let Some(auth) = enabled(args.get("auth")) {
                if !message.contains("{{secret}}") {
                    return Err(Error::bad("Mark the secret's place with {{secret}}."));
                }
                let (_, _, secret) = capabilities::authorization(s, context, auth)
                    .await?
                    .ok_or_else(|| Error::forbidden("This instance has no credential."))?;
                let escaped = serde_json::to_string(&secret)?;
                message = message.replace("{{secret}}", &escaped[1..escaped.len() - 1]);
            }
            link.send(Command::Send(message))?;
        }
        "socket.close" => {
            let code = args["code"].as_u64().unwrap_or(1000);
            if code != 1000 && !(3000..=4999).contains(&code) {
                return Err(Error::bad("Close with 1000 or a code from 3000 to 4999."));
            }
            link.send(Command::Close(code as u16))?;
        }
        "socket.every" => {
            let every = args["ms"].as_f64().unwrap_or(-1.0);
            if every != 0.0 && !(100.0..=3_600_000.0).contains(&every) {
                return Err(Error::bad(
                    "Ticks run every 100 ms to 1 hour, or 0 to stop.",
                ));
            }
            let first = args["first"]
                .as_f64()
                .unwrap_or(every)
                .clamp(0.0, 3_600_000.0);
            link.send(Command::Every {
                every: Duration::from_millis(every as u64),
                first: Duration::from_millis(first as u64),
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
    let context = Context {
        connection: connection.clone(),
        manifest: session.manifest().clone(),
        event: json!({ "name": "socket", "revision": connection.revision }),
        link: Some(link.clone()),
        draft: None,
    };
    let refused = Mutex::new(None::<Error>);
    let (context_ref, refused_ref) = (&context, &refused);
    let result = session
        .dispatch("socket", event, |method, args| async move {
            capabilities::checked(s, context_ref, &method, args)
                .await
                .map_err(|error| {
                    let code = capabilities::code(&error);
                    *refused_ref.lock().unwrap() = Some(error);
                    Denied(code)
                })
        })
        .await;
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

struct State {
    socket: Option<Socket>,
    tick: Option<(Instant, Duration)>,
    connect: Option<(Instant, String, Header)>,
    last_attempt: Option<Instant>,
    backoff: Duration,
}

enum Wake {
    Stop,
    Frame(Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>),
    Tick,
    Connect,
}

fn closed(code: Option<u16>, reason: &str, error: Option<&str>) -> Value {
    json!({ "type": "close", "code": code, "reason": reason, "error": error })
}

impl State {
    /// Applies the handler's commands. A failed send ends the socket like a remote close.
    async fn apply(&mut self, command: Command) -> Option<Value> {
        match command {
            Command::Connect { url, header } => {
                // Reconnections slow down until the package reports a healthy connection.
                let at = self.last_attempt.map_or_else(Instant::now, |last| {
                    (last + self.backoff).max(Instant::now())
                });
                self.connect = Some((at, url, header));
            }
            Command::Send(message) => {
                let socket = self.socket.as_mut()?;
                if socket.send(Message::Text(message.into())).await.is_err() {
                    self.drop_socket();
                    return Some(closed(None, "", Some("interrupted")));
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
                self.tick = None;
            }
            Command::Every { every, first } => {
                self.tick = (!every.is_zero()).then(|| (Instant::now() + first, every));
            }
            Command::Healthy => self.backoff = Duration::from_secs(1),
        }
        None
    }

    fn drop_socket(&mut self) {
        self.socket = None;
        self.tick = None;
    }

    async fn open(&mut self, s: &Service) -> Value {
        let Some((_, url, header)) = self.connect.take() else {
            return Value::Null;
        };
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
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
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
        tick: None,
        connect: None,
        last_attempt: None,
        backoff: Duration::from_secs(1),
    };
    let mut event = Some(json!({
        "type": "start",
        "instance": connection.id,
        "settings": connection.settings,
    }));
    loop {
        if let Some(value) = event.take() {
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
                    event = Some(value);
                }
            }
            if event.is_some() {
                continue;
            }
        }
        let tick_at = state.tick.map(|(at, _)| at);
        let connect_at = state.connect.as_ref().map(|(at, _, _)| *at);
        let wake = tokio::select! {
            () = stop.cancelled() => Wake::Stop,
            frame = async { state.socket.as_mut().expect("socket").next().await }, if state.socket.is_some() => Wake::Frame(frame),
            () = tokio::time::sleep_until(tick_at.unwrap_or_else(Instant::now)), if tick_at.is_some() => Wake::Tick,
            () = tokio::time::sleep_until(connect_at.unwrap_or_else(Instant::now)), if connect_at.is_some() => Wake::Connect,
        };
        event = match wake {
            Wake::Stop => {
                if let Some(mut socket) = state.socket.take() {
                    let _ = tokio::time::timeout(Duration::from_secs(2), socket.close(None)).await;
                }
                return Ok(());
            }
            Wake::Tick => {
                if let Some((at, every)) = state.tick.as_mut() {
                    *at = Instant::now() + *every;
                }
                Some(json!({ "type": "tick" }))
            }
            Wake::Connect => Some(state.open(s).await),
            Wake::Frame(Some(Ok(Message::Text(message)))) => {
                Some(json!({ "type": "message", "text": message.as_str() }))
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
    }
}
