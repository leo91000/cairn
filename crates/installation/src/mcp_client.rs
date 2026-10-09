use crate::{
    error::{Error, Result},
    mcps::record::{AuthKind, McpSecrets, McpServer},
    network,
    process::{Environment, command},
    rpc::{
        Session,
        jsonrpc::{Frame, METHOD_NOT_FOUND, Message},
    },
    service::Service,
    validation::text,
};
use reqwest::{
    Method,
    header::{HeaderMap, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{collections::HashSet, time::Duration};

pub const MODERN: &str = "2026-07-28";

/// Every supported protocol version, newest first. The others use `initialize`.
pub const VERSIONS: &[&str] = &[
    MODERN,
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];

/// Offered to servers without the modern protocol, and chosen for legacy clients.
pub const LEGACY: &str = "2025-11-25";
const MAX_TOOLS: usize = 1000;

pub fn client_info() -> Value {
    json!({
        "name": "cairn-mcp-client",
        "version": env!("CARGO_PKG_VERSION")
    })
}

/// Request `_meta` that replaces the `initialize` handshake in the modern protocol.
pub fn modern_meta() -> Map<String, Value> {
    Map::from_iter([
        (
            "io.modelcontextprotocol/protocolVersion".to_owned(),
            MODERN.into(),
        ),
        (
            "io.modelcontextprotocol/clientInfo".to_owned(),
            client_info(),
        ),
        (
            "io.modelcontextprotocol/clientCapabilities".to_owned(),
            json!({}),
        ),
    ])
}

fn unsupported_version() -> Error {
    Error::bad_gateway("No supported MCP protocol version.")
}

pub struct Client {
    transport: Transport,
    pub capabilities: Value,
    protocol: String,
    sequence: u64,
}

enum Transport {
    Stdio(Session),
    Http(HttpTransport),
}

struct HttpTransport {
    url: String,
    allow_private: bool,
    bearer: String,
    session: Option<String>,
}

impl Client {
    pub async fn connect(s: &Service, item: &Value) -> Result<Self> {
        Self::connect_server(s, &McpServer::deserialize(item)?).await
    }

    /// Connects with the stored credentials, refreshing OAuth tokens that expired
    /// or that the server rejects.
    pub async fn connect_server(s: &Service, server: &McpServer) -> Result<Self> {
        let refreshable =
            |secrets: &McpSecrets| server.uses_oauth() && !secrets.refresh_token().is_empty();
        let mut secrets = s.mcps.server_secrets(s, &server.id).await?;
        let expired = secrets
            .token_expires_at
            .is_some_and(|time| time <= crate::config::now());
        if expired && refreshable(&secrets) {
            crate::mcp_oauth::refresh(s, server).await?;
            secrets = s.mcps.server_secrets(s, &server.id).await?;
        }
        let first = Self::open(s, server, &secrets).await;
        if first.as_ref().is_err_and(Error::is_unauthorized) && refreshable(&secrets) {
            crate::mcp_oauth::refresh(s, server).await?;
            let secrets = s.mcps.server_secrets(s, &server.id).await?;
            return Self::open(s, server, &secrets).await;
        }
        first
    }

    /// Connects, reads the tool catalog and disconnects.
    pub async fn list_tools(s: &Service, server: &McpServer) -> Result<Vec<Value>> {
        let mut client = Self::connect_server(s, server).await?;
        let tools = client.discover().await;
        client.close().await;
        tools
    }

    fn new(transport: Transport, protocol: &str) -> Self {
        Self {
            transport,
            capabilities: json!({}),
            protocol: protocol.into(),
            sequence: 0,
        }
    }

    fn is_stdio(&self) -> bool {
        matches!(self.transport, Transport::Stdio(_))
    }

    async fn open(s: &Service, server: &McpServer, secrets: &McpSecrets) -> Result<Self> {
        let mut client = Self::new(Self::transport(s, server, secrets).await?, MODERN);
        match client.request("server/discover", json!({})).await {
            Ok(discovery) => {
                let modern = discovery["supportedVersions"]
                    .as_array()
                    .is_some_and(|versions| versions.iter().any(|v| v == MODERN));
                if !modern {
                    client.close().await;
                    return Err(unsupported_version());
                }
                client.capabilities = discovery["capabilities"].clone();
                Ok(client)
            }
            Err(error) if [400, 405, 501].contains(&error.status) || client.is_stdio() => {
                client.initialize_legacy(s, server, secrets).await
            }
            Err(error) => {
                client.close().await;
                Err(error)
            }
        }
    }

    async fn initialize_legacy(
        self,
        s: &Service,
        server: &McpServer,
        secrets: &McpSecrets,
    ) -> Result<Self> {
        // Command servers may exit on the unknown modern probe: restart them.
        let mut client = if self.is_stdio() {
            self.close().await;
            Self::new(Self::transport(s, server, secrets).await?, LEGACY)
        } else {
            self
        };
        client.protocol = LEGACY.into();
        let params = json!({
            "protocolVersion": LEGACY,
            "capabilities": {},
            "clientInfo": client_info(),
        });
        let result = client.request("initialize", params).await?;
        let version = text(&result, "protocolVersion");
        if !VERSIONS[1..].contains(&version) {
            return Err(unsupported_version());
        }
        client.protocol = version.into();
        client.capabilities = result["capabilities"].clone();
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(client)
    }

    async fn transport(s: &Service, server: &McpServer, secrets: &McpSecrets) -> Result<Transport> {
        if server.is_http() {
            let bearer = match server.auth {
                AuthKind::Bearer => secrets.token(),
                AuthKind::OAuth => secrets.access_token(),
                AuthKind::None => "",
            };
            return Ok(Transport::Http(HttpTransport {
                url: server.url.clone(),
                allow_private: server.allow_private_network,
                bearer: bearer.into(),
                session: None,
            }));
        }
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
        let env = Environment::from([
            ("HOME".into(), s.config.home.to_string_lossy().into_owned()),
            ("PATH".into(), path),
        ]);
        let mut env = crate::toolkit::environment(&s.config.home, env).await?;
        if let Some(secrets) = &secrets.env {
            env.extend(secrets.clone());
        }
        let command = command(&server.command, &server.args, &env, Some(&s.config.home));
        Ok(Transport::Stdio(
            Session::spawn_with_protocol(command, true).await?,
        ))
    }

    fn is_modern(&self) -> bool {
        self.protocol == MODERN
    }

    fn with_meta(&self, params: Value) -> Value {
        let mut params = if params.is_object() {
            params
        } else {
            json!({})
        };
        if self.is_modern() {
            let meta = &mut params["_meta"];
            if !meta.is_object() {
                *meta = json!({});
            }
            if let Some(meta) = meta.as_object_mut() {
                meta.extend(modern_meta());
            }
        }
        params
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let params = self.with_meta(params);
        match &mut self.transport {
            Transport::Stdio(session) => session.request(method, params).await,
            Transport::Http(http) => {
                self.sequence += 1;
                http.request(self.sequence, &self.protocol, method, params)
                    .await
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        match &mut self.transport {
            Transport::Stdio(session) => session.rpc.notify(method, params).await,
            Transport::Http(http) => http.notify(&self.protocol, method, params).await,
        }
    }

    pub async fn discover(&mut self) -> Result<Vec<Value>> {
        if self.capabilities.get("tools").is_none() {
            return Ok(vec![]);
        }
        let mut tools = Vec::new();
        let mut cursors = HashSet::new();
        let mut cursor = Value::Null;
        loop {
            let params = if cursor.is_null() {
                json!({})
            } else {
                json!({ "cursor": cursor })
            };
            let page = self.request("tools/list", params).await?;
            let Some(batch) = page["tools"].as_array() else {
                return Err(Error::bad_gateway(
                    "MCP server returned an invalid tool catalog.",
                ));
            };
            tools.extend(batch.iter().cloned());
            cursor = page["nextCursor"].clone();
            let repeated = !cursor.is_null() && !cursors.insert(cursor.to_string());
            if tools.len() > MAX_TOOLS || repeated {
                return Err(Error::bad_gateway(
                    "Tool catalog is too large or has an invalid cursor.",
                ));
            }
            if cursor.is_null() {
                return Ok(tools);
            }
        }
    }

    pub async fn close(self) {
        match self.transport {
            Transport::Stdio(session) => session.close().await,
            Transport::Http(http) => http.close(&self.protocol).await,
        }
    }
}

fn check_status(status: u16) -> Result<()> {
    match status {
        200..=299 => Ok(()),
        401 => Err(Error::unauthorized("Sign in to connect this server.")),
        400 | 405 => Err(Error::new(status, "MCP protocol negotiation failed.")),
        _ => Err(Error::bad_gateway("MCP request failed.")),
    }
}

fn response_result(reply: &Value, id: u64) -> Result<Value> {
    if reply["id"] != id {
        return Err(Error::bad_gateway("MCP response identifier mismatch."));
    }
    if !reply["error"].is_null() {
        let status = if reply["error"]["code"] == METHOD_NOT_FOUND {
            501
        } else {
            502
        };
        return Err(Error::new(
            status,
            "MCP server could not complete the request.",
        ));
    }
    Ok(reply["result"].clone())
}

impl HttpTransport {
    fn headers(&self, protocol: &str) -> Result<HeaderMap> {
        headers(&self.bearer, protocol, self.session.as_deref())
    }

    async fn post(&self, headers: HeaderMap, frame: &Frame) -> Result<network::Response> {
        let body = serde_json::to_vec(frame)?;
        network::fetch(
            &self.url,
            Method::POST,
            headers,
            Some(body),
            self.allow_private,
        )
        .await
    }

    async fn request(
        &mut self,
        id: u64,
        protocol: &str,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let modern = protocol == MODERN;
        let mut headers = self.headers(protocol)?;
        if modern {
            let value =
                HeaderValue::from_str(method).map_err(|_| Error::bad("Invalid MCP method."))?;
            headers.insert("mcp-method", value);
            if ["tools/call", "prompts/get"].contains(&method) {
                let value = HeaderValue::from_str(text(&params, "name"))
                    .map_err(|_| Error::bad("Invalid MCP name."))?;
                headers.insert("mcp-name", value);
            }
        }
        let method = method.to_owned();
        let frame = Frame::strict(Message::Request { id, method, params });
        let response = self.post(headers, &frame).await?;
        check_status(response.status)?;
        if !modern
            && let Some(value) = response
                .headers
                .get("mcp-session-id")
                .and_then(|v| v.to_str().ok())
        {
            self.session = Some(value.into());
        }
        let reply = if network::is_event_stream(&response.headers) {
            sse_result(&response.bytes, &json!(id))?
        } else {
            response.json()?
        };
        response_result(&reply, id)
    }

    async fn notify(&self, protocol: &str, method: &str, params: Value) -> Result<()> {
        let method = method.to_owned();
        let frame = Frame::strict(Message::Notification { method, params });
        let response = self.post(self.headers(protocol)?, &frame).await?;
        if !(200..300).contains(&response.status) {
            return Err(Error::bad_gateway("MCP initialization failed."));
        }
        Ok(())
    }

    /// Ends a legacy session. Best effort: the server expires abandoned sessions.
    async fn close(self, protocol: &str) {
        if self.session.is_none() {
            return;
        }
        let Ok(headers) = self.headers(protocol) else {
            return;
        };
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            network::fetch(&self.url, Method::DELETE, headers, None, self.allow_private),
        )
        .await;
    }
}

pub fn headers(bearer: &str, version: &str, session: Option<&str>) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "accept",
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    headers.insert(
        "mcp-protocol-version",
        HeaderValue::from_str(version).map_err(|_| Error::bad("Invalid protocol version."))?,
    );
    if !bearer.is_empty() {
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {bearer}"))
                .map_err(|_| Error::bad("Invalid MCP credential."))?,
        );
    }
    if let Some(session) = session {
        headers.insert(
            "mcp-session-id",
            HeaderValue::from_str(session)
                .map_err(|_| Error::bad_gateway("Invalid MCP session."))?,
        );
    }
    Ok(headers)
}

pub fn sse_result(bytes: &[u8], id: &Value) -> Result<Value> {
    let data = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    for event in data.split("\n\n") {
        let data = event
            .lines()
            .filter_map(|line| {
                line.strip_prefix("data:")
                    .map(|line| line.strip_prefix(' ').unwrap_or(line))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if let Ok(value) = serde_json::from_str::<Value>(&data)
            && value.get("id") == Some(id)
            && (value.get("result").is_some() || value.get("error").is_some())
        {
            return Ok(value);
        }
    }
    Err(Error::bad_gateway("MCP stream ended without a response."))
}
