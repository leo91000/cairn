//! Installation-owned plugins. Every package (bundled or installed) runs in an isolated
//! Luau worker and reaches Cairn only through capabilities its manifest declares and
//! the owner approved.
pub mod agents;
pub mod capabilities;
pub mod cli;
pub mod connections;
pub mod forms;
pub mod grants;
pub mod legacy;
pub mod oauth;
pub mod panels;
pub mod programs;
pub mod registry;
pub(crate) mod requesters;
pub mod resources;
pub mod runtime;
pub mod scope;
pub mod socket;
pub mod webhooks;
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
    /// When each paced key (e.g. a bot token's Identify budget) may send next.
    paces: socket::pacing::Paces,
    oauth: oauth::Authorizations,
}

impl Default for Plugins {
    fn default() -> Self {
        Self {
            host: Host::default(),
            changed: Notify::new(),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("plugin HTTP client"),
            test_origin: Mutex::new(None),
            socket_origin: Mutex::new(None),
            worker: Mutex::new(None),
            programs: Mutex::new(HashMap::new()),
            paces: socket::pacing::Paces::default(),
            oauth: oauth::Authorizations::default(),
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

    pub(crate) fn program_path(&self, plugin: &str, program: &Program) -> PathBuf {
        self.programs
            .lock()
            .unwrap()
            .get(&(plugin.to_owned(), program.name.clone()))
            .cloned()
            .unwrap_or_else(|| PathBuf::from(&program.path))
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

/// Owner management, plus authenticated accounts' own personal OAuth flows.
pub async fn routes(s: &Arc<Service>, input: &Input) -> Result<Value> {
    let parts = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    let body = &input.body;
    let version = || crate::validation::text(body, "version").to_owned();
    match (input.method.as_str(), parts.as_slice()) {
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
        ("POST", ["plugins", "packages", id, "approve"]) => {
            registry::approve(s, id, &version(), crate::validation::text(body, "digest")).await
        }
        ("POST", ["plugins", "packages", id, "activate"]) => {
            registry::activate(s, id, &version()).await
        }
        ("DELETE", ["plugins", "packages", id]) => registry::uninstall(s, id).await,
        ("POST", ["plugins", id, "form"]) => forms::form(s, id, body).await,
        (
            method,
            [
                "plugins",
                id,
                "connections",
                instance,
                "resources",
                collection,
                resource,
                "oauth",
                name,
            ],
        ) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |identity| identity.account_id.as_str());
            match method {
                "POST" => {
                    oauth::start(s, id, instance, collection, resource, name, account, body).await
                }
                "GET" => oauth::status(s, id, instance, collection, resource, name, account).await,
                "DELETE" => {
                    oauth::revoke(s, id, instance, collection, resource, name, account).await
                }
                _ => Err(Error::method_not_allowed("Method not allowed.")),
            }
        }
        (
            "POST",
            [
                "plugins",
                id,
                "connections",
                instance,
                "resources",
                collection,
                resource,
                "oauth",
                name,
                "finish",
            ],
        ) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |identity| identity.account_id.as_str());
            oauth::finish(s, id, instance, collection, resource, name, account, body).await
        }
        (
            "POST",
            [
                "plugins",
                id,
                "connections",
                connection,
                "resources",
                collection,
            ],
        ) => resources::save(s, id, connection, collection, body, None).await,
        (
            "GET",
            [
                "plugins",
                id,
                "connections",
                connection,
                "resources",
                collection,
            ],
        ) => {
            resources::list(
                s,
                id,
                connection,
                collection,
                input.query.get("cursor").map(String::as_str),
            )
            .await
        }
        (
            "GET",
            [
                "plugins",
                id,
                "connections",
                connection,
                "resources",
                collection,
                resource,
            ],
        ) => Ok(serde_json::to_value(
            resources::get(s, id, connection, collection, resource).await?,
        )?),
        (
            "PUT",
            [
                "plugins",
                id,
                "connections",
                connection,
                "resources",
                collection,
                resource,
            ],
        ) => resources::save(s, id, connection, collection, body, Some(resource)).await,
        (
            "DELETE",
            [
                "plugins",
                id,
                "connections",
                connection,
                "resources",
                collection,
                resource,
            ],
        ) => resources::remove(s, id, connection, collection, resource).await,
        (
            "POST",
            [
                "plugins",
                id,
                "connections",
                connection,
                "resources",
                collection,
                resource,
                "grants",
            ],
        ) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |identity| identity.account_id.as_str());
            grants::create(s, id, connection, collection, resource, account, body).await
        }
        (
            "GET",
            [
                "plugins",
                id,
                "connections",
                connection,
                "resources",
                collection,
                resource,
                "grants",
            ],
        ) => grants::list(s, id, connection, collection, resource).await,
        (
            "DELETE",
            [
                "plugins",
                id,
                "connections",
                connection,
                "resources",
                collection,
                resource,
                "grants",
                grant,
            ],
        ) => grants::remove(s, id, connection, collection, resource, grant).await,
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
