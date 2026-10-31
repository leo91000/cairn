//! Helpers shared by the integration test crates.
//!
//! Every test crate compiles this module on its own and uses only part of it.
#![allow(dead_code)]

use self::run_fixture::DbRunFixture;

pub mod github;

use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    http::{Request, request::Builder},
    response::Response,
};
use cairn_installation::{config::Config, service::Service, store::Store};
use serde_json::Value;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinHandle};
use tower::ServiceExt;

/// An operation travels through the compact MCP execute interface; native tools
/// remain directly callable for discovery and code-composition tests.
pub fn mcp_arguments(name: &str, args: &Value) -> Value {
    if matches!(name, "search" | "execute") {
        serde_json::json!({ "name": name, "arguments": args })
    } else {
        let operation = serde_json::to_string(name).unwrap();
        serde_json::json!({
            "name": "execute",
            "arguments": { "code": format!("return await cairn.call({operation}, input);"), "input": args },
        })
    }
}

/// The host every in-process request is addressed to.
pub const HOST: &str = "localhost:4310";
/// Largest response body the helpers read.
const BODY_LIMIT: usize = 1_000_000;

/// A configuration rooted in `root`, without worker or runner, with one run slot.
pub fn config(root: &Path) -> Config {
    Config {
        data_dir: root.join("data"),
        home: root.join("home"),
        workspace_roots: vec![root.to_owned()],
        public_url: format!("http://{HOST}"),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: cairn_installation::accounts::opencode::USAGE_URL.into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    }
}

/// The repository checkout, which holds the Node tooling and `tests/`.
pub fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

/// Path of a file under the repository's `tests/fixtures`.
pub fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// [`fixture_path`] as the string form expected by [`Config`] binaries.
pub fn fixture(name: &str) -> String {
    fixture_path(name).to_string_lossy().into_owned()
}

/// Marks the default Codex home as managed, so runs never read host credentials.
pub fn managed_codex_home(home: &Path) {
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(home.join(".codex/cairn-managed-auth"), "1").unwrap();
}

/// Binds a loopback listener on a free port.
pub async fn bind() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (listener, address)
}

/// Serves `app` on `listener` until the returned task is aborted.
pub fn serve(listener: TcpListener, app: Router) -> JoinHandle<()> {
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() })
}

/// Serves `app` on a free loopback port and returns its base URL.
pub async fn serve_locally(app: Router) -> (String, JoinHandle<()>) {
    let (listener, address) = bind().await;
    (format!("http://{address}"), serve(listener, app))
}

/// Polls `probe` until it yields a value, failing the test after `timeout`.
pub async fn eventually<T>(
    timeout: Duration,
    interval: Duration,
    mut probe: impl AsyncFnMut() -> Option<T>,
) -> T {
    tokio::time::timeout(timeout, async {
        loop {
            if let Some(value) = probe().await {
                return value;
            }
            tokio::time::sleep(interval).await;
        }
    })
    .await
    .expect("the awaited condition never held")
}

/// Changes the configuration of a freshly created service.
///
/// `Service::new` hands a clone of the service to a startup task (the artifact
/// preview recovery), so `Arc::get_mut` only succeeds once that task finished.
pub async fn reconfigure(service: &mut Arc<Service>, change: impl FnOnce(&mut Config)) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while Arc::get_mut(service).is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the startup tasks never released the service");
    change(&mut Arc::get_mut(service).unwrap().config);
}

/// Inserts a run record as it would have been persisted.
pub async fn add_run(store: &Store, record: &Value) {
    let record = record.clone();
    store
        .write(move |db| db.add_run_fixture(&record, None))
        .await
        .unwrap();
}

/// Saves the worker checkpoint of `run`.
pub async fn set_checkpoint(store: &Store, run: &str, checkpoint: Value) {
    store
        .save_checkpoint_boundary(run, checkpoint)
        .await
        .unwrap();
}

/// A request builder addressed to [`HOST`].
pub fn request(method: &str, uri: &str) -> Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("host", HOST)
}

/// Sends one request through an in-process router.
pub async fn send(app: &Router, request: Request<Body>) -> Response {
    app.clone().oneshot(request).await.unwrap()
}

pub async fn read_bytes(response: Response) -> Bytes {
    to_bytes(response.into_body(), BODY_LIMIT).await.unwrap()
}

pub async fn read_json(response: Response) -> Value {
    serde_json::from_slice(&read_bytes(response).await).unwrap()
}

pub async fn read_text(response: Response) -> String {
    String::from_utf8(read_bytes(response).await.to_vec()).unwrap()
}

/// A synthetic authenticated relay context, confined to handler tests.
#[derive(Clone)]
pub struct RelayContext {
    token: String,
}

impl RelayContext {
    pub fn new(context: &Value) -> Self {
        Self {
            token: context["value"].as_str().unwrap().to_owned(),
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// The test adapter verifies this token before attaching trusted identity.
    pub fn authorize(&self, request: Builder) -> Builder {
        request.header("x-test-relay-token", &self.token)
    }
}

#[derive(Clone, Copy)]
pub enum Credentials<'a> {
    Anonymous,
    /// Obsolete local cookies must never authenticate an installation request.
    Cookie(&'a RelayContext),
    Owner(&'a RelayContext),
}

impl Credentials<'_> {
    pub fn apply(self, request: Builder) -> Builder {
        match self {
            Self::Anonymous => request,
            Self::Cookie(context) => {
                request.header("cookie", format!("cairn_session={}", context.token))
            }
            Self::Owner(context) => context.authorize(request),
        }
    }
}

/// Authenticated relay transport and task-author adapter, confined to tests.
pub mod relay_fixture;

/// Build a current typed project from the concise setup used by older domain fixtures.
pub trait ProjectFixture {
    fn project_fixture(
        &self,
        input: Value,
        existing: Option<&str>,
    ) -> impl std::future::Future<Output = cairn_installation::error::Result<Value>>;
}

impl ProjectFixture for Service {
    async fn project_fixture(
        &self,
        mut input: Value,
        existing: Option<&str>,
    ) -> cairn_installation::error::Result<Value> {
        let creation = input
            .as_object_mut()
            .and_then(|fields| fields.remove("creationId"));
        let settings: cairn_contracts::ProjectSettings = serde_json::from_value(input)
            .map_err(|error| cairn_installation::error::Error::bad(error.to_string()))?;
        let project = if let Some(id) = existing {
            let current = self.get_project(id).await?;
            self.replace_project(
                cairn_contracts::ReplaceProjectInput {
                    project_id: current.id,
                    revision: current.revision,
                    settings,
                },
                |_| Ok(()),
            )
            .await?
        } else {
            let input = serde_json::from_value(
                serde_json::json!({ "settings": settings, "creationId": creation }),
            );
            let input: cairn_contracts::CreateProjectInput = if creation.is_some() {
                input.map_err(|error| cairn_installation::error::Error::bad(error.to_string()))?
            } else {
                cairn_contracts::CreateProjectInput {
                    settings,
                    creation_id: None,
                }
            };
            self.create_project(input, None, |_| Ok(())).await?
        };
        Ok(serde_json::to_value(project)?)
    }
}

pub mod agent_fixture;

#[allow(unused_imports)]
pub use agent_fixture::{AgentFixture, StoreAgentFixture};

pub mod mission_fixture;

#[allow(unused_imports)]
pub use mission_fixture::{MissionFixture, StoreMissionFixture};

/// JSON is fixture input only; production MCP writers accept the current Rust command.
pub trait McpFixture {
    fn mcp_fixture(
        &self,
        input: Value,
        existing: Option<&str>,
    ) -> impl std::future::Future<Output = cairn_installation::error::Result<Value>>;
}

impl McpFixture for Service {
    async fn mcp_fixture(
        &self,
        input: Value,
        existing: Option<&str>,
    ) -> cairn_installation::error::Result<Value> {
        let input: cairn_contracts::McpInput = serde_json::from_value(input)?;
        let revision = if let Some(id) = existing {
            Some(self.mcps.server(self, id).await?.revision)
        } else {
            None
        };
        Ok(serde_json::to_value(
            self.mcps.save(self, input, existing, revision).await?,
        )?)
    }
}

/// Helpers use the revision a real editor received, while explicit revision tests
/// call the capability directly and deliberately keep stale commands.
pub async fn save_plugin(
    s: &Service,
    plugin: &str,
    input: &Value,
    existing: Option<&str>,
) -> cairn_installation::error::Result<Value> {
    let mut command = input.clone();
    if command.get("settings").is_none() {
        command["settings"] = serde_json::json!({});
    }
    if let Some(id) = existing
        && command.get("revision").is_none()
    {
        command["revision"] =
            serde_json::json!(s.store.plugin_instance(id).await?.unwrap().revision);
    }
    let saved = cairn_installation::plugins::connections::save(
        s,
        plugin,
        serde_json::from_value(command)
            .map_err(|error| cairn_installation::error::Error::bad(error.to_string()))?,
        existing,
    )
    .await?;
    s.instance_fixture(&saved.id).await
}

#[allow(async_fn_in_trait)]
pub trait PluginStoreFixture {
    async fn instance_fixture(&self, id: &str) -> cairn_installation::error::Result<Option<Value>>;

    async fn status_fixture(&self, id: &str) -> cairn_installation::error::Result<Option<Value>>;

    async fn instances_fixture(&self) -> cairn_installation::error::Result<Vec<Value>>;

    async fn put_instance_fixture(&self, value: Value) -> cairn_installation::error::Result<Value>;
}

impl PluginStoreFixture for Store {
    async fn instance_fixture(&self, id: &str) -> cairn_installation::error::Result<Option<Value>> {
        self.plugin_instance(id)
            .await?
            .map(serde_json::to_value)
            .transpose()
            .map_err(Into::into)
    }

    async fn status_fixture(&self, id: &str) -> cairn_installation::error::Result<Option<Value>> {
        let id = id.to_owned();
        self.read(move |db| {
            db.plugin_statuses()?
                .remove(&id)
                .map(serde_json::to_value)
                .transpose()
                .map_err(Into::into)
        })
        .await
    }

    async fn instances_fixture(&self) -> cairn_installation::error::Result<Vec<Value>> {
        self.plugin_instances()
            .await?
            .into_iter()
            .map(|instance| Ok(serde_json::to_value(instance)?))
            .collect()
    }

    async fn put_instance_fixture(&self, value: Value) -> cairn_installation::error::Result<Value> {
        use cairn_installation::plugins::connections::Connection;
        let mut draft = serde_json::to_value(Connection::draft(
            value["id"].as_str().unwrap(),
            value["pluginId"].as_str().unwrap_or("fixture"),
            serde_json::json!({}),
            Vec::new(),
        ))?;
        draft["name"] = serde_json::json!("Fixture");
        draft["revision"] = serde_json::json!("fixture-revision");
        draft["maxMessagesPerHour"] = serde_json::json!(20);
        for (key, value) in value.as_object().unwrap() {
            draft[key] = value.clone();
        }
        if let Some(access) = draft["grantedAccess"].as_object() {
            let mut complete = serde_json::to_value(cairn_contracts::AccessPolicy::default())?;
            for (key, value) in access {
                complete[key] = value.clone();
            }
            draft["grantedAccess"] = complete;
        }
        let mut instance: Connection = serde_json::from_value(draft)?;
        self.transaction(move |db| {
            if !instance.agent_id.is_empty() && instance.granted_access.is_none() {
                instance.granted_access = db
                    .agent(&instance.agent_id)?
                    .map(|agent| agent.settings.access);
            }
            db.save_plugin_version(&instance.plugin_id, "fixture", &"0".repeat(64))?;
            db.save_plugin_instance(&instance)?;
            Ok(serde_json::to_value(&instance)?)
        })
        .await
    }
}

#[allow(async_fn_in_trait)]
pub trait PluginServiceFixture {
    async fn instance_fixture(&self, id: &str) -> cairn_installation::error::Result<Value>;
}

impl PluginServiceFixture for Service {
    async fn instance_fixture(&self, id: &str) -> cairn_installation::error::Result<Value> {
        self.store.instance_fixture(id).await?.ok_or_else(|| {
            cairn_installation::error::Error::not_found("Plugin instance fixture not found")
        })
    }
}

pub async fn plugin_form(
    s: &Service,
    plugin: &str,
    input: &Value,
) -> cairn_installation::error::Result<Value> {
    let mut input = input.clone();
    if input.get("settings").is_none() {
        input["settings"] = serde_json::json!({});
    }
    if input.get("secrets").is_none() {
        input["secrets"] = serde_json::json!({});
    }
    Ok(serde_json::to_value(
        cairn_installation::plugins::forms::form(s, plugin, &serde_json::from_value(input)?)
            .await?,
    )?)
}

pub async fn plugin_action(
    s: &Service,
    plugin: &str,
    instance: &str,
    input: &Value,
) -> cairn_installation::error::Result<Value> {
    let mut input = input.clone();
    if input.get("values").is_none() {
        input["values"] = serde_json::json!({});
    }
    cairn_installation::plugins::panels::action(
        s,
        plugin,
        instance,
        &serde_json::from_value(input)?,
    )
    .await
}

pub async fn enable_plugin(
    s: &Service,
    plugin: &str,
    instance: &str,
    input: &Value,
) -> cairn_installation::error::Result<Value> {
    Ok(serde_json::to_value(
        cairn_installation::plugins::connections::set_enabled(
            s,
            plugin,
            instance,
            &serde_json::from_value(input.clone())?,
        )
        .await?,
    )?)
}

pub async fn plugin_test(
    s: &Service,
    plugin: &str,
    instance: &str,
) -> cairn_installation::error::Result<Value> {
    let message = cairn_installation::plugins::panels::test(s, plugin, instance).await?;
    Ok(serde_json::json!({ "ok": true, "message": message }))
}

pub async fn plugin_people(
    s: &Service,
    plugin: &str,
    instance: &str,
    person: &str,
    action: &str,
) -> cairn_installation::error::Result<Value> {
    let operation: cairn_contracts::PluginPersonAction =
        serde_json::from_value(serde_json::json!(action))
            .map_err(|error| cairn_installation::error::Error::bad(error.to_string()))?;
    let instance =
        cairn_installation::plugins::connections::people(s, plugin, instance, person, operation)
            .await?;
    Ok(serde_json::to_value(instance)?)
}

pub fn instance_configuration(
    instance: &cairn_installation::plugins::connections::Connection,
) -> Value {
    serde_json::to_value(cairn_contracts::PluginConfiguration {
        name: instance.name.clone(),
        enabled: instance.enabled,
        revision: Some(instance.revision.clone()),
        settings: serde_json::from_value(instance.settings.clone()).unwrap(),
        secrets: None,
        token: None,
        credential_flow: None,
        shared_from: None,
        agent_id: instance.agent_id.clone(),
        project_id: instance.project_id.clone(),
        access_mode: instance.access_mode,
        user_ids: instance.user_ids.clone(),
        people: instance.people.clone(),
        max_messages_per_hour: instance.max_messages_per_hour,
        max_per_person_per_hour: instance.max_per_person_per_hour,
        agent_ids: None,
    })
    .unwrap()
}

pub mod run_fixture;

// Existing behavioral suites use JSON wire assertions. Inputs decode through the
// canonical package contract; these helpers own no validation or persistence.
pub async fn package_install(
    s: &Service,
    value: &Value,
) -> cairn_installation::error::Result<Value> {
    let mut value = value.clone();
    if value.get("modules").is_none() {
        value["modules"] = serde_json::json!({});
    }
    let input = serde_json::from_value(value)
        .map_err(|error| cairn_installation::error::Error::bad(error.to_string()))?;
    Ok(serde_json::to_value(
        cairn_installation::plugins::registry::install(s, &input).await?,
    )?)
}

pub async fn package_catalog(s: &Service) -> cairn_installation::error::Result<Value> {
    Ok(serde_json::to_value(
        cairn_installation::plugins::registry::catalog(s).await?,
    )?)
}

pub async fn package_approve(
    s: &Service,
    plugin: &str,
    version: &str,
    digest: &str,
) -> cairn_installation::error::Result<Value> {
    Ok(serde_json::to_value(
        cairn_installation::plugins::registry::approve(s, plugin, version, digest).await?,
    )?)
}

pub async fn plugin_events(store: &Store) -> cairn_installation::error::Result<Vec<Value>> {
    let mut result = Vec::new();
    for instance in store.plugin_instances().await? {
        result.extend(
            store
                .plugin_events(&instance.id, i64::MAX)
                .await?
                .into_iter()
                .map(serde_json::to_value)
                .collect::<serde_json::Result<Vec<_>>>()?,
        );
    }
    Ok(result)
}

pub async fn plugin_deliveries(store: &Store) -> cairn_installation::error::Result<Vec<Value>> {
    let mut result = Vec::new();
    for instance in store.plugin_instances().await? {
        result.extend(
            store
                .plugin_deliveries(&instance.id, i64::MAX)
                .await?
                .into_iter()
                .map(serde_json::to_value)
                .collect::<serde_json::Result<Vec<_>>>()?,
        );
    }
    Ok(result)
}

pub async fn put_plugin_delivery(
    store: &Store,
    value: Value,
) -> cairn_installation::error::Result<()> {
    store
        .save_plugin_delivery(serde_json::from_value(value)?)
        .await
}

pub async fn plugin_requests(store: &Store) -> cairn_installation::error::Result<Vec<Value>> {
    store
        .read(|db| {
            Ok(db
                .plugin_requests()?
                .into_iter()
                .map(serde_json::to_value)
                .collect::<serde_json::Result<_>>()?)
        })
        .await
}

pub async fn plugin_activity(store: &Store) -> cairn_installation::error::Result<Vec<Value>> {
    store
        .read(|db| {
            Ok(db
                .plugin_activity()?
                .into_iter()
                .map(serde_json::to_value)
                .collect::<serde_json::Result<_>>()?)
        })
        .await
}

pub async fn plugin_timers(store: &Store) -> cairn_installation::error::Result<Vec<Value>> {
    store
        .read(|db| {
            let mut result = Vec::new();
            for instance in db.plugin_instances()? {
                result.extend(
                    db.plugin_timers(&instance.id, i64::MAX)?
                        .into_iter()
                        .map(serde_json::to_value)
                        .collect::<serde_json::Result<Vec<_>>>()?,
                );
            }
            Ok(result)
        })
        .await
}

pub async fn put_plugin_timer(
    store: &Store,
    value: Value,
) -> cairn_installation::error::Result<()> {
    let timer: cairn_installation::plugins::state::Timer = serde_json::from_value(value)?;
    store
        .transaction(move |db| {
            db.remove_plugin_timer(&timer.id)?;
            db.save_plugin_timer(&timer)
        })
        .await
}

pub async fn plugin_reply(
    s: &Service,
    connection: &cairn_installation::plugins::connections::Connection,
    value: &Value,
) -> cairn_installation::error::Result<Option<cairn_installation::plugins::work::Reply>> {
    cairn_installation::plugins::work::reply(s, connection, &serde_json::from_value(value.clone())?)
        .await
}

pub async fn plugin_progress(
    s: &Service,
    connection: &cairn_installation::plugins::connections::Connection,
    value: &Value,
) -> cairn_installation::error::Result<Option<Value>> {
    cairn_installation::plugins::work::progress(
        s,
        connection,
        &serde_json::from_value(value.clone())?,
    )
    .await
}

pub async fn plugin_questions(
    s: &Service,
    connection: &cairn_installation::plugins::connections::Connection,
    value: &Value,
) -> cairn_installation::error::Result<Vec<Value>> {
    cairn_installation::plugins::work::questions(
        s,
        connection,
        &serde_json::from_value(value.clone())?,
    )
    .await
}

pub async fn owner_api_route(
    s: &Arc<Service>,
    method: &str,
    path: &str,
    body: Value,
) -> cairn_installation::error::Result<Value> {
    let input = cairn_installation::http::Input {
        method: method.into(),
        path: path.into(),
        body,
        query: std::collections::HashMap::new(),
        headers: axum::http::HeaderMap::new(),
        identity: Some(cairn_installation::auth::InstallationIdentity::trusted(
            cairn_installation::auth::InstallationRole::Owner,
            "fixture-owner",
        )),
    };
    let value = cairn_installation::api::dispatch(s, &input).await?;
    resource_result(s, value).await
}

pub async fn owner_plugin_route(
    s: &Arc<Service>,
    method: &str,
    path: &str,
    body: Value,
) -> cairn_installation::error::Result<Value> {
    let input = cairn_installation::http::Input {
        method: method.into(),
        path: path.into(),
        body,
        query: std::collections::HashMap::new(),
        headers: axum::http::HeaderMap::new(),
        identity: Some(cairn_installation::auth::InstallationIdentity::trusted(
            cairn_installation::auth::InstallationRole::Owner,
            "fixture-owner",
        )),
    };
    cairn_installation::plugins::routes(s, &input).await
}

pub mod node_fixture;

pub mod account_fixture;

use cairn_installation::validation::text;

/// Behavioral HTTP suites await the same bounded manager operations as both apps.
pub async fn resource_result(
    s: &Service,
    value: Value,
) -> cairn_installation::error::Result<Value> {
    let Some(id) = value["operation"].as_str() else {
        return Ok(value);
    };
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        loop {
            let state = s.plugins.operations.get(id)?;
            match text(&state, "state") {
                "completed" => return Ok(resource_output(&state["result"])),
                "failed" | "cancelled" => {
                    return Err(cairn_installation::error::Error::new(
                        state["errorStatus"].as_u64().unwrap_or(502) as u16,
                        text(&state, "error"),
                    ));
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
            }
        }
    })
    .await
    .expect("Resource preparation did not finish")
}

pub fn resource_output(value: &Value) -> Value {
    let field = match text(value, "kind") {
        "project" => "project",
        "sourceProjects" => "page",
        "skillImport" => "imported",
        "skillSource" => "status",
        "skillBundle" => "bundle",
        other => panic!("Unexpected resource result {other}"),
    };
    value[field].clone()
}

/// Complete persisted metadata, with overrides for a behavior exercised by a test.
pub fn deliverable(run: &str, id: &str, overrides: Value) -> cairn_contracts::Deliverable {
    let mut value = serde_json::json!({
        "id": id, "runId": run, "messageId": null, "key": "report", "version": 1,
        "title": "Report", "name": "report.md", "group": "", "kind": "markdown",
        "mediaType": "text/markdown", "size": 0, "createdAt": 1, "url": "",
        "previewStatus": "none", "visibility": "private",
    });
    cairn_installation::store::merge(&mut value, &overrides);
    serde_json::from_value(value).unwrap()
}
