#![allow(dead_code)]

use async_trait::async_trait;
use leo_official_service::{EmailSender, router};
use reqwest::Client;
use serde_json::Value;
use sqlx_core::query::query;
use sqlx_postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use std::{
    str::FromStr,
    sync::{Arc, Mutex},
};
use tokio::task::JoinHandle;
use uuid::Uuid;

#[derive(Default)]
pub struct Mailbox(pub Mutex<Vec<(String, String)>>);

#[async_trait]
impl EmailSender for Mailbox {
    async fn send_code(&self, email: &str, code: &str) -> Result<(), String> {
        self.0.lock().unwrap().push((email.into(), code.into()));
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
}

impl Fixture {
    pub async fn new() -> Self {
        Self::with_oauth(leo_official_service::OAuthProviders::default()).await
    }

    pub async fn with_oauth(oauth: leo_official_service::OAuthProviders) -> Self {
        Self::with_pool_size(oauth, 5).await
    }

    pub async fn with_pool_size(oauth: leo_official_service::OAuthProviders, connections: u32) -> Self {
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
        let app =
            leo_official_service::router_with_oauth(pool.clone(), mail.clone(), url.clone(), oauth)
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
