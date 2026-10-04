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
