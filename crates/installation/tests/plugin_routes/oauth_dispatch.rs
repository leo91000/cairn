//! A personal OAuth mutation retains the issuing handler's package approval.
use super::*;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn an_authorized_external_handler_disconnects_its_personal_authorization() {
    Box::pin(mutation_approval("disconnect", false)).await;
}

#[tokio::test]
async fn a_retired_external_handler_cannot_disconnect_its_personal_authorization() {
    Box::pin(mutation_approval("disconnect", true)).await;
}

#[tokio::test]
async fn an_authorized_external_handler_starts_personal_consent() {
    Box::pin(mutation_approval("start", false)).await;
}

#[tokio::test]
async fn a_retired_external_handler_cannot_start_personal_consent() {
    Box::pin(mutation_approval("start", true)).await;
}

async fn mutation_approval(action: &str, reapprove: bool) {
    let (_root, s, instance, resource, app) = fixture().await;
    let (results, mut received) = tokio::sync::mpsc::unbounded_channel();
    let provider = axum::Router::new()
        .route(
            "/token",
            axum::routing::post(|| async {
                axum::Json(json!({
                    "access_token": "external-approval-fixture",
                    "scope": "messages:read",
                    "team": {"id": "T111"},
                    "authed_user": {"id": "U111"},
                }))
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
                "name": "Personal authorization approval",
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
            "name": "Personal channel", "enabled": true,
            "match": {"channel": "C1"}, "agentId": agent["id"], "userIds": ["U111"],
        }),
    )
    .await;
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
            "id": cairn_installation::config::id(), "kind": "interaction", "resource": resource,
            "author": {"id": "U111"}, "context": {"channel": "C1", "thread": "approval-consent"},
            "body": {"oauth": action},
        })
    };
    let before = if action == "disconnect" {
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
        assert_eq!(
            common::send(
                &app,
                common::request(
                    "GET",
                    &format!("/api/plugin-oauth/{instance}/callback?state={state}&code=fixture")
                )
                .body(Body::empty())
                .unwrap()
            )
            .await
            .status(),
            StatusCode::OK
        );
        let finished = oauth_action(
            &s,
            &app,
            &connection,
            &mut session,
            interaction("finish"),
            &mut received,
        )
        .await;
        assert_eq!(finished["status"], "authorized");
        let before = oauth_action(
            &s,
            &app,
            &connection,
            &mut session,
            interaction("status"),
            &mut received,
        )
        .await;
        assert_eq!(before["status"], "authorized");
        assert_eq!(before["subject"], "U111");
        before
    } else {
        Value::Null
    };

    let gate = common::relay_fixture::owner_lookups(&s).await;
    assert_eq!(
        deliver(&app, &instance, interaction(action)).await.1["status"],
        "accepted"
    );
    gate.calls.store(0, Ordering::SeqCst);
    // dispatch.bound and capability.ensure_owner precede oauth.call's own
    // external-person lookup. The third is inside the capability, after its
    // original loaded approval check and before the OAuth mutation transaction.
    gate.hold_at.store(3, Ordering::SeqCst);
    let (running, routed) = (s.clone(), connection.clone());
    let mut pending = tokio::spawn(async move {
        let result = runtime::pump(&running, &routed, &mut session).await;
        (session, result)
    });
    tokio::select! {
        () = gate.entered.notified() => {},
        result = &mut pending => {
            let ended = result.map(|(_, result)| result);
            panic!("Handler ended before OAuth authority barrier: {ended:?}");
        },
        () = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("OAuth authority barrier not reached"),
    }
    if reapprove {
        registry::approve(&s, "router", "1", &package.digest)
            .await
            .unwrap();
    }
    gate.release.notify_one();
    let (mut session, handled) = pending.await.unwrap();
    handled.unwrap();
    gate.hold_at.store(usize::MAX, Ordering::SeqCst);
    if reapprove {
        assert!(
            received.try_recv().is_err(),
            "Retired handler published a result"
        );
    } else {
        let result = received
            .try_recv()
            .expect("Authorized OAuth mutation result");
        if action == "start" {
            assert!(result["url"].is_string(), "{result}");
        } else {
            assert_eq!(result["status"], "disconnected");
        }
    }
    // A fresh admitted handler observes the effect. A refusal from the old
    // handler alone cannot prove that it did not already delete credentials.
    let after = oauth_action(
        &s,
        &app,
        &connection,
        &mut session,
        interaction(if action == "start" {
            "finish"
        } else {
            "status"
        }),
        &mut received,
    )
    .await;
    let expected = match (action, reapprove) {
        ("start", false) => "pending",
        ("disconnect", true) => "authorized",
        _ => "disconnected",
    };
    assert_eq!(
        after["status"], expected,
        "An obsolete dispatch committed {action} before rejecting its result: {after}"
    );
    if reapprove && action == "disconnect" {
        assert_eq!(after, before);
    }
    session.stop().await;
    provider_server.abort();
    s.shutdown.cancel();
}
