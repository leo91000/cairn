mod common;

use common::{RelayedInstallation, login};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn deleting_an_owner_detaches_sharing_and_preserves_data_for_a_new_claim() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sharing = format!("/api/installations/{id}/sharing");
    let chat: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/api/chats"),
        )
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let (member_cookie, member_session) = login(app, "member@example.test").await;
    let invite: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("{sharing}/invitations"),
        )
        .json(&json!({ "email": "member@example.test" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let accepted = app
        .authenticated(
            &member_cookie,
            &member_session,
            Method::POST,
            &format!(
                "/api/account/invitations/{}/accept",
                invite["id"].as_str().unwrap()
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
    let pending = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("{sharing}/invitations"),
        )
        .json(&json!({ "email": "pending@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(pending.status(), StatusCode::CREATED);
    let mut owner_stream = relay.get("/chats/stream").send().await.unwrap();
    owner_stream.chunk().await.unwrap().unwrap();
    let mut member_stream = app
        .client
        .get(format!("{}/chats/stream", relay.base))
        .header("cookie", &member_cookie)
        .send()
        .await
        .unwrap();
    member_stream.chunk().await.unwrap().unwrap();

    let mismatched = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "someone-else@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(mismatched.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    let deleted = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(
        deleted.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    for stream in [&mut owner_stream, &mut member_stream] {
        tokio::time::timeout(Duration::from_secs(1), async {
            while let Ok(Some(_)) = stream.chunk().await {}
        })
        .await
        .expect("account deletion must close all owned tunnels immediately");
    }
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.client
            .get(format!("{}/chats", relay.base))
            .header("cookie", &member_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let member: Value = app
        .authenticated(
            &member_cookie,
            &member_session,
            Method::GET,
            "/api/account/session",
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(member["authenticated"], true);
    assert_eq!(member["installations"], json!([]));
    tokio::time::timeout(Duration::from_secs(3), &mut relay.connector)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let directory = relay
        .installation
        .config
        .data_dir
        .join("installation-relay");
    let (display, receipt) = tokio::sync::oneshot::channel();
    let claim_directory = directory.clone();
    let claim = tokio::spawn(async move {
        leo_agent_manager::relay::device_claim(
            None,
            &claim_directory,
            "Recovered",
            tokio_util::sync::CancellationToken::new(),
            move |_, code, _, _| {
                display.send(code.to_owned()).unwrap();
            },
        )
        .await
    });
    let user_code = receipt.await.unwrap();
    let (new_cookie, new_session) = login(app, "new-owner@example.test").await;
    let preview: Value = app
        .authenticated(
            &new_cookie,
            &new_session,
            Method::POST,
            "/api/installations/device-claim/preview",
        )
        .json(&json!({ "code": user_code }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let approved = app
        .authenticated(
            &new_cookie,
            &new_session,
            Method::POST,
            "/api/installations/device-claim",
        )
        .json(&json!({ "code": user_code, "confirmation": preview["confirmation"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(5), claim)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let router = leo_agent_manager::http::router(relay.installation.clone())
        .await
        .unwrap();
    relay.connector = tokio::spawn(leo_agent_manager::relay::connect(
        directory,
        router,
        relay.stop.clone(),
    ));
    relay.cookie = new_cookie;
    relay.session = new_session;
    let chats = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = relay.get("/chats").send().await.unwrap();
            if response.status() == StatusCode::OK {
                break response.json::<Value>().await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(chats[0]["id"], chat["id"]);
    let sharing: Value = relay
        .app
        .authenticated(&relay.cookie, &relay.session, Method::GET, &sharing)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sharing["members"], json!([]));
    assert_eq!(sharing["invitations"], json!([]));
    drop(owner_stream);
    drop(member_stream);
    relay.close().await;
}

#[tokio::test]
async fn deleting_a_member_preserves_the_owners_installation_and_requires_current_authorization() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let sharing = format!("/api/installations/{id}/sharing");
    let (cookie, session) = login(app, "departing@example.test").await;
    let invite: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("{sharing}/invitations"),
        )
        .json(&json!({ "email": "departing@example.test" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let accepted = app
        .authenticated(
            &cookie,
            &session,
            Method::POST,
            &format!(
                "/api/account/invitations/{}/accept",
                invite["id"].as_str().unwrap()
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
    for (origin, csrf, expected) in [
        (&app.url[..], "wrong", StatusCode::FORBIDDEN),
        (
            "https://foreign.example",
            session["csrf"].as_str().unwrap(),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let rejected = app
            .client
            .post(format!("{}/api/account/delete", app.url))
            .header("origin", origin)
            .header("cookie", &cookie)
            .header("x-csrf-token", csrf)
            .json(&json!({ "email": "departing@example.test" }))
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), expected);
    }
    let mut stream = app
        .client
        .get(format!("{}/chats/stream", relay.base))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    stream.chunk().await.unwrap().unwrap();
    let deleted = app
        .authenticated(&cookie, &session, Method::POST, "/api/account/delete")
        .json(&json!({ "email": "departing@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Ok(Some(_)) = stream.chunk().await {}
    })
    .await
    .expect("deleting a member must close only their live access");
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    let shared: Value = app
        .authenticated(&relay.cookie, &relay.session, Method::GET, &sharing)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(shared["members"], json!([]));
    let replayed = app
        .authenticated(&cookie, &session, Method::POST, "/api/account/delete")
        .json(&json!({ "email": "departing@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(replayed.status(), StatusCode::UNAUTHORIZED);
    drop(stream);
    relay.close().await;
}
