#![allow(dead_code)]

use async_trait::async_trait;
use leo_official_service::EmailSender;
use reqwest::Client;
use serde_json::{Value, json};
use sqlx_core::query::query;
use sqlx_postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use std::{
    str::FromStr,
    sync::{Arc, Mutex},
};
use tokio::task::JoinHandle;
use uuid::Uuid;

#[derive(Default)]
pub struct Mailbox(
    pub Mutex<Vec<(String, String)>>,
    pub Mutex<Vec<(String, String, String)>>,
);

#[async_trait]
impl EmailSender for Mailbox {
    async fn send_code(&self, email: &str, code: &str) -> Result<(), String> {
        self.0.lock().unwrap().push((email.into(), code.into()));
        Ok(())
    }

    async fn send_invitation(
        &self,
        email: &str,
        installation: &str,
        url: &str,
    ) -> Result<(), String> {
        self.1
            .lock()
            .unwrap()
            .push((email.into(), installation.into(), url.into()));
        Ok(())
    }
}

pub struct Fixture {
    pub url: String,
    pub client: Client,
    pub mail: Arc<Mailbox>,
    pub pool: PgPool,
    pub admin: PgPool,
    pub schema: String,
    pub server: JoinHandle<()>,
    pub relay: leo_official_service::Relay,
}

impl Fixture {
    pub async fn new() -> Self {
        Self::with_oauth(leo_official_service::OAuthProviders::default()).await
    }

    pub async fn with_oauth(oauth: leo_official_service::OAuthProviders) -> Self {
        Self::with_pool_size(oauth, 5).await
    }

    pub async fn with_pool_size(
        oauth: leo_official_service::OAuthProviders,
        connections: u32,
    ) -> Self {
        Self::with_network(
            oauth,
            connections,
            leo_official_service::TrustedProxies::default(),
        )
        .await
    }

    pub async fn with_network(
        oauth: leo_official_service::OAuthProviders,
        connections: u32,
        proxies: leo_official_service::TrustedProxies,
    ) -> Self {
        let database = std::env::var("LEO_OFFICIAL_TEST_DATABASE_URL")
            .expect("Set LEO_OFFICIAL_TEST_DATABASE_URL to a disposable Postgres database");
        let admin = PgPool::connect(&database).await.unwrap();
        let schema = format!("account_{}", Uuid::new_v4().simple());
        query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let mut pool_options = PgPoolOptions::new().max_connections(connections);
        if connections == 1 {
            pool_options = pool_options.acquire_timeout(std::time::Duration::from_secs(2));
        }

        let pool = pool_options.connect_with(options).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://localhost:{}", listener.local_addr().unwrap().port());
        let mail = Arc::new(Mailbox::default());
        let relay = leo_official_service::Relay::default();
        let app = leo_official_service::router_with_network(
            pool.clone(),
            mail.clone(),
            url.clone(),
            oauth,
            relay.clone(),
            proxies,
        )
        .await
        .unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            url,
            client: Client::new(),
            mail,
            pool,
            admin,
            schema,
            server,
            relay,
        }
    }

    pub async fn post(&self, route: &str, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}{route}", self.url))
            .header("origin", &self.url)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    pub fn authenticated(
        &self,
        cookie: &str,
        session: &Value,
        method: reqwest::Method,
        route: &str,
    ) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{route}", self.url))
            .header("origin", &self.url)
            .header("cookie", cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
    }

    pub async fn close(self) {
        self.server.abort();
        self.pool.close().await;
        query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}

pub async fn login(app: &Fixture, email: &str) -> (String, Value) {
    let challenge: Value = app
        .post("/api/account/email-code", json!({ "email": email }))
        .await
        .json()
        .await
        .unwrap();
    let code = app.mail.0.lock().unwrap().last().unwrap().1.clone();
    let response = app
        .post(
            "/api/account/verify",
            json!({ "challenge": challenge["challenge"], "code": code }),
        )
        .await;
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    (cookie, response.json().await.unwrap())
}

/// A real installation router behind its outbound connector and official HTTP API.
/// Extra HTTP handlers let tests supply slow or broken responses at the transport seam.
pub struct RelayedInstallation {
    pub app: Fixture,
    pub installation: Arc<leo_agent_manager::service::Service>,
    pub cookie: String,
    pub session: Value,
    pub base: String,
    pub stop: tokio_util::sync::CancellationToken,
    pub connector: JoinHandle<leo_agent_manager::error::Result<()>>,
    pub root: tempfile::TempDir,
}

impl RelayedInstallation {
    pub async fn new(extra_routes: axum::Router) -> Self {
        Self::with_runner_url(extra_routes, String::new()).await
    }

    pub async fn with_runner_url(extra_routes: axum::Router, runner_url: String) -> Self {
        Self::with_app(extra_routes, runner_url, Fixture::new().await).await
    }

    pub async fn with_pool_size(extra_routes: axum::Router, connections: u32) -> Self {
        let app =
            Fixture::with_pool_size(leo_official_service::OAuthProviders::default(), connections)
                .await;
        Self::with_app(extra_routes, String::new(), app).await
    }

    async fn with_app(extra_routes: axum::Router, runner_url: String, app: Fixture) -> Self {
        use leo_agent_manager::{config::Config, service::Service};
        use serde_json::json;
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;

        let (cookie, session) = login(&app, "relay-owner@example.test").await;
        let claim: Value = app
            .client
            .post(format!("{}/api/installations/claim-code", app.url))
            .header("origin", &app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let config: Config = serde_json::from_value(json!({
            "dataDir": root.path().join("data"),
            "home": root.path().join("home"),
            "workspaceRoots": [root.path()],
            "publicUrl": "http://localhost:4310",
            "host": "127.0.0.1",
            "port": 0,
            "codexBin": "codex",
            "claudeBin": "claude",
            "ghBin": "gh",
            "concurrency": 1,
            "logger": false,
            "workerEnabled": false,
            "runnerUrl": runner_url,
        }))
        .unwrap();
        std::fs::create_dir_all(root.path().join("home/.codex")).unwrap();
        std::fs::write(root.path().join("home/.codex/leo-managed-auth"), "1").unwrap();
        let installation = Service::new(config).await.unwrap();
        let router = leo_agent_manager::http::router(installation.clone())
            .await
            .unwrap()
            .merge(extra_routes);
        let identity_dir = installation.config.data_dir.join("installation-relay");
        leo_agent_manager::relay::claim(
            &app.url,
            &identity_dir,
            claim["code"].as_str().unwrap(),
            "Real installation",
        )
        .await
        .unwrap();
        let session: Value = app
            .client
            .get(format!("{}/api/account/session", app.url))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = session["installations"][0]["id"].as_str().unwrap();
        let stop = CancellationToken::new();
        let connector = tokio::spawn(leo_agent_manager::relay::connect(
            identity_dir,
            router,
            stop.clone(),
        ));
        let base = format!("{}/api/installations/{id}/api", app.url);
        let fixture = Self {
            app,
            installation,
            cookie,
            session,
            base,
            stop,
            connector,
            root,
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let response = fixture.get("/chats").send().await.unwrap();
                if response.status() == reqwest::StatusCode::OK {
                    assert_eq!(response.json::<Value>().await.unwrap(), json!([]));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("installation must become accessible through the relay");
        fixture
    }

    pub fn get(&self, route: &str) -> reqwest::RequestBuilder {
        self.app
            .client
            .get(format!("{}{route}", self.base))
            .header("cookie", &self.cookie)
    }

    pub async fn close(self) {
        self.stop.cancel();
        self.connector.await.unwrap().unwrap();
        self.installation.shutdown.cancel();
        self.installation.avatars.close().await;
        self.app.close().await;
    }
}
