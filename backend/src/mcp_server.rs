use crate::{
    error::{Error, Result},
    http::{App, Input},
    mcp_client::MODERN,
    service::Service,
    validation::{parse, text},
};
use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::sync::{Arc, LazyLock};
pub static CATALOG: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("../schemas/mcp-tools.json")).unwrap());
const VERSIONS: &[&str] = &[
    "2026-07-28",
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];
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
fn rpc_error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({
    "code":code,"message":message}
    );
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({
    "jsonrpc":"2.0","id":id,"error":error}
    )
}
pub async fn handle(State(app): State<App>, request: Request) -> Result<Response> {
    let s = &app.service;
    let bearer = bearer(&request);
    let workspace = request.uri().path() == "/mcp-workspace";
    let gateway = request
        .uri()
        .path()
        .strip_prefix("/mcp-gateway/")
        .map(str::to_owned);
    if workspace {
        crate::project_workspaces::authorize(s, &bearer).await?;
    } else if let Some(id) = &gateway {
        crate::validation::uuid(id)?;
        s.mcps.grant(s, id, &bearer).await?;
    } else if s.auth.verify(&bearer, None).await.is_err() {
        let mut response = (
            StatusCode::UNAUTHORIZED,
            Json(json!({
            "error":"unauthorized"}
            )),
        )
            .into_response();
        response.headers_mut().insert(
            "www-authenticate",
            HeaderValue::from_str(&format!(
                "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
                s.config.public_url
            ))
            .unwrap(),
        );
        return Ok(response);
    }
    if request.method() != "POST" {
        return Err(Error::new(405, "Method not allowed."));
    }
    let input = Input::read(request).await?;
    let body = &input.body;
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let method = text(body, "method");
    if body["jsonrpc"] != "2.0"
        || method.is_empty()
        || (!id.is_null() && !id.is_string() && !id.is_number())
    {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(rpc_error(id, -32600, "Invalid JSON-RPC request.", None)),
        )
            .into_response());
    }
    let version = input
        .headers
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok())
        .or_else(|| body["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"].as_str())
        .unwrap_or("2025-03-26");
    if !VERSIONS.contains(&version) {
        return Ok(Json(rpc_error(
            id,
            -32022,
            "Unsupported protocol version.",
            Some(json!({
            "requested":version,"supported":VERSIONS}
            )),
        ))
        .into_response());
    }
    let modern = version == MODERN;
    if modern {
        let header_method = input
            .headers
            .get("mcp-method")
            .and_then(|v| v.to_str().ok());
        let body_version =
            body["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"].as_str();
        if header_method.is_some_and(|m| m != method) || body_version.is_some_and(|v| v != version)
        {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(rpc_error(
                    id,
                    -32600,
                    "MCP request headers do not match the body.",
                    None,
                )),
            )
                .into_response());
        }
    }
    if id.is_null() {
        return Ok(StatusCode::ACCEPTED.into_response());
    }
    let server_info = json!({
    "name":if gateway.is_some(){
    "leo-mcp-gateway"}
    else{
    "leo-agent-manager"}
    ,"version":env!("CARGO_PKG_VERSION")}
    );
    let capabilities = if gateway.is_some() {
        json!({
        "tools":{
        }
        ,"resources":{
        }
        ,"prompts":{
        }
        }
        )
    } else {
        json!({
        "tools":{
        }
        }
        )
    };
    let result = match method {
        "server/discover" => Ok(json!({
        "supportedVersions":VERSIONS,"capabilities":capabilities,"_meta":{
        "io.modelcontextprotocol/serverInfo":server_info}
        }
        )),
        "initialize" if !modern => {
            let offered = text(&body["params"], "protocolVersion");
            let chosen = if VERSIONS[1..].contains(&offered) {
                offered
            } else {
                "2025-11-25"
            };
            Ok(json!({
            "protocolVersion":chosen,"capabilities":capabilities,"serverInfo":server_info}
            ))
        }
        "ping" => Ok(json!({})),
        _ => {
            if workspace {
                crate::project_workspaces::rpc(s, &bearer, method, &body["params"]).await
            } else if let Some(gateway) = &gateway {
                proxy(s, gateway, &bearer, method, body["params"].clone()).await
            } else {
                match method {
                    "tools/list" => Ok(json!({
                    "tools":catalog()}
                    )),
                    "tools/call" => {
                        call(
                            s,
                            &bearer,
                            text(&body["params"], "name"),
                            body["params"]
                                .get("arguments")
                                .cloned()
                                .unwrap_or_else(|| json!({})),
                        )
                        .await
                    }
                    "resources/list" => Ok(json!({
                    "resources":[]}
                    )),
                    "resources/templates/list" => Ok(json!({
                    "resourceTemplates":[]}
                    )),
                    "prompts/list" => Ok(json!({
                    "prompts":[]}
                    )),
                    _ => Err(Error::new(404, "Method not found")),
                }
            }
        }
    };
    let response = match result {
        Ok(mut result) => {
            if modern
                && [
                    "server/discover",
                    "tools/list",
                    "resources/list",
                    "resources/templates/list",
                    "prompts/list",
                ]
                .contains(&method)
            {
                result["ttlMs"] = 0.into();
                result["cacheScope"] = "private".into();
            }
            if modern && result.get("resultType").is_none() {
                result["resultType"] = "complete".into();
            }
            if !modern && let Some(object) = result.as_object_mut() {
                object.remove("resultType");
            }
            json!({
            "jsonrpc":"2.0","id":id,"result":result}
            )
        }
        Err(error) => rpc_error(
            id,
            if error.status == 404 { -32601 } else { -32603 },
            &error.message,
            None,
        ),
    };
    let mut response = Json(response).into_response();
    response.headers_mut().insert(
        "mcp-protocol-version",
        HeaderValue::from_str(version).unwrap(),
    );
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    Ok(response)
}
fn catalog() -> Vec<Value> {
    CATALOG
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| {
            let name = text(tool, "name");
            let scope = text(tool, "scope");
            json!({
            "name":name,"description":tool["description"],"title":name.replace('_'," "),"inputSchema":tool["inputSchema"],"outputSchema":{
            "type":"object","properties":{
            "result":{
            }
            }
            ,"required":["result"]}
            ,"_meta":{
            "securitySchemes":[{
            "type":"oauth2","scopes":[scope]}
            ]}
            ,"annotations":{
            "readOnlyHint":scope=="read","destructiveHint":tool["destructive"]==true||scope=="run"||name.starts_with("update_")||name=="save_skill","openWorldHint":scope!="read"}
            }
            )
        })
        .collect()
}
async fn call(s: &Arc<Service>, bearer: &str, name: &str, args: Value) -> Result<Value> {
    let tool = CATALOG
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .ok_or_else(|| Error::new(404, "Unknown tool"))?;
    let scope = text(tool, "scope");
    let operation = async {
        s.auth.verify(bearer, Some(scope)).await?;
        let args = parse(&format!("mcp:{name}"), args)?;
        invoke(s, name, args).await
    }
    .await;
    Ok(match operation {
        Ok(result) => {
            json!({
            "structuredContent":{
            "result":result}
            ,"content":[{
            "type":"text","text":result.to_string()}
            ]}
            )
        }
        Err(error) => {
            let mut result = json!({
            "isError":true,"content":[{
            "type":"text","text":error.message}
            ]}
            );
            if [401, 403].contains(&error.status) {
                result["_meta"] = json!({
                "mcp/www_authenticate":format!("Bearer error=\"insufficient_scope\", error_description=\"The {scope} scope is required\", scope=\"{scope}\", resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",s.config.public_url)}
                );
            }
            result
        }
    })
}
// Leave ample headroom below the gateway's 8 MiB ceiling: MCP serializes the
// result both as structured data and as escaped text for older clients.
const RUN_PAGE_BYTES: usize = 256 * 1024;

fn run_record_preview(record: Value, fields: &[&str]) -> Result<(Value, usize)> {
    let bytes = serde_json::to_vec(&record)?.len();
    if bytes <= RUN_PAGE_BYTES {
        return Ok((record, bytes));
    }
    let mut preview = json!({"truncated":true,"totalBytes":bytes});
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

fn run_history(db: &crate::store::Db<'_>, id: &str, after: i64) -> Result<Value> {
    let run = crate::error::required(db.run(id)?, "Run not found")?;
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
    Ok(json!({"run":run,"events":events,"nextAfter":next,"hasMore":more}))
}

fn read_run_content(db: &crate::store::Db<'_>, args: &Value) -> Result<Value> {
    let id = text(args, "runId");
    let data = if let Some(event_id) = args["eventId"].as_i64() {
        db.require_run(id)?;
        let event = db
            .event_batch(id, event_id - 1, 1, RUN_PAGE_BYTES)?
            .into_iter()
            .find(|event| event.id == event_id);
        // Keep the stored payload as RawValue: reading every chunk must not rebuild
        // a potentially huge nested JSON object just to serialize it again.
        serde_json::to_string(&crate::error::required(
            event,
            "Event not found in this run",
        )?)?
    } else {
        crate::error::required(db.run(id)?, "Run not found")?.to_string()
    };
    let digest = crate::auth::hex_digest(&data);
    let offset = args["offset"].as_u64().unwrap() as usize;
    if (offset > 0 || args["sha256"].is_string()) && text(args, "sha256") != digest {
        return Err(Error::new(
            409,
            "Content changed or checksum missing. Restart at offset 0 and pass the returned sha256 with each subsequent chunk.",
        ));
    }
    if offset > data.len() || !data.is_char_boundary(offset) {
        return Err(Error::bad(
            "Offset must be a UTF-8 byte boundary within the content. Use nextOffset from the previous response.",
        ));
    }
    let end = data.floor_char_boundary((offset + RUN_PAGE_BYTES).min(data.len()));
    Ok(json!({
        "runId":id,"eventId":args["eventId"],"encoding":"utf-8","format":"json",
        "offset":offset,"totalBytes":data.len(),"sha256":digest,
        "data":&data[offset..end],"nextOffset":if end < data.len() { Some(end) } else { None },
    }))
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
            Ok(json!({
            "cancelled":true}
            ))
        }
        "list_runs" => {
            s.store
                .read(move |db| {
                    Ok(db
                        .runs(
                            args["status"].as_str(),
                            None,
                            args["limit"].as_i64().unwrap(),
                            args["offset"].as_i64().unwrap(),
                            false,
                        )?
                        .into())
                })
                .await
        }
        "get_run" => {
            let id = text(&args, "runId").to_owned();
            s.store
                .read(move |db| run_history(db, &id, args["after"].as_i64().unwrap()))
                .await
        }
        "read_run_content" => s.store.read(move |db| read_run_content(db, &args)).await,
        "list_skills" | "save_skill" => {
            let project = if let Some(id) = args["projectId"].as_str() {
                Some(std::path::PathBuf::from(text(
                    &s.get("projects", id).await?,
                    "path",
                )))
            } else {
                None
            };
            if name == "list_skills" {
                Ok(s.skills
                    .list(
                        args["projectId"].as_str().unwrap_or("global"),
                        project.as_deref(),
                    )
                    .await?
                    .into())
            } else {
                s.skills
                    .save(
                        text(&args, "name"),
                        text(&args, "content"),
                        project.as_deref(),
                    )
                    .await
            }
        }
        "list_mcps" => Ok(s.mcps.list(s).await?.into()),
        "create_mcp" => {
            let mut result = s.mcps.save(s, args, None).await?;
            result["managementUrl"] = format!("{}/mcps", s.config.public_url).into();
            Ok(result)
        }
        "update_mcp" | "test_mcp" | "disconnect_mcp" | "delete_mcp" => {
            let id = text(&args, "id");
            s.mcps.assert_management(s, id).await?;
            match name {
                "update_mcp" => {
                    let mut result = s.mcps.save(s, args["connection"].clone(), Some(id)).await?;
                    result["managementUrl"] = format!("{}/mcps", s.config.public_url).into();
                    Ok(result)
                }
                "test_mcp" => s.mcps.test(s, id).await,
                _ => {
                    s.mcps.disconnect(s, id, name == "delete_mcp").await?;
                    Ok(if name == "delete_mcp" {
                        json!({
                        "deleted":true}
                        )
                    } else {
                        json!({
                        "disconnected":true}
                        )
                    })
                }
            }
        }
        _ => Err(Error::new(404, "Unknown tool")),
    }
}
async fn proxy(
    s: &Arc<Service>,
    id: &str,
    bearer: &str,
    method: &str,
    params: Value,
) -> Result<Value> {
    let _guard = s.mcps.lock(id).await;
    let (item, scope) = s.mcps.grant(s, id, bearer).await?;
    if method == "tools/call" && !crate::service::allowed(&scope["tools"], text(&params, "name")) {
        return Err(Error::new(403, "This tool is unavailable to this agent."));
    }
    let result = async {
        let mut client = crate::mcp_client::Client::connect(s, &item).await?;
        let result = match method {
            "tools/list" => client.discover().await.map(|tools| {
                json!({
                "tools":tools.into_iter().filter(|t|crate::service::allowed(&scope["tools"],text(t,"name"))).collect::<Vec<_>>()}
                )
            }),
            "resources/list" | "resources/templates/list" if client.capabilities.get("resources").is_none() => Ok(if method == "resources/list" {
                json!({
                "resources":[]}
                )
            } else {
                json!({
                "resourceTemplates":[]}
                )
            }),
            "prompts/list" if client.capabilities.get("prompts").is_none() => Ok(json!({
            "prompts":[]}
            )),
            "tools/call" | "resources/list" | "resources/templates/list" | "resources/read" | "prompts/list" | "prompts/get" => client.request(method, params).await,
            _ => Err(Error::new(404, "Method not found")),
        };
        client.close().await;
        result
    }
    .await;
    if let Err(error) = &result {
        s.mcps.failure(s, item, error).await?;
    }
    result
}
pub async fn routes(s: &Arc<Service>, input: &Input) -> Result<Value> {
    let segments = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    match (input.method.as_str(), segments.as_slice()) {
        ("GET", ["mcps"]) => Ok(s.mcps.list(s).await?.into()),
        ("POST", ["mcps"]) => s.mcps.save(s, input.body.clone(), None).await,
        ("PUT", ["mcps", id]) => {
            crate::validation::uuid(id)?;
            s.mcps.get(s, id).await?;
            s.mcps.save(s, input.body.clone(), Some(id)).await
        }
        ("DELETE", ["mcps", id]) => {
            crate::validation::uuid(id)?;
            s.mcps.disconnect(s, id, true).await?;
            Ok(json!({
            "ok":true}
            ))
        }
        ("POST", ["mcps", id, "test"]) => {
            crate::validation::uuid(id)?;
            s.mcps.test(s, id).await
        }
        ("POST", ["mcps", id, "connect"]) => {
            crate::validation::uuid(id)?;
            let session = s
                .auth
                .read(&crate::http::cookie(&input.headers))
                .await?
                .ok_or_else(|| Error::new(401, "Please sign in."))?;
            if input.body["native"] == true {
                s.mcps.connect_native(s, id, text(&session, "csrf")).await
            } else {
                s.mcps.connect(s, id, text(&session, "csrf")).await
            }
        }
        ("POST", ["mcps", id, "callback"]) => {
            crate::validation::uuid(id)?;
            let session = s
                .auth
                .read(&crate::http::cookie(&input.headers))
                .await?
                .ok_or_else(|| Error::new(401, "Please sign in."))?;
            s.mcps
                .finish_native_callback(s, id, text(&session, "csrf"))
                .await
        }
        ("POST", ["mcps", id, "disconnect"]) => {
            crate::validation::uuid(id)?;
            s.mcps.disconnect(s, id, false).await?;
            Ok(json!({
            "ok":true}
            ))
        }
        _ => Err(Error::new(404, "Not found")),
    }
}
