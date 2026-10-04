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
