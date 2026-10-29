use super::test_common as common;

use super::{Duration, Relay};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::time::Instant;

// Server-side queries can outlive cancelled SQLx futures. Record the actual
// application future lifetime so database statistics cannot mask cancellation.
#[derive(Default)]
pub(super) struct IdentityChecks {
    next: usize,
    pending: HashMap<usize, Instant>,
}

pub(super) struct IdentityCheck {
    checks: Arc<Mutex<IdentityChecks>>,
    id: usize,
}

pub(super) fn track_identity_check(relay: &Relay) -> IdentityCheck {
    let mut checks = relay.identity_checks.lock().unwrap();
    let id = checks.next;
    checks.next += 1;
    checks.pending.insert(id, Instant::now());
    IdentityCheck {
        checks: relay.identity_checks.clone(),
        id,
    }
}

impl Drop for IdentityCheck {
    fn drop(&mut self) {
        self.checks.lock().unwrap().pending.remove(&self.id);
    }
}

#[tokio::test]
async fn session_expiry_is_not_delayed_by_a_database_revocation_check() {
    use axum::{body::Body, routing::get};
    use common::{RelayedInstallation, login};
    use reqwest::StatusCode;
    use sqlx_core::query::query;
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

    let (session,): (String,) = query_as("SELECT digest FROM web_sessions WHERE csrf = $1")
        .bind(relay.session["csrf"].as_str().unwrap())
        .fetch_one(&relay.app.pool)
        .await
        .unwrap();
    let tunnel = relay
        .app
        .relay
        .connections
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    let (stream_id, mut revoked) = {
        let access = tunnel.access.lock().unwrap();
        let (id, registered) = access
            .streams
            .iter()
            .find(|(_, registered)| registered.session.as_deref() == Some(session.as_str()))
            .unwrap();
        (id.clone(), registered.revoked.subscribe())
    };

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
        let (pending_since, pending_check) = loop {
            if let Some(query_start) = blocked_query_start().await {
                let checks = relay.app.relay.identity_checks.lock().unwrap();
                if let Some((&id, &entered)) = checks.pending.iter().next() {
                    assert_eq!(checks.pending.len(), 1);
                    break (query_start, (id, entered));
                }
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the real revocation query and its application future must become pending"
            );
            advance(Duration::from_millis(1)).await;
        };

        // Configure the cached authorization deadline through the private stream
        // fixture seam, after all network/DB setup and actual check admission.
        // The existing integration test covers registration from SQL expiry.
        let expiry = Instant::now() + Duration::from_millis(500);
        // Tokio timers have millisecond resolution. Cross one timer tick while
        // retaining a tighter bound than the original one-second observation slack.
        let observed = expiry + Duration::from_millis(1);
        assert!(observed <= started + Duration::from_secs(31));
        assert!(observed < pending_check.1 + Duration::from_secs(5));
        {
            let mut access = tunnel.access.lock().unwrap();
            let registered = access.streams.get_mut(&stream_id).unwrap();
            assert!(!*registered.revoked.borrow());
            registered.expires_at = Some(expiry);
        }
        tunnel.access_changed.notify_one();

        advance(expiry - Duration::from_millis(1) - Instant::now()).await;
        chunks.send("before expiry").unwrap();
        assert_eq!(
            stream.chunk().await.unwrap().as_deref(),
            Some("before expiry".as_bytes()),
            "the established stream must remain usable while the database query is pending"
        );
        assert_eq!(other.chunk().await.unwrap().unwrap(), "before expiry");
        assert_eq!(blocked_query_start().await.as_ref(), Some(&pending_since));

        assert_eq!(
            relay
                .app
                .relay
                .identity_checks
                .lock()
                .unwrap()
                .pending
                .get(&pending_check.0),
            Some(&pending_check.1)
        );
        assert!(!*revoked.borrow());
        assert!(Instant::now() < expiry);
        advance(observed - Instant::now()).await;
        revoked.changed().await.unwrap();
        assert!(*revoked.borrow());
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
        assert_eq!(
            relay
                .app
                .relay
                .identity_checks
                .lock()
                .unwrap()
                .pending
                .get(&pending_check.0),
            Some(&pending_check.1)
        );
        assert_eq!(Instant::now(), observed);
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
