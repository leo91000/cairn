//! Authenticated execution interface shared by chats, scheduled tasks and recovery.
use crate::runner::exclusive::Reservations;

mod broker;
mod client;
pub(crate) mod exclusive;
pub(crate) mod logs;
mod routes;

pub(crate) use broker::validate as validate_plan;
pub(crate) use client::{ControllerResult, wait_for_exit};
pub use client::{TerminalReason, client};

use crate::{
    error::{Error, Result},
    microvm::{host, pool::Pool},
    skills::{atomic_write, private_dir},
};
use axum::{Router, routing::any};
use broker::Broker;
use std::{
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::PathBuf,
};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

pub const CONTROLLER_INTERRUPTED: i32 = 75;
/// The controller itself failed while running the attempt (EX_SOFTWARE). The
/// disk is kept; the manager retries a bounded number of times, even on remote
/// nodes, because such a failure may be deterministic.
pub const CONTROLLER_FAILED: i32 = 70;
/// Internal guest result outside the valid provider exit-code range.
pub(crate) const FINALIZATION_FAILED_SENTINEL: i32 = -1;

/// Publish the final controller result after physical cleanup.
pub(crate) async fn write_final_result(
    path: &std::path::Path,
    outcome: &[u8],
    failed: bool,
) -> Result<()> {
    if failed
        && serde_json::from_slice::<serde_json::Value>(outcome).ok()
            == Some(
                serde_json::json!({ "StatusCode": CONTROLLER_FAILED, "FinalizationFailed": true }),
            )
        && let Ok(file) = tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .await
        && file.metadata().await.is_ok_and(|metadata| {
            metadata.is_file() && metadata.nlink() == 1 && metadata.len() <= 1024
        })
        && let Ok(existing) = async {
            let mut bytes = Vec::new();
            file.take(1025).read_to_end(&mut bytes).await.map(|_| bytes)
        }
        .await
        && serde_json::from_slice::<serde_json::Value>(&existing).ok()
            == Some(
                serde_json::json!({ "StatusCode": CONTROLLER_FAILED, "FinalizationFailed": true }),
            )
    {
        return Ok(());
    }
    atomic_write(path, outcome).await
}

fn router(broker: Broker) -> Router {
    Router::new()
        .fallback(any(routes::handler))
        .with_state(broker)
}

fn env_path(name: &str, default: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| default.into()))
}

/// A controller container restart fences its entire PID namespace. Mark interrupted attempts
/// exited, but retain all guest disks so the manager can launch replacement attempts.
async fn fence_interrupted_attempts(state: &std::path::Path) -> Result<()> {
    let mut files = tokio::fs::read_dir(state).await?;
    while let Some(file) = files.next_entry().await? {
        if file.path().extension().is_some_and(|s| s == "active") {
            let exit = file.path().with_extension("exit");
            if !exit.exists() {
                atomic_write(&exit, b"143").await?;
            }
            tokio::fs::remove_file(file.path()).await?;
        }
    }
    Ok(())
}

pub async fn serve(stop: CancellationToken) -> Result<()> {
    let concurrency = crate::config::concurrency()?;
    let data = env_path("DATA_DIR", "/data");
    let state = env_path("RUNNER_STATE_DIR", "/runner-state");
    private_dir(&state).await?;
    let controller = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state.join("controller.lock"))?;
    if unsafe { libc::flock(controller.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::unavailable(
            "Another VM controller owns this storage.",
        ));
    }
    // Reclaim obsolete export staging left by older controllers; it is never a disk base.
    if state.join("snapshots").exists() {
        tokio::fs::remove_dir_all(state.join("snapshots")).await?;
    }
    let image = host::assets(&state).await?;
    fence_interrupted_attempts(&state).await?;
    let pool = Pool::new(state.clone(), image, stop.clone(), concurrency).await?;
    pool.initialize(crate::microvm::budget::cgroup().await?)
        .await?;
    let memory_monitor = tokio::spawn(pool.clone().monitor());
    let broker = Broker::new(data, state, pool, stop.clone());
    broker.recover_exclusive().await?;
    let exclusive_monitor = tokio::spawn(broker.clone().monitor_exclusive());
    let draining = broker.clone();
    let address = std::env::var("RUNNER_BIND").unwrap_or_else(|_| "0.0.0.0:4311".into());
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, router(broker))
        .with_graceful_shutdown(stop.cancelled_owned())
        .await?;
    let attempts = draining
        .active
        .lock()
        .await
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for id in attempts {
        draining.stop(&id).await?;
    }
    draining.pool.drain().await;
    memory_monitor.await.map_err(Error::internal)?;
    exclusive_monitor.await.map_err(Error::internal)?;
    drop(controller);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn interrupted_restart_preserves_committed_finalization_failure() {
        let root = tempfile::tempdir().unwrap();
        let active = root.path().join("attempt.active");
        let exit = root.path().join("attempt.exit");
        tokio::fs::write(&active, b"run").await.unwrap();
        let failure = br#"{"StatusCode":70,"FinalizationFailed":true}"#;
        tokio::fs::write(&exit, failure).await.unwrap();
        super::fence_interrupted_attempts(root.path())
            .await
            .unwrap();
        assert!(!active.exists());
        assert_eq!(tokio::fs::read(exit).await.unwrap(), failure);
    }

    use super::*;
    use crate::microvm::plan::Plan;
    use axum::body::Body;
    use broker::Attempt;
    use futures_util::StreamExt;
    use serde_json::{Value, json};
    use std::{
        io::{Seek, SeekFrom, Write},
        os::unix::fs::MetadataExt,
        path::Path,
        sync::Arc,
        time::Duration,
    };
    use tokio::sync::watch;
    use tower::ServiceExt;

    const SECRET: &str = "synthetic-runner-secret";

    /// Broker over private `data` and `state` directories under `root`.
    async fn fixture_broker(root: &Path) -> Broker {
        let data = root.join("data");
        let state = root.join("state");
        private_dir(&data).await.unwrap();
        private_dir(&state).await.unwrap();
        std::fs::write(data.join("runner-secret"), SECRET).unwrap();
        let stop = CancellationToken::new();
        let pool = Pool::new(state.clone(), root.into(), stop.clone(), 1)
            .await
            .unwrap();
        Broker::new(data, state, pool, stop)
    }

    /// An attempt that is still starting: it has no guest socket yet.
    fn starting_attempt(run: &str) -> (watch::Sender<bool>, Attempt) {
        let (done, receiver) = watch::channel(false);
        let attempt = Attempt {
            stop: CancellationToken::new(),
            done: receiver,
            physical_teardown: None,
            socket: Arc::default(),
            plan: Plan::new(json!({
                "runId": run
            })),
            imports: Arc::default(),
            control: Arc::default(),
        };
        (done, attempt)
    }

    #[tokio::test]
    async fn vm_stop_cancels_workload_when_stop_marker_cannot_be_written() {
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let id = crate::config::id();
        let (_done, attempt) = starting_attempt(&id);
        let stop = attempt.stop.clone();
        broker.active.lock().await.insert(id.clone(), attempt);
        tokio::fs::create_dir(broker.state.join(format!("{id}.stopped")))
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), broker.stop(&id))
                .await
                .unwrap()
                .is_err()
        );
        assert!(stop.is_cancelled());
        assert!(broker.active.lock().await.contains_key(&id));
    }

    fn authorized_post(uri: String, body: Body) -> axum::http::Request<Body> {
        axum::http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("authorization", format!("Bearer {SECRET}"))
            .body(body)
            .unwrap()
    }

    #[tokio::test]
    async fn quarantined_pool_preserves_liveness_without_advertising_execution_readiness() {
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        broker.pool.quarantine();
        let request = axum::http::Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let response = router(broker).oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 16_384)
            .await
            .unwrap();
        let health: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(health["status"], "quarantined");
        assert_eq!(health["pool"]["quarantined"], true);
    }

    #[tokio::test]
    async fn wait_does_not_report_completion_until_the_attempt_is_released() {
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let id = crate::config::id();
        let (_done, attempt) = starting_attempt(&crate::config::id());
        broker.active.lock().await.insert(id.clone(), attempt);

        // Execution writes its exit marker before asynchronous attempt cleanup.
        // A caller may resume this workspace as soon as /wait returns its status.
        std::fs::write(broker.state.join(format!("{id}.exit")), "0").unwrap();
        let app = router(broker.clone());
        let response = app
            .oneshot(authorized_post(format!("/runs/{id}/wait"), Body::empty()))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let mut body = response.into_body().into_data_stream();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), body.next())
                .await
                .is_err(),
            "completion must not escape while the previous attempt still owns the workspace"
        );

        broker.active.lock().await.remove(&id);
        let bytes = tokio::time::timeout(Duration::from_secs(2), async {
            let mut bytes = Vec::new();
            while let Some(chunk) = body.next().await {
                bytes.extend_from_slice(&chunk.unwrap());
            }
            bytes
        })
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({ "StatusCode": 0 })
        );
    }

    #[tokio::test]
    async fn vm_failed_flush_survives_read_only_final_write_after_cleanup() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let id = crate::config::id();
        let (_done, attempt) = starting_attempt(&crate::config::id());
        broker.active.lock().await.insert(id.clone(), attempt);
        let exit = broker.state.join(format!("{id}.exit"));
        let record = br#"{"StatusCode":70,"FinalizationFailed":true}"#;
        atomic_write(&exit, record).await.unwrap();
        let response = router(broker.clone())
            .oneshot(authorized_post(format!("/runs/{id}/wait"), Body::empty()))
            .await
            .unwrap();
        let mut body = response.into_body().into_data_stream();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), body.next())
                .await
                .is_err()
        );
        let other_path = broker.state.join(format!("{}.exit", crate::config::id()));
        std::os::unix::fs::symlink(&exit, &other_path).unwrap();
        std::fs::set_permissions(&broker.state, std::fs::Permissions::from_mode(0o500)).unwrap();
        let written = write_final_result(&exit, record, true).await;
        let other = write_final_result(&other_path, record, true).await;
        std::fs::write(&exit, b"{broken").unwrap();
        let corrupt = write_final_result(&exit, record, true).await;
        std::fs::write(&exit, record).unwrap();
        std::fs::set_permissions(&broker.state, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            written.is_ok(),
            "a committed failure must not require another write: {written:?}"
        );
        assert!(other.is_err() && corrupt.is_err());
        let hardlink = broker.state.join(format!("{}.exit", crate::config::id()));
        std::fs::hard_link(&exit, &hardlink).unwrap();
        std::fs::set_permissions(&broker.state, std::fs::Permissions::from_mode(0o500)).unwrap();
        let other = write_final_result(&hardlink, record, true).await;
        std::fs::set_permissions(&broker.state, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_file(hardlink).unwrap();
        assert!(other.is_err());
        broker.active.lock().await.remove(&id);
        let bytes = tokio::time::timeout(Duration::from_secs(2), async {
            let mut bytes = Vec::new();
            while let Some(chunk) = body.next().await {
                bytes.extend_from_slice(&chunk.unwrap());
            }
            bytes
        })
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({ "StatusCode": 70, "FinalizationFailed": true })
        );
    }

    async fn request(
        app: &Router,
        run: &str,
        action: &str,
        transfer: &str,
        credential: &str,
    ) -> u16 {
        let request = axum::http::Request::builder()
            .method("POST")
            .uri(format!("/disks/{run}/{action}"))
            .header("authorization", format!("Bearer {credential}"))
            .body(Body::from(
                json!({ "transfer": transfer, "permanentlyDeleted": action == "delete" })
                    .to_string(),
            ))
            .unwrap();
        app.clone()
            .oneshot(request)
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    #[tokio::test]
    async fn disconnected_retirement_keeps_ownership_until_its_receipt_is_durable() {
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let run = crate::config::id();
        let retirement = crate::config::id();
        let state = broker.state.clone();
        let directory = state.join("disks").join(&run);
        private_dir(&directory).await.unwrap();
        std::fs::write(directory.join("data.ext4"), b"old disk").unwrap();
        let active = broker.active.lock().await;
        let app = router(broker.clone());
        let caller = tokio::spawn(
            app.oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/disks/{run}/delete"))
                    .header("authorization", format!("Bearer {SECRET}"))
                    .body(Body::from(
                        json!({ "retirementId": retirement }).to_string(),
                    ))
                    .unwrap(),
            ),
        );
        let receipts = state.join("disk-retirements").join(&run);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !receipts.join("lock").exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        caller.abort();
        let _ = caller.await;
        assert!(directory.join("data.ext4").exists());
        drop(active);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !receipts.join(&retirement).exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!directory.join("data.ext4").exists());
        assert_eq!(
            std::fs::read_to_string(receipts.join(&retirement)).unwrap(),
            retirement
        );
    }

    #[tokio::test]
    async fn vm_retirement_erases_only_its_own_attempt_plans_on_a_shared_volume() {
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let run = crate::config::id();
        let (old, remote) = (crate::config::id(), crate::config::id());
        let plans = broker.data.join("runner-plans");
        private_dir(&plans).await.unwrap();
        for attempt in [&old, &remote] {
            std::fs::write(
                plans.join(format!("{attempt}.json")),
                json!({ "runId":run }).to_string(),
            )
            .unwrap();
        }
        std::fs::write(broker.state_file(&old, "run"), &run).unwrap();
        let disk = broker.state.join("disks").join(&run);
        private_dir(&disk).await.unwrap();
        std::fs::write(disk.join("data.ext4"), b"old disk").unwrap();
        let app = router(broker);
        let endpoint = format!("/disks/{run}/delete");
        let unidentified = app
            .clone()
            .oneshot(authorized_post(endpoint.clone(), Body::from("{}")))
            .await
            .unwrap();
        assert_eq!(unidentified.status(), 409);
        assert!(disk.join("data.ext4").exists());
        let retired = app
            .oneshot(authorized_post(
                endpoint,
                Body::from(json!({ "retirementId":crate::config::id() }).to_string()),
            ))
            .await
            .unwrap();
        assert_eq!(retired.status(), 200);
        assert!(!disk.join("data.ext4").exists());
        assert!(!plans.join(format!("{old}.json")).exists());
        assert!(plans.join(format!("{remote}.json")).exists());
    }

    #[tokio::test]
    async fn vm_retirement_retries_log_cleanup_after_restart_before_acknowledging() {
        for owner in ["plan", "run", "vm"] {
            let root = tempfile::tempdir().unwrap();
            let broker = fixture_broker(root.path()).await;
            let run = crate::config::id();
            let attempt = crate::config::id();
            let vm = crate::config::id();
            let retirement = crate::config::id();
            let state = broker.state.clone();
            let identity = match owner {
                "plan" => {
                    let plans = broker.data.join("runner-plans");
                    private_dir(&plans).await.unwrap();
                    std::fs::write(broker.state_file(&attempt, "stopped"), b"").unwrap();
                    let path = plans.join(format!("{attempt}.json"));
                    std::fs::write(&path, json!({ "runId":run }).to_string()).unwrap();
                    path
                }
                "run" => {
                    let path = broker.state_file(&attempt, "run");
                    std::fs::write(&path, &run).unwrap();
                    path
                }
                _ => {
                    let path = broker.state_file(&attempt, "vm.json");
                    std::fs::write(&path, json!({ "runId":run, "vmId":vm }).to_string()).unwrap();
                    for suffix in ["boot.log", "vmm.log"] {
                        std::fs::write(broker.state_file(&vm, suffix), b"old VM output").unwrap();
                    }
                    path
                }
            };
            let log = broker.state_file(&attempt, "log");
            private_dir(&log).await.unwrap();
            let endpoint = format!("/disks/{run}/delete");
            let body = json!({ "retirementId":retirement }).to_string();
            let app = router(broker);
            let failed = app
                .oneshot(authorized_post(endpoint.clone(), Body::from(body.clone())))
                .await
                .unwrap();
            assert!(!failed.status().is_success());
            let receipt = state.join("disk-retirements").join(&run).join(&retirement);
            assert!(!receipt.exists());
            assert!(
                identity.exists(),
                "{owner}: cleanup lost the run's attempt identity"
            );

            std::fs::remove_dir(&log).unwrap();
            std::fs::write(&log, b"old private output").unwrap();
            let restarted = router(fixture_broker(root.path()).await);
            let retired = restarted
                .oneshot(authorized_post(endpoint, Body::from(body)))
                .await
                .unwrap();
            assert_eq!(retired.status(), 200);
            assert!(receipt.exists());
            assert!(!identity.exists());
            assert!(!log.exists());
            for suffix in ["boot.log", "vmm.log"] {
                assert!(!state.join(format!("{vm}.{suffix}")).exists());
            }
        }
    }

    #[tokio::test]
    async fn manager_and_bundled_controller_keep_retry_ownership_across_plan_cleanup() {
        use crate::node_fixture::StoreNodeFixture;
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let (run, attempt, retirement) = (
            crate::config::id(),
            crate::config::id(),
            crate::config::id(),
        );
        let state = broker.state.clone();
        let plans = broker.data.join("runner-plans");
        private_dir(&plans).await.unwrap();
        let plan = plans.join(format!("{attempt}.json"));
        std::fs::write(&plan, json!({ "runId":run }).to_string()).unwrap();
        std::fs::write(broker.state_file(&attempt, "stopped"), b"").unwrap();
        let log = broker.state_file(&attempt, "log");
        private_dir(&log).await.unwrap();
        let (url, server) = crate::common::serve_locally(router(broker)).await;
        let mut config = crate::common::config(root.path());
        config.runner_url = url;
        let manager = crate::service::Service::new(config.clone()).await.unwrap();
        manager
            .store
            .put_node_fixture(json!({ "id":crate::nodes::LOCAL_NODE_ID, "local":true }))
            .await
            .unwrap();
        let pending = json!({
            "id":retirement, "runId":run, "cause":"node-change", "expiredAt":crate::config::now(),
            "nodes":[crate::nodes::LOCAL_NODE_ID], "attempt":attempt, "attemptNode":crate::nodes::LOCAL_NODE_ID,
            "attempts":[], "managerPlans":[attempt], "process":null, "isolated":true,
            "accountId":null, "workspaces":[], "projects":[], "fenced":true, "local":true,
            "publications":crate::nodes::retired_publications::RetiredPublications::default(),
            "cleaned":false, "error":null, "retryAfter":null
        });
        manager
            .store
            .write(move |db| db.put("environment-retirements", &pending))
            .await
            .unwrap();
        manager.retire_environments().await.unwrap();
        let pending = manager
            .store
            .get("environment-retirements", &retirement)
            .await
            .unwrap()
            .unwrap();
        assert!(pending["error"].is_string());
        assert!(
            plan.exists(),
            "manager must not remove a controller's retry identity before disk acknowledgement"
        );
        assert_eq!(
            std::fs::read_to_string(state.join(format!("{attempt}.run"))).unwrap(),
            run
        );
        let receipt = state.join("disk-retirements").join(&run).join(&retirement);
        assert!(!receipt.exists());
        // Ordinary worker release can independently remove the shared plan.
        std::fs::remove_file(&plan).unwrap();
        std::fs::remove_dir(&log).unwrap();
        std::fs::write(&log, b"old private output").unwrap();
        let owned = retirement.clone();
        manager
            .store
            .write(move |db| {
                let mut pending = db.get("environment-retirements", &owned)?.unwrap();
                pending["retryAfter"] = Value::Null;
                db.put("environment-retirements", &pending)
            })
            .await
            .unwrap();
        drop(manager);
        server.abort();
        let _ = server.await;
        let (url, restarted_server) =
            crate::common::serve_locally(router(fixture_broker(root.path()).await)).await;
        config.runner_url = url;
        let restarted = crate::service::Service::new(config).await.unwrap();
        restarted.retire_environments().await.unwrap();
        assert!(receipt.exists());
        assert!(!log.exists());
        assert!(
            restarted
                .store
                .get("environment-retirements", &retirement)
                .await
                .unwrap()
                .is_none()
        );
        restarted_server.abort();
    }

    #[tokio::test]
    async fn captured_legacy_attempts_reclaim_unmapped_logs_without_touching_replacements() {
        use crate::node_fixture::StoreNodeFixture;
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let (run, old, foreign, fresh, retirement) = (
            crate::config::id(),
            crate::config::id(),
            crate::config::id(),
            crate::config::id(),
            crate::config::id(),
        );
        let state = broker.state.clone();
        let log = broker.state_file(&old, "log");
        std::fs::write(&log, b"private output left by the previous version").unwrap();
        std::fs::write(broker.state_file(&old, "stopped"), b"").unwrap();
        let mut captured = vec![old.clone()];
        for _ in 0..crate::storage::retirement::MAX_ATTEMPTS {
            let id = crate::config::id();
            std::fs::write(
                broker.state_file(&id, "log"),
                b"another unmapped old output",
            )
            .unwrap();
            captured.push(id);
        }
        let mut scopes = captured
            .iter()
            .map(|id| json!({ "id":id,"nodeId":crate::nodes::LOCAL_NODE_ID }))
            .collect::<Vec<_>>();
        scopes.push(json!({ "id":foreign,"nodeId":crate::config::id() }));
        let foreign_log = broker.state_file(&foreign, "log");
        std::fs::write(&foreign_log, b"not attributed to this node").unwrap();
        let (url, server) = crate::common::serve_locally(router(broker)).await;
        let mut config = crate::common::config(root.path());
        config.runner_url = url.clone();
        let manager = crate::service::Service::new(config).await.unwrap();
        manager
            .store
            .put_node_fixture(json!({ "id":crate::nodes::LOCAL_NODE_ID, "local":true }))
            .await
            .unwrap();
        let pending = json!({
            "id":retirement, "runId":run, "cause":"inactivity", "expiredAt":crate::config::now(),
            "nodes":[crate::nodes::LOCAL_NODE_ID], "attempt":old, "attemptNode":crate::nodes::LOCAL_NODE_ID,
            "attempts":[], "managerPlans":[old,foreign],
            "diskAttempts":scopes,
            "process":null, "isolated":true, "accountId":null, "workspaces":[], "projects":[], "fenced":true, "local":true,
            "publications":crate::nodes::retired_publications::RetiredPublications::default(), "cleaned":false, "error":null, "retryAfter":null
        });
        manager
            .store
            .write(move |db| db.put("environment-retirements", &pending))
            .await
            .unwrap();
        manager.retire_environments().await.unwrap();
        assert!(!log.exists());
        for id in &captured {
            assert!(!state.join(format!("{id}.log")).exists());
        }
        assert!(
            manager
                .store
                .get("environment-retirements", &retirement)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            std::fs::read(&foreign_log).unwrap(),
            b"not attributed to this node"
        );
        let disk = crate::storage::environment::directory(&state, &run).unwrap();
        std::fs::write(disk.join("replacement"), b"new disk").unwrap();
        let fresh_log = state.join(format!("{fresh}.log"));
        std::fs::write(&fresh_log, b"new private output").unwrap();
        std::fs::write(state.join(format!("{fresh}.run")), &run).unwrap();
        // Completed receipts still honor old cleanup hints, but cannot re-scan
        // this run's new attempts or erase the replacement's canonical disk.
        let response = manager
            .http
            .post(format!("{url}/disks/{run}/delete"))
            .bearer_auth(SECRET)
            .json(&json!({ "retirementId":retirement,"attempts":[old] }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            std::fs::read(disk.join("replacement")).unwrap(),
            b"new disk"
        );
        assert_eq!(std::fs::read(&fresh_log).unwrap(), b"new private output");
        assert!(foreign_log.exists());
        server.abort();
    }

    #[tokio::test]
    async fn legacy_publication_fence_needs_no_vm_disk_or_publication_api() {
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let state = broker.state.clone();
        let run = crate::config::id();
        let publication = crate::config::id();
        let app = router(broker);
        let request = |operation: &str| {
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/disks/{run}/{operation}"))
                .header("authorization", format!("Bearer {SECRET}"))
                .body(Body::from(json!({ "backupId": publication }).to_string()))
                .unwrap()
        };
        let response = app
            .clone()
            .oneshot(request("publication-fence"))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["fenced"],
            publication
        );
        assert!(!state.join("disks").join(&run).exists());
        assert!(matches!(
            app.oneshot(request("published"))
                .await
                .unwrap()
                .status()
                .as_u16(),
            404 | 405
        ));
    }

    #[tokio::test]
    async fn late_retirement_retry_preserves_the_next_vm_environment() {
        async fn retire(app: &Router, run: &str, id: &str) -> axum::response::Response {
            app.clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri(format!("/disks/{run}/delete"))
                        .header("authorization", format!("Bearer {SECRET}"))
                        .body(Body::from(json!({ "retirementId": id }).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
        }
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let state = broker.state.clone();
        let run = crate::config::id();
        let retirement = crate::config::id();
        let directory = state.join("disks").join(&run);
        private_dir(&directory).await.unwrap();
        std::fs::write(directory.join("data.ext4"), b"old disk").unwrap();
        let app = router(broker);
        let response = retire(&app, &run, &retirement).await;
        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["retirementId"],
            retirement
        );
        assert!(!directory.join("data.ext4").exists());
        std::fs::write(directory.join("data.ext4"), b"new disk").unwrap();
        let active = crate::storage::environment::lock(&state, &run, "active")
            .await
            .unwrap();
        // Simulate a restarted controller: its receipt is entirely node-local.
        let restarted = router(fixture_broker(root.path()).await);
        assert_eq!(retire(&restarted, &run, &retirement).await.status(), 200);
        assert_eq!(
            std::fs::read(directory.join("data.ext4")).unwrap(),
            b"new disk"
        );
        drop(active);
        assert_eq!(
            retire(&restarted, &run, &crate::config::id())
                .await
                .status(),
            200
        );
        assert!(!directory.join("data.ext4").exists());
        std::fs::write(directory.join("data.ext4"), b"third environment").unwrap();
        assert_eq!(retire(&restarted, &run, &retirement).await.status(), 200);
        assert_eq!(
            std::fs::read(directory.join("data.ext4")).unwrap(),
            b"third environment"
        );
    }

    #[tokio::test]
    async fn deleting_an_attributed_disk_preserves_ownership_and_other_environments() {
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let state = broker.state.clone();
        let run = crate::config::id();
        let environment = crate::config::id();
        let directory = state.join("environments").join(&environment);
        private_dir(&directory).await.unwrap();
        std::fs::write(directory.join("private-work"), b"user data").unwrap();
        let physical = crate::file_lock::exclusive(&directory.join("lock"), "busy").unwrap();
        let owner = crate::storage::environment::assign(&state, &environment, &run)
            .await
            .unwrap();
        let logical = state.join("disks").join(&run);
        let inode = std::fs::metadata(directory.join("lock")).unwrap().ino();
        let ownership_inode = std::fs::metadata(logical.join("ownership.lock"))
            .unwrap()
            .ino();
        let other = state.join("environments").join(crate::config::id());
        private_dir(&other).await.unwrap();
        std::fs::write(other.join("private-work"), b"keep").unwrap();
        let app = router(broker);
        let transfer = crate::config::id();
        assert_eq!(request(&app, &run, "delete", &transfer, SECRET).await, 409);
        drop((owner, physical));
        assert_eq!(request(&app, &run, "delete", &transfer, SECRET).await, 200);
        assert!(!directory.join("private-work").exists());
        assert_eq!(
            crate::storage::environment::directory(&state, &run).unwrap(),
            directory
        );
        assert_eq!(
            std::fs::metadata(directory.join("lock")).unwrap().ino(),
            inode
        );
        assert_eq!(
            std::fs::metadata(logical.join("ownership.lock"))
                .unwrap()
                .ino(),
            ownership_inode
        );
        assert_eq!(std::fs::read(other.join("private-work")).unwrap(), b"keep");
        assert!(
            crate::storage::environment::assign(&state, &environment, &crate::config::id())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn stale_vm_disk_cleanup_preserves_native_workspace_scope() {
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let run = crate::config::id();
        let vm_attempt = crate::config::id();
        let native_attempt = crate::config::id();
        let plans = broker.data.join("runner-plans");
        private_dir(&plans).await.unwrap();

        for (attempt, backend) in [(&vm_attempt, "firecracker"), (&native_attempt, "native")] {
            let plan = json!({
                "id": attempt,
                "runId": run,
                "backend": backend,
                "nodeLeaseRequired": true,
            });
            std::fs::write(
                plans.join(format!("{attempt}.json")),
                serde_json::to_vec(&plan).unwrap(),
            )
            .unwrap();
        }
        let native_path = plans.join(format!("{native_attempt}.json"));
        let native_bytes = std::fs::read(&native_path).unwrap();
        std::fs::write(broker.state_file(&vm_attempt, "run"), &run).unwrap();
        let vm_stopped = broker.state_file(&vm_attempt, "stopped");
        let native_stopped = broker.state_file(&native_attempt, "stopped");
        let app = router(broker);

        assert_eq!(
            request(&app, &run, "delete", &crate::config::id(), SECRET).await,
            200
        );
        assert!(!plans.join(format!("{vm_attempt}.json")).exists());
        assert!(vm_stopped.exists(), "Deleted VM attempts must stay fenced");
        assert!(
            native_path.exists(),
            "Retiring an old VM disk must retain the native plan used by workspace MCP authorization"
        );
        assert_eq!(std::fs::read(native_path).unwrap(), native_bytes);
        assert!(!native_stopped.exists());
    }

    #[tokio::test]
    async fn local_disk_deletion_needs_neither_publication_nor_readable_base_blocks() {
        use crate::storage::Disk;
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let state = broker.state.clone();
        let app = router(broker);
        for missing_base in [false, true] {
            let run = crate::config::id();
            let directory = state.join("disks").join(&run);
            let lazy = directory.join("lazy");
            let disk = crate::storage::runtime::create(
                &lazy,
                &json!({
                    "version": 1, "size": 4096, "blockSize": 4194304,
                    "blocks": [{ "offset": 0, "size": 4096, "hash": null }],
                }),
                &json!({
                    "master": "http://127.0.0.1:1/", "grant": "local-fixture",
                    "policy": { "reserveMiB": 64, "reservePercent": 1 },
                }),
            )
            .await
            .unwrap();
            disk.write_at(0, b"local-only work").unwrap();
            if missing_base {
                disk.seal().unwrap();
                disk.consolidate().unwrap();
                let file = std::fs::read_dir(lazy.join("cache"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap();
                std::fs::remove_file(file.path()).unwrap();
            } else {
                assert_ne!(disk.accounting().unwrap()["dirtyBytes"], 0);
            }
            drop(disk);
            assert_eq!(
                request(&app, &run, "delete", &crate::config::id(), SECRET).await,
                200
            );
            assert!(!lazy.exists());
            assert!(directory.join("lock").exists());
        }
    }

    #[tokio::test]
    async fn workspace_disk_deletion_uses_authenticated_http_and_preserves_the_lock() {
        let root = tempfile::TempDir::new().unwrap();
        let data = root.path().join("data");
        let state = root.path().join("state");
        private_dir(&data).await.unwrap();
        private_dir(&state).await.unwrap();
        std::fs::write(data.join("runner-secret"), SECRET).unwrap();
        let stop = CancellationToken::new();
        let pool = Pool::new(
            state.clone(),
            root.path().join("unused-image"),
            stop.clone(),
            1,
        )
        .await
        .unwrap();
        let app = router(Broker::new(data.clone(), state.clone(), pool, stop));
        let run = crate::config::id();
        let failed_attempt = crate::config::id();
        std::fs::write(state.join(format!("{failed_attempt}.run")), &run).unwrap();
        std::fs::write(
            state.join(format!("{failed_attempt}.log")),
            "private agent output",
        )
        .unwrap();
        let transfer = crate::config::id();
        let directory = state.join("disks").join(&run);
        private_dir(&directory).await.unwrap();
        let mut file = std::fs::File::create(directory.join("data.ext4")).unwrap();
        file.seek(SeekFrom::Start(8 * 1024 * 1024)).unwrap();
        file.write_all(b"native session and unpublished work")
            .unwrap();
        file.sync_all().unwrap();
        assert_eq!(request(&app, &run, "delete", &transfer, "wrong").await, 401);
        std::fs::write(directory.join("stale-older.ext4"), "older copy").unwrap();
        assert_eq!(request(&app, &run, "prune", &transfer, SECRET).await, 200);
        assert!(!directory.join("stale-older.ext4").exists());
        assert!(directory.join("data.ext4").exists());
        assert_eq!(request(&app, &run, "export", &transfer, SECRET).await, 405);
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert_eq!(request(&app, &run, "delete", &transfer, SECRET).await, 409);
        let inode = lock.metadata().unwrap().ino();
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
        drop(lock);
        assert_eq!(request(&app, &run, "delete", &transfer, SECRET).await, 200);
        assert!(!directory.join("data.ext4").exists());
        assert!(!state.join(format!("{failed_attempt}.log")).exists());
        assert!(state.join(format!("{failed_attempt}.stopped")).exists());
        assert_eq!(
            std::fs::metadata(directory.join("lock")).unwrap().ino(),
            inode
        );
        assert_eq!(request(&app, &run, "import", &transfer, SECRET).await, 405);
        assert!(!directory.join("data.ext4").exists());
    }
}
