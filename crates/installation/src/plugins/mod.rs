//! Installation-owned plugins. Every package (bundled or installed) runs in an isolated
//! Luau worker and reaches Cairn only through capabilities its manifest declares and
//! the owner approved.
mod addresses;
pub mod agents;
pub mod capabilities;
pub mod cli;
pub mod connections;
pub mod forms;
pub mod grants;
pub mod legacy;
pub mod oauth;
pub mod operations;
mod paging;
pub mod panels;
pub mod program_installation;
pub mod programs;
pub mod registry;
pub(crate) mod requesters;
pub mod resources;
pub mod routes;
pub mod runtime;
pub mod scope;
pub mod setup;
pub mod socket;
pub(crate) mod turns;
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
    pub(crate) program_installation: tokio::sync::Mutex<()>,
    pub(crate) installer_host: Host,
    pub operations: operations::Operations,
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
            program_installation: tokio::sync::Mutex::new(()),
            installer_host: Host::with_limits(4, std::time::Duration::from_secs(120)),
            operations: operations::Operations::default(),
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

/// Owner management, plus authenticated accounts' own personal OAuth flows.
pub async fn routes(s: &Arc<Service>, input: &Input) -> Result<Value> {
    let parts = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    let body = &input.body;
    let version = || crate::validation::text(body, "version").to_owned();
    // Resource forms use the same tracked preparation as instance forms. Keep
    // the configuration target and owner captured before any cold worker runs.
    let resource_handler = match (input.method.as_str(), parts.as_slice()) {
        (
            "POST",
            [
                "plugins",
                id,
                namespace @ ("connections" | "setup-drafts"),
                root,
                "resources",
                collection,
                "form",
            ],
        ) => Some((*id, *namespace, *root, *collection, None, true)),
        (
            "POST",
            [
                "plugins",
                id,
                namespace @ ("connections" | "setup-drafts"),
                root,
                "resources",
                collection,
            ],
        ) => Some((*id, *namespace, *root, *collection, None, false)),
        (
            "PUT",
            [
                "plugins",
                id,
                namespace @ ("connections" | "setup-drafts"),
                root,
                "resources",
                collection,
                resource,
            ],
        ) => Some((*id, *namespace, *root, *collection, Some(*resource), false)),
        _ => None,
    };
    if let Some((id, namespace, root, collection, resource, preview)) = resource_handler {
        let account = input
            .identity
            .as_ref()
            .map_or("", |identity| identity.account_id.as_str());
        let target = if namespace == "setup-drafts" {
            resources::target::Target::setup(s, id, root, account).await?
        } else {
            resources::target::Target::active(s, id, root).await?
        };
        if resources::declared(target.manifest(), collection)?.form {
            let owner = Arc::new(operations::Owner::for_account(s, account).await?);
            let tracked = program_installation::needs_preparation(s, target.package());
            let (service, collection, resource, body) = (
                s.clone(),
                collection.to_owned(),
                resource.map(str::to_owned),
                body.clone(),
            );
            let work = async move {
                if preview {
                    resources::forms::preview(&service, target, &collection, &body, owner).await
                } else {
                    resources::save_for(
                        &service,
                        target,
                        &collection,
                        &body,
                        resource.as_deref(),
                        owner,
                    )
                    .await
                }
            };
            return if tracked {
                s.plugins.operations.start(s.clone(), work)
            } else {
                work.await
            };
        }
    }
    let setup_handler = match (input.method.as_str(), parts.as_slice()) {
        ("POST", ["plugins", id, "setup-drafts", draft, "form"]) => Some((*id, *draft, true)),
        ("POST", ["plugins", id, "setup-drafts", draft, "activate"]) => Some((*id, *draft, false)),
        _ => None,
    };
    if let Some((id, draft, form)) = setup_handler {
        let account = input
            .identity
            .as_ref()
            .map_or("", |identity| identity.account_id.as_str());
        let owner = operations::Owner::for_account(s, account).await?;
        let (_, approved) = setup::configuration(s, id, draft, account).await?;
        let tracked = approved.package.manifest.instance_form
            && program_installation::needs_preparation(s, &approved.package);
        let (service, id, draft, account, body) = (
            s.clone(),
            id.to_owned(),
            draft.to_owned(),
            account.to_owned(),
            body.clone(),
        );
        let work = async move {
            owner.recheck(&service).await?;
            // These functions retain the setup's exact owner, revision, expiry
            // and approval checks, including through final form execution.
            if form {
                Box::pin(setup::form(&service, &id, &draft, &account, &body)).await
            } else {
                Box::pin(setup::activate(&service, &id, &draft, &account, &body)).await
            }
        };
        return if tracked {
            s.plugins.operations.start(s.clone(), work)
        } else {
            work.await
        };
    }
    // Bind warm and tracked handlers to the owner who admitted the request.
    // Cold work may outlive both the relay request and that owner's access.
    let owner_handler = match (input.method.as_str(), parts.as_slice()) {
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
    if let Some((id, instance, handler)) = owner_handler {
        let account = input
            .identity
            .as_ref()
            .map_or("", |identity| identity.account_id.as_str());
        let owner = Arc::new(operations::Owner::for_account(s, account).await?);
        let package = registry::active(s, id).await?;
        let invokes_handler = match handler {
            "form" | "save" => package.manifest.instance_form,
            "view" | "action" => package.manifest.instance_view,
            "test" => package.manifest.instance_test,
            _ => false,
        };
        let tracked = invokes_handler && program_installation::needs_preparation(s, &package);
        let (service, id, instance, body) =
            (s.clone(), id.to_owned(), instance.to_owned(), body.clone());
        let work = async move {
            match handler {
                "form" => forms::form_owned(&service, &id, &body, owner).await,
                "view" => panels::view_owned(&service, &id, &instance, owner).await,
                "test" => panels::test_owned(&service, &id, &instance, owner).await,
                "action" => panels::action_owned(&service, &id, &instance, &body, owner).await,
                "save" => {
                    if !instance.is_empty() {
                        connections::get(&service, &id, &instance).await?;
                    }
                    connections::save_owned(
                        &service,
                        &id,
                        &body,
                        (!instance.is_empty()).then_some(instance.as_str()),
                        owner,
                    )
                    .await
                }
                _ => unreachable!("classified owner handler"),
            }
        };
        return if tracked {
            s.plugins.operations.start(s.clone(), work)
        } else {
            work.await
        };
    }
    match (input.method.as_str(), parts.as_slice()) {
        ("GET", ["plugins", "personal-authorizations"]) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |identity| identity.account_id.as_str());
            oauth::personal::list(s, account, input.query.get("cursor").map(String::as_str)).await
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
            catalog["setupDrafts"] = json!(true);
            catalog["resourceGrantCounts"] = s.store.read(|db| grants::counts(db)).await?;
            Ok(catalog)
        }
        ("GET", ["plugins", "packages"]) => {
            registry::owner::list(s, input.query.get("cursor").map(String::as_str)).await
        }
        ("GET", ["plugins", "packages", id]) => registry::owner::detail(s, id).await,
        ("GET", ["plugins", "packages", id, "connections"]) => {
            connections::owner::list(s, id, input.query.get("cursor").map(String::as_str)).await
        }
        ("GET", ["plugins", "packages", id, "connections", instance]) => {
            connections::owner::get(s, id, instance).await
        }
        ("GET", ["plugins", "packages", id, "versions"]) => {
            registry::owner::versions(s, id, input.query.get("cursor").map(String::as_str)).await
        }
        ("GET", ["plugins", "packages", id, "versions", version]) => {
            registry::owner::version(s, id, version).await
        }
        ("POST", ["plugins", "packages"]) => registry::install(s, body).await,
        ("GET", ["plugins", "packages", "operations", id]) => s.plugins.operations.get(id),
        ("POST", ["plugins", "packages", id, "approve"]) => {
            registry::activate_request(
                s,
                id,
                &version(),
                Some(crate::validation::text(body, "digest")),
                input
                    .identity
                    .as_ref()
                    .map_or("", |identity| identity.account_id.as_str()),
            )
            .await
        }
        ("POST", ["plugins", "packages", id, "activate"]) => {
            registry::activate_request(
                s,
                id,
                &version(),
                None,
                input
                    .identity
                    .as_ref()
                    .map_or("", |identity| identity.account_id.as_str()),
            )
            .await
        }
        ("DELETE", ["plugins", "packages", id]) => registry::uninstall(s, id).await,
        ("GET", ["plugins", id, "setup-drafts"]) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |identity| identity.account_id.as_str());
            setup::list(
                s,
                id,
                account,
                input.query.get("cursor").map(String::as_str),
            )
            .await
        }
        ("POST", ["plugins", id, "setup-drafts"]) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |identity| identity.account_id.as_str());
            setup::save(s, id, account, body, None).await
        }
        (
            _,
            [
                "plugins",
                id,
                "setup-drafts",
                draft,
                "resources",
                collection,
            ],
        ) => resources::setup_routes(s, input, id, draft, collection, None).await,
        (
            _,
            [
                "plugins",
                id,
                "setup-drafts",
                draft,
                "resources",
                collection,
                resource,
            ],
        ) => resources::setup_routes(s, input, id, draft, collection, Some(resource)).await,
        (
            _,
            [
                "plugins",
                id,
                "setup-drafts",
                draft,
                "resources",
                collection,
                resource,
                "oauth",
                name,
                tail @ ..,
            ],
        ) => {
            Box::pin(oauth::setup_routes(
                s,
                input,
                id,
                draft,
                &[collection, resource, name],
                tail,
            ))
            .await
        }
        (method, ["plugins", id, "setup-drafts", draft]) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |identity| identity.account_id.as_str());
            match method {
                "GET" => setup::get(s, id, draft, account).await,
                "PATCH" => setup::save(s, id, account, body, Some(draft)).await,
                "DELETE" => setup::remove(s, id, draft, account, body).await,
                _ => Err(Error::method_not_allowed("Method not allowed.")),
            }
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
                "route-eligibility",
            ],
        ) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |i| i.account_id.as_str());
            routes::eligibility(
                s,
                id,
                connection,
                collection,
                resource,
                account,
                &input.query,
            )
            .await
        }
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
        ) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |identity| identity.account_id.as_str());
            let owner = Arc::new(operations::Owner::for_account(s, account).await?);
            Box::pin(resources::save_owned(
                s, id, connection, collection, body, None, owner,
            ))
            .await
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
                "form",
            ],
        ) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |identity| identity.account_id.as_str());
            let owner = Arc::new(operations::Owner::for_account(s, account).await?);
            let target = resources::target::Target::active(s, id, instance).await?;
            Box::pin(resources::forms::preview(
                s, target, collection, body, owner,
            ))
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
        ) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |identity| identity.account_id.as_str());
            let owner = Arc::new(operations::Owner::for_account(s, account).await?);
            Box::pin(resources::save_owned(
                s,
                id,
                connection,
                collection,
                body,
                Some(resource),
                owner,
            ))
            .await
        }
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
            method,
            [
                "plugins",
                id,
                "connections",
                connection,
                "resources",
                collection,
                resource,
                "routes",
            ],
        ) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |i| i.account_id.as_str());
            match method {
                "POST" => {
                    routes::save(s, id, connection, collection, resource, account, body, None).await
                }
                "GET" => {
                    routes::list(
                        s,
                        id,
                        connection,
                        collection,
                        resource,
                        input.query.get("cursor").map(String::as_str),
                    )
                    .await
                }
                _ => Err(Error::method_not_allowed("Method not allowed.")),
            }
        }
        (
            method,
            [
                "plugins",
                id,
                "connections",
                connection,
                "resources",
                collection,
                resource,
                "routes",
                route,
            ],
        ) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |i| i.account_id.as_str());
            match method {
                "PUT" => {
                    routes::save(
                        s,
                        id,
                        connection,
                        collection,
                        resource,
                        account,
                        body,
                        Some(route),
                    )
                    .await
                }
                "DELETE" => {
                    routes::remove(
                        s, id, connection, collection, resource, route, account, body,
                    )
                    .await
                }
                _ => Err(Error::method_not_allowed("Method not allowed.")),
            }
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
                "routes",
                route,
                "requests",
            ],
        ) => routes::requests(s, id, connection, collection, resource, route).await,
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
                "routes",
                route,
                "people",
                user,
            ],
        ) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |i| i.account_id.as_str());
            routes::people(
                s, id, connection, collection, resource, route, account, user, body,
            )
            .await
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
        ) => {
            grants::list(
                s,
                id,
                connection,
                collection,
                resource,
                input.query.get("cursor").map(String::as_str),
            )
            .await
        }
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
        ("GET", ["plugins", id, "programs"]) => program_installation::list(s, id).await,
        ("POST", ["plugins", id, "programs", name, action]) => {
            let account = input
                .identity
                .as_ref()
                .map_or("", |identity| identity.account_id.as_str());
            let owner = operations::Owner::for_account(s, account).await?;
            let (service, id, name, action, body) = (
                s.clone(),
                id.to_string(),
                name.to_string(),
                action.to_string(),
                body.clone(),
            );
            s.plugins.operations.start(s.clone(), async move {
                program_installation::action_owned(&service, &id, &name, &action, &body, &owner)
                    .await
            })
        }
        ("DELETE", ["plugins", id, "connections", connection]) => {
            connections::remove(s, id, connection).await
        }
        ("POST", ["plugins", id, "connections", connection, "enabled"]) => {
            connections::set_enabled(s, id, connection, body).await
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
