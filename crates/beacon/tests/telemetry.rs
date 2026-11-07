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

fn web_report() -> Value {
    json!({
        "schema": 2,
        "platform": "web",
        "buildId": "a".repeat(64),
        "buildType": "release",
        "browser": "FIREFOX",
        "connection": "NONE",
        "measurements": {
            "windowId": Uuid::new_v4().to_string(),
            "sequence": 4,
            "elapsedMs": 10000,
            "foregroundMs": 8000,
            "operations": [{ "operation": "API_READ", "screen": "CHAT", "count": 2, "startedOnMainThread": 2, "meanMs": 100.0, "maxMs": 180.0, "p95UpperBoundMs": 180.0 }],
            "apiFailures": [{ "operation": "API_READ", "screen": "CHAT", "errors": 1, "cancelled": 0 }],
            "inFlight": [{ "operation": "API_WRITE", "screen": "MISSIONS", "count": 1, "maxElapsedMs": 2000.0 }],
            "omittedInFlight": 0,
            "eventLoop": [{ "screen": "CHAT", "reason": "API", "count": 2, "late": 1, "meanDelayMs": 50.0, "maxDelayMs": 80.0, "p95UpperBoundDelayMs": 80.0 }],
            "longTaskSupported": true,
            "longTasks": [{ "screen": "CHAT", "count": 1, "totalMs": 80.0, "maxMs": 80.0 }],
            "recentSlowSamples": [{ "elapsedMs": 9000, "screen": "CHAT", "kind": "LONG_TASK", "durationMs": 80.0, "outcome": "ok" }]
        }
    })
}

#[tokio::test]
async fn web_and_android_reports_share_private_account_storage_and_limits() {
    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "web-performance@example.test").await;
    let mut web = web_report();
    let window = web["measurements"]["windowId"].clone();
    for sequence in [4, 3, 5] {
        web["measurements"]["sequence"] = json!(sequence);
        let response = app
            .authenticated(&cookie, &session, Method::POST, PATH)
            .json(&web)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    for _ in 0..3 {
        let response = app
            .authenticated(&cookie, &session, Method::POST, PATH)
            .json(&report())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let limited = app
        .authenticated(&cookie, &session, Method::POST, PATH)
        .json(&web)
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    let response = app
        .authenticated(&cookie, &session, Method::GET, PATH)
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let stored: Value = response.json().await.unwrap();
    let rows = stored["reports"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    let saved = rows.iter().find(|row| row["platform"] == "web").unwrap();
    assert_eq!(saved["measurements"]["windowId"], window);
    assert_eq!(saved["measurements"]["sequence"], 5);
    let (other_cookie, other_session) = login(&app, "web-other@example.test").await;
    let other: Value = app
        .authenticated(&other_cookie, &other_session, Method::GET, PATH)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(other["reports"], json!([]));
    app.close().await;
}

#[tokio::test]
async fn report_receipt_time_advances_only_with_new_web_or_android_measurements() {
    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "receipt-time@example.test").await;
    for mut data in [report(), web_report()] {
        let response = app
            .authenticated(&cookie, &session, Method::POST, PATH)
            .json(&data)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let window = data["measurements"]["windowId"].as_str().unwrap();
        let (received_at,): (i64,) = query_as(
            "UPDATE client_performance
             SET updated_at = date_trunc('second',now()) - interval '1 day' + interval '123 milliseconds'
             WHERE account_id = $1 AND window_id = $2
             RETURNING floor(extract(epoch FROM updated_at) * 1000)::bigint",
        )
        .bind(session["account"]["id"].as_str().unwrap())
        .bind(window)
        .fetch_one(&app.pool)
        .await
        .unwrap();

        let duplicate = app
            .authenticated(&cookie, &session, Method::POST, PATH)
            .json(&data)
            .send()
            .await
            .unwrap();
        assert_eq!(duplicate.status(), StatusCode::NO_CONTENT);
        let stored: Value = app
            .authenticated(&cookie, &session, Method::GET, PATH)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let mut saved = stored["reports"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["measurements"]["windowId"] == window)
            .unwrap()
            .clone();
        assert_eq!(
            saved.as_object_mut().unwrap().remove("receivedAt"),
            Some(json!(received_at)),
            "Report reads must expose the server receipt time without refreshing duplicates"
        );
        assert_eq!(saved, data);

        data["measurements"]["sequence"] =
            json!(data["measurements"]["sequence"].as_i64().unwrap() + 1);
        let newer = app
            .authenticated(&cookie, &session, Method::POST, PATH)
            .json(&data)
            .send()
            .await
            .unwrap();
        assert_eq!(newer.status(), StatusCode::NO_CONTENT);
        let stored: Value = app
            .authenticated(&cookie, &session, Method::GET, PATH)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let saved = stored["reports"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["measurements"]["windowId"] == data["measurements"]["windowId"])
            .unwrap();
        assert!(saved["receivedAt"].as_i64().unwrap() > received_at);
        assert_eq!(saved["measurements"], data["measurements"]);
    }
    app.close().await;
}

#[tokio::test]
async fn web_schema_rejects_unbounded_labels_content_and_inconsistent_aggregates() {
    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "web-schema@example.test").await;
    for (path, value) in [
        ("/url", json!("https://private.test/chat")),
        ("/buildId", json!("private-label")),
        ("/browser", json!("private-user-agent")),
        ("/schema", json!(1)),
        ("/platform", json!("android")),
        ("/measurements/foregroundMs", json!(10001)),
        ("/measurements/operations/0/name", json!("private payload")),
        ("/measurements/operations/0/p95UpperBoundMs", json!(181)),
        ("/measurements/apiFailures/0/errors", json!(3)),
        ("/measurements/inFlight/0/count", json!(65)),
        ("/measurements/eventLoop/0/reason", json!("private route")),
        ("/measurements/eventLoop/0/late", json!(3)),
        ("/measurements/longTaskSupported", json!(false)),
        ("/measurements/longTasks/0/totalMs", json!(79)),
        (
            "/measurements/recentSlowSamples/0/outcome",
            json!("private error"),
        ),
    ] {
        let mut input = web_report();
        let (parent, key) = path.rsplit_once('/').unwrap();
        input.pointer_mut(parent).unwrap()[key] = value;
        let response = app
            .authenticated(&cookie, &session, Method::POST, PATH)
            .json(&input)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error(), "accepted {path}");
    }
    app.close().await;
}

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
