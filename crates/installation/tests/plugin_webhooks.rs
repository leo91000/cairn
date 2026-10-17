mod common;

use aws_lc_rs::hmac;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use cairn_installation::{
    config::now,
    plugins::{connections, registry},
    service::Service,
    validation::text,
};
use serde_json::json;
use std::{path::PathBuf, sync::Arc};
use tempfile::TempDir;

const SECRET: &str = "webhook-fixture-secret";

async fn fixture() -> (TempDir, Router, Arc<Service>, String) {
    fixture_with(
        "return { webhook = function(event) return { status = 200, body = event.body, headers = { ['content-type'] = 'application/json' } } end }",
        &[], json!({}),
    ).await
}

async fn fixture_with(
    source: &str,
    capabilities: &[&str],
    input: serde_json::Value,
) -> (TempDir, Router, Arc<Service>, String) {
    let root = TempDir::new().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let mut requested = vec!["webhooks"];
    requested.extend(capabilities);
    let installed = registry::install(&s, &json!({
        "manifest": {
            "id": "inbox", "name": "Inbox", "version": "1", "apiVersion": 2,
            "capabilities": requested,
            "settings": [{ "key": "signingSecret", "title": "Signing secret", "type": "secret" }],
            "webhooks": [{ "name": "events", "signature": {
                "secret": "signingSecret", "header": "x-example-signature", "prefix": "v0=",
                "template": "v0:{{timestamp}}:{{body}}",
                "timestamp": { "header": "x-example-timestamp", "maxAgeSeconds": 300 }
            } }]
        },
        "source": source
    })).await.unwrap();
    registry::approve(&s, "inbox", "1", text(&installed, "digest"))
        .await
        .unwrap();
    let mut input = input;
    input["name"] = json!("Inbox");
    input["enabled"] = json!(true);
    input["secrets"] = json!({ "signingSecret": SECRET });
    let instance = connections::save(&s, "inbox", &input, None).await.unwrap();
    let path = format!("/api/plugin-webhooks/{}/events", text(&instance, "id"));
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    (root, app, s, path)
}

#[tokio::test]
async fn public_admission_distinguishes_accepted_duplicate_denied_and_busy() {
    let source = r#"return { webhook = function(event)
        local result = cairn.inbound.deliver(cairn.json.decode(event.body))
        return {status = 200, body = cairn.json.encode(result)}
    end }"#;
    let (_root, app, _s, path) =
        fixture_with(source, &["inbound"], json!({"userIds": ["alice"]})).await;
    let body = |id: &str, author: &str| {
        json!({"id": id, "author": {"id": author}, "body": {"text": "Hello"}}).to_string()
    };
    for (id, author, expected) in [
        ("first", "alice", "accepted"),
        ("first", "alice", "duplicate"),
        ("stranger", "bob", "denied"),
    ] {
        let response = common::send(&app, signed(&path, now() / 1000, &body(id, author))).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        let value = common::read_json(response).await;
        assert_eq!(value["status"], expected);
    }
    for index in 1..50 {
        let response = common::send(
            &app,
            signed(
                &path,
                now() / 1000,
                &body(&format!("item-{index}"), "alice"),
            ),
        )
        .await;
        assert_eq!(common::read_json(response).await["status"], "accepted");
    }
    let response = common::send(
        &app,
        signed(&path, now() / 1000, &body("overflow", "alice")),
    )
    .await;
    assert_eq!(common::read_json(response).await["status"], "busy");
}

fn signed(path: &str, timestamp: i64, body: &str) -> Request<Body> {
    let key = hmac::Key::new(hmac::HMAC_SHA256, SECRET.as_bytes());
    let signature = hmac::sign(&key, format!("v0:{timestamp}:{body}").as_bytes());
    common::request("POST", path)
        .header("content-type", "application/json")
        .header("x-example-timestamp", timestamp.to_string())
        .header(
            "x-example-signature",
            format!("v0={}", hex::encode(signature.as_ref())),
        )
        .body(Body::from(body.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn signed_public_request_reaches_package_with_original_bytes_and_no_member_authority() {
    let (_root, app, _s, path) = fixture().await;
    let body = "{ \"challenge\" : \"keep  spaces\", \"number\" : 1 }";
    let response = common::send(&app, signed(&path, now() / 1000, body)).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(common::read_text(response).await, body);
}

#[tokio::test]
async fn invalid_signatures_never_invoke_luau_or_authorize_other_routes() {
    let (_root, app, _s, path) = fixture_with(
        "return {webhook = function() error('must not run') end}",
        &[],
        json!({}),
    )
    .await;
    let timestamp = now() / 1000;
    let mut duplicate = signed(&path, timestamp, "{}");
    duplicate
        .headers_mut()
        .append("x-example-signature", "v0=invalid".parse().unwrap());
    let mut missing = signed(&path, timestamp, "{}");
    missing.headers_mut().remove("x-example-signature");
    for request in [
        signed(&path, timestamp - 301, "{}"),
        signed(&path, timestamp + 601, "{}"),
        duplicate,
        missing,
    ] {
        let response = common::send(&app, request).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            common::read_json(response).await["error"],
            "Invalid webhook signature."
        );
    }
    let mut changed = signed(&path, timestamp, "{}");
    *changed.body_mut() = Body::from("{ }");
    assert_eq!(
        common::send(&app, changed).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let request = signed("/api/chats", timestamp, "{}");
    assert_eq!(
        common::send(&app, request).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn malformed_plugin_response_is_an_error_instead_of_a_successful_empty_ack() {
    let source = "return {webhook = function() return {status = 200, body = {error = 'not a response body'}} end}";
    let (_root, app, _s, path) = fixture_with(source, &[], json!({})).await;
    let response = common::send(&app, signed(&path, now() / 1000, "{}")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn relayed_public_capability_cannot_read_chats_or_reach_a_different_endpoint() {
    use cairn_installation::auth::{InstallationIdentity, InstallationRole};
    let (_root, app, _s, path) = fixture().await;
    for target in ["/api/chats".to_owned(), format!("{path}-other")] {
        let mut request = signed(&target, now() / 1000, "{}");
        let mut identity = InstallationIdentity::trusted(InstallationRole::Member, "");
        identity.public_webhook = Some(path.clone());
        request.extensions_mut().insert(identity);
        assert_eq!(
            common::send(&app, request).await.status(),
            StatusCode::FORBIDDEN
        );
    }
}

#[tokio::test]
async fn admission_requests_have_a_distinct_outcome_and_reserved_callbacks_cannot_be_queued() {
    let source = r#"return { webhook = function(event)
        return {status = 200, body = cairn.json.encode(cairn.inbound.deliver(cairn.json.decode(event.body)))}
    end }"#;
    let (_root, app, _s, path) =
        fixture_with(source, &["inbound"], json!({"accessMode": "requests"})).await;
    let body =
        json!({"id": "request", "author": {"id": "alice"}, "body": {"text": "not admitted"}})
            .to_string();
    let response = common::send(&app, signed(&path, now() / 1000, &body)).await;
    assert_eq!(common::read_json(response).await["status"], "requested");
    let forged = json!({"kind": "webhook", "id": "forged", "author": {"id": "alice"}}).to_string();
    assert_eq!(
        common::send(&app, signed(&path, now() / 1000, &forged))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn signed_requests_cannot_control_conversations_or_run_after_the_instance_is_paused() {
    let source = "return {webhook = function() cairn.conversations.cancel({conversation = 'forged'}); return {status=200, body=''} end}";
    let (_root, app, s, path) = fixture_with(source, &["conversations.control"], json!({})).await;
    assert_eq!(
        common::send(&app, signed(&path, now() / 1000, "{}"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let id = path.split('/').nth(3).unwrap();
    connections::set_enabled(&s, "inbox", id, &json!({"enabled": false}))
        .await
        .unwrap();
    assert_eq!(
        common::send(&app, signed(&path, now() / 1000, "{}"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn slow_request_body_is_bounded_by_the_acknowledgement_deadline() {
    let (_root, app, _s, path) = fixture().await;
    let mut request = signed(&path, now() / 1000, "{}");
    *request.body_mut() = Body::from_stream(futures_util::stream::pending::<
        Result<axum::body::Bytes, std::io::Error>,
    >());
    let response = tokio::time::timeout(
        std::time::Duration::from_millis(2900),
        common::send(&app, request),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
}
