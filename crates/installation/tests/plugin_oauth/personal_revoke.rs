use super::*;

#[derive(Clone, Copy)]
enum Change {
    None,
    Approval,
    Membership,
    Resource,
}

#[tokio::test]
async fn a_personal_disconnect_with_unchanged_authority_removes_its_authorization() {
    Box::pin(delayed_request(Change::None, "DELETE")).await;
}

#[tokio::test]
async fn a_personal_disconnect_cannot_use_a_replacement_package_approval() {
    Box::pin(delayed_request(Change::Approval, "DELETE")).await;
}

#[tokio::test]
async fn an_old_personal_disconnect_cannot_remove_a_new_memberships_consent() {
    Box::pin(delayed_request(Change::Membership, "DELETE")).await;
}

#[tokio::test]
async fn a_personal_status_with_unchanged_authority_returns_its_authorization() {
    Box::pin(delayed_request(Change::None, "GET")).await;
}

#[tokio::test]
async fn a_personal_status_cannot_publish_under_a_replacement_approval() {
    Box::pin(delayed_request(Change::Approval, "GET")).await;
}

#[tokio::test]
async fn a_personal_status_cannot_publish_a_retired_memberships_consent() {
    Box::pin(delayed_request(Change::Membership, "GET")).await;
}

#[tokio::test]
async fn a_personal_status_cannot_publish_a_removed_resources_consent() {
    Box::pin(delayed_request(Change::Resource, "GET")).await;
}

async fn connect(app: &axum::Router, path: &str, instance: &str) {
    let started = common::send(
        app,
        member_request("POST", path, json!({"scopes": ["messages:read"]}), "alice"),
    )
    .await;
    assert_eq!(
        started.status(),
        StatusCode::OK,
        "{}",
        common::read_text(started).await
    );
    let started = common::read_json(started).await;
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
        text(&started, "state")
    );
    assert_eq!(
        common::send(app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    let finished = common::send(
        app,
        member_request(
            "POST",
            &format!("{path}/finish"),
            json!({"state": started["state"]}),
            "alice",
        ),
    )
    .await;
    assert_eq!(
        finished.status(),
        StatusCode::OK,
        "{}",
        common::read_text(finished).await
    );
    assert_eq!(common::read_json(finished).await["status"], "authorized");
}

async fn status(app: &axum::Router, path: &str) -> Value {
    let response = common::send(app, member_request("GET", path, Value::Null, "alice")).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    common::read_json(response).await
}

async fn delayed_request(change: Change, method: &'static str) {
    let (_root, s, instance, _resource, path, app) = configured_fixture(false, true).await;
    let policy = Arc::new(std::sync::Mutex::new(json!({
        "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
        "members": [{"account_id": "alice", "access_id": "alice-membership-1"}],
    })));
    let calls = Arc::new(AtomicUsize::new(0));
    let hold_at = Arc::new(AtomicUsize::new(usize::MAX));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (current, counting, holding, entering, releasing) = (
        policy.clone(),
        calls.clone(),
        hold_at.clone(),
        entered.clone(),
        release.clone(),
    );
    let authority = axum::Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(move || {
            let (policy, calls, hold_at, entered, release) = (
                current.clone(),
                counting.clone(),
                holding.clone(),
                entering.clone(),
                releasing.clone(),
            );
            async move {
                let snapshot = policy.lock().unwrap().clone();
                if calls.fetch_add(1, Ordering::SeqCst) + 1 == hold_at.load(Ordering::SeqCst) {
                    entered.notify_one();
                    release.notified().await;
                }
                axum::Json(snapshot)
            }
        }),
    );
    let (origin, authority_server) = common::serve_locally(authority).await;
    let identity_path = s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: Value =
        serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
    assert_eq!(identity["token"], "fixture-only");
    identity["origin"] = json!(origin);
    std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    let exchanges = Arc::new(AtomicUsize::new(0));
    let exchanging = exchanges.clone();
    let provider = axum::Router::new().route(
        "/token",
        axum::routing::post(move || {
            let exchanges = exchanging.clone();
            async move {
                let attempt = exchanges.fetch_add(1, Ordering::SeqCst) + 1;
                axum::Json(json!({
                    "access_token": format!("personal-revoke-fixture-{attempt}"),
                    "scope": "messages:read", "team": {"id": "T111"}, "authed_user": {"id": "U111"},
                }))
            }
        }),
    );
    let (origin, provider_server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    connect(&app, &path, &instance).await;
    assert_eq!(status(&app, &path).await["status"], "authorized");
    assert_eq!(exchanges.load(Ordering::SeqCst), 1);

    calls.store(0, Ordering::SeqCst);
    // authority() captures the owner's policy and then the member's Actor.
    // The third lookup follows the initial capture, in GET or DELETE's status read.
    hold_at.store(3, Ordering::SeqCst);
    let (running, running_path) = (app.clone(), path.clone());
    let mut pending = tokio::spawn(async move {
        common::send(
            &running,
            member_request(method, &running_path, Value::Null, "alice"),
        )
        .await
    });
    tokio::select! {
        () = entered.notified() => {},
        result = &mut pending => panic!("{method} ended before the authority barrier: {:?}", result.map(|r| r.status())),
        () = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("{method} authority barrier not reached"),
    }
    match change {
        Change::None => {}
        Change::Approval => {
            let package = registry::active(&s, "example").await.unwrap();
            registry::approve(&s, "example", "1", &package.digest)
                .await
                .unwrap();
        }
        Change::Membership => {
            policy.lock().unwrap()["members"] = json!([]);
            assert_eq!(
                common::send(&app, member_request("GET", &path, Value::Null, "alice"))
                    .await
                    .status(),
                StatusCode::FORBIDDEN
            );
            policy.lock().unwrap()["members"] =
                json!([{"account_id": "alice", "access_id": "alice-membership-2"}]);
            connect(&app, &path, &instance).await;
            assert_eq!(exchanges.load(Ordering::SeqCst), 2);
            assert_eq!(status(&app, &path).await["status"], "authorized");
        }
        Change::Resource => {
            assert_eq!(method, "GET");
            let resource_path = path.rsplit_once("/oauth/").unwrap().0;
            let removed = common::send(
                &app,
                request("DELETE", resource_path, Value::Null, Some("fixture-owner")),
            )
            .await;
            assert_eq!(removed.status(), StatusCode::OK);
        }
    }
    release.notify_one();
    let response = pending.await.unwrap();
    hold_at.store(usize::MAX, Ordering::SeqCst);
    let code = response.status();
    let body = common::read_text(response).await;
    if method == "GET" {
        if matches!(change, Change::None) {
            assert_eq!(code, StatusCode::OK, "{body}");
            assert_eq!(
                serde_json::from_str::<Value>(&body).unwrap()["status"],
                "authorized"
            );
        } else {
            assert!(
                !code.is_success(),
                "Retired personal GET published: {code} {body}"
            );
            assert!(
                !body.contains("U111"),
                "Retired read exposed its external subject: {body}"
            );
        }
    } else {
        let after = status(&app, &path).await;
        let expected = if matches!(change, Change::None) {
            "disconnected"
        } else {
            "authorized"
        };
        assert_eq!(
            after["status"], expected,
            "Delayed DELETE crossed its original authority and removed authorization: {code} {body}; current {after}"
        );
        if matches!(change, Change::None) {
            assert_eq!(code, StatusCode::OK);
        } else {
            assert!(
                !code.is_success(),
                "Retired DELETE returned success: {code} {body}"
            );
        }
    }
    authority_server.abort();
    provider_server.abort();
}
