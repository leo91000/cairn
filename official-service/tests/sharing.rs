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
        .header("x-leo-role", "owner")
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
    use leo_agent_manager::config::MAIN_AGENT_ID;
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
        let image = include_bytes!("../../tests/fixtures/artifacts/thumbnail.png");
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
async fn relay_rate_limits_are_per_leo_account_and_headers_cannot_choose_a_bucket() {
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
                "x-leo-account-id",
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
    for _ in 0..leo_relay_protocol::MAX_IN_FLIGHT {
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
        let mut uploads: Vec<_> = (0..leo_relay_protocol::MAX_IN_FLIGHT)
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
            "x-leo-account-id",
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
