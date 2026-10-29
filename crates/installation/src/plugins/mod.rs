//! Installation-owned plugins. Every package (bundled or installed) runs in an isolated
//! Luau worker and reaches Cairn only through capabilities its manifest declares and
//! the owner approved.
pub mod agents;
pub mod capabilities;
pub mod cli;
pub mod conditions;
pub mod connections;
pub mod device_flow;
pub mod files;
pub mod forms;
pub mod hooks;
mod idempotency;
pub mod instances;
pub(crate) mod management;
mod mime;
pub mod oauth;
pub mod operations;
pub mod panels;
mod program_cache;
pub mod program_installation;
pub mod programs;
pub mod projects;
pub mod queues;
pub mod registry;
pub mod resources;
pub mod run_credentials;
pub mod runtime;
pub mod scope;
pub mod socket;
pub mod state;
pub mod work;

use crate::{
    error::{Error, Result},
    http::{App, Input},
    service::Service,
};
use axum::{
    Json,
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use cairn_plugin_api::Program;
use cairn_plugin_host::Host;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;

pub struct Plugins {
    pub host: Host,
    pub changed: Notify,
    /// Plugin HTTP never follows redirects, so credentials can't leave declared hosts.
    pub http: reqwest::Client,
    test_origin: Mutex<Option<String>>,
    socket_origin: Mutex<Option<String>>,
    worker: Mutex<Option<std::path::PathBuf>>,
    programs: Mutex<HashMap<(String, String), PathBuf>>,
    pub(crate) program_installation: tokio::sync::Mutex<()>,
    pub(crate) program_caches: program_cache::Caches,
    pub(crate) package_publication: tokio::sync::Mutex<()>,
    pub(crate) installer_host: Host,
    pub operations: operations::Operations,
    pub device_flows: device_flow::Flows,
    pub oauth_flows: oauth::Flows,
    pub files: files::Files,
    /// When each paced key (e.g. a bot token's Identify budget) may send next.
    paces: socket::pacing::Paces,
}

impl Default for Plugins {
    fn default() -> Self {
        Self {
            host: Host::default(),
            changed: Notify::new(),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                // Some APIs refuse requests without one; packages may set their own.
                .user_agent("Cairn")
                .build()
                .expect("plugin HTTP client"),
            test_origin: Mutex::new(None),
            socket_origin: Mutex::new(None),
            worker: Mutex::new(None),
            programs: Mutex::new(HashMap::new()),
            program_installation: tokio::sync::Mutex::new(()),
            program_caches: program_cache::Caches::default(),
            package_publication: tokio::sync::Mutex::new(()),
            installer_host: Host::with_limits(4, std::time::Duration::from_secs(120)),
            operations: operations::Operations::default(),
            device_flows: device_flow::Flows::default(),
            oauth_flows: oauth::Flows::default(),
            files: files::Files::default(),
            paces: socket::pacing::Paces::default(),
        }
    }
}

impl Plugins {
    /// Tests only: send plugin HTTP for declared hosts to a local server instead.
    #[doc(hidden)]
    pub fn route_http_to(&self, origin: Option<String>) {
        *self.test_origin.lock().unwrap() = origin;
    }

    pub(crate) fn test_origin(&self) -> Option<String> {
        self.test_origin.lock().unwrap().clone()
    }

    /// Tests only: open plugin sockets for declared hosts on a local server (`ws://…`).
    #[doc(hidden)]
    pub fn route_sockets_to(&self, origin: Option<String>) {
        *self.socket_origin.lock().unwrap() = origin;
    }

    pub(crate) fn socket_origin(&self) -> Option<String> {
        self.socket_origin.lock().unwrap().clone()
    }

    /// Tests only: integration tests run from a test binary, not `cairn`.
    #[doc(hidden)]
    pub fn use_worker(&self, path: std::path::PathBuf) {
        *self.worker.lock().unwrap() = Some(path);
    }

    /// Workers are this same binary in its private `plugin-worker` mode.
    pub(crate) fn executable(&self) -> Result<std::path::PathBuf> {
        match self.worker.lock().unwrap().clone() {
            Some(path) => Ok(path),
            None => Ok(std::env::current_exe()?),
        }
    }

    /// Tests only: run `plugin`'s declared `program` from another path.
    #[doc(hidden)]
    pub fn use_program(&self, plugin: &str, program: &str, path: PathBuf) {
        self.programs
            .lock()
            .unwrap()
            .insert((plugin.to_owned(), program.to_owned()), path);
    }

    pub(crate) fn program_override(&self, plugin: &str, program: &Program) -> Option<PathBuf> {
        self.programs
            .lock()
            .unwrap()
            .get(&(plugin.to_owned(), program.name.clone()))
            .cloned()
    }

    /// Run-scoped CLI commands declared by any package the run's agent is granted.
    pub async fn command(&self, s: &Service, bearer: &str, input: &Value) -> Result<Value> {
        crate::project_workspaces::authorize(s, bearer).await?;
        agents::command(s, bearer, input).await
    }

    pub async fn start(self: Arc<Self>, s: Arc<Service>) {
        runtime::maintain(s).await;
    }
}

pub async fn handle(State(app): State<App>, request: Request) -> Response {
    command_response(&app.service, request)
        .await
        .into_response()
}

pub(crate) async fn command_response(s: &Service, request: Request) -> Result<Response> {
    if request.method() != "POST" {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let bearer = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned();
    let input = Input::read(request).await?;
    let value = s.plugins.command(s, &bearer, &input.body).await?;
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    Ok(response)
}

/// Owner-only management API (enforced by the HTTP layer for `/api/plugins`).
pub async fn routes(s: &Arc<Service>, input: &Input) -> Result<Value> {
    let parts = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    if input.method == "POST" && parts.as_slice() == ["plugins", "manage"] {
        let command = management::decode(input.body.clone())?;
        return manage_http(s, input, command, true).await;
    }
    let body = &input.body;
    // Each route is an authenticated transport adapter to the same typed command.
    let command = match (input.method.as_str(), parts.as_slice()) {
        ("POST", ["plugins", plugin, "form"]) => Some(
            json!({ "kind": "form", "pluginId": plugin, "instanceId": body["instance"], "data": body }),
        ),
        ("POST", ["plugins", plugin, "connections"]) => {
            Some(json!({ "kind": "save", "pluginId": plugin, "data": body }))
        }
        ("PUT", ["plugins", plugin, "connections", instance]) => Some(
            json!({ "kind": "save", "pluginId": plugin, "instanceId": instance, "data": body }),
        ),
        ("DELETE", ["plugins", plugin, "connections", instance]) => {
            Some(json!({ "kind": "delete", "pluginId": plugin, "instanceId": instance }))
        }
        ("POST", ["plugins", plugin, "connections", instance, "enabled"]) => Some(
            json!({ "kind": "enabled", "pluginId": plugin, "instanceId": instance, "data": body }),
        ),
        ("POST", ["plugins", plugin, "connections", instance, "disconnect"]) => {
            Some(json!({ "kind": "disconnect", "pluginId": plugin, "instanceId": instance }))
        }
        ("POST", ["plugins", plugin, "connections", instance, "test"]) => {
            Some(json!({ "kind": "test", "pluginId": plugin, "instanceId": instance }))
        }
        ("GET", ["plugins", plugin, "connections", instance, "view"]) => {
            Some(json!({ "kind": "view", "pluginId": plugin, "instanceId": instance }))
        }
        ("POST", ["plugins", plugin, "connections", instance, "actions"]) => Some(
            json!({ "kind": "action", "pluginId": plugin, "instanceId": instance, "data": body }),
        ),
        (
            "POST",
            [
                "plugins",
                plugin,
                "connections",
                instance,
                "people",
                person,
                operation,
            ],
        ) => Some(
            json!({ "kind": "people", "pluginId": plugin, "instanceId": instance, "userId": person, "operation": operation }),
        ),
        _ => None,
    };
    if let Some(command) = command {
        return manage_http(s, input, management::decode(command)?, false).await;
    }
    let package_command = match (input.method.as_str(), parts.as_slice()) {
        ("GET", ["plugins", id, "connections", instance, "resources"]) => Some((
            "list_plugin_resources",
            json!({
                "pluginId": id,
                "instanceId": instance,
                "resourceType": input.query.get("resourceType").cloned().unwrap_or_default(),
                "page": input.number("page", 1, 1, 10_000)?,
                "query": input.query.get("query").cloned().unwrap_or_default(),
            }),
        )),
        (
            "GET",
            [
                "plugins",
                id,
                "connections",
                instance,
                "resources",
                "ancestors",
            ],
        ) => Some((
            "resolve_plugin_resource",
            json!({
                "pluginId": id,
                "instanceId": instance,
                "resourceType": input.query.get("resourceType").cloned().unwrap_or_default(),
                "id": input.query.get("id").cloned().unwrap_or_default(),
            }),
        )),
        ("GET", ["plugins"]) => Some(("list_plugins", json!({}))),
        ("POST", ["plugins", "packages"]) => Some(("install_plugin_package", body.clone())),
        ("GET", ["plugins", "packages", "operations", id]) => {
            Some(("get_plugin_operation", json!({ "operationId": id })))
        }
        ("DELETE", ["plugins", "packages", "operations", id]) => {
            Some(("cancel_plugin_operation", json!({ "operationId": id })))
        }
        ("POST", ["plugins", "packages", id, "approve"]) => {
            Some(("approve_plugin_package", package_path_input(body, id)?))
        }
        ("POST", ["plugins", "packages", id, "activate"]) => {
            Some(("activate_plugin_package", package_path_input(body, id)?))
        }
        ("DELETE", ["plugins", "packages", id]) => {
            Some(("delete_plugin_package", json!({ "pluginId": id })))
        }
        ("GET", ["plugins", id, "programs"]) => {
            Some(("list_plugin_programs", json!({ "pluginId": id })))
        }
        ("POST", ["plugins", "programs", "prepare"]) => {
            Some(("prepare_plugin_program", body.clone()))
        }
        ("POST", ["plugins", id, "programs", name, action]) => {
            let mut command = json!({ "kind": action, "pluginId": id, "program": name });
            if *action == "install" {
                command["digest"] = body["digest"].clone();
            }
            Some(("prepare_plugin_program", command))
        }
        _ => None,
    };
    if let Some((name, command)) = package_command {
        return crate::capabilities::plugin_package_operation(name)
            .unwrap()
            .invoke(s, command, crate::capabilities::ResumeContext::http(input)?)
            .await;
    }
    match (input.method.as_str(), parts.as_slice()) {
        ("POST", ["plugins", "oauth", "callback"]) => {
            let identity = input
                .identity
                .as_ref()
                .ok_or_else(|| Error::unauthorized("Please sign in."))?;
            oauth::callback(s, body, identity.oauth_binding()).await
        }
        ("GET", ["plugins", id, "oauth"]) => {
            registry::active(s, id).await?;
            Ok(json!({ "callbackUrl": oauth::callback_url(s).await? }))
        }
        ("POST", ["plugins", id, "oauth"]) => {
            let identity = input
                .identity
                .as_ref()
                .ok_or_else(|| Error::unauthorized("Please sign in."))?;
            oauth::start(s, id, body, identity.oauth_binding()).await
        }
        ("GET", ["plugins", id, "oauth", flow]) => {
            let identity = input
                .identity
                .as_ref()
                .ok_or_else(|| Error::unauthorized("Please sign in."))?;
            s.plugins
                .oauth_flows
                .status(id, flow, identity.oauth_binding())
        }
        ("DELETE", ["plugins", id, "oauth", flow]) => {
            let identity = input
                .identity
                .as_ref()
                .ok_or_else(|| Error::unauthorized("Please sign in."))?;
            s.plugins
                .oauth_flows
                .cancel(id, flow, identity.oauth_binding())
        }
        ("POST", ["plugins", id, "device-flow"]) => device_flow::start(s, id).await,
        ("GET", ["plugins", id, "device-flow", flow]) => device_flow::status(s, id, flow),
        ("DELETE", ["plugins", id, "device-flow", flow]) => Ok(device_flow::cancel(s, id, flow)),
        ("GET", ["plugins", id, "projects"]) => {
            let command = cairn_contracts::ListSourceProjectsInput {
                plugin_id: (*id).into(),
                source: input.query.get("source").cloned().unwrap_or_default(),
                instance: input.query.get("instance").cloned().unwrap_or_default(),
                page: input.number("page", 1, 1, 10000)? as u32,
                query: input.query.get("query").cloned().unwrap_or_default(),
            };
            crate::capabilities::resource_operation("list_source_projects")
                .unwrap()
                .invoke(
                    s,
                    serde_json::to_value(command)?,
                    crate::capabilities::ResumeContext::http(input)?,
                )
                .await
        }
        _ => Err(Error::not_found("Not found.")),
    }
}

fn package_path_input(body: &Value, id: &str) -> Result<Value> {
    let mut command = body
        .as_object()
        .cloned()
        .ok_or_else(|| Error::bad("Supply a package command object."))?;
    command.insert("pluginId".into(), json!(id));
    Ok(Value::Object(command))
}

async fn manage_http(
    s: &Arc<Service>,
    input: &Input,
    command: cairn_contracts::ManagePluginInput,
    wrapped: bool,
) -> Result<Value> {
    use cairn_contracts::ManagePluginInput as Command;
    let preparation = match &command {
        Command::Form { plugin_id, .. }
        | Command::Save { plugin_id, .. }
        | Command::Test { plugin_id, .. }
        | Command::View { plugin_id, .. }
        | Command::Action { plugin_id, .. } => Some(plugin_id),
        _ => None,
    };
    if let Some(plugin) = preparation {
        let package = registry::active(s, plugin).await?;
        if program_installation::needs_preparation(s, &package) {
            let crate::capabilities::ResumeContext::Human(account) =
                crate::capabilities::ResumeContext::http(input)?
            else {
                return Err(Error::forbidden(
                    "Owner preparation requires a signed-in owner.",
                ));
            };
            let authority =
                crate::capabilities::plugin_packages::Authority::human(s, &account).await?;
            let origin = crate::native::policy::origin();
            let service = s.clone();
            return s.plugins.operations.start(
                s.clone(),
                operations::Audience::Owner,
                async move {
                    crate::native::policy::DELEGATION
                        .scope(origin, async move {
                            Ok(cairn_contracts::PluginOperationResult::Management {
                                output: Box::pin(management::invoke(
                                    &service,
                                    command,
                                    None,
                                    true,
                                    Some(authority),
                                ))
                                .await?,
                                wrapped,
                            })
                        })
                        .await
                },
            );
        }
    }
    present(
        Box::pin(management::invoke(s, command, None, true, None)).await?,
        wrapped,
    )
}

/// The per-command routes keep their native response shape; all results originate
/// from the typed capability, including deferred preparation results.
fn present(output: cairn_contracts::ManagePluginOutput, wrapped: bool) -> Result<Value> {
    use cairn_contracts::ManagePluginOutput as Output;
    if wrapped {
        return Ok(serde_json::to_value(output)?);
    }
    Ok(match output {
        Output::Form { form } => serde_json::to_value(form)?,
        Output::Saved { instance } | Output::Enabled { instance } | Output::People { instance } => {
            serde_json::to_value(instance)?
        }
        Output::Deleted { deleted } => json!({ "deleted": deleted }),
        Output::Disconnected {
            disconnected,
            revoked,
            message,
        } => json!({ "disconnected": disconnected, "revoked": revoked, "message": message }),
        Output::Tested { ok, message } => json!({ "ok": ok, "message": message }),
        Output::View { view } => json!({ "view": view }),
        Output::Action { result } => result,
        Output::Signin { flow } => serde_json::to_value(flow)?,
        Output::SigninCancelled { cancelled } => json!({ "cancelled": cancelled }),
        Output::Setup {
            manifest,
            setup,
            callback_url,
            management_url,
        } => {
            json!({ "manifest": manifest, "setup": setup, "callbackUrl": callback_url, "managementUrl": management_url })
        }
    })
}
