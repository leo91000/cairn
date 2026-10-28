use super::*;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Clone, Copy, PartialEq)]
enum Change {
    None,
    ParticipantBeforeExchange,
    ParticipantDuringExchange,
    ApprovalDuringExchange,
}

#[tokio::test]
async fn a_routed_handler_renews_its_workspace_oauth_while_still_authorized() {
    Box::pin(renew(Change::None)).await;
}

#[tokio::test]
async fn a_revoked_route_participant_cannot_send_a_claimed_resource_refresh() {
    Box::pin(renew(Change::ParticipantBeforeExchange)).await;
}

#[tokio::test]
async fn a_revoked_route_participant_cannot_publish_a_resource_refresh_result() {
    Box::pin(renew(Change::ParticipantDuringExchange)).await;
}

#[tokio::test]
async fn replacing_approval_during_resource_refresh_prevents_publication() {
    Box::pin(renew(Change::ApprovalDuringExchange)).await;
}

async fn renew(change: Change) {
    let (_root, s, instance, resource, app) =
        configured_fixture_with_resource_oauth(None, "http://runner:4311", true).await;
    let refreshes = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let renewing = refreshes.clone();
    let reading = reads.clone();
    let (exchanging, exchanged) = (entered.clone(), release.clone());
    let provider = axum::Router::new()
        .route(
            "/token",
            axum::routing::post(
                move |axum::Form(fields): axum::Form<HashMap<String, String>>| {
                    let (refreshes, entered, release) =
                        (renewing.clone(), exchanging.clone(), exchanged.clone());
                    async move {
                        if fields["grant_type"] == "refresh_token" {
                            assert_eq!(fields["refresh_token"], "route-resource-refresh-fixture");
                            refreshes.fetch_add(1, Ordering::SeqCst);
                            if matches!(
                                change,
                                Change::ParticipantDuringExchange | Change::ApprovalDuringExchange
                            ) {
                                entered.notify_one();
                                release.notified().await;
                            }
                            axum::Json(json!({
                                "access_token": "route-resource-renewed-fixture",
                                "refresh_token": "route-resource-next-fixture",
                                "expires_in": 43200,
                                "scope": "messages:read",
                                "team": {"id": "T111"}
                            }))
                        } else {
                            axum::Json(json!({
                                "access_token": "route-resource-expiring-fixture",
                                "refresh_token": "route-resource-refresh-fixture",
                                "expires_in": 1,
                                "scope": "messages:read",
                                "team": {"id": "T111"}
                            }))
                        }
                    }
                },
            ),
        )
        .route(
            "/messages",
            axum::routing::get(move |headers: axum::http::HeaderMap| {
                let reads = reading.clone();
                async move {
                    assert_eq!(
                        headers["authorization"],
                        "Bearer route-resource-renewed-fixture"
                    );
                    reads.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({"ok": true}))
                }
            }),
        );
    let (origin, provider_server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let path = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/oauth/bot"
    );
    let response = common::send(
        &app,
        owner_request("POST", &path, json!({"scopes": ["messages:read"]})),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let consent = common::read_json(response).await;
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=route-consent-fixture",
        text(&consent, "state")
    );
    assert_eq!(
        common::send(
            &app,
            common::request("GET", &callback)
                .body(Body::empty())
                .unwrap()
        )
        .await
        .status(),
        StatusCode::OK
    );
    let response = common::send(
        &app,
        owner_request(
            "POST",
            &format!("{path}/finish"),
            json!({"state": consent["state"]}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let agent = s.agent_fixture(json!({"name": "Resource refresh route", "access": {"projects": [], "installation": false}}), None).await.unwrap();
    let route = create_route(
        &app,
        &instance,
        &resource,
        json!({"name": "Support", "enabled": true,
        "match": {"channel": "C1"}, "agentId": agent["id"], "userIds": ["alice"]}),
    )
    .await;
    let connection = connections::get(&s, "router", &instance).await.unwrap();
    let package = registry::active(&s, "router").await.unwrap();
    let mut session = s
        .plugins
        .host
        .start(
            &PathBuf::from(env!("CARGO_BIN_EXE_cairn")),
            &package.manifest,
            &package.source,
        )
        .await
        .unwrap();
    assert_eq!(
        deliver(
            &app,
            &instance,
            json!({"id": "route-resource-refresh", "resource": resource,
        "author": {"id": "alice"}, "context": {"channel": "C1", "thread": "resource-refresh"},
        "body": {"text": "Read workspace", "refreshOwner": true}})
        )
        .await
        .1["status"],
        "accepted"
    );
    let slot = format!("plugin-oauth:{resource}:bot");
    let mut authority_server = None;
    if change == Change::ParticipantBeforeExchange {
        let (service, slot) = (s.clone(), slot.clone());
        let held = Arc::new(AtomicBool::new(false));
        let (entering, releasing) = (entered.clone(), release.clone());
        let authority = axum::Router::new().route(
            "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
            axum::routing::get(move || {
                let (service, slot, held, entered, release) = (service.clone(), slot.clone(), held.clone(), entering.clone(), releasing.clone());
                async move {
                    let refreshing = service.vault.get(&slot).await.unwrap().is_some_and(|v| v["refreshing"] == true);
                    if refreshing && !held.swap(true, Ordering::SeqCst) {
                        entered.notify_one();
                        release.notified().await;
                    }
                    axum::Json(json!({"owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"}, "members": []}))
                }
            }),
        );
        let (origin, server) = common::serve_locally(authority).await;
        authority_server = Some(server);
        let identity_path = s.config.data_dir.join("installation-relay/identity.json");
        let mut identity: Value =
            serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
        identity["origin"] = json!(origin);
        std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    }
    let running = s.clone();
    let mut pending = tokio::spawn(async move {
        let result = runtime::pump(&running, &connection, &mut session).await;
        session.stop().await;
        result
    });
    if change != Change::None {
        tokio::select! {
            () = entered.notified() => {},
            result = &mut pending => panic!("Handler ended before the refresh barrier: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("Refresh barrier not reached"),
        }
        if change == Change::ApprovalDuringExchange {
            registry::approve(&s, "router", "1", &package.digest)
                .await
                .unwrap();
        } else {
            let person = format!(
                "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}/people/alice",
                text(&route, "id")
            );
            assert_eq!(
                common::send(
                    &app,
                    owner_request(
                        "POST",
                        &person,
                        json!({
                            "action": "remove",
                            "revision": route["revision"]
                        })
                    )
                )
                .await
                .status(),
                StatusCode::OK
            );
        }
        release.notify_one();
    }
    pending.await.unwrap().unwrap();
    let saved = s.vault.get(&slot).await.unwrap().unwrap();
    provider_server.abort();
    if let Some(server) = authority_server {
        server.abort();
    }
    if change == Change::None {
        assert_eq!(
            refreshes.load(Ordering::SeqCst),
            1,
            "Healthy handler did not renew"
        );
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "Healthy handler did not use its new token"
        );
        assert_eq!(saved["refreshing"], false);
        assert_eq!(saved["tokens"]["access"], "route-resource-renewed-fixture");
    } else {
        assert_eq!(
            reads.load(Ordering::SeqCst),
            0,
            "Revoked source started the resource request"
        );
        assert_eq!(
            refreshes.load(Ordering::SeqCst),
            usize::from(change != Change::ParticipantBeforeExchange),
            "Revoked source sent a refresh token before its provider request"
        );
        assert_eq!(
            saved["refreshing"], true,
            "Revoked source published the renewed credential"
        );
        assert!(
            saved["tokens"]["refresh"].is_null(),
            "Consumed credential was made reusable"
        );
    }
}
