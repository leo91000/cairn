use super::test_common as common;

use super::{Duration, Relay};
use common::RelayedInstallation;
use reqwest::StatusCode;
use sqlx_core::query::query;
use std::future::Future;
use tokio::time::{Instant, advance};

// Observe the periodic monitor, independently of the setup identity queries.
// Only completed SQL errors count as database failures, not inferred elapsed time
// or cancellation by the monitor's outer timeout.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum CheckOutcome {
    Current,
    Revoked,
    DatabaseError,
    TimedOut,
}

#[derive(Default)]
pub(super) struct IdentityMonitor {
    pub started: usize,
    pub completed: Vec<CheckOutcome>,
}

async fn with_frozen_time(assertions: impl Future<Output = ()>) {
    tokio::time::pause();
    // Real HTTP/Postgres I/O must progress without Tokio automatically jumping
    // to the next timer whenever the runtime would otherwise become idle.
    let clock_guard = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let (finished, cancellation) = std::sync::mpsc::channel();
    let (expired, watchdog) = tokio::sync::oneshot::channel();
    let watchdog_thread = std::thread::spawn(move || {
        if cancellation.recv_timeout(Duration::from_secs(35)).is_err() {
            let _ = expired.send(());
        }
    });
    tokio::select! {
        biased;

        _ = watchdog => panic!("real HTTP/Postgres I/O stalled while Tokio time was frozen"),
        _ = assertions => {},
    }
    finished.send(()).unwrap();
    watchdog_thread.join().unwrap();
    clock_guard.abort();
    let _ = clock_guard.await;
    tokio::time::resume();
}

async fn begin_next_check(relay: &Relay) -> usize {
    let previous = {
        let monitor = relay.identity_monitor.lock().unwrap();
        assert_eq!(monitor.started, monitor.completed.len());
        monitor.started
    };
    let deadline = Instant::now() + Duration::from_secs(30) + Duration::from_millis(1);
    loop {
        if relay.identity_monitor.lock().unwrap().started > previous {
            return previous;
        }
        assert!(Instant::now() < deadline, "the periodic monitor must run");
        advance(Duration::from_millis(1)).await;
    }
}

async fn completed_check(relay: &Relay, index: usize) -> CheckOutcome {
    loop {
        if let Some(outcome) = relay
            .identity_monitor
            .lock()
            .unwrap()
            .completed
            .get(index)
            .copied()
        {
            return outcome;
        }
        tokio::task::yield_now().await;
    }
}

async fn failed_check(relay: &Relay) {
    let index = begin_next_check(relay).await;
    // begin_next_check observes the real query future after its first poll.
    // The fixture's held sole connection makes SQLx's two-second acquire timer
    // fail before the monitor's five-second timeout. Freeze again for I/O.
    advance(Duration::from_secs(2) + Duration::from_millis(1)).await;
    assert_eq!(
        completed_check(relay, index).await,
        CheckOutcome::DatabaseError
    );
}

async fn streaming_fixture() -> (
    RelayedInstallation,
    tokio::sync::broadcast::Sender<&'static str>,
    reqwest::Response,
) {
    use axum::{body::Body, routing::get};

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
    let relay = RelayedInstallation::with_pool_size(routes, 1).await;
    let mut stream = relay.get("/fixture/session/stream").send().await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    chunks.send("opened").unwrap();
    assert_eq!(stream.chunk().await.unwrap().unwrap(), "opened");
    (relay, chunks, stream)
}

#[tokio::test]
async fn a_temporary_database_failure_preserves_an_established_tunnel_and_stream() {
    let (relay, chunks, mut stream) = streaming_fixture().await;
    with_frozen_time(async {
        let mut unavailable = relay.app.pool.acquire().await.unwrap();
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
        failed_check(&relay.app.relay).await;
        assert!(!*tunnel.stop.borrow());
        chunks.send("during failure").unwrap();
        assert_eq!(stream.chunk().await.unwrap().unwrap(), "during failure");

        // Drop spawns SQLx's asynchronous return/ping task. Await that work and
        // observe an idle connection before making a finite API request.
        unavailable.return_to_pool().await;
        assert_eq!(relay.app.pool.num_idle(), 1);
        assert_eq!(
            relay.get("/chats").send().await.unwrap().status(),
            StatusCode::OK
        );

        let index = begin_next_check(&relay.app.relay).await;
        assert_eq!(
            completed_check(&relay.app.relay, index).await,
            CheckOutcome::Current
        );
        let current = relay
            .app
            .relay
            .connections
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .clone();
        assert!(
            std::sync::Arc::ptr_eq(&tunnel, &current),
            "recovery must preserve the established tunnel generation"
        );
        chunks.send("after recovery").unwrap();
        assert_eq!(stream.chunk().await.unwrap().unwrap(), "after recovery");
    })
    .await;
    drop(stream);
    relay.close().await;
}

#[tokio::test]
async fn an_established_tunnel_closes_after_three_consecutive_database_failures() {
    let (relay, chunks, mut stream) = streaming_fixture().await;
    with_frozen_time(async {
        let unavailable = relay.app.pool.acquire().await.unwrap();
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
        for chunk in ["first failure", "second failure"] {
            failed_check(&relay.app.relay).await;
            assert!(
                !*tunnel.stop.borrow(),
                "two failed checks must remain within the retry budget"
            );
            chunks.send(chunk).unwrap();
            assert_eq!(stream.chunk().await.unwrap().unwrap(), chunk);
        }
        failed_check(&relay.app.relay).await;
        assert!(
            *tunnel.stop.borrow(),
            "the third failure must stop the tunnel"
        );
        assert!(
            stream.chunk().await.unwrap().is_none(),
            "the third failure must close the stream"
        );
        drop(unavailable);
    })
    .await;
    drop(stream);
    relay.close().await;
}

#[tokio::test]
async fn deleting_the_owner_revokes_an_established_installation_tunnel() {
    let (relay, _chunks, mut stream) = streaming_fixture().await;
    with_frozen_time(async {
        query("DELETE FROM cairn_accounts WHERE id = $1")
            .bind(relay.session["account"]["id"].as_str().unwrap())
            .execute(&relay.app.pool)
            .await
            .unwrap();
        let index = begin_next_check(&relay.app.relay).await;
        assert_eq!(
            completed_check(&relay.app.relay, index).await,
            CheckOutcome::Revoked
        );
        assert!(
            stream.chunk().await.unwrap().is_none(),
            "persisted owner revocation must end the existing stream"
        );
        assert_eq!(
            relay.get("/chats").send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    })
    .await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while !relay.connector.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the revoked machine identity must stop reconnecting");
    drop(stream);
    relay.close().await;
}
