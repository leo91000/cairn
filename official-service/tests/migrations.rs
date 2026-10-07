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

#[tokio::test]
async fn existing_sessions_keep_their_access_and_deadlines_but_must_confirm_sensitive_changes() {
    let database = std::env::var("LEO_OFFICIAL_TEST_DATABASE_URL").unwrap();
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
    let migrations = sqlx_macros::migrate!("./migrations");
    let legacy = Migrator {
        migrations: Cow::Owned(
            migrations
                .iter()
                .filter(|migration| migration.version < 202610061550)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    legacy.run(&pool).await.unwrap();
    let account = Uuid::new_v4().to_string();
    query("INSERT INTO leo_accounts (id, email) VALUES ($1, 'legacy@example.test')")
        .bind(&account)
        .execute(&pool)
        .await
        .unwrap();
    for token in ["legacy-browser", "legacy-phone"] {
        query("INSERT INTO web_sessions (digest, account_id, csrf, expires_at) VALUES ($1, $2, 'fixture-csrf', '2099-01-01T00:00:00Z')")
            .bind(hex::encode(Sha256::digest(token))).bind(&account).execute(&pool).await.unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let mailbox = Arc::new(Mailbox::default());
    let app = leo_official_service::router(pool.clone(), mailbox.clone(), origin.clone())
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
    let client = Client::new();
    let sessions: serde_json::Value = client
        .get(format!("{origin}/api/account/sessions"))
        .header("cookie", "leo_session=legacy-browser")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let records = sessions["sessions"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_ne!(records[0]["id"], records[1]["id"]);
    assert_eq!(
        records
            .iter()
            .filter(|record| record["current"] == true)
            .count(),
        1
    );
    for record in records {
        assert_eq!(record["createdAt"], "2098-12-25T00:00:00Z");
        assert_eq!(record["expiresAt"], "2099-01-01T00:00:00Z");
        assert_eq!(record["device"], "Unknown device");
    }
    let deletion = client
        .post(format!("{origin}/api/account/delete"))
        .header("cookie", "leo_session=legacy-browser")
        .header("origin", &origin)
        .header("x-csrf-token", "fixture-csrf")
        .json(&json!({ "email": "legacy@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(deletion.status(), StatusCode::FORBIDDEN);
    let phone = records
        .iter()
        .find(|record| record["current"] == false)
        .unwrap();
    let revoked = client
        .delete(format!(
            "{origin}/api/account/sessions/{}",
            phone["id"].as_str().unwrap()
        ))
        .header("cookie", "leo_session=legacy-browser")
        .header("origin", &origin)
        .header("x-csrf-token", "fixture-csrf")
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        client
            .get(format!("{origin}/api/account/sessions"))
            .header("cookie", "leo_session=legacy-phone")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    let challenge: serde_json::Value = client
        .post(format!("{origin}/api/account/email-code"))
        .header("origin", &origin)
        .json(&json!({ "email": "legacy@example.test" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let code = mailbox.0.lock().unwrap().last().unwrap().1.clone();
    let confirmed = client
        .post(format!("{origin}/api/account/reauth/email"))
        .header("cookie", "leo_session=legacy-browser")
        .header("origin", &origin)
        .header("x-csrf-token", "fixture-csrf")
        .json(&json!({
            "challenge": challenge["challenge"],
            "code": code
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(confirmed.status(), StatusCode::NO_CONTENT);
    let confirmed_sessions: serde_json::Value = client
        .get(format!("{origin}/api/account/sessions"))
        .header("cookie", "leo_session=legacy-browser")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(confirmed_sessions, sessions);

    let revoked = client
        .delete(format!(
            "{origin}/api/account/sessions/{}",
            phone["id"].as_str().unwrap()
        ))
        .header("cookie", "leo_session=legacy-browser")
        .header("origin", &origin)
        .header("x-csrf-token", "fixture-csrf")
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        client
            .get(format!("{origin}/api/account/sessions"))
            .header("cookie", "leo_session=legacy-phone")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{origin}/api/account/sessions"))
            .header("cookie", "leo_session=legacy-browser")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    server.abort();
    pool.close().await;
    query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
async fn renaming_the_proof_timestamp_preserves_existing_session_proofs_and_deadlines() {
    let database = std::env::var("LEO_OFFICIAL_TEST_DATABASE_URL").unwrap();
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
    let migrations = sqlx_macros::migrate!("./migrations");
    let legacy = Migrator {
        migrations: Cow::Owned(
            migrations
                .iter()
                .filter(|migration| migration.version < 202610061900)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    legacy.run(&pool).await.unwrap();
    let cases = [
        ("recent", Some(-60), StatusCode::NO_CONTENT),
        ("stale", Some(-600), StatusCode::FORBIDDEN),
        ("unconfirmed", None, StatusCode::FORBIDDEN),
        ("future", Some(60), StatusCode::FORBIDDEN),
    ];
    for (token, seconds, _) in cases {
        let account = Uuid::new_v4().to_string();
        query("INSERT INTO leo_accounts (id, email) VALUES ($1, $2)")
            .bind(&account)
            .bind(format!("{token}@example.test"))
            .execute(&pool)
            .await
            .unwrap();
        query("INSERT INTO web_sessions (digest, account_id, csrf, expires_at, authenticated_at) VALUES ($1, $2, 'fixture-csrf', '2099-01-01T00:00:00Z', clock_timestamp() + $3 * interval '1 second')")
            .bind(hex::encode(Sha256::digest(token))).bind(&account).bind(seconds)
            .execute(&pool).await.unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let app =
        leo_official_service::router(pool.clone(), Arc::new(Mailbox::default()), origin.clone())
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
    let client = Client::new();
    for (token, _, expected) in cases {
        let sessions: serde_json::Value = client
            .get(format!("{origin}/api/account/sessions"))
            .header("cookie", format!("leo_session={token}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(sessions["sessions"][0]["expiresAt"], "2099-01-01T00:00:00Z");
        assert_eq!(sessions["sessions"][0]["current"], true);
        let deletion = client
            .post(format!("{origin}/api/account/delete"))
            .header("cookie", format!("leo_session={token}"))
            .header("origin", &origin)
            .header("x-csrf-token", "fixture-csrf")
            .json(&json!({ "email": format!("{token}@example.test") }))
            .send()
            .await
            .unwrap();
        assert_eq!(deletion.status(), expected, "{token}");
    }
    server.abort();
    pool.close().await;
    query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
