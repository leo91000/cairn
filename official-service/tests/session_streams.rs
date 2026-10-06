mod common;

use common::{RelayedInstallation, login};
use reqwest::StatusCode;
use serde_json::json;
use sqlx_core::query::query;
use std::time::Duration;

#[tokio::test]
async fn logout_ends_idle_streams_of_that_session_and_preserves_another_device() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&relay.app.pool).await.unwrap();
    let (other_cookie, other_session) = login(&relay.app, "relay-owner@example.test").await;
    let mut revoked = relay.get("/chats/stream").send().await.unwrap();
    revoked.chunk().await.unwrap().unwrap();
    let mut other = relay
        .app
        .client
        .get(format!("{}/chats/stream", relay.base))
        .header("cookie", &other_cookie)
        .send()
        .await
        .unwrap();
    other.chunk().await.unwrap().unwrap();
    let response = relay
        .app
        .client
        .post(format!("{}/api/account/logout", relay.app.url))
        .header("origin", &relay.app.url)
        .header("cookie", &relay.cookie)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Ok(Some(_)) = revoked.chunk().await {}
    })
    .await
    .expect("logout must close idle streams in other tabs of the same session");
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let response = relay
        .app
        .client
        .post(format!("{}/chats", relay.base))
        .header("origin", &relay.app.url)
        .header("cookie", &other_cookie)
        .header("x-csrf-token", other_session["csrf"].as_str().unwrap())
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), other.chunk())
            .await
            .unwrap()
            .unwrap()
            .is_some()
    );
    drop(other);
    relay.close().await;
}

#[tokio::test]
async fn expiration_ends_idle_streams_without_stopping_the_installation() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    query("UPDATE web_sessions SET expires_at = clock_timestamp() + interval '2 seconds'")
        .execute(&relay.app.pool)
        .await
        .unwrap();
    let mut stream = relay.get("/chats/stream").send().await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    stream.chunk().await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Ok(Some(_)) = stream.chunk().await {}
    })
    .await
    .expect("an idle stream must end when its browser session expires");
    tokio::time::timeout(Duration::from_secs(1), async {
        while relay.get("/chats").send().await.unwrap().status() != StatusCode::UNAUTHORIZED {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the expired session must also lose finite API access");
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&relay.app.pool).await.unwrap();
    let (new_cookie, _) = login(&relay.app, "relay-owner@example.test").await;
    assert_eq!(
        relay
            .app
            .client
            .get(format!("{}/chats", relay.base))
            .header("cookie", new_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn a_temporary_database_failure_preserves_an_established_tunnel_and_stream() {
    let relay = RelayedInstallation::with_pool_size(axum::Router::new(), 1).await;
    let mut stream = relay.get("/chats/stream").send().await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    stream.chunk().await.unwrap().unwrap();
    let unavailable = relay.app.pool.acquire().await.unwrap();
    // Keep the sole connection busy across the persisted revocation check.
    // The fixture's two-second acquire timeout produces a real database error.
    let restored = tokio::time::Instant::now() + Duration::from_secs(35);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(restored) => break,
            chunk = stream.chunk() => {
                assert!(
                    matches!(chunk, Ok(Some(_))),
                    "a transient database error must not close an established stream"
                );
            }
        }
    }
    drop(unavailable);
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    drop(stream);
    relay.close().await;
}

#[tokio::test]
async fn an_established_tunnel_closes_after_three_consecutive_database_failures() {
    let relay = RelayedInstallation::with_pool_size(axum::Router::new(), 1).await;
    let mut stream = relay.get("/chats/stream").send().await.unwrap();
    stream.chunk().await.unwrap().unwrap();
    let unavailable = relay.app.pool.acquire().await.unwrap();
    let grace = tokio::time::Instant::now() + Duration::from_secs(85);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(grace) => break,
            chunk = stream.chunk() => {
                assert!(
                    matches!(chunk, Ok(Some(_))),
                    "two failed checks must remain within the retry budget"
                );
            }
        }
    }
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Ok(Some(_)) = stream.chunk().await {}
    })
    .await
    .expect("an unverifiable identity must close after the third failed check");
    drop(unavailable);
    drop(stream);
    relay.close().await;
}

#[tokio::test]
async fn deleting_the_owner_revokes_an_established_installation_tunnel() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let mut stream = relay.get("/chats/stream").send().await.unwrap();
    stream.chunk().await.unwrap().unwrap();
    query("DELETE FROM leo_accounts WHERE id = $1")
        .bind(relay.session["account"]["id"].as_str().unwrap())
        .execute(&relay.app.pool)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(35), async {
        while let Ok(Some(_)) = stream.chunk().await {}
    })
    .await
    .expect("the persisted owner revocation must end the existing tunnel");
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while !relay.connector.is_finished() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the revoked machine identity must stop reconnecting");
    drop(stream);
    relay.close().await;
}

#[tokio::test]
async fn session_expiry_is_not_delayed_by_a_database_revocation_check() {
    let relay = RelayedInstallation::with_pool_size(axum::Router::new(), 1).await;
    query("UPDATE web_sessions SET expires_at = clock_timestamp() + interval '30 seconds'")
        .execute(&relay.app.pool)
        .await
        .unwrap();
    let mut stream = relay.get("/chats/stream").send().await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    stream.chunk().await.unwrap().unwrap();
    let unavailable = relay.app.pool.acquire().await.unwrap();
    tokio::time::timeout(Duration::from_secs(31), async {
        while let Ok(Some(_)) = stream.chunk().await {}
    })
    .await
    .expect("session expiry must not wait for a failing database check");
    drop(unavailable);
    drop(stream);
    relay.close().await;
}
