mod common;

use common::RelayedInstallation;
use reqwest::StatusCode;
use std::time::Duration;

#[tokio::test]
async fn an_idle_stream_is_closed_immediately_when_the_installation_is_detached() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let mut response = relay.get("/chats/stream").send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.chunk().await.unwrap().unwrap();
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    // Detachment's account-management endpoint belongs to another ticket.
    // Its committed ownership removal and transport hook are exercised here;
    // assertions still observe only the beacon HTTP API and its open body.
    sqlx_core::query::query("DELETE FROM installations WHERE id = $1")
        .bind(id)
        .execute(&relay.app.pool)
        .await
        .unwrap();
    relay.app.relay.revoke_access(id, None);
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Ok(Some(_)) = response.chunk().await {}
    })
    .await
    .expect("an idle detached stream must end without waiting for a heartbeat");
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    assert!(!relay.installation.shutdown.is_cancelled());
    relay.close().await;
}

#[tokio::test]
async fn access_revocation_targets_the_account_and_preserves_the_tunnel() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let account = relay.session["account"]["id"].as_str().unwrap();
    let mut stream = relay.get("/chats/stream").send().await.unwrap();
    stream.chunk().await.unwrap().unwrap();
    relay.app.relay.revoke_access(id, Some("unrelated-account"));
    let response = relay
        .app
        .client
        .post(format!("{}/chats", relay.base))
        .header("cookie", &relay.cookie)
        .header("origin", &relay.app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), stream.chunk())
            .await
            .unwrap()
            .unwrap()
            .is_some()
    );

    relay.app.relay.revoke_access(id, Some(account));
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Ok(Some(_)) = stream.chunk().await {}
    })
    .await
    .expect("revoked clients must immediately stop receiving even idle streams");
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}
