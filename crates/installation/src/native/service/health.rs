//! Expensive diagnostics never own the controller liveness path.
use super::*;
use std::time::Instant;

const SAMPLE_PERIOD: Duration = Duration::from_secs(3);
const MAX_SAMPLE_AGE: Duration = Duration::from_secs(15);

#[derive(Clone, Default)]
pub(super) struct Telemetry(Arc<Mutex<Option<(Instant, Value)>>>);

impl Controller {
    pub(super) async fn health(&self) -> Value {
        let sample = self.telemetry.0.lock().await.clone();
        let (mut value, age) = match sample {
            Some((sampled, value)) => (value, Some(sampled.elapsed())),
            None => (json!({}), None),
        };
        let fresh = age.is_some_and(|age| age <= MAX_SAMPLE_AGE);
        value["status"] = if fresh { "ok" } else { "degraded" }.into();
        value["backend"] = "native".into();
        value["nodeProtocol"] = 2.into();
        value["directStorageProtocol"] = 1.into();
        value["runtimeId"] = crate::nodes::component_runtime().into();
        value["dataRoot"] = json!(self.data);
        if let Ok(active) = self.active.try_lock() {
            value["activeRuns"] = active.len().into();
        }
        value["telemetryAgeMs"] = age.map(|age| age.as_millis() as u64).into();
        value["bootstrapObservation"] = self.bootstrap.health();
        value
    }

    async fn sample_health_once(&self) -> Result<Value> {
        let state = self.state.clone();
        let capabilities = tokio::task::spawn_blocking(move || -> Result<Value> {
            let capabilities = crate::nodes::connector::capabilities(&state)?;
            systemd::tools(&crate::nodes::component_runtime())?;
            Ok(capabilities)
        })
        .await
        .map_err(Error::internal)??;
        let usage = self.usage().await?;
        let budget = self.budget.lock().await.clone();
        let capacity = self.capacity.lock().await.clone();
        let disk_growth = if storage::ublk::supports_growth() {
            serde_json::to_value(
                capacity
                    .report(&self.state, usage["diskBytes"].as_u64().unwrap_or(0))
                    .await?,
            )?
        } else {
            Value::Null
        };
        Ok(json!({
            "capabilities": capabilities,
            "activeRuns": self.active.lock().await.len(),
            "nativeMode": self.mode.lock().await.clone(),
            "pool": { "capacity": budget["slots"] },
            "sharedResources": true,
            "exclusiveReservations": true,
            "budget": budget,
            "usage": usage,
            "pressure": null,
            "diskGrowth": disk_growth,
            "storagePolicy": self.storage_policy.current().await
        }))
    }

    pub(super) async fn sample_health(self) {
        loop {
            let started = Instant::now();
            match self.sample_health_once().await {
                Ok(value) => *self.telemetry.0.lock().await = Some((Instant::now(), value)),
                Err(error) => tracing::warn!(
                    target: "cairn_performance",
                    status = error.status,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "Native telemetry sampling failed; retaining last sample"
                ),
            }
            tokio::select! {
                () = self.stop.cancelled() => return,
                () = tokio::time::sleep(SAMPLE_PERIOD) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blocked_diagnostic_read_keeps_single_worker_health_and_leases_responsive() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("tools");
        std::fs::create_dir(&bundle).unwrap();
        std::fs::write(bundle.join("manifest.json"), b"{}").unwrap();
        let fifo = std::ffi::CString::new(
            root.path()
                .join("release.json")
                .as_os_str()
                .as_encoded_bytes(),
        )
        .unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "native::service::health::tests::blocked_diagnostic_read_child",
                "--ignored",
            ])
            .env("CAIRN_NATIVE_TOOLS", bundle)
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(15), child.output())
            .await
            .expect("isolated diagnostic child must finish")
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed;"));
    }

    #[test]
    #[ignore = "isolated native tool bundle environment; executed by parent regression"]
    fn blocked_diagnostic_read_child() {
        use std::io::Write;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (_root, controller) = runtime.block_on(super::super::tests::fixture());
        let attempt = crate::config::id();
        std::fs::create_dir_all(controller.file(&attempt, "lease").parent().unwrap()).unwrap();
        let release_path = std::path::PathBuf::from(std::env::var("CAIRN_NATIVE_TOOLS").unwrap())
            .parent()
            .unwrap()
            .join("release.json");
        let (opened, observed_open) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(release_path)
                .unwrap();
            opened.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(10)).unwrap();
            file.write_all(
                &serde_json::to_vec(&json!({
                    "runtimeId": crate::nodes::component_runtime(),
                    "os": "linux",
                    "arch": "x86_64",
                }))
                .unwrap(),
            )
            .unwrap();
        });
        let sampling = controller.clone();
        let sampler = runtime.spawn(async move { sampling.sample_health_once().await });
        let opening = observed_open.recv_timeout(Duration::from_secs(5));
        let controls = controller.clone();
        let (progress, responsive) = std::sync::mpsc::channel();
        let control = runtime.spawn(async move {
            let health = super::super::handler(
                State(controls.clone()),
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(health.status(), 200);
            let request = Request::builder()
                .method("POST")
                .uri(format!("/runs/{attempt}/lease"))
                .header("authorization", "Bearer synthetic-private-controller-token")
                .header("content-type", "application/json")
                .body(Body::from(json!({"remainingMs": 60_000}).to_string()))
                .unwrap();
            assert_eq!(
                super::super::handler(State(controls.clone()), request)
                    .await
                    .unwrap()
                    .status(),
                200
            );
            assert!(controls.leased(&attempt).await);
            progress.send(()).unwrap();
        });
        let result = responsive.recv_timeout(Duration::from_secs(2));
        release.send(()).unwrap();
        writer.join().unwrap();
        runtime.block_on(sampler).unwrap().unwrap();
        runtime.block_on(control).unwrap();
        opening.expect("sampler must reach the real blocking release metadata read");
        result.expect("blocked diagnostics must not hold the sole HTTP worker");
    }

    #[tokio::test]
    async fn staging_rechecks_live_quota_after_a_copy_even_with_fresh_or_stale_telemetry() {
        let (_root, controller) = super::super::tests::fixture().await;
        let budget = json!({
            "slots": 1,
            "limits": {"cpu": 1, "memoryMiB": 512, "diskMiB": 5},
        });
        *controller.budget.lock().await = budget.clone();
        *controller.telemetry.0.lock().await = Some((
            Instant::now(),
            json!({
                "budget": budget,
                "usage": {"diskBytes": 0},
            }),
        ));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new()
            .fallback(super::super::handler)
            .with_state(controller.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let _ownership = disk::lock(&controller.state).await.unwrap();
        let client = reqwest::Client::new();
        let first = tokio::time::timeout(
            Duration::from_secs(2),
            disk::staging_budget_from(&client, &origin),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(first > 3 * disk::MIB);
        tokio::fs::write(
            controller.data.join("first-copy"),
            vec![1; (3 * disk::MIB) as usize],
        )
        .await
        .unwrap();

        for age in [Duration::ZERO, MAX_SAMPLE_AGE + Duration::from_secs(1)] {
            controller.telemetry.0.lock().await.as_mut().unwrap().0 = Instant::now() - age;
            let remaining = tokio::time::timeout(
                Duration::from_secs(2),
                disk::staging_budget_from(&client, &origin),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(
                remaining < 2 * disk::MIB,
                "another 2MiB copy must not reuse the cached free space: {remaining}"
            );
        }
        assert_eq!(
            controller.health().await["usage"]["diskBytes"],
            0,
            "health remains an independent diagnostic snapshot"
        );
        server.abort();
    }

    #[tokio::test]
    async fn staging_never_falls_back_to_cached_health_without_authoritative_quota() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new().route(
            "/health",
            axum::routing::get(|| async {
                axum::Json(json!({
                    "status": "ok",
                    "budget": {"limits": {"diskMiB": 1024}},
                    "usage": {"diskBytes": 0},
                }))
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let result = disk::staging_budget_from(&reqwest::Client::new(), &origin).await;
        server.abort();
        assert!(
            result.is_err(),
            "a missing authoritative endpoint must not authorize staging from health"
        );
    }

    #[tokio::test]
    async fn health_keeps_identity_without_waiting_for_diagnostics_or_configuration() {
        let (_root, controller) = super::super::tests::fixture().await;
        let _budget = controller.budget.lock().await;
        let _capacity = controller.capacity.lock().await;
        let _mode = controller.mode.lock().await;
        let value = tokio::time::timeout(Duration::from_secs(1), controller.health())
            .await
            .unwrap();
        assert_eq!(value["status"], "degraded");
        assert_eq!(value["nodeProtocol"], 2);
        assert_eq!(value["runtimeId"], crate::nodes::component_runtime());
        assert_eq!(value["activeRuns"], 0);
    }

    #[tokio::test]
    async fn project_copy_ownership_cannot_hold_controller_liveness() {
        let (_root, controller) = super::super::tests::fixture().await;
        let _active = controller.active.lock().await;
        let value = tokio::time::timeout(Duration::from_secs(1), controller.health())
            .await
            .unwrap();
        assert_eq!(value["nodeProtocol"], 2);
        assert_eq!(value["status"], "degraded");
        assert!(value["activeRuns"].is_null());
    }

    #[tokio::test]
    async fn cursor_heartbeats_and_retained_output_survive_busy_attempt_ownership() {
        use futures_util::StreamExt;
        let (_root, controller) = super::super::tests::fixture().await;
        let id = crate::config::id();
        let path = controller.file(&id, "log");
        tokio::fs::write(&path, b"").await.unwrap();
        let active = controller.active.lock().await;
        let request = Request::builder()
            .uri(format!("/runs/{id}/logs?cursor=1&offset=0"))
            .header("authorization", "Bearer synthetic-private-controller-token")
            .body(Body::empty())
            .unwrap();
        let response = super::super::handler(axum::extract::State(controller.clone()), request)
            .await
            .unwrap();
        let stream = response
            .into_body()
            .into_data_stream()
            .map(|result| result.map_err(std::io::Error::other));
        let mut reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(stream));
        for _ in 0..2 {
            let packet = tokio::time::timeout(Duration::from_secs(2), wire::read(&mut reader))
                .await
                .expect("busy ownership must not suppress cursor heartbeats")
                .unwrap()
                .unwrap();
            assert_eq!(packet["type"], "heartbeat");
        }
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .await
            .unwrap();
        wire::write(
            &mut file,
            &Event::Output {
                stderr: false,
                data: "YQ==".into(),
            },
        )
        .await
        .unwrap();
        drop(file);
        drop(active);
        let packet = tokio::time::timeout(Duration::from_secs(2), wire::read(&mut reader))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(packet["event"]["data"], "YQ==");
        assert_eq!(
            wire::read(&mut reader).await.unwrap().unwrap()["type"],
            "end"
        );
    }

    #[tokio::test]
    async fn stale_telemetry_fences_admission_without_losing_controller_identity() {
        let (_root, controller) = super::super::tests::fixture().await;
        *controller.telemetry.0.lock().await =
            Some((Instant::now(), json!({"pool": {"capacity": 3}})));
        assert_eq!(controller.health().await["status"], "ok");
        controller.telemetry.0.lock().await.as_mut().unwrap().0 =
            Instant::now() - MAX_SAMPLE_AGE - Duration::from_secs(1);
        let value = controller.health().await;
        assert_eq!(value["status"], "degraded");
        assert_eq!(value["pool"]["capacity"], 3);
        assert_eq!(value["nodeProtocol"], 2);
    }
}
