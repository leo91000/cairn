mod common;

use common::{Fixture, login};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sqlx_core::{query::query, query_as::query_as};
use uuid::Uuid;

fn report() -> Value {
    json!({
        "schema": 1,
        "appVersion": "0.55.1",
        "buildType": "release",
        "manufacturer": "Test manufacturer",
        "model": "Test phone",
        "androidApi": 35,
        "connection": "RELAY",
        "heapUsedBytes": 1000,
        "heapLimitBytes": 10000,
        "measurements": {
            "windowId": Uuid::new_v4().to_string(),
            "sequence": 1,
            "elapsedMs": 10000,
            "operations": [{ "operation": "MARKDOWN", "screen": "CHAT", "count": 1, "startedOnMainThread": 0, "meanMs": 50.0, "maxMs": 50.0, "p95UpperBoundMs": 50.0 }],
            "frames": [{ "screen": "CHAT", "swiping": true, "count": 100, "missedDeadline": 5, "frozen": 1, "droppedReports": 0, "maxMs": 800.0, "maxUiDelayMs": 10.0, "maxLayoutMs": 20.0, "maxDrawMs": 10.0, "maxGpuMs": 10.0 }],
            "recentSlowSamples": [{ "elapsedMs": 9000, "screen": "CHAT", "kind": "FRAME", "durationMs": 800.0, "startedOnMainThread": true, "swiping": true }]
        }
    })
}

const PATH: &str = "/api/account/telemetry/performance";

#[tokio::test]
async fn uploads_are_authenticated_private_and_idempotent_without_an_installation() {
    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "performance@example.test").await;
    let mut data = report();
    let anonymous = app
        .client
        .post(format!("{}{PATH}", app.url))
        .header("origin", &app.url)
        .json(&data)
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let csrf = app
        .client
        .post(format!("{}{PATH}", app.url))
        .header("origin", &app.url)
        .header("cookie", &cookie)
        .json(&data)
        .send()
        .await
        .unwrap();
    assert_eq!(csrf.status(), StatusCode::FORBIDDEN);
    for sequence in [1, 2, 1, 2] {
        data["measurements"]["sequence"] = json!(sequence);
        data["measurements"]["frames"][0]["count"] = json!(100 * sequence);
        let response = app
            .authenticated(&cookie, &session, Method::POST, PATH)
            .json(&data)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let stored: Value = app
        .authenticated(&cookie, &session, Method::GET, PATH)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stored["reports"].as_array().unwrap().len(), 1);
    assert_eq!(stored["reports"][0]["measurements"]["sequence"], 2);
    assert_eq!(
        stored["reports"][0]["measurements"]["frames"][0]["count"],
        200
    );
    let (other_cookie, other_session) = login(&app, "other@example.test").await;
    let other: Value = app
        .authenticated(&other_cookie, &other_session, Method::GET, PATH)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(other["reports"], json!([]));
    for secret in [
        cookie.as_str(),
        session["csrf"].as_str().unwrap(),
        "performance@example.test",
    ] {
        assert!(!stored.to_string().contains(secret));
    }
    let account = session["account"]["id"].as_str().unwrap();
    query("DELETE FROM cairn_accounts WHERE id = $1")
        .bind(account)
        .execute(&app.pool)
        .await
        .unwrap();
    let (count,): (i64,) = query_as("SELECT count(*) FROM client_performance")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    app.close().await;
}

#[tokio::test]
async fn schema_and_size_limits_reject_content_and_invalid_metrics() {
    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "validation@example.test").await;
    let mut invalids = Vec::new();
    let mut input = report();
    input["message"] = json!("private content");
    invalids.push(input);
    let mut input = report();
    input["measurements"]["frames"][0]["chatId"] = json!("private id");
    invalids.push(input);
    let mut input = report();
    input["measurements"]["operations"][0]["meanMs"] = json!(-1);
    invalids.push(input);
    let mut input = report();
    input["measurements"]["frames"][0]["screen"] = json!("chat/private-id");
    invalids.push(input);
    let mut input = report();
    input["measurements"]["recentSlowSamples"] = json!(vec![
            report()["measurements"]["recentSlowSamples"][0].clone();
            41
        ]);
    invalids.push(input);
    let mut input = report();
    input["schema"] = json!(2);
    invalids.push(input);
    for input in invalids {
        let response = app
            .authenticated(&cookie, &session, Method::POST, PATH)
            .json(&input)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error());
    }
    let oversized = app
        .authenticated(&cookie, &session, Method::POST, PATH)
        .header("Content-Type", "application/json")
        .body(" ".repeat(65537))
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let (count,): (i64,) = query_as("SELECT count(*) FROM client_performance")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    app.close().await;
}

#[tokio::test]
async fn retention_bounds_rate_limits_and_expiry_are_enforced() {
    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "retention@example.test").await;
    let account = session["account"]["id"].as_str().unwrap();
    let encoded = report().to_string();
    query("INSERT INTO client_performance (account_id, window_id, sequence, report, updated_at) SELECT $1, n::text, 1, $2::jsonb, now() - interval '1 day' FROM generate_series(1, 256) n")
        .bind(account).bind(&encoded).execute(&app.pool).await.unwrap();
    let data = report();
    for _ in 0..6 {
        let response = app
            .authenticated(&cookie, &session, Method::POST, PATH)
            .json(&data)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let limited = app
        .authenticated(&cookie, &session, Method::POST, PATH)
        .json(&data)
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    let (count,): (i64,) = query_as("SELECT count(*) FROM client_performance")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 256);
    query("UPDATE client_performance SET updated_at = now() - interval '15 days'")
        .execute(&app.pool)
        .await
        .unwrap();
    let expired: Value = app
        .authenticated(&cookie, &session, Method::GET, PATH)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(expired["reports"], json!([]));
    cairn_beacon::cleanup_expired(&app.pool).await.unwrap();
    let (count,): (i64,) = query_as("SELECT count(*) FROM client_performance")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    app.close().await;
}
