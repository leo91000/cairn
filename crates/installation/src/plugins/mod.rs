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
mod idempotency;
pub(crate) mod management;
mod mime;
pub mod oauth;
pub mod operations;
pub mod panels;
pub mod program_installation;
pub mod programs;
pub mod projects;
pub mod registry;
pub mod run_credentials;
pub mod runtime;
pub mod scope;
pub mod socket;
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
    let body = &input.body;
    let version = || crate::validation::text(body, "version").to_owned();
    // Cold owner handlers may prepare programs. Keep that work outside the relay's
    // short request lifetime, including views/forms that call exec lazily.
    let cold_handler = match (input.method.as_str(), parts.as_slice()) {
        ("POST", ["plugins", id, "form"]) => Some((*id, "", "form")),
        ("GET", ["plugins", id, "connections", instance, "view"]) => Some((*id, *instance, "view")),
        ("POST", ["plugins", id, "connections", instance, "test"]) => {
            Some((*id, *instance, "test"))
        }
        ("POST", ["plugins", id, "connections", instance, "actions"]) => {
            Some((*id, *instance, "action"))
        }
        ("POST", ["plugins", id, "connections"]) => Some((*id, "", "save")),
        ("PUT", ["plugins", id, "connections", instance]) => Some((*id, *instance, "save")),
        _ => None,
    };
    if let Some((id, instance, handler)) = cold_handler {
        let package = registry::active(s, id).await?;
        let invokes_handler = match handler {
            "form" | "save" => package.manifest.instance_form,
            "view" | "action" => package.manifest.instance_view,
            "test" => package.manifest.instance_test,
            _ => false,
        };
        if invokes_handler && program_installation::needs_preparation(s, &package) {
            let (service, id, instance, body) =
                (s.clone(), id.to_owned(), instance.to_owned(), body.clone());
            return s.plugins.operations.start(s.clone(), async move {
                match handler {
                    "form" => forms::form(&service, &id, &body).await,
                    "view" => panels::view(&service, &id, &instance).await,
                    "test" => panels::test(&service, &id, &instance).await,
                    "action" => panels::action(&service, &id, &instance, &body).await,
                    "save" => {
                        if !instance.is_empty() {
                            connections::get(&service, &id, &instance).await?;
                        }
                        connections::save(
                            &service,
                            &id,
                            &body,
                            (!instance.is_empty()).then_some(instance.as_str()),
                        )
                        .await
                    }
                    _ => unreachable!("classified owner handler"),
                }
            });
        }
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
        ("GET", ["plugins"]) => {
            let mut catalog = registry::catalog(s).await?;
            let mut overview = connections::overview(s).await?;
            // Instances of packages without a worker are ready whenever enabled.
            for connection in &mut overview {
                let worker = catalog["packages"].as_array().and_then(|packages| {
                    packages
                        .iter()
                        .find(|package| package["id"] == connection["pluginId"])
                        .and_then(|package| {
                            serde_json::from_value::<cairn_plugin_api::Manifest>(
                                package["manifest"].clone(),
                            )
                            .ok()
                        })
                        .map(|manifest| manifest.needs_worker())
                });
                if worker == Some(false) && connection["enabled"] == true {
                    connection["status"] = json!({ "state": "ready" });
                }
            }
            catalog["connections"] = json!(overview);
            Ok(catalog)
        }
        ("POST", ["plugins", "packages"]) => registry::install(s, body).await,
        ("GET", ["plugins", "packages", "operations", id]) => s.plugins.operations.get(id),
        ("POST", ["plugins", "packages", id, "approve"]) => {
            registry::activate_request(
                s,
                id,
                &version(),
                Some(crate::validation::text(body, "digest")),
            )
            .await
        }
        ("POST", ["plugins", "packages", id, "activate"]) => {
            registry::activate_request(s, id, &version(), None).await
        }
        ("DELETE", ["plugins", "packages", id]) => registry::uninstall(s, id).await,
        ("POST", ["plugins", id, "form"]) => forms::form(s, id, body).await,
        ("POST", ["plugins", id, "device-flow"]) => device_flow::start(s, id).await,
        ("GET", ["plugins", id, "device-flow", flow]) => device_flow::status(s, id, flow),
        ("DELETE", ["plugins", id, "device-flow", flow]) => Ok(device_flow::cancel(s, id, flow)),
        ("GET", ["plugins", id, "projects"]) => projects::list(s, id, input).await,
        ("GET", ["plugins", id, "programs"]) => program_installation::list(s, id).await,
        ("POST", ["plugins", id, "programs", name, action]) => {
            let (service, id, name, action, body) = (
                s.clone(),
                id.to_string(),
                name.to_string(),
                action.to_string(),
                body.clone(),
            );
            s.plugins.operations.start(s.clone(), async move {
                program_installation::action(&service, &id, &name, &action, &body).await
            })
        }
        ("POST", ["plugins", id, "connections"]) => connections::save(s, id, body, None).await,
        ("PUT", ["plugins", id, "connections", connection]) => {
            connections::get(s, id, connection).await?;
            connections::save(s, id, body, Some(connection)).await
        }
        ("DELETE", ["plugins", id, "connections", connection]) => {
            connections::remove(s, id, connection).await
        }
        ("POST", ["plugins", id, "connections", connection, "enabled"]) => {
            connections::set_enabled(s, id, connection, body).await
        }
        ("POST", ["plugins", id, "connections", connection, "disconnect"]) => {
            oauth::disconnect(s, id, connection, |_| Ok(())).await
        }
        ("POST", ["plugins", id, "connections", connection, "test"]) => {
            panels::test(s, id, connection).await
        }
        ("GET", ["plugins", id, "connections", connection, "view"]) => {
            panels::view(s, id, connection).await
        }
        ("POST", ["plugins", id, "connections", connection, "actions"]) => {
            panels::action(s, id, connection, body).await
        }
        (
            "POST",
            [
                "plugins",
                id,
                "connections",
                connection,
                "people",
                user,
                action,
            ],
        ) => connections::people(s, id, connection, user, action).await,
        _ => Err(Error::not_found("Not found.")),
    }
}
