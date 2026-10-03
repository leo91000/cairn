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

#[tokio::test]
async fn foreign_and_missing_origins_cannot_start_or_complete_sign_in() {
    let app = Fixture::new().await;
    for origin in [None, Some("https://attacker.example")] {
        for route in ["email-code", "verify"] {
            let mut request = app.client.post(format!("{}/api/account/{route}", app.url));
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            let response = request.json(&json!({ "email": "alice@example.test", "challenge": "missing", "code": "12345678" })).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
    assert!(app.mail.0.lock().unwrap().is_empty());
    app.close().await;
}

impl Fixture {
    async fn code(&self, email: &str) -> (Value, String) {
        let response = self
            .post("/api/account/email-code", json!({ "email": email }))
            .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let challenge: Value = response.json().await.unwrap();
        let code = self.mail.0.lock().unwrap().last().unwrap().1.clone();
        (challenge["challenge"].clone(), code)
    }

    async fn verify(&self, challenge: &Value, code: &str) -> reqwest::Response {
        self.post(
            "/api/account/verify",
            json!({ "challenge": challenge, "code": code }),
        )
        .await
    }
}

#[tokio::test]
async fn logout_requires_origin_and_csrf_and_revokes_the_server_session() {
    let app = Fixture::new().await;
    let (challenge, code) = app.code("logout@example.test").await;
    let response = app.verify(&challenge, &code).await;
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let session: Value = response.json().await.unwrap();
    for (origin, csrf) in [
        ("https://attacker.test", session["csrf"].as_str().unwrap()),
        (app.url.as_str(), "wrong"),
        (app.url.as_str(), ""),
    ] {
        let response = app
            .client
            .post(format!("{}/api/account/logout", app.url))
            .header("origin", origin)
            .header("cookie", &cookie)
            .header("x-csrf-token", csrf)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let response = app
        .client
        .post(format!("{}/api/account/logout", app.url))
        .header("origin", &app.url)
        .header("cookie", &cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    let response = app
        .client
        .get(format!("{}/api/account/session", app.url))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let session: Value = response.json().await.unwrap();
    assert_eq!(session["authenticated"], false);
    app.close().await;
}

#[tokio::test]
async fn five_failed_attempts_invalidate_a_code_and_success_is_single_use() {
    let app = Fixture::new().await;
    let (challenge, code) = app.code("attempts@example.test").await;
    for _ in 0..5 {
        assert_eq!(
            app.verify(&challenge, "wrong").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        app.verify(&challenge, &code).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let (challenge, code) = app.code("single-use@example.test").await;
    let (first, second) =
        tokio::join!(app.verify(&challenge, &code), app.verify(&challenge, &code));
    let statuses = [first.status(), second.status()];
    assert!(statuses.contains(&StatusCode::OK));
    assert!(statuses.contains(&StatusCode::UNAUTHORIZED));
    app.close().await;
}

#[tokio::test]
async fn email_delivery_is_limited_across_processes_and_replaced_codes_stop_working() {
    let app = Fixture::new().await;
    let (challenge, code) = app.code("rate@example.test").await;
    let response = app
        .post(
            "/api/account/email-code",
            json!({ "email": " RATE@EXAMPLE.TEST " }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(app.mail.0.lock().unwrap().len(), 1);
    // Advance the persisted limiter's time; observe behavior only through HTTP.
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second_url = format!("http://{}", listener.local_addr().unwrap());
    let second_router = router(app.pool.clone(), app.mail.clone(), second_url.clone())
        .await
        .unwrap();
    let second = tokio::spawn(async move {
        axum::serve(
            listener,
            second_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let response = app
        .client
        .post(format!("{second_url}/api/account/email-code"))
        .header("origin", &second_url)
        .json(&json!({ "email": "rate@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        app.verify(&challenge, &code).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.post(
            "/api/account/email-code",
            json!({ "email": "rate@example.test" })
        )
        .await
        .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    second.abort();
    app.close().await;
}

#[tokio::test]
async fn a_peer_cannot_bypass_delivery_limits_by_changing_email_or_forwarded_ip() {
    let app = Fixture::new().await;
    for index in 0..11 {
        let response = app
            .client
            .post(format!("{}/api/account/email-code", app.url))
            .header("origin", &app.url)
            .header("x-forwarded-for", format!("192.0.2.{index}"))
            .json(&json!({ "email": format!("rate-{index}@example.test") }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if index < 10 {
                StatusCode::ACCEPTED
            } else {
                StatusCode::TOO_MANY_REQUESTS
            }
        );
    }
    for index in 0..31 {
        assert_eq!(
            app.verify(&json!("missing"), "wrong").await.status(),
            if index < 30 {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::TOO_MANY_REQUESTS
            }
        );
    }
    app.close().await;
}

#[tokio::test]
async fn invalid_email_is_rejected_before_delivery() {
    let app = Fixture::new().await;
    for email in [
        "",
        "not-an-email",
        "a@",
        "@example.test",
        "a\r\nb@example.test",
        "a@@example.test",
    ] {
        let response = app
            .post("/api/account/email-code", json!({ "email": email }))
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(app.mail.0.lock().unwrap().is_empty());
    app.close().await;
}

#[tokio::test]
async fn expired_codes_and_sessions_cannot_authenticate_and_sign_in_reuses_the_account() {
    let app = Fixture::new().await;
    let (challenge, code) = app.code("expired@example.test").await;
    query("UPDATE email_codes SET expires_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        app.verify(&challenge, &code).await.status(),
        StatusCode::UNAUTHORIZED
    );
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    let (challenge, code) = app.code("expired@example.test").await;
    let response = app.verify(&challenge, &code).await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let first: Value = response.json().await.unwrap();
    query("UPDATE web_sessions SET expires_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    let response = app
        .client
        .get(format!("{}/api/account/session", app.url))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    let session: Value = response.json().await.unwrap();
    assert_eq!(session["authenticated"], false);
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    let (challenge, code) = app.code(" EXPIRED@EXAMPLE.TEST ").await;
    let response = app.verify(&challenge, &code).await;
    assert_eq!(response.status(), StatusCode::OK);
    let second: Value = response.json().await.unwrap();
    assert_eq!(second["account"], first["account"]);
    app.close().await;
}
