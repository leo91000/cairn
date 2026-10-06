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

#[tokio::test]
async fn revoking_other_devices_preserves_the_caller_and_can_revoke_the_current_session() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&relay.app.pool).await.unwrap();
    let (other_cookie, _) = login(&relay.app, "relay-owner@example.test").await;
    let (stranger_cookie, _) = login(&relay.app, "stranger@example.test").await;
    let route = format!("{}/api/account/sessions/revoke-others", relay.app.url);
    for (origin, csrf) in [
        (&relay.app.url[..], "wrong"),
        (
            "https://foreign.example",
            relay.session["csrf"].as_str().unwrap(),
        ),
    ] {
        let rejected = relay
            .app
            .client
            .post(&route)
            .header("origin", origin)
            .header("cookie", &relay.cookie)
            .header("x-csrf-token", csrf)
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    }
    let revoked = relay
        .app
        .client
        .post(&route)
        .header("origin", &relay.app.url)
        .header("cookie", &relay.cookie)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        relay
            .app
            .client
            .get(format!("{}/chats", relay.base))
            .header("cookie", other_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let remaining: Value = relay
        .app
        .client
        .get(format!("{}/api/account/sessions", relay.app.url))
        .header("cookie", &relay.cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(remaining["sessions"].as_array().unwrap().len(), 1);
    let current = remaining["sessions"][0]["id"].as_str().unwrap();
    let revoked = relay
        .app
        .client
        .delete(format!("{}/api/account/sessions/{current}", relay.app.url))
        .header("origin", &relay.app.url)
        .header("cookie", &relay.cookie)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    assert!(
        revoked.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let stranger: Value = relay
        .app
        .client
        .get(format!("{}/api/account/session", relay.app.url))
        .header("cookie", stranger_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stranger["authenticated"], true);
    relay.close().await;
}

#[tokio::test]
async fn sessions_show_a_bounded_device_description_and_hide_expired_devices() {
    let app = common::Fixture::new().await;
    let challenge: Value = app
        .post(
            "/api/account/email-code",
            serde_json::json!({ "email": "devices@example.test" }),
        )
        .await
        .json()
        .await
        .unwrap();
    let code = app.mail.0.lock().unwrap().last().unwrap().1.clone();
    let signed_in = app
        .client
        .post(format!("{}/api/account/verify", app.url))
        .header("origin", &app.url)
        .header("user-agent", "Lost phone browser")
        .json(&serde_json::json!({ "challenge": challenge["challenge"], "code": code }))
        .send()
        .await
        .unwrap();
    let cookie = signed_in.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let sessions: Value = app
        .client
        .get(format!("{}/api/account/sessions", app.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sessions["sessions"][0]["device"], "Lost phone browser");
    assert!(
        sessions["sessions"][0]["createdAt"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    query("UPDATE web_sessions SET expires_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    let expired = app
        .client
        .get(format!("{}/api/account/sessions", app.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&app.pool).await.unwrap();
    let (cookie, _) = login(&app, "devices@example.test").await;
    let sessions: Value = app
        .client
        .get(format!("{}/api/account/sessions", app.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(sessions["sessions"][0]["current"], true);
    app.close().await;
}
