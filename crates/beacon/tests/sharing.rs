mod common;

use common::{RelayedInstallation, login};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

fn request(
    relay: &RelayedInstallation,
    cookie: &str,
    session: &Value,
    method: Method,
    path: &str,
) -> reqwest::RequestBuilder {
    relay
        .app
        .client
        .request(method, format!("{}{path}", relay.app.url))
        .header("origin", &relay.app.url)
        .header("cookie", cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
}

#[tokio::test]
async fn invited_new_account_accepts_and_uses_the_shared_installation() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let sharing = format!("/api/installations/{id}/sharing");
    let response = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::POST,
        &format!("{sharing}/invitations"),
    )
    .json(&json!({ "email": "  MEMBER@Example.test  " }))
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let invitation: Value = response.json().await.unwrap();
    assert_eq!(invitation["email"], "member@example.test");
    let (cookie, session) = login(&relay.app, "member@example.test").await;
    let pending: Value = request(
        &relay,
        &cookie,
        &session,
        Method::GET,
        "/api/account/invitations",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(pending[0]["id"], invitation["id"]);
    assert_eq!(pending[0]["installationName"], "Real installation");
    assert_eq!(session["installations"], json!([]));
    let accepted = request(
        &relay,
        &cookie,
        &session,
        Method::POST,
        &format!(
            "/api/account/invitations/{}/accept",
            invitation["id"].as_str().unwrap()
        ),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
    let session: Value = request(
        &relay,
        &cookie,
        &session,
        Method::GET,
        "/api/account/session",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(session["installations"][0]["role"], "member");
    assert_eq!(session["installations"][0]["id"], id);

    // A new member uses no agent until the owner gives access.
    let denied = request(
        &relay,
        &cookie,
        &session,
        Method::POST,
        &format!("/api/installations/{id}/api/chats"),
    )
    .json(&json!({}))
    .send()
    .await
    .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    allow_main(&relay, &session).await;
    let response = request(
        &relay,
        &cookie,
        &session,
        Method::POST,
        &format!("/api/installations/{id}/api/chats"),
    )
    .json(&json!({}))
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let chat: Value = response.json().await.unwrap();
    let chats: Value = relay
        .get("/chats")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(chats[0]["id"], chat["id"]);
    let sharing: Value = request(&relay, &relay.cookie, &relay.session, Method::GET, &sharing)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sharing["members"][0]["email"], "member@example.test");
    assert!(
        !sharing["members"][0]["accessId"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    assert_eq!(sharing["invitations"], json!([]));
    relay.close().await;
}

/// The owner lets a member use the main agent, from the apps' owner-only route (ADR-0075).
async fn allow_main(relay: &RelayedInstallation, member: &Value) {
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let account = member["account"]["id"].as_str().unwrap();
    let response = request(
        relay,
        &relay.cookie,
        &relay.session,
        Method::PUT,
        &format!(
            "/api/installations/{id}/api/agents/{}/accounts/{account}",
            cairn_installation::config::MAIN_AGENT_ID
        ),
    )
    .json(&json!({}))
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn member(relay: &RelayedInstallation, email: &str) -> (String, Value) {
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let invitation: Value = request(
        relay,
        &relay.cookie,
        &relay.session,
        Method::POST,
        &format!("/api/installations/{id}/sharing/invitations"),
    )
    .json(&json!({ "email": email }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let (cookie, session) = login(&relay.app, email).await;
    assert_eq!(
        request(
            relay,
            &cookie,
            &session,
            Method::POST,
            &format!(
                "/api/account/invitations/{}/accept",
                invitation["id"].as_str().unwrap()
            )
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NO_CONTENT
    );
    allow_main(relay, &session).await;
    (cookie, session)
}

#[tokio::test]
async fn member_cannot_manage_projects_skills_agents_or_installation_settings() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "member@example.test").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    for (method, path) in [
        (Method::PUT, "/skills/global/shared"),
        (Method::DELETE, "/skills/global/shared"),
        (Method::POST, "/projects"),
        (
            Method::PUT,
            "/projects/00000000-0000-0000-0000-000000000000",
        ),
        (
            Method::DELETE,
            "/projects/00000000-0000-0000-0000-000000000000",
        ),
        (Method::POST, "/agents"),
        (Method::GET, "/plugins/github/projects"),
        (Method::GET, "/agents/00000000-0000-0000-0000-000000000000"),
        (Method::GET, "/nodes"),
        (Method::GET, "/accounts"),
        (Method::GET, "/plugins"),
        (Method::GET, "/mcps"),
        (Method::GET, "/settings"),
    ] {
        let response = request(
            &relay,
            &cookie,
            &session,
            method.clone(),
            &format!("/api/installations/{id}/api{path}"),
        )
        .header("x-cairn-role", "owner")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {path}");
    }
    for path in ["/agents", "/skills", "/projects", "/chats"] {
        assert_eq!(
            request(
                &relay,
                &cookie,
                &session,
                Method::GET,
                &format!("/api/installations/{id}/api{path}")
            )
            .send()
            .await
            .unwrap()
            .status(),
            StatusCode::OK,
            "{path}"
        );
    }
    for (method, path, body) in [
        (
            Method::GET,
            format!("/api/installations/{id}/sharing"),
            json!({}),
        ),
        (
            Method::POST,
            format!("/api/installations/{id}/sharing/invitations"),
            json!({ "email": "other@example.test" }),
        ),
        (
            Method::PATCH,
            format!("/api/installations/{id}"),
            json!({ "name": "Member rename" }),
        ),
        (
            Method::DELETE,
            format!(
                "/api/installations/{id}/sharing/members/{}",
                relay.session["account"]["id"].as_str().unwrap()
            ),
            json!({}),
        ),
        (
            Method::POST,
            format!("/api/installations/{id}/detach"),
            json!({}),
        ),
    ] {
        assert_eq!(
            request(&relay, &cookie, &session, method, &path)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    relay.close().await;
}

#[tokio::test]
async fn removal_and_departure_close_member_streams_and_preserve_owner_access() {
    use cairn_installation::config::MAIN_AGENT_ID;
    use std::time::Duration;

    for leaving in [false, true] {
        let relay = RelayedInstallation::new(axum::Router::new()).await;
        let (cookie, session) = member(&relay, "member@example.test").await;
        let id = relay.session["installations"][0]["id"].as_str().unwrap();
        let api = format!("/api/installations/{id}/api");
        let chat: Value = request(
            &relay,
            &cookie,
            &session,
            Method::POST,
            &format!("{api}/chats"),
        )
        .json(&json!({ "agentId": MAIN_AGENT_ID }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let chat_id = chat["id"].as_str().unwrap();
        let sent = request(
            &relay,
            &cookie,
            &session,
            Method::POST,
            &format!("{api}/chats/{chat_id}/messages"),
        )
        .json(&json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "text": "Continue independently",
        }))
        .send()
        .await
        .unwrap();
        assert_eq!(sent.status(), StatusCode::OK);
        relay
            .installation
            .chat_tick(&std::collections::HashSet::new())
            .await
            .unwrap();
        let detail: Value = request(
            &relay,
            &cookie,
            &session,
            Method::GET,
            &format!("{api}/chats/{chat_id}"),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let run_id = detail["runId"].as_str().unwrap().to_owned();
        let run_path = format!("{api}/runs/{run_id}/stream");
        let mut run_stream = request(&relay, &cookie, &session, Method::GET, &run_path)
            .send()
            .await
            .unwrap();
        assert_eq!(run_stream.status(), StatusCode::OK);
        assert_eq!(run_stream.headers()["content-type"], "text/event-stream");
        let first = run_stream.chunk().await.unwrap().unwrap();
        assert!(
            std::str::from_utf8(&first)
                .unwrap()
                .contains("event: batch")
        );
        let mut chats = request(
            &relay,
            &cookie,
            &session,
            Method::GET,
            &format!("{api}/chats/stream"),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(chats.status(), StatusCode::OK);
        chats.chunk().await.unwrap().unwrap();
        let mut owner = relay.get("/chats/stream").send().await.unwrap();
        owner.chunk().await.unwrap().unwrap();

        let avatar = format!("{api}/agents/{MAIN_AGENT_ID}/avatar");
        let image = include_bytes!("../../../tests/fixtures/artifacts/thumbnail.png");
        let data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, image);
        let uploaded = request(
            &relay,
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("{avatar}/upload"),
        )
        .json(&json!({ "agentId": MAIN_AGENT_ID, "data": data }))
        .send()
        .await
        .unwrap();
        assert_eq!(uploaded.status(), StatusCode::OK);
        let response = request(&relay, &cookie, &session, Method::GET, &avatar)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "image/png");
        let member_image = response.bytes().await.unwrap();
        let owner_image = relay
            .get(&format!("/agents/{MAIN_AGENT_ID}/avatar"))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(member_image, owner_image);

        let path = if leaving {
            format!("/api/installations/{id}/sharing/membership")
        } else {
            format!(
                "/api/installations/{id}/sharing/members/{}",
                session["account"]["id"].as_str().unwrap()
            )
        };
        let (acting_cookie, acting_session) = if leaving {
            (&cookie, &session)
        } else {
            (&relay.cookie, &relay.session)
        };
        assert_eq!(
            request(&relay, acting_cookie, acting_session, Method::DELETE, &path)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        for stream in [&mut run_stream, &mut chats] {
            tokio::time::timeout(Duration::from_secs(1), async {
                while let Ok(Some(_)) = stream.chunk().await {}
            })
            .await
            .expect("removed members lose idle streams immediately");
        }
        for path in [&run_path, &avatar, &format!("{api}/chats")] {
            assert_eq!(
                request(&relay, &cookie, &session, Method::GET, path)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
        let response = request(
            &relay,
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("{api}/chats"),
        )
        .json(&json!({}))
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), owner.chunk())
                .await
                .unwrap()
                .unwrap()
                .is_some()
        );
        let run: Value = relay
            .get(&format!("/runs/{run_id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(run["status"], "queued");
        drop(owner);
        relay.close().await;
    }
}

#[tokio::test]
async fn only_the_recipient_can_accept_and_only_the_owner_can_cancel_an_invitation() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let path = format!("/api/installations/{id}/sharing/invitations");
    let response = request(&relay, &relay.cookie, &relay.session, Method::POST, &path)
        .json(&json!({ "email": "recipient@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let invitation: Value = response.json().await.unwrap();
    let invitation_id = invitation["id"].as_str().unwrap();
    let accept = format!("/api/account/invitations/{invitation_id}/accept");
    let cancel = format!("{path}/{invitation_id}");
    let (outsider_cookie, outsider_session) = login(&relay.app, "outsider@example.test").await;
    let pending: Value = request(
        &relay,
        &outsider_cookie,
        &outsider_session,
        Method::GET,
        "/api/account/invitations",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(pending, json!([]));
    for (method, path) in [(Method::POST, &accept), (Method::DELETE, &cancel)] {
        assert_eq!(
            request(&relay, &outsider_cookie, &outsider_session, method, path)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    let (cookie, session) = login(&relay.app, "recipient@example.test").await;
    let without_csrf = relay
        .app
        .client
        .post(format!("{}{accept}", relay.app.url))
        .header("origin", &relay.app.url)
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(without_csrf.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        request(
            &relay,
            &relay.cookie,
            &relay.session,
            Method::DELETE,
            &cancel
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&relay, &cookie, &session, Method::POST, &accept)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let pending: Value = request(
        &relay,
        &cookie,
        &session,
        Method::GET,
        "/api/account/invitations",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(pending, json!([]));
    relay.close().await;
}

#[tokio::test]
async fn relay_rate_limits_are_per_cairn_account_and_headers_cannot_choose_a_bucket() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "member@example.test").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let path = format!("/api/installations/{id}/api/chats");
    for _ in 0..300 {
        assert_eq!(
            request(&relay, &cookie, &session, Method::GET, &path)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        request(&relay, &cookie, &session, Method::GET, &path)
            .header(
                "x-cairn-account-id",
                relay.session["account"]["id"].as_str().unwrap()
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn member_streams_leave_capacity_for_owner_messages() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "member@example.test").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let path = format!("/api/installations/{id}/api/chats/stream");
    let mut streams = Vec::new();
    for _ in 0..cairn_protocol::MAX_IN_FLIGHT {
        let response = request(&relay, &cookie, &session, Method::GET, &path)
            .send()
            .await
            .unwrap();
        if response.status() == StatusCode::SERVICE_UNAVAILABLE {
            break;
        }
        assert_eq!(response.status(), StatusCode::OK);
        streams.push(response);
    }
    assert!(!streams.is_empty());
    assert_eq!(
        request(
            &relay,
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/api/chats")
        )
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    drop(streams);
    relay.close().await;
}

#[tokio::test]
async fn installation_renames_are_rate_limited_per_account() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let path = format!("/api/installations/{id}");
    for _ in 0..10 {
        assert_eq!(
            request(&relay, &relay.cookie, &relay.session, Method::PATCH, &path)
                .json(&json!({ "name": "Renamed installation" }))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        request(&relay, &relay.cookie, &relay.session, Method::PATCH, &path)
            .json(&json!({ "name": "Too many renames" }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    relay.close().await;
}

#[tokio::test]
async fn detachment_forgets_members_and_pending_invitations() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "member@example.test").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let path = format!("/api/installations/{id}");
    assert_eq!(
        request(
            &relay,
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("{path}/sharing/invitations")
        )
        .json(&json!({ "email": "later@example.test" }))
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        request(
            &relay,
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("{path}/detach")
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NO_CONTENT
    );
    let response = request(&relay, &cookie, &session, Method::GET, "/api/installations")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap(), json!([]));
    assert_eq!(
        request(
            &relay,
            &cookie,
            &session,
            Method::GET,
            &format!("{path}/api/chats")
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NOT_FOUND
    );
    let (later_cookie, later_session) = login(&relay.app, "later@example.test").await;
    let pending: Value = request(
        &relay,
        &later_cookie,
        &later_session,
        Method::GET,
        "/api/account/invitations",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(pending, json!([]));

    let identity: Value = serde_json::from_slice(
        &tokio::fs::read(
            relay
                .installation
                .config
                .data_dir
                .join("installation-relay/identity.json"),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let started = relay
        .app
        .post(
            "/api/relay/device-claim/start",
            json!({
                "name": "Recovered",
                "protocol": cairn_protocol::PROTOCOL_VERSION,
                "identity": identity,
            }),
        )
        .await;
    assert_eq!(started.status(), StatusCode::CREATED);
    let device: Value = started.json().await.unwrap();
    let reviewed = request(
        &relay,
        &later_cookie,
        &later_session,
        Method::POST,
        "/api/installations/device-claim/preview",
    )
    .json(&json!({ "code": device["userCode"] }))
    .send()
    .await
    .unwrap();
    assert_eq!(reviewed.status(), StatusCode::OK);
    let reviewed: Value = reviewed.json().await.unwrap();
    assert_eq!(
        request(
            &relay,
            &later_cookie,
            &later_session,
            Method::POST,
            "/api/installations/device-claim"
        )
        .json(&json!({
            "code": device["userCode"],
            "confirmation": reviewed["confirmation"],
        }))
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    let reclaimed = relay
        .app
        .post(
            "/api/relay/device-claim/poll",
            json!({ "deviceCode": device["deviceCode"] }),
        )
        .await;
    assert_eq!(reclaimed.status(), StatusCode::OK);
    assert_eq!(
        reclaimed.json::<Value>().await.unwrap()["installationId"],
        id
    );
    let sharing = request(
        &relay,
        &later_cookie,
        &later_session,
        Method::GET,
        &format!("{path}/sharing"),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(sharing.status(), StatusCode::OK);
    assert_eq!(
        sharing.json::<Value>().await.unwrap(),
        json!({ "members": [], "invitations": [] })
    );
    assert_eq!(
        request(&relay, &cookie, &session, Method::GET, "/api/installations")
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        json!([])
    );

    relay.close().await;
}

#[tokio::test]
async fn revoked_members_cannot_finish_previously_authorized_uploads() {
    use std::time::Duration;
    use tokio::sync::mpsc;

    for leaving in [false, true] {
        let relay = RelayedInstallation::new(axum::Router::new()).await;
        let (cookie, session) = member(&relay, "member@example.test").await;
        let id = relay.session["installations"][0]["id"].as_str().unwrap();
        // Filling finite-request capacity observes that all uploads passed access
        // checking and reached body reading, without inspecting relay internals.
        let start_upload = || {
            let (sender, receiver) = mpsc::channel::<String>(2);
            let body = reqwest::Body::wrap_stream(futures_util::stream::unfold(
                receiver,
                |mut receiver| async move {
                    receiver
                        .recv()
                        .await
                        .map(|chunk| (Ok::<_, std::io::Error>(chunk), receiver))
                },
            ));
            sender.try_send("{".into()).unwrap();
            let upload = request(
                &relay,
                &cookie,
                &session,
                Method::POST,
                &format!("/api/installations/{id}/api/chats"),
            )
            .header("content-type", "application/json")
            .body(body);
            (
                sender,
                tokio::spawn(async move { upload.send().await.unwrap() }),
            )
        };
        let mut uploads: Vec<_> = (0..cairn_protocol::MAX_IN_FLIGHT)
            .map(|_| start_upload())
            .collect();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let status = relay.get("/chats").send().await.unwrap().status();
                if status == StatusCode::SERVICE_UNAVAILABLE {
                    break;
                }
                assert_eq!(status, StatusCode::OK);
                // The probe can take the last slot while uploads are entering.
                // Replace only uploads rejected as busy, before revoking access.
                for upload in &mut uploads {
                    if upload.1.is_finished() {
                        let (_, rejected) = std::mem::replace(upload, start_upload());
                        assert_eq!(
                            rejected.await.unwrap().status(),
                            StatusCode::SERVICE_UNAVAILABLE
                        );
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("uploads must reach the relay's finite-request capacity");

        let member_id = session["account"]["id"].as_str().unwrap();
        let (actor_cookie, actor_session, path) = if leaving {
            (
                &cookie,
                &session,
                format!("/api/installations/{id}/sharing/membership"),
            )
        } else {
            (
                &relay.cookie,
                &relay.session,
                format!("/api/installations/{id}/sharing/members/{member_id}"),
            )
        };
        assert_eq!(
            request(&relay, actor_cookie, actor_session, Method::DELETE, &path)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        for (sender, response) in uploads {
            sender.send("}".into()).await.unwrap();
            drop(sender);
            assert_eq!(response.await.unwrap().status(), StatusCode::NOT_FOUND);
        }
        assert_eq!(
            relay
                .get("/chats")
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap(),
            json!([])
        );
        relay.close().await;
    }
}

#[tokio::test]
async fn invitation_emails_neutralize_owner_supplied_urls() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let renamed = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::PATCH,
        &format!("/api/installations/{id}"),
    )
    .json(&json!({ "name": "https://evil.example/login?token=ignored" }))
    .send()
    .await
    .unwrap();
    assert_eq!(renamed.status(), StatusCode::OK);
    let invited = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::POST,
        &format!("/api/installations/{id}/sharing/invitations"),
    )
    .json(&json!({ "email": "recipient@example.test" }))
    .send()
    .await
    .unwrap();
    assert_eq!(invited.status(), StatusCode::CREATED);
    let delivery = relay.app.mail.1.lock().unwrap().last().unwrap().clone();
    assert_eq!(delivery.1, "https evil example login token ignored");
    assert_eq!(delivery.2, format!("{}/?invitations=1", relay.app.url));
    relay.close().await;
}

#[tokio::test]
async fn cancelling_and_reinviting_cannot_bypass_the_daily_email_budget() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let owner = relay.session["account"]["id"].as_str().unwrap();
    let path = format!("/api/installations/{id}/sharing/invitations");
    for attempt in 0..21 {
        if attempt % 10 == 0 {
            // Advance only the one-minute bucket at the clock/database seam.
            sqlx_core::query::query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key = $1")
                .bind(format!("invitation:{owner}")).execute(&relay.app.pool).await.unwrap();
        }
        let response = request(&relay, &relay.cookie, &relay.session, Method::POST, &path)
            .json(&json!({ "email": "recipient@example.test" }))
            .send()
            .await
            .unwrap();
        if attempt == 20 {
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
            break;
        }
        assert_eq!(response.status(), StatusCode::CREATED);
        let invitation: Value = response.json().await.unwrap();
        let cancelled = request(
            &relay,
            &relay.cookie,
            &relay.session,
            Method::DELETE,
            &format!("{path}/{}", invitation["id"].as_str().unwrap()),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(cancelled.status(), StatusCode::NO_CONTENT);
    }
    assert_eq!(relay.app.mail.1.lock().unwrap().len(), 20);
    let sharing: Value = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::GET,
        &format!("/api/installations/{id}/sharing"),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(sharing["invitations"], json!([]));
    relay.close().await;
}

#[tokio::test]
async fn account_stream_limits_preserve_the_owners_live_views_and_release_on_cancellation() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "member@example.test").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let path = format!("/api/installations/{id}/api/chats/stream");
    let mut streams = Vec::new();
    for _ in 0..8 {
        let response = request(&relay, &cookie, &session, Method::GET, &path)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        streams.push(response);
    }
    sqlx_core::query::query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&relay.app.pool).await.unwrap();
    let (other_cookie, other_session) = login(&relay.app, "member@example.test").await;
    let other_device = request(&relay, &other_cookie, &other_session, Method::GET, &path)
        .send()
        .await
        .unwrap();
    assert_eq!(
        other_device.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "another session cannot bypass the account allowance"
    );
    let exhausted = request(&relay, &cookie, &session, Method::GET, &path)
        .header(
            "x-cairn-account-id",
            relay.session["account"]["id"].as_str().unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(exhausted.status(), StatusCode::SERVICE_UNAVAILABLE);
    let owner = relay.get("/chats/stream").send().await.unwrap();
    assert_eq!(owner.status(), StatusCode::OK);
    drop(streams);
    let replacement = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let response = request(&relay, &cookie, &session, Method::GET, &path)
                .send()
                .await
                .unwrap();
            if response.status() == StatusCode::OK {
                break response;
            }
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cancellation must release the account's stream allowance");
    drop(replacement);
    drop(owner);
    relay.close().await;
}

#[tokio::test]
async fn expired_invitations_are_hidden_and_cannot_be_accepted() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let (cookie, session) = login(&relay.app, "recipient@example.test").await;
    let invited: Value = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::POST,
        &format!("/api/installations/{id}/sharing/invitations"),
    )
    .json(&json!({ "email": "recipient@example.test" }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    sqlx_core::query::query(
        "UPDATE installation_invitations SET expires_at = now() - interval '1 second'",
    )
    .execute(&relay.app.pool)
    .await
    .unwrap();
    let pending: Value = request(
        &relay,
        &cookie,
        &session,
        Method::GET,
        "/api/account/invitations",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(pending, json!([]));
    let accepted = request(
        &relay,
        &cookie,
        &session,
        Method::POST,
        &format!(
            "/api/account/invitations/{}/accept",
            invited["id"].as_str().unwrap()
        ),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(accepted.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        request(
            &relay,
            &cookie,
            &session,
            Method::GET,
            &format!("/api/installations/{id}/api/chats")
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NOT_FOUND
    );
    relay.close().await;
}

fn clock_rule_definition(name: &str, prompt: &str, agent_id: &str) -> Value {
    json!({
        "name": name,
        "description": "Scheduled hook rule.",
        "trigger": {
            "type": "clock",
            "cron": "0 9 * * *",
            "timezone": "UTC",
        },
        "executionAgentId": agent_id,
        "steps": [{
            "type": "action",
            "id": "run",
            "action": {
                "type": "startAgent",
                "agentId": agent_id,
                "prompt": { "type": "literal", "value": prompt },
            },
        }],
        "limits": {
            "triggeringsPerHour": 60,
            "conversationsPerHour": 10,
        },
    })
}

fn has_block(rule: &Value, kind: &str) -> bool {
    rule["blocks"]
        .as_array()
        .is_some_and(|blocks| blocks.iter().any(|block| block["kind"] == kind))
}

#[tokio::test]
async fn shared_hook_rules_record_the_verified_creator_and_restrict_deletion() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "rule-author@example.test").await;
    let (other_cookie, other_session) = member(&relay, "other-member@example.test").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let rules_path = format!("/api/installations/{id}/api/hook-rules");
    let response = request(&relay, &cookie, &session, Method::POST, &rules_path)
        .json(&json!({
            "definition": clock_rule_definition(
                "Shared rule",
                "Keep working",
                cairn_installation::config::MAIN_AGENT_ID,
            ),
            "enabled": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rule: Value = response.json().await.unwrap();
    assert_eq!(rule["creatorAccountId"], session["account"]["id"]);
    assert!(rule["creatorAccountId"].is_string());

    let path = format!("{rules_path}/{}", rule["id"].as_str().unwrap());
    assert_eq!(
        request(&relay, &other_cookie, &other_session, Method::DELETE, &path)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&relay, &relay.cookie, &relay.session, Method::DELETE, &path)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn members_cannot_rewrite_or_enable_another_creators_hook_rule() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "rule-editor@example.test").await;
    let (other_cookie, other_session) = member(&relay, "other-author@example.test").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let rules_path = format!("/api/installations/{id}/api/hook-rules");
    let other_agent: Value = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::POST,
        &format!("/api/installations/{id}/api/agents"),
    )
    .json(&json!({
        "settings": {
            "name": "Alternative agent",
            "access": cairn_contracts::AccessPolicy::default(),
            "exclusiveNodes": [],
            "plugins": [],
        },
        "review": {},
    }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(other_agent["id"].is_string());

    let definition = clock_rule_definition(
        "Paused commitment",
        "Owner-approved work",
        cairn_installation::config::MAIN_AGENT_ID,
    );

    for (author_cookie, author_session) in [
        (&relay.cookie, &relay.session),
        (&other_cookie, &other_session),
    ] {
        let rule: Value = request(
            &relay,
            author_cookie,
            author_session,
            Method::POST,
            &rules_path,
        )
        .json(&json!({ "definition": definition, "enabled": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let rule_id = rule["id"].as_str().unwrap();
        let path = format!("{rules_path}/{rule_id}");

        let mutations = [
            ("prompt", {
                let mut d = definition.clone();
                d["steps"][0]["action"]["prompt"]["value"] = json!("Unapproved work");
                d
            }),
            ("cron", {
                let mut d = definition.clone();
                d["trigger"]["cron"] = json!("* * * * *");
                d
            }),
            ("timezone", {
                let mut d = definition.clone();
                d["trigger"]["timezone"] = json!("Europe/Paris");
                d
            }),
            ("executionAgentId", {
                let mut d = definition.clone();
                d["executionAgentId"] = other_agent["id"].clone();
                d
            }),
        ];
        for (field, changed) in mutations {
            let response = request(&relay, &cookie, &session, Method::PUT, &path)
                .json(&json!({
                    "ruleId": rule_id,
                    "revision": rule["revision"],
                    "definition": changed,
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "member changed {field}"
            );
        }

        let enabled_response = request(
            &relay,
            &cookie,
            &session,
            Method::POST,
            &format!("{path}/enabled"),
        )
        .json(&json!({ "ruleId": rule_id, "enabled": true }))
        .send()
        .await
        .unwrap();
        assert_eq!(enabled_response.status(), StatusCode::FORBIDDEN);

        let unchanged: Value = relay
            .get(&format!("/hook-rules/{rule_id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            unchanged["definition"]["steps"][0]["action"]["prompt"]["value"],
            "Owner-approved work"
        );
        assert!(has_block(&unchanged, "disabled"));

        let mut updated = definition.clone();
        updated["steps"][0]["action"]["prompt"]["value"] = json!("Author's updated work");
        assert_eq!(
            request(&relay, author_cookie, author_session, Method::PUT, &path)
                .json(&json!({
                    "ruleId": rule_id,
                    "revision": rule["revision"],
                    "definition": updated,
                }))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    relay.close().await;
}

#[tokio::test]
async fn removing_a_rule_creator_stops_their_schedules_and_preserves_admitted_work() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "scheduled-author@example.test").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let api = format!("/api/installations/{id}/api");
    let rules_path = format!("{api}/hook-rules");
    let definition = clock_rule_definition(
        "Scheduled shared rule",
        "Keep working",
        cairn_installation::config::MAIN_AGENT_ID,
    );
    let member_rule: Value = request(&relay, &cookie, &session, Method::POST, &rules_path)
        .json(&json!({ "definition": definition, "enabled": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let owner_rule: Value = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::POST,
        &rules_path,
    )
    .json(&json!({ "definition": definition, "enabled": true }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();

    let chat: Value = request(
        &relay,
        &cookie,
        &session,
        Method::POST,
        &format!("{api}/chats"),
    )
    .json(&json!({ "agentId": cairn_installation::config::MAIN_AGENT_ID }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let chat_id = chat["id"].as_str().unwrap();
    let sent = request(
        &relay,
        &cookie,
        &session,
        Method::POST,
        &format!("{api}/chats/{chat_id}/messages"),
    )
    .json(&json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "text": "Keep working",
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(sent.status(), StatusCode::OK);
    relay
        .installation
        .chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let detail = relay.installation.chat_detail(chat_id).await.unwrap();
    let run_id = detail["runId"].as_str().unwrap().to_owned();

    let removed = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::DELETE,
        &format!(
            "/api/installations/{id}/sharing/members/{}",
            session["account"]["id"].as_str().unwrap()
        ),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    let listed: Value = relay
        .get("/hook-rules")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rules = listed["rules"].as_array().unwrap();
    let retired = rules
        .iter()
        .find(|value| value["id"] == member_rule["id"])
        .unwrap();
    assert!(has_block(retired, "membershipEnded"), "{retired}");
    assert!(retired["nextRun"].is_null(), "{retired}");
    assert_eq!(retired["creatorAccountId"], session["account"]["id"]);
    let owner_stored = rules
        .iter()
        .find(|value| value["id"] == owner_rule["id"])
        .unwrap();
    assert_eq!(owner_stored["blocks"], json!([]));
    assert!(owner_stored["nextRun"].is_number(), "{owner_stored}");
    assert_eq!(
        owner_stored["creatorAccountId"],
        relay.session["account"]["id"]
    );
    let admitted: Value = relay
        .get(&format!("/runs/{run_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(admitted["status"], "queued");
    relay.close().await;
}

#[tokio::test]
async fn offline_removal_and_reinvitation_do_not_revive_old_hook_rule_commitments() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "returning-author@example.test").await;
    let id = relay.session["installations"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let rules_path = format!("/api/installations/{id}/api/hook-rules");
    let definition = clock_rule_definition(
        "Old commitment",
        "Keep working",
        cairn_installation::config::MAIN_AGENT_ID,
    );
    let rule: Value = request(&relay, &cookie, &session, Method::POST, &rules_path)
        .json(&json!({ "definition": definition, "enabled": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rule_id = rule["id"].as_str().unwrap().to_owned();
    let rule_path = format!("{rules_path}/{rule_id}");

    relay.stop.cancel();
    (&mut relay.connector).await.unwrap().unwrap();
    assert_eq!(
        request(
            &relay,
            &relay.cookie,
            &relay.session,
            Method::DELETE,
            &format!(
                "/api/installations/{id}/sharing/members/{}",
                session["account"]["id"].as_str().unwrap()
            )
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NO_CONTENT
    );
    let invitation: Value = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::POST,
        &format!("/api/installations/{id}/sharing/invitations"),
    )
    .json(&json!({ "email": "returning-author@example.test" }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(
        request(
            &relay,
            &cookie,
            &session,
            Method::POST,
            &format!(
                "/api/account/invitations/{}/accept",
                invitation["id"].as_str().unwrap()
            )
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NO_CONTENT
    );
    relay.stop = tokio_util::sync::CancellationToken::new();
    relay.connector = tokio::spawn(cairn_installation::relay::connect_with_direct(
        relay
            .installation
            .config
            .data_dir
            .join("installation-relay"),
        relay.router.clone(),
        relay.installation.clone(),
        relay.stop.clone(),
        relay.direct.clone(),
    ));
    let retired: Value = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let response = relay.get("/hook-rules").send().await.unwrap();
            if response.status() == StatusCode::OK {
                let listed: Value = response.json().await.unwrap();
                if let Some(found) = listed["rules"]
                    .as_array()
                    .and_then(|rules| rules.iter().find(|value| value["id"] == rule["id"]))
                    && has_block(found, "membershipEnded")
                {
                    break found.clone();
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(has_block(&retired, "membershipEnded"), "{retired}");
    assert!(retired["nextRun"].is_null(), "{retired}");

    // Account access to agents belonged to the earlier membership as well.
    let refused = request(&relay, &cookie, &session, Method::POST, &rules_path)
        .json(&json!({ "definition": definition, "enabled": true }))
        .send()
        .await
        .unwrap();
    let status = refused.status();
    let error: Value = refused.json().await.unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("access to this agent"),
        "{error}"
    );
    assert_eq!(
        request(
            &relay,
            &relay.cookie,
            &relay.session,
            Method::PUT,
            &rule_path
        )
        .json(&json!({
            "ruleId": rule_id,
            "revision": retired["revision"],
            "definition": definition,
        }))
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::FORBIDDEN
    );

    allow_main(&relay, &session).await;
    assert_eq!(
        request(&relay, &cookie, &session, Method::PUT, &rule_path)
            .json(&json!({
                "ruleId": rule_id,
                "revision": retired["revision"],
                "definition": definition,
            }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &relay,
            &cookie,
            &session,
            Method::POST,
            &format!("{rule_path}/enabled"),
        )
        .json(&json!({ "ruleId": rule_id, "enabled": true }))
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::CONFLICT
    );
    let replacement: Value = request(&relay, &cookie, &session, Method::POST, &rules_path)
        .json(&json!({ "definition": definition, "enabled": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replacement["blocks"], json!([]));
    assert!(replacement["nextRun"].is_number(), "{replacement}");
    assert_eq!(replacement["creatorAccountId"], session["account"]["id"]);
    relay.close().await;
}

#[tokio::test]
async fn leaving_or_deleting_the_creator_account_stops_their_schedules() {
    for deleting_account in [false, true] {
        let relay = RelayedInstallation::new(axum::Router::new()).await;
        let email = "departing-author@example.test";
        let (cookie, session) = member(&relay, email).await;
        let id = relay.session["installations"][0]["id"].as_str().unwrap();
        let rule: Value = request(
            &relay,
            &cookie,
            &session,
            Method::POST,
            &format!("/api/installations/{id}/api/hook-rules"),
        )
        .json(&json!({
            "definition": clock_rule_definition(
                "Member commitment",
                "Scheduled work",
                cairn_installation::config::MAIN_AGENT_ID,
            ),
            "enabled": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let path = if deleting_account {
            "/api/account/delete".to_owned()
        } else {
            format!("/api/installations/{id}/sharing/membership")
        };
        let method = if deleting_account {
            Method::POST
        } else {
            Method::DELETE
        };
        let response = request(&relay, &cookie, &session, method, &path)
            .json(&json!({ "email": email }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let listed: Value = relay
            .get("/hook-rules")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let stored = listed["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stored| stored["id"] == rule["id"])
            .unwrap();
        assert!(has_block(stored, "membershipEnded"), "{stored}");
        assert!(stored["nextRun"].is_null(), "{stored}");
        assert_eq!(stored["creatorAccountId"], session["account"]["id"]);
        relay.close().await;
    }
}

#[tokio::test]
async fn member_order_uses_insertion_time_after_a_blocked_acceptance() {
    use sqlx_core::{query::query, query_as::query_as};
    use std::time::Duration;

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let invitation: Value = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::POST,
        &format!("/api/installations/{id}/sharing/invitations"),
    )
    .json(&json!({ "email": "blocked-member@example.test" }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let (cookie, session) = login(&relay.app, "blocked-member@example.test").await;
    let account = session["account"]["id"].as_str().unwrap();

    let mut lock = relay.app.pool.begin().await.unwrap();
    query("SELECT id FROM installations WHERE id=$1 FOR UPDATE")
        .bind(id)
        .execute(&mut *lock)
        .await
        .unwrap();
    let (blocking_pid,): (i32,) = query_as("SELECT pg_backend_pid()")
        .fetch_one(&mut *lock)
        .await
        .unwrap();
    let acceptance = request(
        &relay,
        &cookie,
        &session,
        Method::POST,
        &format!(
            "/api/account/invitations/{}/accept",
            invitation["id"].as_str().unwrap()
        ),
    );
    let accepting = tokio::spawn(async move { acceptance.send().await.unwrap() });

    // Observe the real wait, and a time strictly later than its transaction start.
    // This avoids scheduling assumptions or a fixed sleep between request and release.
    let cutoff = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let boundary: Option<(i64,)> = query_as(
                "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)) AND ceil(extract(epoch FROM xact_start) * 1000) < floor(extract(epoch FROM clock_timestamp()) * 1000) LIMIT 1",
            )
            .bind(blocking_pid).fetch_optional(&relay.app.pool).await.unwrap();
            if let Some((cutoff,)) = boundary {
                break cutoff;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("invitation acceptance did not wait on the installation lock");
    lock.commit().await.unwrap();
    assert_eq!(accepting.await.unwrap().status(), StatusCode::NO_CONTENT);

    let (joined_at,): (i64,) = query_as(
        "SELECT ceil(extract(epoch FROM joined_at) * 1000)::bigint FROM installation_members WHERE installation_id=$1 AND account_id=$2",
    )
    .bind(id).bind(account).fetch_one(&relay.app.pool).await.unwrap();
    assert!(
        joined_at >= cutoff,
        "a membership accepted after the wait must not predate its insertion"
    );
    relay.close().await;
}

#[tokio::test]
async fn author_policy_requires_the_current_machine_identity_and_never_browser_cookies() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let identity: Value = serde_json::from_slice(
        &tokio::fs::read(
            relay
                .installation
                .config
                .data_dir
                .join("installation-relay/identity.json"),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let id = identity["installationId"].as_str().unwrap();
    let endpoint = format!("{}/api/relay/{id}/task-authors", relay.app.url);
    assert_eq!(
        relay
            .app
            .client
            .get(&endpoint)
            .header("cookie", &relay.cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        relay
            .app
            .client
            .get(&endpoint)
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = relay
        .app
        .client
        .get(&endpoint)
        .bearer_auth(identity["token"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let policy: Value = response.json().await.unwrap();
    assert_eq!(
        policy["owner"]["account_id"],
        relay.session["account"]["id"]
    );
    assert_eq!(policy["members"], json!([]));

    let (_, session) = member(&relay, "rollout@example.test").await;
    let account = session["account"]["id"].as_str().unwrap();
    let policy: Value = relay
        .app
        .client
        .get(&endpoint)
        .bearer_auth(identity["token"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(policy["members"][0]["account_id"], account);
    assert!(policy["members"][0].get("joined_at").is_none());
    assert!(
        !policy["members"][0]["access_id"]
            .as_str()
            .unwrap()
            .is_empty()
    );

    assert_eq!(
        request(
            &relay,
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/detach")
        )
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        relay
            .app
            .client
            .get(&endpoint)
            .bearer_auth(identity["token"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    relay.close().await;
}
