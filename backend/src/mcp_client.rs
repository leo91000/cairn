use crate::{
    error::{Error, Result},
    network,
    process::{Environment, command},
    rpc::Session,
    service::Service,
    validation::text,
};
use reqwest::{
    Method,
    header::{HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};

pub const MODERN: &str = "2026-07-28";
const LEGACY: &str = "2025-11-25";

pub struct Client {
    transport: Transport,
    pub capabilities: Value,
    protocol: String,
    sequence: u64,
}

enum Transport {
    Stdio(Session),
    Http {
        url: String,
        allow_private: bool,
        bearer: String,
        session: Option<String>,
    },
}

impl Client {
    pub async fn connect(s: &Service, item: &Value) -> Result<Self> {
        let mut secrets = s.mcps.secrets(s, text(item, "id")).await?;
        if item["auth"] == "oauth"
            && secrets["tokenExpiresAt"]
                .as_i64()
                .is_some_and(|time| time <= crate::config::now())
            && !text(&secrets["tokens"], "refresh_token").is_empty()
        {
            crate::mcp_oauth::refresh(s, item).await?;
            secrets = s.mcps.secrets(s, text(item, "id")).await?;
        }
        let first = Self::open(s, item, &secrets).await;
        if first.as_ref().is_err_and(|error| error.status == 401)
            && item["auth"] == "oauth"
            && !text(&secrets["tokens"], "refresh_token").is_empty()
        {
            crate::mcp_oauth::refresh(s, item).await?;
            return Self::open(s, item, &s.mcps.secrets(s, text(item, "id")).await?).await;
        }
        first
    }

    async fn open(s: &Service, item: &Value, secrets: &Value) -> Result<Self> {
        let transport = Self::transport(s, item, secrets).await?;
        let mut client = Self {
            transport,
            capabilities: json!({}),
            protocol: MODERN.into(),
            sequence: 0,
        };
        match client.request("server/discover", json!({})).await {
            Ok(discovery) => {
                if !discovery["supportedVersions"]
                    .as_array()
                    .is_some_and(|v| v.iter().any(|v| v == MODERN))
                {
                    client.close().await;
                    return Err(Error::new(502, "No supported MCP protocol version."));
                }
                client.capabilities = discovery["capabilities"].clone();
            }
            Err(error)
                if [400, 405, 501].contains(&error.status)
                    || matches!(&client.transport, Transport::Stdio(_)) =>
            {
                if matches!(&client.transport, Transport::Stdio(_)) {
                    client.close().await;
                    client = Self {
                        transport: Self::transport(s, item, secrets).await?,
                        capabilities: json!({}),
                        protocol: LEGACY.into(),
                        sequence: 0,
                    };
                }
                client.protocol = LEGACY.into();
                let result = client
                    .request(
                        "initialize",
                        json!({
                            "protocolVersion": LEGACY,
                            "capabilities": {},
                            "clientInfo": {
                                "name": "leo-mcp-client",
                                "version": env!("CARGO_PKG_VERSION")
                            }
                        }),
                    )
                    .await?;
                let version = text(&result, "protocolVersion");
                if !["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"].contains(&version) {
                    return Err(Error::new(502, "No supported MCP protocol version."));
                }
                client.protocol = version.into();
                client.capabilities = result["capabilities"].clone();
                client
                    .notify("notifications/initialized", json!({}))
                    .await?;
            }
            Err(error) => {
                client.close().await;
                return Err(error);
            }
        }
        Ok(client)
    }

    async fn transport(s: &Service, item: &Value, secrets: &Value) -> Result<Transport> {
        Ok(if item["transport"] == "stdio" {
            let mut env = Environment::from([
                ("HOME".into(), s.config.home.to_string_lossy().into_owned()),
                (
                    "PATH".into(),
                    std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
                ),
            ]);
            env = crate::toolkit::environment(&s.config.home, env).await?;
            if let Some(secrets) = secrets["env"].as_object() {
                for (key, value) in secrets {
                    env.insert(key.clone(), value.as_str().unwrap_or("").into());
                }
            }
            let args = item["args"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            Transport::Stdio(
                Session::spawn_with_protocol(
                    command(text(item, "command"), &args, &env, Some(&s.config.home)),
                    true,
                )
                .await?,
            )
        } else {
            Transport::Http {
                url: text(item, "url").into(),
                allow_private: item["allowPrivateNetwork"] == true,
                bearer: match text(item, "auth") {
                    "bearer" => text(secrets, "token"),
                    "oauth" => text(&secrets["tokens"], "access_token"),
                    _ => "",
                }
                .into(),
                session: None,
            }
        })
    }

    pub async fn request(&mut self, method: &str, mut params: Value) -> Result<Value> {
        if !params.is_object() {
            params = json!({});
        }
        let modern = self.protocol == MODERN;
        if modern {
            if !params["_meta"].is_object() {
                params["_meta"] = json!({});
            }
            params["_meta"]["io.modelcontextprotocol/protocolVersion"] = MODERN.into();
            params["_meta"]["io.modelcontextprotocol/clientInfo"] = json!({
                "name": "leo-mcp-client",
                "version": env!("CARGO_PKG_VERSION")
            });
            params["_meta"]["io.modelcontextprotocol/clientCapabilities"] = json!({});
        }
        match &mut self.transport {
            Transport::Stdio(session) => session.request(method, params).await,
            Transport::Http {
                url,
                allow_private,
                bearer,
                session,
            } => {
                self.sequence += 1;
                let id = self.sequence;
                let body = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": method,
                    "params": params
                });
                let mut headers = headers(bearer, &self.protocol, session.as_deref())?;
                if modern {
                    headers.insert(
                        "mcp-method",
                        HeaderValue::from_str(method)
                            .map_err(|_| Error::bad("Invalid MCP method."))?,
                    );
                    if ["tools/call", "prompts/get"].contains(&method) {
                        headers.insert(
                            "mcp-name",
                            HeaderValue::from_str(text(&params, "name"))
                                .map_err(|_| Error::bad("Invalid MCP name."))?,
                        );
                    }
                }
                let response = network::fetch(
                    url,
                    Method::POST,
                    headers,
                    Some(serde_json::to_vec(&body)?),
                    *allow_private,
                )
                .await?;
                if response.status == 401 {
                    return Err(Error::new(401, "Sign in to connect this server."));
                }
                if response.status == 400 || response.status == 405 {
                    return Err(Error::new(
                        response.status,
                        "MCP protocol negotiation failed.",
                    ));
                }
                if !(200..300).contains(&response.status) {
                    return Err(Error::new(502, "MCP request failed."));
                }
                if !modern
                    && let Some(value) = response
                        .headers
                        .get("mcp-session-id")
                        .and_then(|v| v.to_str().ok())
                {
                    *session = Some(value.into());
                }
                let result = if response
                    .headers
                    .get("content-type")
                    .is_some_and(|v| v.to_str().unwrap_or("").starts_with("text/event-stream"))
                {
                    sse_result(&response.bytes, &json!(id))?
                } else {
                    response.json()?
                };
                if result["id"] != id {
                    return Err(Error::new(502, "MCP response identifier mismatch."));
                }
                if !result["error"].is_null() {
                    return Err(Error::new(
                        if result["error"]["code"] == -32601 {
                            501
                        } else {
                            502
                        },
                        "MCP server could not complete the request.",
                    ));
                }
                Ok(result["result"].clone())
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        match &mut self.transport {
            Transport::Stdio(session) => session.rpc.notify(method, params).await,
            Transport::Http {
                url,
                allow_private,
                bearer,
                session,
            } => {
                let response = network::fetch(
                    url,
                    Method::POST,
                    headers(bearer, &self.protocol, session.as_deref())?,
                    Some(serde_json::to_vec(&json!({
                        "jsonrpc": "2.0",
                        "method": method,
                        "params": params
                    }))?),
                    *allow_private,
                )
                .await?;
                if !(200..300).contains(&response.status) {
                    return Err(Error::new(502, "MCP initialization failed."));
                }
                Ok(())
            }
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
                json!({
                    "cursor": cursor
                })
            };
            let page = self.request("tools/list", params).await?;
            tools.extend(
                page["tools"].as_array().cloned().ok_or_else(|| {
                    Error::new(502, "MCP server returned an invalid tool catalog.")
                })?,
            );
            cursor = page["nextCursor"].clone();
            if tools.len() > 1000 || (!cursor.is_null() && !cursors.insert(cursor.to_string())) {
                return Err(Error::new(
                    502,
                    "Tool catalog is too large or has an invalid cursor.",
                ));
            }
            if cursor.is_null() {
                break;
            }
        }
        Ok(tools)
    }

    pub async fn close(self) {
        match self.transport {
            Transport::Stdio(session) => session.close().await,
            Transport::Http {
                url,
                allow_private,
                bearer,
                session: Some(session),
            } => {
                if let Ok(headers) = headers(&bearer, &self.protocol, Some(&session)) {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(5),
                        network::fetch(&url, Method::DELETE, headers, None, allow_private),
                    )
                    .await;
                }
            }
            _ => {}
        }
    }
}

fn headers(bearer: &str, version: &str, session: Option<&str>) -> Result<HeaderMap> {
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
            HeaderValue::from_str(session).map_err(|_| Error::new(502, "Invalid MCP session."))?,
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
    Err(Error::new(502, "MCP stream ended without a response."))
}
