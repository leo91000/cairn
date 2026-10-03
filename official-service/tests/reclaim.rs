mod common;

use common::{RelayedInstallation, login};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn detach_revokes_the_tunnel_and_keeps_installation_data() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let response = relay
        .app
        .client
        .post(format!("{}/chats", relay.base))
        .header("origin", &relay.app.url)
        .header("cookie", &relay.cookie)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let chat: Value = response.json().await.unwrap();
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let endpoint = format!("{}/api/installations/{id}/detach", relay.app.url);
    let (foreign, foreign_session) = login(&relay.app, "foreign@example.test").await;
    for (cookie, csrf, expected) in [
        (relay.cookie.as_str(), "", StatusCode::FORBIDDEN),
        (
            foreign.as_str(),
            foreign_session["csrf"].as_str().unwrap(),
            StatusCode::NOT_FOUND,
        ),
        (
            relay.cookie.as_str(),
            relay.session["csrf"].as_str().unwrap(),
            StatusCode::NO_CONTENT,
        ),
    ] {
        let response = relay
            .app
            .client
            .post(&endpoint)
            .header("origin", &relay.app.url)
            .header("cookie", cookie)
            .header("x-csrf-token", csrf)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let session: Value = relay
        .app
        .client
        .get(format!("{}/api/account/session", relay.app.url))
        .header("cookie", &relay.cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session["installations"], json!([]));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !relay.connector.is_finished() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a revoked identity must stop reconnecting and allow leo claim");
    assert_eq!(
        relay.installation.chat_list().await.unwrap()[0]["id"],
        chat["id"]
    );
    relay.close().await;
}

#[tokio::test]
async fn deleting_the_owner_revokes_the_active_tunnel_without_losing_the_installation() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    // Account management is #59. Deleting the account is a fixture action at
    // the external database seam; all access/revocation assertions use HTTP.
    sqlx_core::query::query("DELETE FROM leo_accounts WHERE id = $1")
        .bind(relay.session["account"]["id"].as_str().unwrap())
        .execute(&relay.app.pool)
        .await
        .unwrap();
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !relay.connector.is_finished() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("deleting the owner must disconnect the active tunnel");
    relay.close().await;
}
