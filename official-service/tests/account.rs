use async_trait::async_trait;
use leo_official_service::{EmailSender, router};
use reqwest::{Client, StatusCode};
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
struct Mailbox(Mutex<Vec<(String, String)>>);

#[async_trait]
impl EmailSender for Mailbox {
    async fn send_code(&self, email: &str, code: &str) -> Result<(), String> {
        self.0.lock().unwrap().push((email.into(), code.into()));
        Ok(())
    }
}

struct Fixture {
    url: String,
    client: Client,
    mail: Arc<Mailbox>,
    pool: PgPool,
    admin: PgPool,
    schema: String,
    server: JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
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
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mail = Arc::new(Mailbox::default());
        let app = router(pool.clone(), mail.clone(), url.clone())
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
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

    async fn post(&self, route: &str, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}{route}", self.url))
            .header("origin", &self.url)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn close(self) {
        self.server.abort();
        self.pool.close().await;
        query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}

#[tokio::test]
async fn email_code_creates_a_verified_leo_account_and_a_persistent_session() {
    let app = Fixture::new().await;
    let response = app
        .post(
            "/api/account/email-code",
            json!({ "email": " Alice@Example.test " }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let challenge: Value = response.json().await.unwrap();
    assert!(challenge.get("code").is_none());
    let (email, code) = app.mail.0.lock().unwrap()[0].clone();
    assert_eq!(email, "alice@example.test");
    let response = app
        .post(
            "/api/account/verify",
            json!({ "challenge": challenge["challenge"], "code": code }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
    let account: Value = response.json().await.unwrap();
    assert_eq!(account["account"]["email"], "alice@example.test");
    assert!(account["csrf"].as_str().unwrap().len() >= 32);
    let response = app
        .client
        .get(format!("{}/api/account/session", app.url))
        .header("cookie", cookie.split(';').next().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session: Value = response.json().await.unwrap();
    assert_eq!(session["account"], account["account"]);
    assert_eq!(session["installations"], json!([]));
    app.close().await;
}
