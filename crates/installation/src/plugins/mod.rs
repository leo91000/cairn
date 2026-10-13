//! Installation-owned plugins. Every package (bundled or installed) runs in an isolated
//! Luau worker and reaches Cairn only through capabilities its manifest declares and
//! the owner approved.
pub mod capabilities;
pub mod cli;
pub mod connections;
pub mod discord;
pub mod registry;
pub mod runtime;
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
use cairn_plugin_host::{Denied, Host};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub struct Plugins {
    pub host: Host,
    pub changed: Notify,
    /// Plugin HTTP never follows redirects, so credentials can't leave declared hosts.
    pub http: reqwest::Client,
    test_origin: Mutex<Option<String>>,
    worker: Mutex<Option<std::path::PathBuf>>,
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
            worker: Mutex::new(None),
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

    /// Run-scoped CLI commands. Only the built-in 1Password package has one today.
    pub async fn command(&self, s: &Service, bearer: &str, input: &Value) -> Result<Value> {
        crate::project_workspaces::authorize(s, bearer).await?;
        if input["plugin"] != "onepassword" {
            return Err(Error::not_found("Plugin command not available."));
        }
        let package = registry::builtin("onepassword").expect("bundled plugin");
        // Host errors never reach the script, but the CLI user should see them.
        let refused = Mutex::new(None::<Error>);
        let refused_ref = &refused;
        let result = self
            .host
            .invoke(
                &self.executable()?,
                &package.manifest,
                &package.source,
                input["arguments"].clone(),
                |method, args| async move {
                    if method != "onepassword.read" {
                        return Err(Denied("forbidden"));
                    }
                    crate::onepassword::call(s, bearer, &args)
                        .await
                        .map_err(|error| {
                            let code = capabilities::code(&error);
                            *refused_ref.lock().unwrap() = Some(error);
                            Denied(code)
                        })
                },
            )
            .await;
        match (result, refused.into_inner().unwrap()) {
            (Ok(value), _) => Ok(value),
            (Err(_), Some(error)) => Err(error),
            (Err(error), None) => Err(Error::bad_gateway(error)),
        }
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
    match (input.method.as_str(), parts.as_slice()) {
        ("GET", ["plugins"]) => {
            let mut catalog = registry::catalog(s).await?;
            catalog["connections"] = json!(connections::overview(s).await?);
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
        ("POST", ["plugins", id, "discover"]) => {
            let package = registry::active(s, id).await?;
            if !package.manifest.permits_capability("transport.discord") {
                return Err(Error::not_found("This plugin has no setup discovery."));
            }
            discord::discover(s, id, body).await
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
        ("POST", ["plugins", id, "connections", connection, "test"]) => {
            let connection = connections::get(s, id, connection).await?;
            let package = registry::active(s, id).await?;
            if !package.manifest.permits_capability("transport.discord") {
                return Err(Error::not_found("This plugin has no connection test."));
            }
            discord::test(s, &connection).await
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
