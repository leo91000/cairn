extern crate sqlx_core as sqlx;

mod common;

use common::Mailbox;
use reqwest::{Client, StatusCode};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx_core::{migrate::Migrator, query::query};
use sqlx_postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use std::{borrow::Cow, str::FromStr, sync::Arc};
use uuid::Uuid;

#[tokio::test]
async fn startup_migrates_legacy_codes_with_300_failures_without_reopening_their_budget() {
    let database = std::env::var("LEO_OFFICIAL_TEST_DATABASE_URL")
        .expect("Set LEO_OFFICIAL_TEST_DATABASE_URL to a disposable Postgres database");
    let admin = PgPool::connect(&database).await.unwrap();
    let schema = format!("migration_{}", Uuid::new_v4().simple());
    query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();

    // Start with the schema before this PR's challenge migrations, as on main.
    let migrations = sqlx_macros::migrate!("./migrations");
    let legacy = Migrator {
        migrations: Cow::Owned(
            migrations
                .iter()
                .filter(|migration| migration.version < 202610051245)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    legacy.run(&pool).await.unwrap();
    for (challenge, failures, lifetime) in [
        ("exhausted", 300, 600),
        ("expired", 300, -1),
        ("usable", 2, 600),
        ("limited", 2, 600),
    ] {
        let digest = hex::encode(Sha256::digest(format!("{challenge}:12345678")));
        query("INSERT INTO email_codes (challenge, email, code_digest, attempts, expires_at) VALUES ($1, $2, $3, $4, now() + $5 * interval '1 second')")
            .bind(challenge)
            .bind(format!("{challenge}@example.test"))
            .bind(digest)
            .bind(failures)
            .bind(lifetime)
            .execute(&pool)
            .await
            .unwrap();
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let app =
        leo_official_service::router(pool.clone(), Arc::new(Mailbox::default()), origin.clone())
            .await
            .expect("startup must migrate codes with more than fifty legacy failures");
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    let client = Client::new();
    for (challenge, code, expected) in [
        ("exhausted", "12345678", StatusCode::UNAUTHORIZED),
        ("expired", "12345678", StatusCode::UNAUTHORIZED),
        ("usable", "12345678", StatusCode::OK),
        ("usable", "12345678", StatusCode::UNAUTHORIZED),
        ("limited", "wrong", StatusCode::UNAUTHORIZED),
        ("limited", "wrong", StatusCode::UNAUTHORIZED),
        ("limited", "wrong", StatusCode::UNAUTHORIZED),
        ("limited", "12345678", StatusCode::UNAUTHORIZED),
    ] {
        let response = client
            .post(format!("{origin}/api/account/verify"))
            .header("origin", &origin)
            .json(&json!({
                "challenge": challenge,
                "code": code,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "legacy challenge: {challenge}");
    }

    server.abort();
    pool.close().await;
    query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
