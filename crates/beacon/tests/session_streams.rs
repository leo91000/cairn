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
    query("DELETE FROM cairn_accounts WHERE id = $1")
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
    use axum::{body::Body, routing::get};
    use sqlx_core::query_as::query_as;
    use tokio::time::{Instant, advance, pause, resume};

    let (chunks, _) = tokio::sync::broadcast::channel::<&'static str>(4);
    let subscription = chunks.clone();
    let routes = axum::Router::new().route(
        "/api/fixture/session/stream",
        get(move || {
            let receiver = subscription.subscribe();
            async move {
                let body = futures_util::stream::unfold(receiver, |mut receiver| async move {
                    receiver
                        .recv()
                        .await
                        .ok()
                        .map(|chunk| (Ok::<_, std::io::Error>(chunk), receiver))
                });
                Body::from_stream(body)
            }
        }),
    );
    let tunnel_setup_started = Instant::now();
    let relay = RelayedInstallation::with_pool_size(routes, 1).await;

    // Age the established tunnel's revocation timer, without a wall-clock wait,
    // so its first check starts before the new stream's 30-second expiry.
    pause();
    advance(Duration::from_secs(1)).await;
    resume();

    query("UPDATE web_sessions SET expires_at = clock_timestamp() + interval '30 seconds'")
        .execute(&relay.app.pool)
        .await
        .unwrap();
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&relay.app.pool).await.unwrap();
    let (other_cookie, _) = login(&relay.app, "relay-owner@example.test").await;
    let mut other = relay
        .app
        .client
        .get(format!("{}/fixture/session/stream", relay.base))
        .header("cookie", other_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), StatusCode::OK);

    let mut stream = relay.get("/fixture/session/stream").send().await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    chunks.send("opened").unwrap();
    assert_eq!(stream.chunk().await.unwrap().unwrap(), "opened");
    assert_eq!(other.chunk().await.unwrap().unwrap(), "opened");

    let (connection,): (i32,) = query_as("SELECT pg_backend_pid()")
        .fetch_one(&relay.app.pool)
        .await
        .unwrap();
    let mut blocked = relay.app.admin.begin().await.unwrap();
    let (blocker,): (i32,) = query_as("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocked)
        .await
        .unwrap();
    query(&format!(
        "LOCK TABLE {}.installations IN ACCESS EXCLUSIVE MODE",
        relay.app.schema
    ))
    .execute(&mut *blocked)
    .await
    .unwrap();

    let blocked_query_start = || async {
        let waiting: Option<(String,)> = query_as(
            "SELECT query_start::text FROM pg_stat_activity WHERE pid = $1 AND state = 'active' AND wait_event_type = 'Lock' AND query = 'SELECT id FROM installations WHERE id = $1 AND token_digest = $2 AND owner_id IS NOT NULL' AND $2 = ANY(pg_blocking_pids(pid))",
        )
        .bind(connection)
        .bind(blocker)
        .fetch_optional(&relay.app.admin)
        .await
        .unwrap();
        waiting.map(|(started,)| started)
    };

    pause();
    // Paused Tokio time normally jumps forward whenever no task is runnable.
    // Keep it fixed while the real HTTP and Postgres sockets make progress.
    let clock_guard = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let started = Instant::now();

    // This is an infrastructure watchdog, using the original 31-second bound.
    // Security assertions below use only explicitly advanced Tokio time.
    let (finished, cancellation) = std::sync::mpsc::channel();
    let (expired, watchdog) = tokio::sync::oneshot::channel();
    let watchdog_thread = std::thread::spawn(move || {
        if cancellation.recv_timeout(Duration::from_secs(31)).is_err() {
            let _ = expired.send(());
        }
    });
    let assertions = async {
        // The first check cannot precede tunnel setup plus 30 seconds. Use an
        // earlier bound and subtract setup time, including any host suspension.
        let before_check = tunnel_setup_started + Duration::from_secs(28);
        if Instant::now() < before_check {
            advance(before_check - Instant::now()).await;
        }
        let pending_since = loop {
            if let Some(query_start) = blocked_query_start().await {
                break query_start;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the real revocation query must start before stream expiry"
            );
            advance(Duration::from_millis(1)).await;
        };

        chunks.send("before expiry").unwrap();
        assert_eq!(
            stream.chunk().await.unwrap().as_deref(),
            Some("before expiry".as_bytes()),
            "the established stream must remain usable while the database query is pending"
        );
        assert_eq!(other.chunk().await.unwrap().unwrap(), "before expiry");
        assert_eq!(blocked_query_start().await.as_ref(), Some(&pending_since));

        advance(Duration::from_secs(31) - started.elapsed()).await;
        assert!(
            stream.chunk().await.unwrap().is_none(),
            "session expiry must close the stream without waiting for the database query"
        );
        assert_eq!(
            blocked_query_start().await.as_ref(),
            Some(&pending_since),
            "the same database query must still be blocked after expiry closes the stream"
        );
        chunks.send("after expiry").unwrap();
        assert_eq!(
            other.chunk().await.unwrap().unwrap(),
            "after expiry",
            "expiry must revoke only the expiring session, not the established tunnel"
        );
        assert_eq!(blocked_query_start().await.as_ref(), Some(&pending_since));
        assert_eq!(started.elapsed(), Duration::from_secs(31));
    };
    tokio::select! {
        biased;

        _ = watchdog => panic!("real HTTP/Postgres I/O stalled while Tokio time was frozen"),
        _ = assertions => {},
    }
    finished.send(()).unwrap();
    watchdog_thread.join().unwrap();
    clock_guard.abort();
    let _ = clock_guard.await;
    resume();

    blocked.rollback().await.unwrap();
    drop(stream);
    drop(other);
    relay.close().await;
}
