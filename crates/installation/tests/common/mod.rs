//! Helpers shared by the integration test crates.
//!
//! Every test crate compiles this module on its own and uses only part of it.
#![allow(dead_code)]

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
        .write(move |db| db.add_run(&record, None))
        .await
        .unwrap();
}

/// Saves the worker checkpoint of `run`.
pub async fn set_checkpoint(store: &Store, run: &str, checkpoint: Value) {
    store
        .set(&format!("run-checkpoint:{run}"), checkpoint, None)
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
