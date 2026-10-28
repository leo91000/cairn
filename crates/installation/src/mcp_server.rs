mod chat_files;
mod chat_tools;
pub mod code_mode;
mod node_tools;

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

/// An entry of the management operation catalog (`schemas/mcp-operations.json`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogOperation {
    name: String,
    description: String,
    scope: String,
    #[serde(default)]
    destructive: bool,
    input_schema: Value,
}

static CATALOG: LazyLock<Vec<CatalogOperation>> =
    LazyLock::new(|| serde_json::from_str(include_str!("../schemas/mcp-operations.json")).unwrap());

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

/// The MCP endpoints this server exposes.
enum Endpoint {
    /// `/mcp-installation`: opt-in installation tools, authorized by a run grant.
    Installation,
    /// `/mcp`: the owner's management tools, authorized by personal tokens.
    Management,
    /// `/mcp-workspace`: run-scoped workspace tools.
    Workspace,
    /// `/mcp-gateway/{id}`: a run-scoped proxy to a configured connection.
    Gateway(String),
}

impl Endpoint {
    /// The endpoint at exactly `path`: no other path is an MCP endpoint.
    fn of(path: &str) -> Option<Self> {
        match path {
            "/api/mcp" => Some(Self::Management),
            "/mcp-workspace" => Some(Self::Workspace),
            "/mcp-installation" => Some(Self::Installation),
            _ => path
                .strip_prefix("/mcp-gateway/")
                .filter(|id| !id.is_empty() && !id.contains('/'))
                .map(|id| Self::Gateway(id.to_owned())),
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
        Endpoint::Installation => {
            crate::installation_mcp::authorize(s, bearer).await?;
        }
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
    respond(&app.service, request).await
}

/// Run-scoped MCP received on the channel of `run`: a VM reaches only the
/// workspace, installation and gateway endpoints, with a token granted to that same run.
pub(crate) async fn run_scoped(s: &Arc<Service>, run: &str, request: Request) -> Result<Response> {
    if request.uri().path() == "/plugin-command" {
        let granted = crate::mcps::granted_run(s, &bearer(&request)).await?;
        if granted.as_deref() != Some(run) {
            return Err(Error::unauthorized(
                "Plugin run access expired or was revoked.",
            ));
        }
        return crate::plugins::command_response(s, request).await;
    }
    let endpoint = Endpoint::of(request.uri().path());
    if !matches!(
        endpoint,
        Some(Endpoint::Workspace | Endpoint::Installation | Endpoint::Gateway(_))
    ) {
        return Err(Error::not_found("Not found."));
    }
    let granted = crate::mcps::granted_run(s, &bearer(&request)).await?;
    if granted.as_deref() != Some(run) {
        return Err(Error::unauthorized(
            "MCP run access expired or was revoked.",
        ));
    }
    respond(s, request).await
}

async fn respond(s: &Arc<Service>, request: Request) -> Result<Response> {
    let started = std::time::Instant::now();
    let bearer = bearer(&request);
    let endpoint =
        Endpoint::of(request.uri().path()).ok_or_else(|| Error::not_found("Not found."))?;
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
    let migration_owner = input
        .identity
        .as_ref()
        .filter(|identity| {
            relayed_management && identity.role == crate::auth::InstallationRole::Owner
        })
        .map(|identity| identity.account_id.clone());
    let dispatched = crate::backend_migration::OWNER
        .scope(
            migration_owner,
            dispatch(
                s,
                &endpoint,
                &bearer,
                input.identity.as_ref(),
                method,
                &body["params"],
                modern,
            ),
        )
        .await;
    if matches!(method, "initialize" | "tools/list") {
        let (endpoint, connection_id) = match &endpoint {
            Endpoint::Management => ("management", "unknown"),
            Endpoint::Workspace => ("workspace", "unknown"),
            Endpoint::Installation => ("installation", "unknown"),
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
    identity: Option<&crate::auth::InstallationIdentity>,
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
            Endpoint::Management => management(s, identity, method, params).await,
            Endpoint::Installation => crate::installation_mcp::rpc(s, bearer, method, params).await,
        },
    }
}

async fn management(
    s: &Arc<Service>,
    identity: Option<&crate::auth::InstallationIdentity>,
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
            let owner = identity
                .filter(|identity| identity.role == crate::auth::InstallationRole::Owner)
                .map(|identity| identity.account_id.clone());
            // Keep verified public callers separate from run-scoped delegation.
            // Box the dispatcher so nested task-local scopes keep a bounded stack.
            crate::native::policy::CALLER
                .scope(
                    owner,
                    Box::pin(call(s, identity, text(params, "name"), args)),
                )
                .await
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
struct OperationDescriptor {
    name: &'static str,
    description: &'static str,
    title: String,
    input_schema: &'static Value,
    #[serde(rename = "_meta")]
    meta: Value,
    annotations: ToolAnnotations,
}

pub(crate) fn operations() -> Vec<Value> {
    let mut operations: Vec<Value> = CATALOG
        .iter()
        .map(|tool| {
            let name = tool.name.as_str();
            let scope = tool.scope.as_str();
            let destructive = tool.destructive
                || scope == "run"
                || name.starts_with("update_")
                || name == "save_skill";
            serde_json::to_value(OperationDescriptor {
                name,
                description: &tool.description,
                title: name.replace('_', " "),
                input_schema: &tool.input_schema,
                meta: json!({ "securitySchemes": [{ "type": "oauth2", "scopes": [scope] }] }),
                annotations: ToolAnnotations {
                    read_only_hint: scope == "read",
                    destructive_hint: destructive,
                    open_world_hint: scope != "read",
                },
            })
            .expect("serializable tool descriptor")
        })
        .collect();
    operations.extend(
        crate::capabilities::AGENT_OPERATIONS
            .iter()
            .map(crate::capabilities::AgentOperation::descriptor),
    );
    operations.extend(
        crate::capabilities::MCP_OPERATIONS
            .iter()
            .map(crate::capabilities::McpOperation::descriptor),
    );
    operations.push(crate::capabilities::RESUME_RUN.descriptor());
    operations.push(crate::capabilities::MANAGE_PLUGIN.descriptor());
    operations.extend(
        crate::capabilities::RUN_OPERATIONS
            .iter()
            .map(crate::capabilities::RunOperation::descriptor),
    );
    operations.extend(
        crate::capabilities::PLUGIN_PACKAGE_OPERATIONS
            .iter()
            .map(crate::capabilities::plugin_packages::PluginPackageOperation::descriptor),
    );
    operations.extend(
        crate::capabilities::PROJECT_OPERATIONS
            .iter()
            .map(crate::capabilities::ProjectOperation::descriptor),
    );
    operations.extend(
        crate::capabilities::MISSION_OPERATIONS
            .iter()
            .map(crate::capabilities::MissionOperation::descriptor),
    );
    operations
}

pub(crate) fn catalog() -> Vec<Value> {
    serde_json::from_str(include_str!("../schemas/mcp-tools.json")).unwrap()
}

async fn call(
    s: &Arc<Service>,
    identity: Option<&crate::auth::InstallationIdentity>,
    name: &str,
    args: Value,
) -> Result<Value> {
    let scopes = identity
        .and_then(|identity| identity.mcp_scopes.as_ref())
        .ok_or_else(|| Error::unauthorized("A verified beacon MCP grant is required."))?;
    code_mode::call(
        s,
        code_mode::Authorization::Public(
            scopes.to_vec(),
            identity.and_then(|identity| identity.mcp_credential_digest.clone()),
        ),
        name,
        args,
    )
    .await
}

pub(crate) async fn operation(
    s: &Arc<Service>,
    scopes: &[String],
    credential: Option<&str>,
    name: &str,
    args: Value,
) -> Result<Value> {
    if let Some(operation) = crate::capabilities::plugin_package_operation(name) {
        return operation
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Public(
                    scopes.to_vec(),
                    credential.map(str::to_owned),
                ),
            )
            .await;
    }
    if name == "manage_plugin" {
        return crate::capabilities::MANAGE_PLUGIN
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Public(
                    scopes.to_vec(),
                    credential.map(str::to_owned),
                ),
            )
            .await;
    }
    if let Some(op) = crate::capabilities::mission_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Public(
                    scopes.to_vec(),
                    credential.map(str::to_owned),
                ),
            )
            .await;
    }
    if let Some(op) = crate::capabilities::mcp_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Public(
                    scopes.to_vec(),
                    credential.map(str::to_owned),
                ),
                None,
            )
            .await;
    }
    if let Some(op) = crate::capabilities::agent_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Public(
                    scopes.to_vec(),
                    credential.map(str::to_owned),
                ),
            )
            .await;
    }
    if let Some(op) = crate::capabilities::project_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Public(
                    scopes.to_vec(),
                    credential.map(str::to_owned),
                ),
            )
            .await;
    }
    if let Some(op) = crate::capabilities::run_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Public(
                    scopes.to_vec(),
                    credential.map(str::to_owned),
                ),
            )
            .await;
    }
    if name == cairn_contracts::RESUME_RUN_ID {
        let input = crate::capabilities::RESUME_RUN.decode(args)?;
        let context = crate::capabilities::ResumeContext::Public(
            scopes.to_vec(),
            credential.map(str::to_owned),
        );
        let output = crate::capabilities::RESUME_RUN
            .invoke(s, input, context)
            .await?;
        return Ok(serde_json::to_value(output)?);
    }
    let tool = CATALOG
        .iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| Error::not_found("Unknown operation"))?;
    let scope = tool.scope.as_str();
    if !scopes.iter().any(|allowed| allowed == scope) {
        return Err(Error::forbidden(format!("The {scope} scope is required.")));
    }
    let args = parse(&format!("mcp:{name}"), args)?;
    invoke(s, name, args, None).await
}

/// Count wire bytes without allocating a second buffer for large raw engine
/// payloads. Sizing a page must not decode or copy those payloads again.
fn encoded_size(value: &impl Serialize) -> Result<usize> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
}

fn event_preview(event: cairn_contracts::RunEvent) -> Result<cairn_contracts::RunEventPage> {
    use cairn_contracts::{EventPreview, RunEventPage};
    let total_bytes = encoded_size(&event)? as u64;
    let complete = RunEventPage::Complete { event };
    if encoded_size(&complete)? <= RUN_PAGE_BYTES - 2 {
        return Ok(complete);
    }
    let RunEventPage::Complete { event } = complete else {
        unreachable!()
    };
    let mut preview = RunEventPage::Truncated {
        total_bytes,
        preview: EventPreview {
            id: event.id,
            run_id: event.run_id,
            created_at: event.created_at,
            kind: None,
            text: None,
        },
    };
    if let RunEventPage::Truncated {
        preview: fields, ..
    } = &mut preview
    {
        // Unknown adapter markers can themselves be oversized. The full event
        // remains addressable by ID; omit fields that do not fit the preview.
        let marker = event.kind.as_str().to_owned();
        let remaining = RUN_PAGE_BYTES - 1024;
        let marker_bytes = encoded_size(&marker)?;
        let text_bytes = encoded_size(&event.text)?;
        if marker_bytes <= remaining {
            fields.kind = Some(marker);
        }
        if marker_bytes.saturating_add(text_bytes) <= remaining {
            fields.text = Some(event.text);
        }
    }
    Ok(preview)
}

/// Keep the newest contiguous suffix of the UI window. A preview retains its
/// event ID for read_run_content; trimming sets hasOlder and never skips a cursor.
pub(crate) fn bounded_history_window(
    page: cairn_contracts::HistoryPage,
) -> Result<cairn_contracts::RunHistoryWindowOutput> {
    let mut events = Vec::new();
    let mut bytes = 2;
    let mut has_older = page.has_older;
    let mut oldest = page.oldest;
    for event in page.events.into_iter().rev() {
        let id = event.id;
        let preview = event_preview(event)?;
        let size = encoded_size(&preview)? + usize::from(!events.is_empty());
        if !events.is_empty() && bytes + size > RUN_PAGE_BYTES {
            has_older = true;
            break;
        }
        bytes += size;
        oldest = id;
        events.push(preview);
    }
    events.reverse();
    Ok(cairn_contracts::RunHistoryWindowOutput {
        events,
        history: page.history,
        oldest,
        has_older,
    })
}

pub(crate) fn run_history(db: &Db<'_>, id: &str, after: i64) -> Result<Value> {
    let run = required(db.run_view(id)?, "Run not found")?;
    let total_bytes = encoded_size(&run)? as u64;
    let run = if total_bytes <= RUN_PAGE_BYTES as u64 {
        cairn_contracts::RunMetadataPage::Complete { run: Box::new(run) }
    } else {
        let state = run.state;
        cairn_contracts::RunMetadataPage::Truncated {
            total_bytes,
            preview: cairn_contracts::RunPreview {
                id: state.id,
                task_id: state.task_id,
                project_id: state.project_id,
                status: state.status,
                trigger: state.trigger,
                created_at: state.created_at,
                started_at: state.started_at,
                finished_at: state.finished_at,
            },
        }
    };
    let mut events = Vec::new();
    let mut bytes = 2;
    let mut next = after;
    for event in db.event_batch(id, after, 100, RUN_PAGE_BYTES)? {
        let event_id = event.id;
        let page = event_preview(event)?;
        let size = encoded_size(&page)? + usize::from(!events.is_empty());
        if !events.is_empty() && bytes + size > RUN_PAGE_BYTES {
            break;
        }
        bytes += size;
        events.push(page);
        next = event_id;
    }
    let (first, newest) = crate::live::event_bounds(db, id)?;
    let history = cairn_contracts::RunHistoryOutput {
        run,
        events,
        history: format!("v1:{id}:{first}"),
        next_after: next,
        has_more: newest > next,
    };
    Ok(serde_json::to_value(history)?)
}

pub(crate) fn read_run_content(
    db: &Db<'_>,
    args: &cairn_contracts::ReadRunContentInput,
) -> Result<Value> {
    let run_id = args.run_id.to_string();
    let id = run_id.as_str();
    let data = if let Some(event_id) = args.event_id {
        db.require_run(id)?;
        let event = db
            .event_batch(id, event_id - 1, 1, RUN_PAGE_BYTES)?
            .into_iter()
            .find(|event| event.id == event_id);
        // Keep the stored payload as RawValue: reading every chunk must not rebuild
        // a potentially huge nested JSON object just to serialize it again.
        serde_json::to_string(&required(event, "Event not found in this run")?)?
    } else {
        serde_json::to_string(&required(db.run_view(id)?, "Run not found")?)?
    };
    let digest = crate::auth::hex_digest(&data);
    let offset = usize::try_from(args.offset).map_err(|_| Error::bad("Invalid content offset."))?;
    if (offset > 0 || args.sha256.is_some()) && args.sha256.as_deref() != Some(&digest) {
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
    let chunk = cairn_contracts::RunContentOutput {
        run_id,
        event_id: args.event_id,
        encoding: "utf-8".into(),
        format: "json".into(),
        offset: offset as u64,
        total_bytes: data.len() as u64,
        sha256: digest,
        data: data[offset..end].to_owned(),
        next_offset: (end < data.len()).then_some(end as u64),
    };
    Ok(serde_json::to_value(chunk)?)
}

pub(crate) async fn invoke(
    s: &Arc<Service>,
    name: &str,
    args: Value,
    bearer: Option<&str>,
) -> Result<Value> {
    let authorize = crate::installation_mcp::management_authorization(bearer);
    match name {
        "get_chat_backend_migration" => {
            crate::backend_migration::preview(s, text(&args, "chatId"), authorize).await
        }
        "migrate_chat_backend" => s.migrate_chat_backend(args, authorize).await,
        "list_nodes"
        | "list_native_callers"
        | "create_node_enrollment"
        | "update_node"
        | "change_native_node_mode"
        | "update_node_storage"
        | "revoke_node" => node_tools::invoke(s, name, args, bearer).await,
        "list_chat_artifacts"
        | "read_chat_artifact"
        | "set_chat_artifact_visibility"
        | "list_chat_attachments"
        | "read_chat_attachment"
        | "upload_chat_attachment" => chat_files::invoke(s, name, args, bearer).await,
        "read_chat_content"
        | "get_chat_coding_agent_options"
        | "switch_chat_coding_agent"
        | "list_chat_questions"
        | "answer_chat_question"
        | "restore_chat"
        | "restart_chat_session"
        | "resume_chat"
        | "list_chats"
        | "get_chat"
        | "create_chat"
        | "send_chat_message"
        | "edit_chat_message"
        | "delete_chat_message"
        | "rename_chat"
        | "set_chat_paused"
        | "stop_chat"
        | "compact_chat"
        | "delete_chat" => chat_tools::invoke(s, name, args, bearer).await,
        "list_models" => {
            crate::provider::Provider::parse(text(&args, "provider"))?
                .adapter()
                .model_catalog(s)
                .await
        }
        "convert_run_to_chat" => {
            s.chat_from_run_authorized(text(&args, "runId"), authorize)
                .await
        }
        "list_skills" | "read_skill" | "save_skill" | "edit_skill_files" => {
            skills_tool(s, name, &args, bearer).await
        }

        _ => Err(Error::not_found("Unknown tool")),
    }
}

async fn skills_tool(s: &Service, name: &str, args: &Value, bearer: Option<&str>) -> Result<Value> {
    let project_id = args["projectId"].as_str();
    let project = match project_id {
        Some(id) => Some(std::path::PathBuf::from(
            s.get_project(id).await?.settings.path,
        )),
        None => None,
    };
    let project = project.as_deref();
    let scope = project_id.unwrap_or("global");
    let skill = text(args, "name");
    match name {
        "list_skills" => {
            let mut skills = s.skills.list(scope, project).await?;
            s.attach_skill_usage(&mut skills).await?;
            Ok(skills.into())
        }
        "read_skill" => {
            if let Some(path) = args["path"].as_str() {
                return s.skills.read(skill, path, project).await;
            }
            let mut bundle = s.skills.bundle(scope, skill, project).await?;
            crate::skills::sources::source(s, scope, skill, &mut bundle).await?;
            Ok(bundle)
        }
        "edit_skill_files" => {
            let (changes, uploads) = s.skills.changes(&args["changes"]).await?;
            let saved = s
                .skills
                .apply_authorized(
                    skill,
                    args["base"].as_str(),
                    &changes,
                    project,
                    Some((
                        s.store.clone(),
                        Box::new(crate::installation_mcp::management_authorization(bearer)),
                    )),
                )
                .await?;
            s.skills.discard(&uploads).await;
            let mut bundle = crate::skills::bundle_response(scope, skill, &saved);
            crate::skills::sources::source(s, scope, skill, &mut bundle).await?;
            Ok(bundle)
        }
        _ => {
            s.skills
                .save_authorized(
                    skill,
                    text(args, "content"),
                    project,
                    Some((
                        s.store.clone(),
                        Box::new(crate::installation_mcp::management_authorization(bearer)),
                    )),
                )
                .await
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
    let authorize = {
        let id = id.to_owned();
        let bearer = bearer.to_owned();
        move |db: &crate::store::Db<'_>| {
            crate::mcps::grant_in(db, &id, &bearer)?;
            Ok(())
        }
    };
    if method == "tools/call" && !scope.allows(text(&params, "name")) {
        return Err(Error::forbidden("This tool is unavailable to this agent."));
    }
    let result = async {
        let mut client =
            crate::mcp_client::Client::connect_server_authorized(s, &server, authorize.clone())
                .await?;
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
        // Recheck the same gateway grant before publishing a failure after network work.
        s.mcps
            .record_discovery_authorized(s, server, &Err(error.clone()), authorize)
            .await?;
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
        _ => {
            let (name, body) = match (input.method.as_str(), segments.as_slice()) {
                ("GET", ["mcps"]) => ("list_mcps", json!({})),
                ("POST", ["mcps"]) => ("create_mcp", input.body.clone()),
                ("PUT", ["mcps", id]) => {
                    let command: cairn_contracts::UpdateMcpInput =
                        serde_json::from_value(input.body.clone())
                            .map_err(|e| Error::bad(e.to_string()))?;
                    if command.id != *id {
                        return Err(Error::bad("Connection identity does not match the path."));
                    }
                    ("update_mcp", input.body.clone())
                }
                ("DELETE", ["mcps", id]) => ("delete_mcp", json!({"id":id})),
                ("POST", ["mcps", id, "test"]) => ("test_mcp", json!({"id":id})),
                ("POST", ["mcps", id, "disconnect"]) => ("disconnect_mcp", json!({"id":id})),
                ("POST", ["mcps", id, "connect"]) => {
                    let mut body = input.body.clone();
                    if !body.is_object() {
                        body = json!({});
                    }
                    body["id"] = (*id).into();
                    ("begin_mcp_connection", body)
                }
                ("POST", ["mcps", id, "callback"]) => ("mcp_connection_status", json!({"id":id})),
                ("POST", ["mcps", id, "cancel"]) => ("cancel_mcp_connection", json!({"id":id})),
                _ => return Err(Error::not_found("Not found")),
            };
            let context = crate::capabilities::ResumeContext::http(input)?;
            let binding = if input
                .identity
                .as_ref()
                .is_some_and(|identity| identity.mcp_scopes.is_none())
            {
                Some(oauth_binding(input)?.to_owned())
            } else {
                None
            };
            crate::capabilities::mcp_operation(name)
                .unwrap()
                .invoke(s, body, context, binding)
                .await
        }
    }
}
