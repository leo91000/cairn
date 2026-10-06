mod common;

use common::{RelayedInstallation, login};
use reqwest::StatusCode;
use serde_json::Value;
use sqlx_core::query::query;
use std::time::Duration;

#[tokio::test]
async fn an_account_lists_and_revokes_only_its_own_sessions_and_live_streams() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&relay.app.pool).await.unwrap();
    let (other_cookie, _) = login(&relay.app, "relay-owner@example.test").await;
    let (stranger_cookie, stranger_session) = login(&relay.app, "stranger@example.test").await;
    let response = relay
        .app
        .client
        .get(format!("{}/api/account/sessions", relay.app.url))
        .header("cookie", &relay.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let sessions: Value = response.json().await.unwrap();
    let sessions = sessions["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        sessions
            .iter()
            .filter(|session| session["current"] == true)
            .count(),
        1
    );
    let target = sessions
        .iter()
        .find(|session| session["current"] == false)
        .unwrap();
    let route = format!(
        "{}/api/account/sessions/{}",
        relay.app.url,
        target["id"].as_str().unwrap()
    );
    let rejected = relay
        .app
        .client
        .delete(&route)
        .header("origin", &relay.app.url)
        .header("cookie", stranger_cookie)
        .header("x-csrf-token", stranger_session["csrf"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::NOT_FOUND);
    let mut stream = relay
        .app
        .client
        .get(format!("{}/chats/stream", relay.base))
        .header("cookie", &other_cookie)
        .send()
        .await
        .unwrap();
    stream.chunk().await.unwrap().unwrap();
    let revoked = relay
        .app
        .client
        .delete(&route)
        .header("origin", &relay.app.url)
        .header("cookie", &relay.cookie)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Ok(Some(_)) = stream.chunk().await {}
    })
    .await
    .expect("individual session revocation must end its idle relay stream immediately");
    assert_eq!(
        relay
            .app
            .client
            .get(format!("{}/chats", relay.base))
            .header("cookie", &other_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    let serialized = serde_json::to_string(sessions).unwrap();
    assert!(!serialized.contains("digest"));
    assert!(!serialized.contains("csrf"));
    assert!(!serialized.contains(other_cookie.split('=').nth(1).unwrap()));
    drop(stream);
    relay.close().await;
}
