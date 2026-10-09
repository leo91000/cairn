use crate::{
    error::{Error, Result, required},
    http::{App, Input},
    mcp_client::{LEGACY, MODERN, VERSIONS},
    rpc::jsonrpc::{Frame, Message},
    service::Service,
    store::Db,
    validation::{parse, text},
};
use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::{Arc, LazyLock};

const INVALID_REQUEST: i64 = -32600;
const UNSUPPORTED_VERSION: i64 = -32022;
const INTERNAL_ERROR: i64 = -32603;
/// Protocol version assumed when a request names none.
const DEFAULT_VERSION: &str = "2025-03-26";
const PROTOCOL_META: &str = "io.modelcontextprotocol/protocolVersion";

/// Listing results that modern clients may cache privately.
const CACHEABLE: [&str; 5] = [
    "server/discover",
    "tools/list",
    "resources/list",
    "resources/templates/list",
    "prompts/list",
];

// Leave ample headroom below the gateway's 8 MiB ceiling: MCP serializes the
// result both as structured data and as escaped text for older clients.
const RUN_PAGE_BYTES: usize = 256 * 1024;

/// An entry of the management tool catalog (`schemas/mcp-tools.json`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogTool {
    name: String,
    description: String,
    scope: String,
    #[serde(default)]
    destructive: bool,
    input_schema: Value,
}

static CATALOG: LazyLock<Vec<CatalogTool>> =
    LazyLock::new(|| serde_json::from_str(include_str!("../schemas/mcp-tools.json")).unwrap());

#[derive(Serialize)]
struct TextContent {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

/// A `tools/call` result.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolResult {
    content: [TextContent; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_content: Option<Value>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    is_error: bool,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    meta: Option<Value>,
}

impl ToolResult {
    pub(crate) fn success(text: String, structured: Value) -> Self {
        Self {
            content: [TextContent { kind: "text", text }],
            structured_content: Some(structured),
            is_error: false,
            meta: None,
        }
    }

    pub(crate) fn failure(message: &str) -> Self {
        Self {
            content: [TextContent {
                kind: "text",
                text: message.to_owned(),
            }],
            structured_content: None,
            is_error: true,
            meta: None,
        }
    }

    /// Reports a failed call as a tool error, the protocol's way of showing it to the model.
    pub(crate) fn from_result(
        result: Result<Value>,
        describe: impl FnOnce(&Value) -> String,
    ) -> Result<Value> {
        let result = match result {
            Ok(value) => Self::success(describe(&value), value),
            Err(error) => Self::failure(&error.message),
        };
        Ok(serde_json::to_value(result)?)
    }
}

/// An empty listing for methods a server supports but has nothing for.
pub(crate) fn empty_listing(method: &str) -> Option<Value> {
    let key = match method {
        "resources/list" => "resources",
        "resources/templates/list" => "resourceTemplates",
        "prompts/list" => "prompts",
        _ => return None,
    };
    Some(Value::Object(Map::from_iter([(key.to_owned(), json!([]))])))
}

/// The three MCP endpoints this server exposes.
enum Endpoint {
    /// `/mcp`: the owner's management tools, authorized by personal tokens.
    Management,
    /// `/mcp-workspace`: run-scoped workspace tools.
    Workspace,
    /// `/mcp-gateway/{id}`: a run-scoped proxy to a configured connection.
    Gateway(String),
}

impl Endpoint {
    fn of(path: &str) -> Self {
        if path == "/mcp-workspace" {
            return Self::Workspace;
        }
        match path.strip_prefix("/mcp-gateway/") {
            Some(id) => Self::Gateway(id.to_owned()),
            None => Self::Management,
        }
    }
}

fn bearer(request: &Request) -> String {
    request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(kind, _)| kind.eq_ignore_ascii_case("bearer"))
        .map(|(_, value)| value.into())
        .unwrap_or_default()
}

async fn resource_metadata(s: &Service) -> String {
    format!(
        "{}/.well-known/oauth-protected-resource/mcp",
        crate::relay::beacon_address(&s.config.data_dir)
            .await
            .ok()
            .flatten()
            .map_or_else(String::new, |(origin, _)| origin)
    )
}

/// Checks the bearer for the endpoint. Management clients without a valid token
/// get the OAuth challenge that starts their sign-in.
async fn authenticate(s: &Service, endpoint: &Endpoint, bearer: &str) -> Result<Option<Response>> {
    match endpoint {
        Endpoint::Workspace => {
            crate::project_workspaces::authorize(s, bearer).await?;
        }
        Endpoint::Gateway(id) => {
            crate::validation::uuid(id)?;
            s.mcps.grant(s, id, bearer).await?;
        }
        Endpoint::Management => {
            return Err(Error::unauthorized("Use the beacon MCP endpoint."));
        }
    }
    Ok(None)
}

fn bad_request(id: Value, message: &str) -> Response {
    let frame = Frame::error(id, INVALID_REQUEST, message, None);
    (StatusCode::BAD_REQUEST, Json(frame)).into_response()
}

fn valid_request(body: &Value, method: &str, id: &Value) -> bool {
    body["jsonrpc"] == "2.0"
        && !method.is_empty()
        && (id.is_null() || id.is_string() || id.is_number())
}

/// Modern requests repeat the method and version in headers; both must agree.
fn headers_match(input: &Input, method: &str, version: &str) -> bool {
    let header_method = input
        .headers
        .get("mcp-method")
        .and_then(|v| v.to_str().ok());
    let body_version = input.body["params"]["_meta"][PROTOCOL_META].as_str();
    !header_method.is_some_and(|m| m != method) && !body_version.is_some_and(|v| v != version)
}

pub async fn handle(State(app): State<App>, request: Request) -> Result<Response> {
    let started = std::time::Instant::now();
    let s = &app.service;
    let bearer = bearer(&request);
    let endpoint = Endpoint::of(request.uri().path());
    let scopes = request
        .extensions()
        .get::<crate::auth::InstallationIdentity>()
        .and_then(|identity| identity.mcp_scopes.clone());
    let relayed_management = request.uri().path() == "/api/mcp";
    if relayed_management {
        if scopes.is_none() {
            return Err(Error::unauthorized(
                "A verified beacon MCP grant is required.",
            ));
        }
    } else if let Some(challenge) = authenticate(s, &endpoint, &bearer).await? {
        return Ok(challenge);
    }
    if request.method() != "POST" {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let input = Input::read(request).await?;
    let body = &input.body;
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let method = text(body, "method");
    if !valid_request(body, method, &id) {
        return Ok(bad_request(id, "Invalid JSON-RPC request."));
    }
    let version = input
        .headers
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok())
        .or_else(|| body["params"]["_meta"][PROTOCOL_META].as_str())
        .unwrap_or(DEFAULT_VERSION);
    if !VERSIONS.contains(&version) {
        let data = json!({
            "requested": version,
            "supported": VERSIONS
        });
        let frame = Frame::error(
            id,
            UNSUPPORTED_VERSION,
            "Unsupported protocol version.",
            Some(data),
        );
        return Ok(Json(frame).into_response());
    }
    let modern = version == MODERN;
    if modern && !headers_match(&input, method, version) {
        return Ok(bad_request(
            id,
            "MCP request headers do not match the body.",
        ));
    }
    if id.is_null() {
        return Ok(StatusCode::ACCEPTED.into_response());
    }
    let dispatched = dispatch(
        s,
        &endpoint,
        &bearer,
        scopes.as_deref(),
        method,
        &body["params"],
        modern,
    )
    .await;
    if matches!(method, "initialize" | "tools/list") {
        let (endpoint, connection_id) = match &endpoint {
            Endpoint::Management => ("management", "unknown"),
            Endpoint::Workspace => ("workspace", "unknown"),
            Endpoint::Gateway(id) => ("gateway", crate::performance::identity(id)),
        };
        tracing::info!(target: "cairn_performance", operation = "mcp_startup", event = "completed",
            endpoint, connection_id, method, elapsed_ms = started.elapsed().as_millis() as u64,
            success = dispatched.is_ok());
    }
    let frame = match dispatched {
        Ok(result) => Frame::strict(Message::Result {
            id,
            result: finish_result(result, method, modern),
        }),
        Err(error) => {
            let code = if error.is_not_found() {
                crate::rpc::jsonrpc::METHOD_NOT_FOUND
            } else {
                INTERNAL_ERROR
            };
            Frame::error(id, code, &error.message, None)
        }
    };
    let mut response = Json(frame).into_response();
    response.headers_mut().insert(
        "mcp-protocol-version",
        HeaderValue::from_str(version).unwrap(),
    );
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    Ok(response)
}

/// Adapts a result to the negotiated protocol's result metadata.
fn finish_result(mut result: Value, method: &str, modern: bool) -> Value {
    if modern && CACHEABLE.contains(&method) {
        result["ttlMs"] = 0.into();
        result["cacheScope"] = "private".into();
    }
    if modern && result.get("resultType").is_none() {
        result["resultType"] = "complete".into();
    }
    if !modern && let Some(object) = result.as_object_mut() {
        object.remove("resultType");
    }
    result
}

async fn dispatch(
    s: &Arc<Service>,
    endpoint: &Endpoint,
    bearer: &str,
    scopes: Option<&[String]>,
    method: &str,
    params: &Value,
    modern: bool,
) -> Result<Value> {
    let gateway = matches!(endpoint, Endpoint::Gateway(_));
    let name = if gateway {
        "cairn-mcp-gateway"
    } else {
        "cairn-installation"
    };
    let server_info = json!({
        "name": name,
        "version": env!("CARGO_PKG_VERSION")
    });
    let capabilities = if gateway {
        json!({
            "tools": {},
            "resources": {},
            "prompts": {}
        })
    } else {
        json!({ "tools": {} })
    };
    match method {
        "server/discover" => Ok(json!({
            "supportedVersions": VERSIONS,
            "capabilities": capabilities,
            "_meta": { "io.modelcontextprotocol/serverInfo": server_info },
        })),
        "initialize" if !modern => {
            let offered = text(params, "protocolVersion");
            let chosen = if VERSIONS[1..].contains(&offered) {
                offered
            } else {
                LEGACY
            };
            Ok(json!({
                "protocolVersion": chosen,
                "capabilities": capabilities,
                "serverInfo": server_info,
            }))
        }
        "ping" => Ok(json!({})),
        _ => match endpoint {
            Endpoint::Workspace => crate::project_workspaces::rpc(s, bearer, method, params).await,
            Endpoint::Gateway(id) => proxy(s, id, bearer, method, params.clone()).await,
            Endpoint::Management => management(s, scopes, method, params).await,
        },
    }
}

async fn management(
    s: &Arc<Service>,
    scopes: Option<&[String]>,
    method: &str,
    params: &Value,
) -> Result<Value> {
    if let Some(listing) = empty_listing(method) {
        return Ok(listing);
    }
    match method {
        "tools/list" => Ok(json!({ "tools": catalog() })),
        "tools/call" => {
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            call(s, scopes, text(params, "name"), args).await
        }
        _ => Err(Error::not_found("Method not found")),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolAnnotations {
    read_only_hint: bool,
    destructive_hint: bool,
    open_world_hint: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolDescriptor {
    name: &'static str,
    description: &'static str,
    title: String,
    input_schema: &'static Value,
    output_schema: Value,
    #[serde(rename = "_meta")]
    meta: Value,
    annotations: ToolAnnotations,
}

fn catalog() -> Vec<ToolDescriptor> {
    CATALOG
        .iter()
        .map(|tool| {
            let name = tool.name.as_str();
            let scope = tool.scope.as_str();
            let destructive = tool.destructive
                || scope == "run"
                || name.starts_with("update_")
                || name == "save_skill";
            ToolDescriptor {
                name,
                description: &tool.description,
                title: name.replace('_', " "),
                input_schema: &tool.input_schema,
                output_schema: json!({
                    "type": "object",
                    "properties": { "result": {} },
                    "required": ["result"],
                }),
                meta: json!({ "securitySchemes": [{ "type": "oauth2", "scopes": [scope] }] }),
                annotations: ToolAnnotations {
                    read_only_hint: scope == "read",
                    destructive_hint: destructive,
                    open_world_hint: scope != "read",
                },
            }
        })
        .collect()
}

async fn call(
    s: &Arc<Service>,
    scopes: Option<&[String]>,
    name: &str,
    args: Value,
) -> Result<Value> {
    let tool = CATALOG
        .iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| Error::not_found("Unknown tool"))?;
    let scope = tool.scope.as_str();
    let operation = async {
        if let Some(scopes) = scopes {
            if !scopes.iter().any(|allowed| allowed == scope) {
                return Err(Error::forbidden(format!("The {scope} scope is required.")));
            }
        } else {
            return Err(Error::unauthorized(
                "A verified beacon MCP grant is required.",
            ));
        }
        let args = parse(&format!("mcp:{name}"), args)?;
        invoke(s, name, args).await
    }
    .await;
    let result = match operation {
        Ok(result) => ToolResult::success(result.to_string(), json!({ "result": result })),
        Err(error) => {
            let mut result = ToolResult::failure(&error.message);
            if [401, 403].contains(&error.status) {
                let challenge = format!(
                    "Bearer error=\"insufficient_scope\", error_description=\"The {scope} scope is required\", scope=\"{scope}\", resource_metadata=\"{}\"",
                    resource_metadata(s).await
                );
                result.meta = Some(json!({ "mcp/www_authenticate": challenge }));
            }
            result
        }
    };
    Ok(serde_json::to_value(result)?)
}

fn run_record_preview(record: Value, fields: &[&str]) -> Result<(Value, usize)> {
    let bytes = serde_json::to_vec(&record)?.len();
    if bytes <= RUN_PAGE_BYTES {
        return Ok((record, bytes));
    }
    let mut preview = json!({
        "truncated": true,
        "totalBytes": bytes
    });
    let mut bytes = serde_json::to_vec(&preview)?.len();
    for field in fields {
        if let Some(value) = record.get(*field) {
            let size = serde_json::to_vec(value)?.len() + field.len() + 4;
            if bytes + size <= RUN_PAGE_BYTES {
                preview[*field] = value.clone();
                bytes += size;
            }
        }
    }
    Ok((preview, bytes))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunHistory {
    run: Value,
    events: Vec<Value>,
    next_after: i64,
    has_more: bool,
}

fn run_history(db: &Db<'_>, id: &str, after: i64) -> Result<Value> {
    let run = required(db.run(id)?, "Run not found")?;
    let (run, _) = run_record_preview(
        run,
        &[
            "id",
            "taskId",
            "projectId",
            "status",
            "trigger",
            "createdAt",
            "startedAt",
            "finishedAt",
        ],
    )?;
    let mut events = Vec::new();
    let mut bytes = 0;
    let mut next = after;
    for event in db.event_batch(id, after, 100, RUN_PAGE_BYTES)? {
        let event_id = event.id;
        let (event, size) = run_record_preview(
            serde_json::to_value(event)?,
            &["id", "runId", "createdAt", "type", "text"],
        )?;
        if !events.is_empty() && bytes + size > RUN_PAGE_BYTES {
            break;
        }
        bytes += size;
        events.push(event);
        next = event_id;
    }
    let more: bool = db.0.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND id>?)",
        rusqlite::params![id, next],
        |row| row.get(0),
    )?;
    let history = RunHistory {
        run,
        events,
        next_after: next,
        has_more: more,
    };
    Ok(serde_json::to_value(history)?)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunContentChunk<'a> {
    run_id: &'a str,
    event_id: &'a Value,
    encoding: &'static str,
    format: &'static str,
    offset: usize,
    total_bytes: usize,
    sha256: String,
    data: &'a str,
    next_offset: Option<usize>,
}

fn read_run_content(db: &Db<'_>, args: &Value) -> Result<Value> {
    let id = text(args, "runId");
    let data = if let Some(event_id) = args["eventId"].as_i64() {
        db.require_run(id)?;
        let event = db
            .event_batch(id, event_id - 1, 1, RUN_PAGE_BYTES)?
            .into_iter()
            .find(|event| event.id == event_id);
        // Keep the stored payload as RawValue: reading every chunk must not rebuild
        // a potentially huge nested JSON object just to serialize it again.
        serde_json::to_string(&required(event, "Event not found in this run")?)?
    } else {
        required(db.run(id)?, "Run not found")?.to_string()
    };
    let digest = crate::auth::hex_digest(&data);
    let offset = args["offset"].as_u64().unwrap() as usize;
    if (offset > 0 || args["sha256"].is_string()) && text(args, "sha256") != digest {
        return Err(Error::conflict(
            "Content changed or checksum missing. Restart at offset 0 and pass the returned sha256 with each subsequent chunk.",
        ));
    }
    if offset > data.len() || !data.is_char_boundary(offset) {
        return Err(Error::bad(
            "Offset must be a UTF-8 byte boundary within the content. Use nextOffset from the previous response.",
        ));
    }
    let end = data.floor_char_boundary((offset + RUN_PAGE_BYTES).min(data.len()));
    let chunk = RunContentChunk {
        run_id: id,
        event_id: &args["eventId"],
        encoding: "utf-8",
        format: "json",
        offset,
        total_bytes: data.len(),
        sha256: digest,
        data: &data[offset..end],
        next_offset: (end < data.len()).then_some(end),
    };
    Ok(serde_json::to_value(chunk)?)
}

async fn invoke(s: &Arc<Service>, name: &str, args: Value) -> Result<Value> {
    match name {
        "list_agents" => Ok(s.store.list("agents").await?.into()),
        "list_projects" => Ok(s.store.list("projects").await?.into()),
        "list_tasks" => Ok(s.store.list("tasks").await?.into()),
        "save_agent" => s.agent(args, None).await,
        "save_project" => s.project(args, None).await,
        "create_task" => s.task(args, None).await,
        "update_agent" => {
            let id = text(&args, "id");
            let mut agent = s.get("agents", id).await?;
            crate::store::merge(&mut agent, &args["agent"]);
            s.agent(agent, Some(id)).await
        }
        "update_task" => s.task(args["task"].clone(), Some(text(&args, "id"))).await,
        "run_task" => s.enqueue(text(&args, "taskId"), "mcp", None).await,
        "cancel_run" => {
            s.worker.cancel(s, text(&args, "runId")).await?;
            Ok(json!({ "cancelled": true }))
        }
        "list_runs" => {
            s.store
                .read(move |db| {
                    let runs = db.runs(
                        args["status"].as_str(),
                        None,
                        args["limit"].as_i64().unwrap(),
                        args["offset"].as_i64().unwrap(),
                        false,
                    )?;
                    Ok(runs.into())
                })
                .await
        }
        "get_run" => {
            let id = text(&args, "runId").to_owned();
            let after = args["after"].as_i64().unwrap();
            s.store.read(move |db| run_history(db, &id, after)).await
        }
        "read_run_content" => s.store.read(move |db| read_run_content(db, &args)).await,
        "list_skills" | "save_skill" => skills_tool(s, name, &args).await,
        "list_mcps" | "create_mcp" | "update_mcp" | "test_mcp" | "disconnect_mcp"
        | "delete_mcp" => connection_tool(s, name, args).await,
        _ => Err(Error::not_found("Unknown tool")),
    }
}

async fn skills_tool(s: &Service, name: &str, args: &Value) -> Result<Value> {
    let project_id = args["projectId"].as_str();
    let project = match project_id {
        Some(id) => Some(std::path::PathBuf::from(text(
            &s.get("projects", id).await?,
            "path",
        ))),
        None => None,
    };
    let project = project.as_deref();
    if name == "list_skills" {
        let scope = project_id.unwrap_or("global");
        return Ok(s.skills.list(scope, project).await?.into());
    }
    s.skills
        .save(text(args, "name"), text(args, "content"), project)
        .await
}

async fn connection_tool(s: &Service, name: &str, args: Value) -> Result<Value> {
    let management_url = crate::relay::beacon_address(&s.config.data_dir)
        .await?
        .map(|(origin, installation)| format!("{origin}/installations/{installation}/mcps"));
    let with_management_url = |mut result: Value| {
        if let Some(url) = &management_url {
            result["managementUrl"] = url.clone().into();
        }
        result
    };
    match name {
        "list_mcps" => return Ok(s.mcps.list(s).await?.into()),
        "create_mcp" => return Ok(with_management_url(s.mcps.save(s, args, None).await?)),
        _ => {}
    }
    let id = text(&args, "id");
    s.mcps.assert_management(s, id).await?;
    match name {
        "update_mcp" => {
            let saved = s.mcps.save(s, args["connection"].clone(), Some(id)).await?;
            Ok(with_management_url(saved))
        }
        "test_mcp" => s.mcps.test(s, id).await,
        "delete_mcp" => {
            s.mcps.disconnect(s, id, true).await?;
            Ok(json!({ "deleted": true }))
        }
        _ => {
            s.mcps.disconnect(s, id, false).await?;
            Ok(json!({ "disconnected": true }))
        }
    }
}

/// Forwards an allowed request to the granted connection.
async fn proxy(
    s: &Arc<Service>,
    id: &str,
    bearer: &str,
    method: &str,
    params: Value,
) -> Result<Value> {
    let _guard = s.mcps.lock(id).await;
    let (server, scope) = s.mcps.grant(s, id, bearer).await?;
    if method == "tools/call" && !scope.allows(text(&params, "name")) {
        return Err(Error::forbidden("This tool is unavailable to this agent."));
    }
    let result = async {
        let mut client = crate::mcp_client::Client::connect_server(s, &server).await?;
        let unsupported = match method {
            "resources/list" | "resources/templates/list" => {
                client.capabilities.get("resources").is_none()
            }
            "prompts/list" => client.capabilities.get("prompts").is_none(),
            _ => false,
        };
        let result = match method {
            "tools/list" => client.discover().await.map(|tools| {
                let tools = tools
                    .into_iter()
                    .filter(|tool| scope.allows(text(tool, "name")))
                    .collect::<Vec<_>>();
                json!({ "tools": tools })
            }),
            _ if unsupported => Ok(empty_listing(method).unwrap_or_default()),
            "tools/call"
            | "resources/list"
            | "resources/templates/list"
            | "resources/read"
            | "prompts/list"
            | "prompts/get" => client.request(method, params).await,
            _ => Err(Error::not_found("Method not found")),
        };
        client.close().await;
        result
    }
    .await;
    if let Err(error) = &result {
        s.mcps.failure(s, server, error).await?;
    }
    result
}

/// OAuth sign-ins are bound to the identity validated at the API auth seam.
fn oauth_binding(input: &Input) -> Result<&str> {
    let identity = input
        .identity
        .as_ref()
        .ok_or_else(|| Error::unauthorized("Please sign in."))?;
    Ok(identity.oauth_binding())
}

pub async fn routes(s: &Arc<Service>, input: &Input) -> Result<Value> {
    let segments = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    match (input.method.as_str(), segments.as_slice()) {
        ("GET", ["mcps"]) => Ok(s.mcps.list(s).await?.into()),
        ("POST", ["mcps", "oauth", "callback"]) => {
            let parameters = serde_json::from_value(input.body.clone())?;
            if s.mcps.capture_native_callback(s, &parameters).await? {
                return Ok(json!({ "result": "native" }));
            }
            let result = s
                .mcps
                .callback(s, &parameters, oauth_binding(input)?)
                .await?;
            Ok(json!({ "result": result }))
        }
        ("POST", ["mcps"]) => s.mcps.save(s, input.body.clone(), None).await,
        ("PUT", ["mcps", id]) => {
            crate::validation::uuid(id)?;
            s.mcps.get(s, id).await?;
            s.mcps.save(s, input.body.clone(), Some(id)).await
        }
        ("DELETE", ["mcps", id]) => {
            crate::validation::uuid(id)?;
            s.mcps.disconnect(s, id, true).await?;
            Ok(json!({ "ok": true }))
        }
        ("POST", ["mcps", id, "test"]) => {
            crate::validation::uuid(id)?;
            s.mcps.test(s, id).await
        }
        ("POST", ["mcps", id, "connect"]) => {
            crate::validation::uuid(id)?;
            let binding = oauth_binding(input)?;
            if input.body["native"] == true {
                s.mcps.connect_native(s, id, binding).await
            } else {
                s.mcps.connect(s, id, binding).await
            }
        }
        ("POST", ["mcps", id, "callback"]) => {
            crate::validation::uuid(id)?;
            let binding = oauth_binding(input)?;
            s.mcps.finish_native_callback(s, id, binding).await
        }
        ("POST", ["mcps", id, "disconnect"]) => {
            crate::validation::uuid(id)?;
            s.mcps.disconnect(s, id, false).await?;
            Ok(json!({ "ok": true }))
        }
        _ => Err(Error::not_found("Not found")),
    }
}
