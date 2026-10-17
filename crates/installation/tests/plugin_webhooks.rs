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

#[tokio::test]
async fn setup_drafts_offer_a_stable_signed_challenge_without_admitting_work() {
    use axum::response::IntoResponse;
    use cairn_installation::auth::{InstallationIdentity, InstallationRole};
    let (_root, app, s, original_path) =
        fixture_with("return {}", &["storage", "inbound"], json!({})).await;
    let original = registry::active(&s, "inbox").await.unwrap();
    let mut manifest = serde_json::to_value(&original.manifest).unwrap();
    manifest["version"] = json!("2");
    manifest["settings"].as_array_mut().unwrap().push(json!({
        "key": "clientId",
        "title": "Client ID",
        "type": "string",
        "required": true,
    }));
    manifest["webhooks"][0]["setup"] = json!({ "match": { "/type": "url_verification" } });
    let source = r#"return { webhook = function(event)
        local body = cairn.json.decode(event.body)
        local allowed = pcall(function() cairn.storage.set('unexpected', 'work') end)
        if allowed then return {status = 500, body = 'Setup acquired operational capabilities'} end
        return {status = 200, body = cairn.json.encode({challenge = body.challenge}),
            headers = {['content-type'] = 'application/json'}}
    end }"#;
    let package = registry::install(
        &s,
        &json!({
            "manifest": manifest,
            "source": source,
        }),
    )
    .await
    .unwrap();
    registry::approve(&s, "inbox", "2", text(&package, "digest"))
        .await
        .unwrap();
    let owner_request = |method: &str, path: &str, body: serde_json::Value| {
        let mut request = common::request(method, path)
            .header("content-type", "application/json")
            .body(axum::Json(body).into_response().into_body())
            .unwrap();
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(
                InstallationRole::Owner,
                "fixture-owner",
            ));
        request
    };
    let collection = "/api/plugins/inbox/setup-drafts";
    let response = common::send(
        &app,
        owner_request(
            "POST",
            collection,
            json!({
                "name": "Incomplete setup",
                "settings": {},
                "secrets": {"signingSecret": SECRET},
            }),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let draft = common::read_json(response).await;
    assert_eq!(draft["status"], "setup");
    assert!(draft["expiresAt"].as_i64().unwrap() > now());
    assert!(
        !draft.to_string().contains(SECRET),
        "setup response disclosed a secret"
    );
    let id = text(&draft, "id");
    let path = format!("/api/plugin-webhooks/{id}/events");
    let resource = format!("{collection}/{id}");
    let mut member = owner_request("GET", &resource, json!({}));
    member
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Member,
            "alice",
        ));
    assert_eq!(
        common::send(&app, member).await.status(),
        StatusCode::FORBIDDEN
    );
    let mut another_account = owner_request("GET", &resource, json!({}));
    another_account
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "other-owner",
        ));
    assert_eq!(
        common::send(&app, another_account).await.status(),
        StatusCode::NOT_FOUND
    );
    let response = common::send(&app, owner_request("GET", collection, json!({}))).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "setup could not be resumed from its owner list"
    );
    let listing = common::read_json(response).await;
    assert_eq!(listing["items"].as_array().unwrap().len(), 1);
    assert_eq!(listing["items"][0]["id"], id);
    assert!(!listing.to_string().contains(SECRET));
    let response = common::send(
        &app,
        owner_request(
            "PATCH",
            &resource,
            json!({
                "revision": draft["revision"],
                "name": "Configured app",
                "settings": {"clientId": "app-fixture"},
            }),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let updated = common::read_json(response).await;
    assert_eq!(updated["id"], id, "editing setup changed its callback URL");
    assert_ne!(updated["revision"], draft["revision"]);
    assert_eq!(updated["secretKeys"], json!(["signingSecret"]));
    let response = common::send(
        &app,
        owner_request(
            "PATCH",
            &resource,
            json!({
                "revision": draft["revision"],
                "name": "Outdated edit",
                "settings": {},
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let policy = Arc::new(std::sync::Mutex::new(json!({
        "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
        "members": [],
    })));
    let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let pause = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pause_at = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let pause_ref = pause.clone();
    let entered_ref = entered.clone();
    let release_ref = release.clone();
    let policy_ref = policy.clone();
    let checks_ref = checks.clone();
    let pause_at_ref = pause_at.clone();
    let authority = Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(move || {
            let ordinal = checks_ref.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let policy = policy_ref.lock().unwrap().clone();
            let paused = pause_ref.swap(false, std::sync::atomic::Ordering::SeqCst)
                || pause_at_ref.load(std::sync::atomic::Ordering::SeqCst) == ordinal;
            let (entered, release) = (entered_ref.clone(), release_ref.clone());
            async move {
                if paused {
                    entered.notify_one();
                    release.notified().await;
                }
                axum::Json(policy)
            }
        }),
    );
    let (origin, authority_server) = common::serve_locally(authority).await;
    let identity_path = s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
    identity["origin"] = json!(origin);
    std::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    let challenge = json!({"type": "url_verification", "challenge": "setup-proof"}).to_string();
    let mut invalid = signed(&path, now() / 1000, &challenge);
    invalid.headers_mut().insert(
        "x-example-signature",
        "v0=0000000000000000000000000000000000000000000000000000000000000000"
            .parse()
            .unwrap(),
    );
    assert_eq!(
        common::send(&app, invalid).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        checks.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "unauthenticated setup traffic reached the owner authority"
    );
    let response = common::send(&app, signed(&path, now() / 1000, &challenge)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    assert_eq!(
        common::read_json(response).await,
        json!({"challenge": "setup-proof"})
    );
    let operational = json!({
        "type": "event_callback",
        "id": "work",
        "author": {"id": "alice"},
        "body": {"text": "Start work"},
    })
    .to_string();
    let response = common::send(&app, signed(&path, now() / 1000, &operational)).await;
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "setup acknowledged operational work"
    );
    assert!(s.chat_list().await.unwrap().is_empty());
    let response = common::send(&app, owner_request("GET", &resource, json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        common::read_json(response).await["settings"]["clientId"],
        "app-fixture"
    );
    let response = common::send(
        &app,
        owner_request(
            "DELETE",
            &resource,
            json!({"revision": updated["revision"]}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = common::send(&app, signed(&path, now() / 1000, &challenge)).await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "deleted setup retained a public endpoint"
    );
    let create = |name: &str| {
        owner_request(
            "POST",
            collection,
            json!({
                "name": name,
                "settings": {},
                "secrets": {"signingSecret": SECRET},
            }),
        )
    };
    let response = common::send(&app, create("Expired setup")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let expired = common::read_json(response).await;
    let expired_id = text(&expired, "id");
    // Prepared expired storage, avoiding a one-day wall-clock delay. Observable
    // assertions stay at the owner and signed public HTTP boundaries.
    for key in [
        format!("plugin-setup:{expired_id}"),
        format!("mcp-secret:plugin-secrets:{expired_id}"),
    ] {
        let value = s.store.kv(&key).await.unwrap().unwrap();
        s.store.set(&key, value, Some(now() - 1)).await.unwrap();
    }
    assert_eq!(
        common::send(
            &app,
            owner_request("GET", &format!("{collection}/{expired_id}"), json!({}))
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        common::send(
            &app,
            signed(
                &format!("/api/plugin-webhooks/{expired_id}/events"),
                now() / 1000,
                &challenge
            )
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let mut first = serde_json::Value::Null;
    for index in 0..32 {
        let response = common::send(&app, create(&format!("Old setup {index}"))).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        let value = common::read_json(response).await;
        if index == 0 {
            first = value;
        }
    }
    assert_eq!(
        common::send(&app, create("Over capacity")).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    policy.lock().unwrap()["owner"]["access_id"] = json!("replacement-owner-access");
    let old_id = text(&first, "id");
    assert_eq!(
        common::send(
            &app,
            signed(
                &format!("/api/plugin-webhooks/{old_id}/events"),
                now() / 1000,
                &challenge
            )
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        common::send(
            &app,
            owner_request("GET", &format!("{collection}/{old_id}"), json!({}))
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let response = common::send(&app, owner_request("GET", collection, json!({}))).await;
    assert_eq!(common::read_json(response).await["items"], json!([]));
    let response = common::send(&app, create("Replacement owner's setup")).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "revoked drafts exhausted the new owner's capacity"
    );
    let replacement = common::read_json(response).await;
    let response = common::send(&app, owner_request("GET", collection, json!({}))).await;
    let listed = common::read_json(response).await;
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);
    assert_eq!(listed["items"][0]["id"], replacement["id"]);
    registry::activate(&s, "inbox", "1").await.unwrap();
    let obsolete_id = text(&replacement, "id");
    let obsolete_path = format!("{collection}/{obsolete_id}");
    assert_eq!(
        common::send(
            &app,
            signed(
                &format!("/api/plugin-webhooks/{obsolete_id}/events"),
                now() / 1000,
                &challenge
            )
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let response = common::send(&app, owner_request("GET", &obsolete_path, json!({}))).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "owner could not discard setup after the package changed"
    );
    assert_eq!(common::read_json(response).await["status"], "review_needed");
    let response = common::send(&app, owner_request("GET", collection, json!({}))).await;
    let obsolete = common::read_json(response).await;
    assert_eq!(obsolete["items"][0]["id"], obsolete_id);
    assert_eq!(obsolete["items"][0]["status"], "review_needed");
    assert_eq!(
        common::send(
            &app,
            owner_request(
                "PATCH",
                &obsolete_path,
                json!({
                    "name": "Cannot reuse old setup",
                    "revision": replacement["revision"],
                    "settings": {},
                })
            )
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        common::send(
            &app,
            owner_request(
                "DELETE",
                &obsolete_path,
                json!({
                    "revision": replacement["revision"],
                })
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    let response = common::send(&app, create("Setup before uninstall")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let removed_setup = common::read_json(response).await;
    let original_instance = original_path.split('/').nth(3).unwrap();
    connections::remove(&s, "inbox", original_instance)
        .await
        .unwrap();
    registry::uninstall(&s, "inbox").await.unwrap();
    let reinstalled = registry::install(
        &s,
        &json!({
            "manifest": original.manifest,
            "source": original.source.main,
        }),
    )
    .await
    .unwrap();
    registry::approve(&s, "inbox", "1", text(&reinstalled, "digest"))
        .await
        .unwrap();
    let old_setup_path = format!("{collection}/{}", text(&removed_setup, "id"));
    let response = common::send(&app, owner_request("GET", &old_setup_path, json!({}))).await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "reinstall revived an uninstalled setup"
    );
    let response = common::send(&app, owner_request("GET", collection, json!({}))).await;
    assert_eq!(common::read_json(response).await["items"], json!([]));
    pause.store(true, std::sync::atomic::Ordering::SeqCst);
    let request = create("Creation spanning an uninstall");
    let pending_app = app.clone();
    let pending = tokio::spawn(async move { common::send(&pending_app, request).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    registry::uninstall(&s, "inbox").await.unwrap();
    let installed_again = registry::install(
        &s,
        &json!({
            "manifest": original.manifest,
            "source": original.source.main,
        }),
    )
    .await
    .unwrap();
    registry::approve(&s, "inbox", "1", text(&installed_again, "digest"))
        .await
        .unwrap();
    release.notify_one();
    let response = pending.await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "in-flight creation survived an uninstall/reinstall"
    );
    let response = common::send(&app, owner_request("GET", collection, json!({}))).await;
    assert_eq!(common::read_json(response).await["items"], json!([]));

    let setup_package = registry::install(
        &s,
        &json!({
            "manifest": manifest,
            "source": source,
        }),
    )
    .await
    .unwrap();
    registry::approve(&s, "inbox", "2", text(&setup_package, "digest"))
        .await
        .unwrap();

    // Hold the former owner's authority response while a replacement owner saves
    // their own setup. The delayed request cannot retire those newer records.
    pause.store(true, std::sync::atomic::Ordering::SeqCst);
    let request = create("Former owner creation");
    let pending_app = app.clone();
    let former_owner = tokio::spawn(async move { common::send(&pending_app, request).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    policy.lock().unwrap()["owner"]["access_id"] = json!("latest-owner-access");
    let response = common::send(&app, create("Current owner's setup")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let current_setup = common::read_json(response).await;
    release.notify_one();
    let response = former_owner.await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "a former owner's save survived a newer owner's write"
    );
    let current_path = format!("{collection}/{}", text(&current_setup, "id"));
    let response = common::send(&app, owner_request("GET", &current_path, json!({}))).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a former owner's cleanup erased the current owner's setup"
    );
    assert_eq!(
        common::read_json(response).await["secretKeys"],
        json!(["signingSecret"])
    );
    let current_webhook = format!("/api/plugin-webhooks/{}/events", text(&current_setup, "id"));
    let response = common::send(&app, signed(&current_webhook, now() / 1000, &challenge)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a former owner's cleanup erased the current owner's signing secret"
    );
    assert_eq!(
        common::read_json(response).await,
        json!({"challenge": "setup-proof"})
    );
    // Suspend the final post-handler authority check, then invalidate setup.
    for change in ["edit", "delete", "expire"] {
        let response = common::send(&app, create("Final challenge check")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let draft = common::read_json(response).await;
        let draft_id = text(&draft, "id");
        let owner_path = format!("{collection}/{draft_id}");
        let public_path = format!("/api/plugin-webhooks/{draft_id}/events");
        pause_at.store(
            checks.load(std::sync::atomic::Ordering::SeqCst) + 2,
            std::sync::atomic::Ordering::SeqCst,
        );
        let request = signed(&public_path, now() / 1000, &challenge);
        let pending_app = app.clone();
        let pending_challenge =
            tokio::spawn(async move { common::send(&pending_app, request).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        match change {
            "edit" | "delete" => {
                let request = owner_request(
                    if change == "edit" { "PATCH" } else { "DELETE" },
                    &owner_path,
                    json!({
                        "revision": draft["revision"],
                        "name": "Changed before challenge completes",
                        "settings": {},
                    }),
                );
                assert_eq!(common::send(&app, request).await.status(), StatusCode::OK);
            }
            "expire" => {
                for key in [
                    format!("plugin-setup:{draft_id}"),
                    format!("mcp-secret:plugin-secrets:{draft_id}"),
                ] {
                    let value = s.store.kv(&key).await.unwrap().unwrap();
                    s.store.set(&key, value, Some(now() - 1)).await.unwrap();
                }
            }
            _ => unreachable!(),
        }
        release.notify_one();
        let response = pending_challenge.await.unwrap();
        assert!(
            matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::CONFLICT | StatusCode::FORBIDDEN
            ),
            "{change} during final owner check allowed challenge: {}",
            response.status()
        );
    }
    authority_server.abort();
}

#[tokio::test]
async fn owner_setup_forms_use_saved_secrets_and_stable_draft_identity_without_activating() {
    use cairn_installation::auth::{InstallationIdentity, InstallationRole};
    let (_root, app, s, _) = fixture_with("return {}", &[], json!({})).await;
    let original = registry::active(&s, "inbox").await.unwrap();
    let mut manifest = serde_json::to_value(&original.manifest).unwrap();
    manifest["version"] = json!("2");
    manifest["capabilities"] = json!(["webhooks", "http", "storage"]);
    manifest["hosts"] = json!(["api.example.test"]);
    manifest["instanceForm"] = json!(true);
    manifest["credential"] = json!({"label": "App token", "header": "x-app-key"});
    manifest["settings"].as_array_mut().unwrap().extend([
        json!({"key": "label", "title": "Label", "type": "string"}),
        json!({"key": "workspaceId", "title": "Workspace", "type": "string", "hidden": true}),
        json!({"key": "callbackId", "title": "Callback", "type": "string", "hidden": true}),
        json!({"key": "callbackUrl", "title": "URL", "type": "url", "hidden": true}),
        json!({"key": "oauthCallback", "title": "OAuth URL", "type": "url", "hidden": true}),
    ]);
    let installed = registry::install(&s, &json!({
        "manifest": manifest,
        "source": r#"return { form = function(event)
            assert(event.setup.revision ~= nil and event.setup.expiresAt > 0)
            assert(event.credential == true)
            local token_probe = cairn.http.request({url = 'https://api.example.test/credential', auth = true})
            assert(token_probe.status == 200)
            local allowed = pcall(function() cairn.storage.set('unexpected', 'work') end)
            if allowed then return {errors = {form = 'Setup acquired storage'}} end
            local response = cairn.http.request({
                url = 'https://api.example.test/probe',
                auth = {secret = 'signingSecret', prefix = 'Bearer '},
            })
            assert(response.status == 200)
            local workspace = response.json
            if event.values.label == "Held result" then cairn.fail("Late provider result " .. workspace.id) end
            return {
                values = {
                    workspaceId = workspace.id, callbackId = event.instance,
                    callbackUrl = event.setup.urls.webhooks.events,
                    oauthCallback = event.setup.urls.oauthCallback,
                },
                view = {{type = 'text', text = event.values.label}},
            }
        end }"#,
    })).await.unwrap();
    registry::approve(&s, "inbox", "2", text(&installed, "digest"))
        .await
        .unwrap();
    let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (held, read_entered, read_release) = (hold.clone(), entered.clone(), release.clone());
    let provider = Router::new().route(
        "/probe",
        axum::routing::get(move |headers: axum::http::HeaderMap| {
            let (held, entered, release) =
                (held.clone(), read_entered.clone(), read_release.clone());
            async move {
                assert_eq!(
                    headers.get("authorization").unwrap(),
                    "Bearer webhook-fixture-secret"
                );
                if held.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    entered.notify_one();
                    release.notified().await;
                }
                axum::Json(json!({"id": "T-fixture"}))
            }
        }),
    );
    let provider = provider.route(
        "/credential",
        axum::routing::get(|headers: axum::http::HeaderMap| async move {
            assert_eq!(headers.get("x-app-key").unwrap(), "app-token-fixture");
            StatusCode::OK
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let request = |method: &str, path: &str, body: serde_json::Value| {
        let mut request = common::request(method, path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(
                InstallationRole::Owner,
                "fixture-owner",
            ));
        request
    };
    let collection = "/api/plugins/inbox/setup-drafts";
    let response = common::send(
        &app,
        request(
            "POST",
            collection,
            json!({
                "name": "Workspace setup",
                "token": "app-token-fixture",
                "settings": {"label": "Saved label"},
                "secrets": {"signingSecret": SECRET},
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let draft = common::read_json(response).await;
    assert_eq!(draft["credential"], true);
    assert!(!draft.to_string().contains("app-token-fixture"));
    let item = format!("{collection}/{}", text(&draft, "id"));
    let saved = common::send(
        &app,
        request(
            "PATCH",
            &item,
            json!({
                "revision": draft["revision"],
                "name": "Workspace setup",
                "settings": {"label": "Saved label"},
            }),
        ),
    )
    .await;
    assert_eq!(saved.status(), StatusCode::OK);
    let updated = common::read_json(saved).await;
    assert_eq!(updated["id"], draft["id"]);
    assert_eq!(updated["expiresAt"], draft["expiresAt"]);
    assert_eq!(updated["credential"], true);
    let draft = updated;
    let form = format!("{item}/form");
    let input = json!({"revision": draft["revision"], "settings": {"label": "Typed label"}});
    let response = common::send(&app, request("POST", &form, input.clone())).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let result = common::read_json(response).await;
    assert_eq!(result["values"]["workspaceId"], "T-fixture");
    assert_eq!(result["values"]["callbackId"], draft["id"]);
    assert_eq!(result["view"][0]["text"], "Typed label");
    assert!(text(&result["values"], "callbackUrl").ends_with(&format!(
        "/api/public/installations/00000000-0000-4000-8000-000000000055/plugins/{}/events",
        text(&draft, "id")
    )));
    assert!(text(&result["values"], "oauthCallback").ends_with(&format!(
        "/api/public/installations/00000000-0000-4000-8000-000000000055/plugin-oauth/{}/callback",
        text(&draft, "id")
    )));
    let direct = common::send(
        &app,
        request(
            "POST",
            &form,
            json!({
                "revision": draft["revision"],
                "directOrigin": "https://cairn.example.test/",
            }),
        ),
    )
    .await;
    assert_eq!(direct.status(), StatusCode::OK);
    let direct = common::read_json(direct).await;
    assert_eq!(
        direct["values"]["callbackUrl"],
        format!(
            "https://cairn.example.test/api/plugin-webhooks/{}/events",
            text(&draft, "id")
        )
    );
    assert_eq!(
        direct["values"]["oauthCallback"],
        format!(
            "https://cairn.example.test/api/plugin-oauth/{}/callback",
            text(&draft, "id")
        )
    );
    for origin in [
        "http://cairn.example.test",
        "https://cairn.example.test/path",
        "https://name:password@cairn.example.test",
        "https://cairn.example.test/?query=1",
    ] {
        let invalid = common::send(
            &app,
            request(
                "POST",
                &form,
                json!({
                    "revision": draft["revision"],
                    "directOrigin": origin,
                }),
            ),
        )
        .await;
        assert_eq!(
            invalid.status(),
            StatusCode::BAD_REQUEST,
            "invalid callback origin accepted: {origin}"
        );
    }
    assert!(
        !result.to_string().contains(SECRET),
        "form exposed a saved secret"
    );
    let response = common::send(&app, request("GET", &item, json!({}))).await;
    let unchanged = common::read_json(response).await;
    assert_eq!(unchanged["revision"], draft["revision"]);
    assert_eq!(unchanged["settings"], json!({"label": "Saved label"}));
    let mut member = request("POST", &form, input);
    member
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Member,
            "alice",
        ));
    assert_eq!(
        common::send(&app, member).await.status(),
        StatusCode::FORBIDDEN
    );
    assert!(s.chat_list().await.unwrap().is_empty());
    hold.store(true, std::sync::atomic::Ordering::SeqCst);
    let pending_app = app.clone();
    let form_request = request(
        "POST",
        &form,
        json!({
            "revision": draft["revision"],
            "settings": {"label": "Held result"},
        }),
    );
    let pending = tokio::spawn(async move { common::send(&pending_app, form_request).await });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let deleted = common::send(
        &app,
        request("DELETE", &item, json!({"revision": draft["revision"]})),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::OK);
    release.notify_one();
    let response = pending.await.unwrap();
    let status = response.status();
    let body = common::read_text(response).await;
    assert!(
        !body.contains("Late provider result"),
        "deleted setup delivered its held HTTP result to Luau: {body}"
    );
    assert!(matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::FORBIDDEN | StatusCode::CONFLICT
    ));
    server.abort();
}
