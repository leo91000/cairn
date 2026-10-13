//! Installation-owned plugins. Bundled packages use the same isolated host API.
pub mod cli;
pub mod discord;

use crate::{
    error::{Error, Result},
    http::{App, Input},
    service::Service,
    validation::text,
};
use axum::{
    Json,
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use cairn_plugin_api::Manifest;
use cairn_plugin_host::Host;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tokio::sync::Notify;

#[derive(Default)]
pub struct Plugins {
    pub host: Host,
    pub changed: Notify,
}

pub fn package(id: &str) -> Result<(Manifest, &'static str)> {
    let (manifest, source) = match id {
        "onepassword" => (
            include_str!("../../../../plugins/onepassword/manifest.json"),
            include_str!("../../../../plugins/onepassword/main.luau"),
        ),
        "discord" => (
            include_str!("../../../../plugins/discord/manifest.json"),
            include_str!("../../../../plugins/discord/main.luau"),
        ),
        _ => return Err(Error::not_found("Plugin not installed.")),
    };
    let manifest: Manifest = serde_json::from_str(manifest)?;
    manifest.validate().map_err(Error::bad)?;
    Ok((manifest, source))
}

impl Plugins {
    pub async fn command(&self, s: &Service, bearer: &str, input: &Value) -> Result<Value> {
        crate::project_workspaces::authorize(s, bearer).await?;
        if input["plugin"] != "onepassword" {
            return Err(Error::not_found("Plugin command not available."));
        }
        let (manifest, source) = package("onepassword")?;
        let executable = std::env::current_exe()?;
        self.host
            .invoke(
                &executable,
                &manifest,
                source,
                input["arguments"].clone(),
                |method, args| async move {
                    if method != "onepassword.read" {
                        return Err("Plugin capability is not granted.".into());
                    }
                    crate::onepassword::call(s, bearer, &args)
                        .await
                        .map_err(|error| error.message)
                },
            )
            .await
            .map_err(Error::bad_gateway)
    }

    pub async fn start(self: Arc<Self>, s: Arc<Service>) {
        discord::maintain(s).await;
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

pub async fn routes(s: &Arc<Service>, input: &Input) -> Result<Value> {
    let parts = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    match (input.method.as_str(), parts.as_slice()) {
        ("GET", ["plugins"]) => {
            let packages =
                ["onepassword", "discord"].map(|id| package(id).map(|(manifest, _)| manifest));
            let packages = packages.into_iter().collect::<Result<Vec<_>>>()?;
            Ok(json!({
                "packages": packages,
                "connections": discord::overview(s).await?,
            }))
        }
        ("POST", ["plugins", "discord"]) => discord::save(s, &input.body, None).await,
        ("POST", ["plugins", "discord", "discover"]) => discord::discover(s, &input.body).await,
        ("PUT", ["plugins", "discord", id]) => discord::save(s, &input.body, Some(id)).await,
        ("DELETE", ["plugins", "discord", id]) => discord::remove(s, id).await,
        ("POST", ["plugins", "discord", id, "test"]) => discord::test(s, id).await,
        ("POST", ["plugins", "discord", id, "enabled"]) => {
            discord::set_enabled(s, id, &input.body).await
        }
        ("POST", ["plugins", "discord", id, "people", user, action]) => {
            discord::people(s, id, user, action).await
        }
        _ => Err(Error::not_found("Not found.")),
    }
}

pub(crate) fn executable() -> Result<std::path::PathBuf> {
    Ok(std::env::current_exe()?)
}

pub(crate) async fn invoke_discord(
    s: &Service,
    executable: &Path,
    input: Value,
    connection: &discord::Connection,
    message: &Value,
) -> Result<Value> {
    let expected = connection.clone();
    s.store
        .read(move |db| discord::authorize(db, &expected))
        .await?;
    let (manifest, source) = package("discord")?;
    s.plugins
        .host
        .invoke(executable, &manifest, source, input, |method, args| {
            let connection = connection.clone();
            let message = message.clone();
            async move {
                if method != "conversations.submit" {
                    return Err("Plugin capability is not granted.".into());
                }
                discord::submit(s, &connection, &message, text(&args, "text"))
                    .await
                    .map_err(|error| error.message)
            }
        })
        .await
        .map_err(Error::bad_gateway)
}
