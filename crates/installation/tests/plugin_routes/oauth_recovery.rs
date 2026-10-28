use crate::common::AgentFixture;

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[tokio::test]
async fn temporary_authority_failure_keeps_an_unconsumed_external_consent() {
    Box::pin(consent_recovery(false)).await;
}

#[tokio::test]
async fn a_callback_during_completion_keeps_the_original_external_consent() {
    Box::pin(consent_recovery(true)).await;
}

async fn consent_recovery(callback_during_finish: bool) {
    let (_root, s, instance, resource, app) = fixture().await;
    let (results, mut received) = tokio::sync::mpsc::unbounded_channel();
    let exchanges = Arc::new(AtomicUsize::new(0));
    let posts = exchanges.clone();
    let provider = axum::Router::new()
        .route(
            "/token",
            axum::routing::post(move || {
                let posts = posts.clone();
                async move {
                    posts.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({
                        "access_token": "recovered-external-fixture",
                        "scope": "messages:read",
                        "team": {"id": "T111"},
                        "authed_user": {"id": "U111"},
                    }))
                }
            }),
        )
        .route(
            "/oauth-result",
            axum::routing::post(move |axum::Json(value): axum::Json<Value>| {
                results.send(value).unwrap();
                async { axum::Json(json!({"ok": true})) }
            }),
        );
    let (origin, provider_server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let agent = s
        .agent_fixture(
            json!({
                "name": "Consent recovery",
                "access": {"projects": [], "installation": false},
            }),
            None,
        )
        .await
        .unwrap();
    create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Personal channel",
            "enabled": true,
            "match": {"channel": "C1"},
            "agentId": agent["id"],
            "userIds": ["U111"],
        }),
    )
    .await;
    let grant_path =
        format!("/api/plugins/router/connections/{instance}/resources/workspace/{resource}/grants");
    assert_eq!(
        common::send(
            &app,
            owner_request(
                "POST",
                &grant_path,
                json!({"agentId": agent["id"], "profile": "read"})
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    let package = registry::active(&s, "router").await.unwrap();
    let connection = connections::get(&s, "router", &instance).await.unwrap();
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
    let interaction = |action: &str| {
        json!({
            "id": cairn_installation::config::id(),
            "kind": "interaction",
            "resource": resource,
            "author": {"id": "U111"},
            "context": {"channel": "C1", "thread": "consent-recovery"},
            "body": {"oauth": action},
        })
    };
    let started = oauth_action(
        &s,
        &app,
        &connection,
        &mut session,
        interaction("start"),
        &mut received,
    )
    .await;
    let url = url::Url::parse(text(&started, "url")).unwrap();
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let callback = format!("/api/plugin-oauth/{instance}/callback?state={state}&code=fixture");
    if !callback_during_finish {
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
    }
    assert_eq!(exchanges.load(Ordering::SeqCst), 0);

    let lookups = Arc::new(AtomicUsize::new(0));
    let interrupt_once = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let (lookup_entered, lookup_resume) = (entered.clone(), resume.clone());
    let (calls, outage) = (lookups.clone(), interrupt_once.clone());
    let authority = axum::Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(move || {
            let (calls, outage) = (calls.clone(), outage.clone());
            let (entered, resume) = (lookup_entered.clone(), lookup_resume.clone());
            async move {
                let lookup = calls.fetch_add(1, Ordering::SeqCst) + 1;
                // The fourth lookup in the admitted handler belongs to consent
                // completion, before its native code claim or provider POST.
                if lookup == 4 && outage.swap(false, Ordering::SeqCst) {
                    if callback_during_finish {
                        entered.notify_one();
                        resume.notified().await;
                    } else {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            "Synthetic authority outage",
                        )
                            .into_response();
                    }
                }
                axum::Json(json!({
                    "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
                    "members": [],
                }))
                .into_response()
            }
        }),
    );
    let (origin, authority_server) = common::serve_locally(authority).await;
    let identity_path = s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: Value =
        serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
    identity["origin"] = json!(origin);
    std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    assert_eq!(
        deliver(&app, &instance, interaction("finish")).await.1["status"],
        "accepted"
    );
    lookups.store(0, Ordering::SeqCst);
    interrupt_once.store(true, Ordering::SeqCst);
    let concurrent_callback = async {
        if callback_during_finish {
            entered.notified().await;
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
            resume.notify_one();
        }
    };
    let (handled, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            runtime::pump(&s, &connection, &mut session),
            concurrent_callback,
        )
    })
    .await
    .unwrap();
    handled.unwrap();
    let initial = tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !interrupt_once.load(Ordering::SeqCst),
        "The controlled authority boundary was not exercised"
    );
    assert_eq!(
        exchanges.load(Ordering::SeqCst),
        0,
        "No code may reach the provider before completion"
    );
    let expected = if callback_during_finish {
        "pending"
    } else {
        "refused"
    };
    assert_eq!(initial["status"], expected, "{initial}");

    let finished = oauth_action(
        &s,
        &app,
        &connection,
        &mut session,
        interaction("finish"),
        &mut received,
    )
    .await;
    assert_eq!(
        finished["status"], "authorized",
        "A transient pre-exchange outage abandoned the original consent: {finished}"
    );
    assert_eq!(finished["subject"], "U111");
    assert_eq!(
        exchanges.load(Ordering::SeqCst),
        1,
        "The original code must be exchanged once"
    );
    assert!(!finished.to_string().contains("recovered-external-fixture"));
    session.stop().await;
    authority_server.abort();
    provider_server.abort();
}
