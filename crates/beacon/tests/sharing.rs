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
    assert_eq!(sharing["invitations"], json!([]));
    relay.close().await;
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
        (Method::GET, "/github/repositories"),
        (Method::GET, "/agents/00000000-0000-0000-0000-000000000000"),
        (Method::GET, "/nodes"),
        (Method::GET, "/accounts"),
        (Method::GET, "/onepassword"),
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
        let task: Value = request(
            &relay,
            &cookie,
            &session,
            Method::POST,
            &format!("{api}/tasks"),
        )
        .json(&json!({
            "name": "Member work",
            "prompt": "Continue independently",
            "agentId": MAIN_AGENT_ID,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let response = request(
            &relay,
            &cookie,
            &session,
            Method::POST,
            &format!("{api}/tasks/{}/run", task["id"].as_str().unwrap()),
        )
        .json(&json!({}))
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let run: Value = response.json().await.unwrap();
        let run_path = format!("{api}/runs/{}/stream", run["id"].as_str().unwrap());
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
        let uploaded = request(&relay, &relay.cookie, &relay.session, Method::PUT, &avatar)
            .header("content-type", "image/png")
            .body(image.as_slice())
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
            .get(&format!("/runs/{}", run["id"].as_str().unwrap()))
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
                "protocol": 2,
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

#[tokio::test]
async fn shared_tasks_record_the_verified_author_and_only_the_owner_can_delete() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "task-author@example.test").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let task_path = format!("/api/installations/{id}/api/tasks");
    let response = request(&relay, &cookie, &session, Method::POST, &task_path)
        .json(&json!({
            "name": "Shared task",
            "prompt": "Keep working",
            "agentId": cairn_installation::config::MAIN_AGENT_ID,
            "authorId": relay.session["account"]["id"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let task: Value = response.json().await.unwrap();
    assert_eq!(task["authorId"], session["account"]["id"]);
    assert!(task["authorId"].is_string());

    let path = format!("{task_path}/{}", task["id"].as_str().unwrap());
    assert_eq!(
        request(&relay, &cookie, &session, Method::DELETE, &path)
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
async fn members_cannot_rewrite_or_resume_another_authors_task() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "task-editor@example.test").await;
    let (other_cookie, other_session) = member(&relay, "other-author@example.test").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let tasks = format!("/api/installations/{id}/api/tasks");
    let other_agent: Value = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::POST,
        &format!("/api/installations/{id}/api/agents"),
    )
    .json(&json!({ "name": "Alternative agent" }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(other_agent["id"].is_string());

    let input = json!({
        "name": "Paused commitment",
        "prompt": "Owner-approved work",
        "agentId": cairn_installation::config::MAIN_AGENT_ID,
        "cron": "0 9 * * *",
        "timezone": "UTC",
        "enabled": false,
    });

    for (author_cookie, author_session, legacy) in [
        (&relay.cookie, &relay.session, false),
        (&relay.cookie, &relay.session, true),
        (&other_cookie, &other_session, false),
    ] {
        let mut task: Value = request(&relay, author_cookie, author_session, Method::POST, &tasks)
            .json(&input)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if legacy {
            task.as_object_mut().unwrap().remove("authorId");
            task.as_object_mut().unwrap().remove("authorAccessId");
            relay
                .installation
                .store
                .save("tasks", task.clone(), "fixture.legacy_task")
                .await
                .unwrap();
        }
        let path = format!("{tasks}/{}", task["id"].as_str().unwrap());

        for (field, value) in [
            ("prompt", json!("Unapproved work")),
            ("cron", json!("* * * * *")),
            ("timezone", json!("Europe/Paris")),
            ("enabled", json!(true)),
            ("worktree", json!(false)),
            ("agentId", other_agent["id"].clone()),
        ] {
            let mut changed = task.clone();
            changed[field] = value;
            let response = request(&relay, &cookie, &session, Method::PUT, &path)
                .json(&changed)
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "member changed {field}"
            );
        }

        let stored: Vec<Value> = relay
            .get("/tasks")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let unchanged = stored
            .iter()
            .find(|stored| stored["id"] == task["id"])
            .unwrap();
        assert_eq!(unchanged["prompt"], "Owner-approved work");
        assert_eq!(unchanged["enabled"], false);

        let mut changed = task.clone();
        changed["prompt"] = "Author's updated work".into();
        assert_eq!(
            request(&relay, author_cookie, author_session, Method::PUT, &path)
                .json(&changed)
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
async fn removing_a_task_author_stops_their_schedules_and_preserves_admitted_work() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "scheduled-author@example.test").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let tasks = format!("/api/installations/{id}/api/tasks");
    let input = json!({
        "name": "Scheduled shared task",
        "prompt": "Keep working",
        "agentId": cairn_installation::config::MAIN_AGENT_ID,
        "cron": "0 9 * * *",
        "timezone": "UTC",
        "enabled": true,
    });
    let task: Value = request(&relay, &cookie, &session, Method::POST, &tasks)
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = task["id"].as_str().unwrap();
    let mut legacy = relay.installation.task(input, None).await.unwrap();
    legacy.as_object_mut().unwrap().remove("authorId");
    legacy.as_object_mut().unwrap().remove("authorAccessId");
    relay
        .installation
        .store
        .save("tasks", legacy.clone(), "fixture.legacy_task")
        .await
        .unwrap();
    let run: Value = request(
        &relay,
        &cookie,
        &session,
        Method::POST,
        &format!("{tasks}/{task_id}/run"),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();

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
    let stored: Vec<Value> = relay
        .get("/tasks")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let retired = stored
        .iter()
        .find(|value| value["id"] == task["id"])
        .unwrap();
    assert_eq!(retired["enabled"], false);
    assert_eq!(retired["nextRun"], Value::Null);
    assert_eq!(retired["authorId"], session["account"]["id"]);
    let owner_task = stored
        .iter()
        .find(|value| value["id"] == legacy["id"])
        .unwrap();
    assert_eq!(owner_task["enabled"], true);
    assert_eq!(owner_task["authorId"], relay.session["account"]["id"]);
    let admitted: Value = relay
        .get(&format!("/runs/{}", run["id"].as_str().unwrap()))
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
async fn offline_removal_and_reinvitation_do_not_revive_old_task_commitments() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "returning-author@example.test").await;
    let id = relay.session["installations"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tasks = format!("/api/installations/{id}/api/tasks");
    let input = json!({
        "name": "Old commitment",
        "prompt": "Keep working",
        "agentId": cairn_installation::config::MAIN_AGENT_ID,
        "cron": "0 9 * * *",
        "timezone": "UTC",
        "enabled": true,
    });
    let task: Value = request(&relay, &cookie, &session, Method::POST, &tasks)
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_path = format!("{tasks}/{}", task["id"].as_str().unwrap());
    let edited: Value = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::PUT,
        &task_path,
    )
    .json(&{
        let mut edited = input.clone();
        edited["authorId"] = relay.session["account"]["id"].clone();
        edited["authorRemoved"] = false.into();
        edited["name"] = "Edited by owner".into();
        edited
    })
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(edited["authorId"], session["account"]["id"]);

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
    let stored: Vec<Value> = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let response = relay.get("/tasks").send().await.unwrap();
            if response.status() == StatusCode::OK {
                break response.json().await.unwrap();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let retired = stored
        .iter()
        .find(|value| value["id"] == task["id"])
        .unwrap();
    assert_eq!(retired["enabled"], false);
    assert_eq!(retired["authorRemoved"], true);
    assert_eq!(retired["nextRun"], Value::Null);
    for (actor_cookie, actor_session) in [(&cookie, &session), (&relay.cookie, &relay.session)] {
        assert_eq!(
            request(&relay, actor_cookie, actor_session, Method::PUT, &task_path)
                .json(&{
                    let mut edited = input.clone();
                    edited["authorId"] = actor_session["account"]["id"].clone();
                    edited["authorRemoved"] = false.into();
                    edited
                })
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
    }
    let replacement: Value = request(&relay, &cookie, &session, Method::POST, &tasks)
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replacement["enabled"], true);
    assert_eq!(replacement["authorId"], session["account"]["id"]);
    assert_ne!(replacement["authorAccessId"], task["authorAccessId"]);
    relay.close().await;
}

#[tokio::test]
async fn leaving_or_deleting_the_author_account_stops_their_schedules() {
    for deleting_account in [false, true] {
        let relay = RelayedInstallation::new(axum::Router::new()).await;
        let email = "departing-author@example.test";
        let (cookie, session) = member(&relay, email).await;
        let id = relay.session["installations"][0]["id"].as_str().unwrap();
        let task: Value = request(
            &relay,
            &cookie,
            &session,
            Method::POST,
            &format!("/api/installations/{id}/api/tasks"),
        )
        .json(&json!({
            "name": "Member commitment",
            "prompt": "Scheduled work",
            "agentId": cairn_installation::config::MAIN_AGENT_ID,
            "cron": "0 9 * * *",
            "timezone": "UTC",
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
        let tasks: Vec<Value> = relay
            .get("/tasks")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let task = tasks
            .iter()
            .find(|stored| stored["id"] == task["id"])
            .unwrap();
        assert_eq!(task["enabled"], false);
        assert_eq!(task["authorRemoved"], true);
        assert_eq!(task["authorId"], session["account"]["id"]);
        relay.close().await;
    }
}

#[tokio::test]
async fn an_authority_wait_never_delays_already_admitted_work() {
    use std::{sync::Arc, time::Duration};
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let tasks = format!("/api/installations/{id}/api/tasks");
    let mut task: Value = request(&relay, &relay.cookie, &relay.session, Method::POST, &tasks)
        .json(&json!({
            "name": "Due commitment",
            "prompt": "Approved work",
            "agentId": cairn_installation::config::MAIN_AGENT_ID,
            "cron": "0 9 * * *",
            "enabled": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let queued: Value = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::POST,
        &format!("{tasks}/{}/run", task["id"].as_str().unwrap()),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    task["nextRun"] = 1.into();
    relay
        .installation
        .store
        .save("tasks", task, "fixture.time")
        .await
        .unwrap();
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let arrival = entered.clone();
    let proxy = AuthorPolicyProxy::new(
        &relay,
        axum::Router::new().route(
            "/api/relay/{installation}/task-authors",
            axum::routing::get(move || {
                let arrival = arrival.clone();
                async move {
                    arrival.add_permits(1);
                    std::future::pending::<StatusCode>().await
                }
            }),
        ),
    )
    .await;
    relay
        .installation
        .worker
        .start(relay.installation.clone())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();

    let launching = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let run: Value = relay
                .get(&format!("/runs/{}", queued["id"].as_str().unwrap()))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if !run["startedAt"].is_null() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    relay.installation.shutdown.cancel();
    relay.installation.worker.close().await;
    proxy.close().await;
    relay.close().await;
    launching.expect("The beacon authority delayed launching already admitted work");
}

#[tokio::test]
async fn a_deferred_author_check_preserves_its_due_time_without_blocking_other_tasks() {
    use axum::{extract::Path, http::HeaderMap, response::IntoResponse};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let tasks_path = format!("/api/installations/{id}/api/tasks");
    for name in ["First commitment", "Second commitment"] {
        let mut task: Value = request(
            &relay,
            &relay.cookie,
            &relay.session,
            Method::POST,
            &tasks_path,
        )
        .json(&json!({
            "name": name,
            "prompt": "Approved work",
            "agentId": cairn_installation::config::MAIN_AGENT_ID,
            "cron": "0 9 * * *",
            "enabled": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        task["nextRun"] = 1.into();
        relay
            .installation
            .store
            .save("tasks", task, "fixture.time")
            .await
            .unwrap();
    }
    let tasks: Vec<Value> = relay
        .get("/tasks")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let deferred_id = tasks[0]["id"].as_str().unwrap();
    let admitted_id = tasks[1]["id"].as_str().unwrap();
    let checks = Arc::new(AtomicUsize::new(0));
    let upstream = relay.app.url.clone();
    let proxy = AuthorPolicyProxy::new(
        &relay,
        axum::Router::new().route(
            "/api/relay/{installation}/task-authors",
            axum::routing::get(move |Path(id): Path<String>, headers: HeaderMap| {
                let checks = checks.clone();
                let upstream = upstream.clone();
                async move {
                    if checks.fetch_add(1, Ordering::SeqCst) == 1 {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    let response = reqwest::Client::new()
                        .get(format!("{upstream}/api/relay/{id}/task-authors"))
                        .header("authorization", headers["authorization"].clone())
                        .send()
                        .await
                        .unwrap();
                    (response.status(), response.bytes().await.unwrap()).into_response()
                }
            }),
        ),
    )
    .await;

    relay.installation.schedule().await.unwrap();
    let runs: Vec<Value> = relay
        .get("/runs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        runs.len(),
        1,
        "One failed author check must not defer unrelated commitments"
    );
    assert_eq!(runs[0]["taskId"], admitted_id);
    let tasks: Vec<Value> = relay
        .get("/tasks")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let deferred = tasks.iter().find(|task| task["id"] == deferred_id).unwrap();
    assert_eq!(deferred["nextRun"], 1);
    assert_eq!(deferred["enabled"], true);
    assert!(
        deferred["scheduleWaitReason"]
            .as_str()
            .unwrap()
            .contains("unavailable")
    );

    relay.installation.schedule().await.unwrap();
    relay.installation.schedule().await.unwrap();
    let runs: Vec<Value> = relay
        .get("/runs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        runs.len(),
        2,
        "Recovery must admit each occurrence only once"
    );
    let tasks: Vec<Value> = relay
        .get("/tasks")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        tasks
            .iter()
            .all(|task| task["nextRun"].as_i64().unwrap() > 1)
    );
    assert!(
        tasks
            .iter()
            .all(|task| task["scheduleWaitReason"].is_null())
    );

    proxy.close().await;
    relay.close().await;
}

#[tokio::test]
async fn revoked_and_unclaimed_identities_explain_scheduled_work_without_audit_flooding() {
    use std::os::unix::fs::PermissionsExt;
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let mut task: Value = request(
        &relay,
        &relay.cookie,
        &relay.session,
        Method::POST,
        &format!("/api/installations/{id}/api/tasks"),
    )
    .json(&json!({
        "name": "Waiting commitment",
        "prompt": "Approved work",
        "agentId": cairn_installation::config::MAIN_AGENT_ID,
        "cron": "0 9 * * *",
        "enabled": true,
    }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    task["nextRun"] = 1.into();
    relay
        .installation
        .store
        .save("tasks", task, "fixture.time")
        .await
        .unwrap();
    let identity_path = relay
        .installation
        .config
        .data_dir
        .join("installation-relay/identity.json");
    let original = tokio::fs::read(&identity_path).await.unwrap();
    let mut revoked: Value = serde_json::from_slice(&original).unwrap();
    revoked["token"] = "fixture-invalid-token".into();
    tokio::fs::write(&identity_path, serde_json::to_vec(&revoked).unwrap())
        .await
        .unwrap();

    relay.installation.schedule().await.unwrap();
    relay.installation.schedule().await.unwrap();
    let tasks: Vec<Value> = relay
        .get("/tasks")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        tasks[0]["scheduleWaitReason"],
        "Installation identity revoked."
    );
    assert_eq!(tasks[0]["nextRun"], 1);
    assert_eq!(tasks[0]["enabled"], true);
    let audit: Vec<Value> = relay
        .get("/audit")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        audit
            .iter()
            .filter(|entry| entry["action"] == "schedule.deferred")
            .count(),
        1
    );

    tokio::fs::remove_file(&identity_path).await.unwrap();
    relay.installation.schedule().await.unwrap();
    relay.installation.schedule().await.unwrap();
    let tasks: Vec<Value> = relay
        .get("/tasks")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        tasks[0]["scheduleWaitReason"],
        "Installation is not claimed. Run cairn claim before scheduling work."
    );
    let runs: Value = relay
        .get("/runs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(runs, json!([]));
    let audit: Vec<Value> = relay
        .get("/audit")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        audit
            .iter()
            .filter(|entry| entry["action"] == "schedule.deferred")
            .count(),
        2
    );

    tokio::fs::write(&identity_path, original).await.unwrap();
    tokio::fs::set_permissions(identity_path, std::fs::Permissions::from_mode(0o600))
        .await
        .unwrap();
    relay.installation.schedule().await.unwrap();
    let tasks: Vec<Value> = relay
        .get("/tasks")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        tasks[0]["scheduleWaitReason"].is_null(),
        "Recovery stayed deferred: {}",
        tasks[0]["scheduleWaitReason"]
    );
    let runs: Vec<Value> = relay
        .get("/runs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(runs.len(), 1);
    relay.close().await;
}

#[tokio::test]
async fn a_task_preparation_error_advances_only_its_occurrence_and_is_audited_once() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let tasks = format!("/api/installations/{id}/api/tasks");
    for (name, skills) in [
        ("Invalid skill", json!(["global/missing-skill"])),
        ("Valid work", Value::Null),
    ] {
        let mut task: Value = request(&relay, &relay.cookie, &relay.session, Method::POST, &tasks)
            .json(&json!({
                "name": name,
                "prompt": "Approved work",
                "agentId": cairn_installation::config::MAIN_AGENT_ID,
                "cron": "0 9 * * *",
                "enabled": true,
                "skills": skills,
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        task["nextRun"] = 1.into();
        // Replay the persisted wait status from an earlier authority outage.
        task["scheduleWaitReason"] = "Task-author check unavailable.".into();
        relay
            .installation
            .store
            .save("tasks", task, "fixture.time")
            .await
            .unwrap();
    }
    relay.installation.schedule().await.unwrap();
    relay.installation.schedule().await.unwrap();
    let runs: Vec<Value> = relay
        .get("/runs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(runs.len(), 1);
    let run: Value = relay
        .get(&format!("/runs/{}", runs[0]["id"].as_str().unwrap()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(run["snapshot"]["task"]["name"], "Valid work");
    let tasks: Vec<Value> = relay
        .get("/tasks")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        tasks
            .iter()
            .all(|task| task["nextRun"].as_i64().unwrap() > 1)
    );
    assert!(
        tasks
            .iter()
            .all(|task| task["scheduleWaitReason"].is_null())
    );

    let audit: Vec<Value> = relay
        .get("/audit")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        audit
            .iter()
            .filter(|entry| entry["action"] == "schedule.failed")
            .count(),
        1
    );
    relay.close().await;
}

// Replaces only the authority's HTTP transport; requests still use the real relay.
struct AuthorPolicyProxy {
    identity_path: std::path::PathBuf,
    original: Vec<u8>,
    server: tokio::task::JoinHandle<()>,
}

impl AuthorPolicyProxy {
    async fn new(relay: &RelayedInstallation, routes: axum::Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://localhost:{}", listener.local_addr().unwrap().port());
        let server = tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });
        let identity_path = relay
            .installation
            .config
            .data_dir
            .join("installation-relay/identity.json");
        let original = tokio::fs::read(&identity_path).await.unwrap();
        let mut identity: Value = serde_json::from_slice(&original).unwrap();
        identity["origin"] = origin.into();
        tokio::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap())
            .await
            .unwrap();
        Self {
            identity_path,
            original,
            server,
        }
    }

    async fn close(self) {
        tokio::fs::write(self.identity_path, self.original)
            .await
            .unwrap();
        self.server.abort();
    }
}

#[tokio::test]
async fn task_pause_and_archive_need_no_beacon_round_trip() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "pause-author@example.test").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let tasks = format!("/api/installations/{id}/api/tasks");
    let mut commitments = Vec::new();
    for (author_cookie, author_session) in [(&relay.cookie, &relay.session), (&cookie, &session)] {
        let response = request(&relay, author_cookie, author_session, Method::POST, &tasks)
            .json(&json!({
                "name": "Active commitment",
                "prompt": "Approved work",
                "agentId": cairn_installation::config::MAIN_AGENT_ID,
                "cron": "0 9 * * *",
                "enabled": true,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        commitments.push(response.json::<Value>().await.unwrap());
    }
    let proxy = AuthorPolicyProxy::new(
        &relay,
        axum::Router::new().route(
            "/api/relay/{installation}/task-authors",
            axum::routing::get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        ),
    )
    .await;

    for mut task in commitments {
        let path = format!("{tasks}/{}", task["id"].as_str().unwrap());
        task["enabled"] = false.into();
        let paused = request(&relay, &cookie, &session, Method::PUT, &path)
            .json(&task)
            .send()
            .await
            .unwrap();
        assert_eq!(paused.status(), StatusCode::OK);
        let paused: Value = paused.json().await.unwrap();
        assert_eq!(paused["enabled"], false);
        assert!(paused["nextRun"].is_null());
        assert_eq!(paused["authorId"], task["authorId"]);

        task["archived"] = true.into();
        let archived = request(&relay, &cookie, &session, Method::PUT, &path)
            .json(&task)
            .send()
            .await
            .unwrap();
        assert_eq!(archived.status(), StatusCode::OK);
        assert_eq!(archived.json::<Value>().await.unwrap()["archived"], true);

        task["enabled"] = true.into();
        task["archived"] = false.into();
        assert_ne!(
            request(&relay, &cookie, &session, Method::PUT, &path)
                .json(&task)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    proxy.close().await;
    relay.close().await;
}

#[tokio::test]
async fn temporary_authority_outage_defers_schedules_without_disabling_them() {
    use axum::{extract::Path, http::HeaderMap, response::IntoResponse, routing::get};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let mut task = relay
        .installation
        .task(
            json!({
                "name": "Owner commitment",
                "prompt": "Scheduled work",
                "agentId": cairn_installation::config::MAIN_AGENT_ID,
                "cron": "0 9 * * *",
                "timezone": "UTC",
                "enabled": true,
            }),
            None,
        )
        .await
        .unwrap();
    // Only time and transport are fixture-controlled; all authorization is real.
    task["nextRun"] = 1.into();
    relay
        .installation
        .store
        .save("tasks", task.clone(), "fixture.time")
        .await
        .unwrap();
    let unavailable = Arc::new(AtomicBool::new(true));
    let gate = unavailable.clone();
    let upstream = relay.app.url.clone();
    let routes = axum::Router::new().route(
        "/api/relay/{installation}/task-authors",
        get(move |Path(id): Path<String>, headers: HeaderMap| {
            let gate = gate.clone();
            let upstream = upstream.clone();
            async move {
                if gate.load(Ordering::SeqCst) {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                let response = reqwest::Client::new()
                    .get(format!("{upstream}/api/relay/{id}/task-authors"))
                    .header("authorization", headers["authorization"].clone())
                    .send()
                    .await
                    .unwrap();
                (response.status(), response.bytes().await.unwrap()).into_response()
            }
        }),
    );
    let proxy = AuthorPolicyProxy::new(&relay, routes).await;
    relay.installation.schedule().await.unwrap();
    let stored: Vec<Value> = relay
        .get("/tasks")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stored[0]["enabled"], true);
    assert_eq!(stored[0]["nextRun"], 1);
    let runs: Value = relay
        .get("/runs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(runs, json!([]));
    unavailable.store(false, Ordering::SeqCst);
    relay.installation.schedule().await.unwrap();
    let runs: Value = relay
        .get("/runs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(runs.as_array().unwrap().len(), 1);
    assert_eq!(runs[0]["taskId"], task["id"]);
    assert_eq!(runs[0]["trigger"], "schedule");
    proxy.close().await;
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

#[tokio::test]
async fn author_removal_during_slow_preparation_prevents_late_scheduled_admission() {
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (cookie, session) = member(&relay, "slow-author@example.test").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let mut task: Value = request(
        &relay,
        &cookie,
        &session,
        Method::POST,
        &format!("/api/installations/{id}/api/tasks"),
    )
    .json(&json!({
        "name": "Slow preparation",
        "prompt": "Scheduled work",
        "agentId": cairn_installation::config::MAIN_AGENT_ID,
        "cron": "0 9 * * *",
        "timezone": "UTC",
        "enabled": true,
    }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    task["nextRun"] = 1.into();
    relay
        .installation
        .store
        .save("tasks", task, "fixture.time")
        .await
        .unwrap();
    let directory = relay.installation.config.home.join(".agents/skills/block");
    tokio::fs::create_dir_all(&directory).await.unwrap();
    let fifo = directory.join("SKILL.md");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let installation = relay.installation.clone();
    let scheduled = tokio::spawn(async move { installation.schedule().await });
    // Writer open is a deterministic barrier: snapshot opened the real skill,
    // but its read cannot finish until the writer closes after the withdrawal.
    let mut writer = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::fs::OpenOptions::new().write(true).open(&fifo),
    )
    .await
    .unwrap()
    .unwrap();
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
    writer
        .write_all(b"---\nname: block\ndescription: Regression fixture.\n---\nScheduled work.\n")
        .await
        .unwrap();
    drop(writer);
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(5), scheduled)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let runs: Value = relay
        .get("/runs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        runs,
        json!([]),
        "A prepared snapshot is not work already admitted before removal"
    );
    let tasks: Vec<Value> = relay
        .get("/tasks")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tasks[0]["enabled"], false);
    relay.close().await;
}
