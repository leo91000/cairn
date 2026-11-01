//! The real pipe readers, worker select loop, redaction seam and durable recorder.
use super::*;
use crate::run_fixture::DbRunFixture;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone)]
pub(super) struct OutputProbe {
    refreshes: Arc<AtomicU64>,
    before_canonical: Arc<AtomicU64>,
    history: Option<Arc<credential_history::PrivacyProbe>>,
}

impl OutputProbe {
    pub(super) async fn refresh(&self) {
        self.refreshes.fetch_add(1, Ordering::Relaxed);
        if self.history.is_some() {
            return;
        }
        // Model repeated sub-threshold account/Store work deterministically.
        tokio::time::sleep(Duration::from_millis(12)).await;
    }

    pub(super) fn before_record(&self, output: &output::Output) -> Result<()> {
        if let Some(history) = &self.history {
            history.before_record(output)?;
        }
        Ok(())
    }

    pub(super) fn recorded(&self, output: &output::Output) {
        if let Some(history) = &self.history {
            history.recorded(output);
        }
    }

    pub(super) fn supplies_flood_marker(&self) -> bool {
        self.history.is_none()
    }

    pub(super) fn canonical(&self) {
        let _ = self.before_canonical.compare_exchange(
            u64::MAX,
            self.refreshes.load(Ordering::Relaxed),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

#[tokio::test]
async fn rpc_flood_does_not_hold_canonical_output_behind_redaction_work() {
    rpc_flood_fixture().await;
}

async fn rpc_flood_fixture() {
    let (root, service) = service_fixture().await;
    rpc_flood_on_service(root, service).await;
}

async fn service_fixture() -> (tempfile::TempDir, Arc<Service>) {
    service_with_codex("unused").await
}

async fn service_with_codex(codex: &str) -> (tempfile::TempDir, Arc<Service>) {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: codex.into(),
        claude_bin: "unused".into(),
        opencode_bin: "unused".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    (root, service)
}

async fn rpc_flood_on_service(root: tempfile::TempDir, service: Arc<Service>) {
    let id = crate::config::id();
    let row = json!({ "id": id, "taskId": id, "status": "running" });
    service
        .store
        .transaction(move |db| db.add_run_fixture(&row, None).map(|_| ()))
        .await
        .unwrap();
    let checkpoint = Checkpoint::new(
        service.store.clone(),
        id.clone(),
        RunCheckpoint::fresh(None),
    );
    let mut run = service.store.run(&id).await.unwrap();
    let mut account = None;
    let mut sensitive = Vec::new();
    let cancel = CancellationToken::new();
    let probe = OutputProbe {
        refreshes: Arc::new(AtomicU64::new(0)),
        before_canonical: Arc::new(AtomicU64::new(u64::MAX)),
        history: None,
    };
    let mut execution = Execution {
        s: &service,
        id: id.clone(),
        run: &mut run,
        account: &mut account,
        preparation: None,
        cancel: &cancel,
        checkpoint: &checkpoint,
        sensitive: &mut sensitive,
        provider: Provider::Codex,
        directory: root.path().into(),
        attempt_id: "fixture-attempt".into(),
        reservation: None,
        output_probe: Some(probe.clone()),
    };
    let mut stderr = String::new();
    for request in 1..=6090 {
        stderr.push_str(&format!("2026-11-01T00:19:49Z INFO cairn_performance: operation=\"codex_rpc\" method=\"thread/items/list\" request_id={request} event=\"started\" timeout_ms=20000\n"));
        stderr.push_str(&format!("2026-11-01T00:19:49Z INFO cairn_performance: operation=\"codex_rpc\" method=\"thread/items/list\" request_id={request} event=\"completed\" elapsed_ms=1 queue_ms=0 timeout_ms=20000 succeeded=true\n"));
    }
    let stdout = concat!(
        "{\"type\":\"turn.started\"}\n",
        "{\"type\":\"item.completed\",\"item\":{\"id\":\"answer\",\"type\":\"agent_message\",\"text\":\"rotated-test-secret answer\"}}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":4,\"output_tokens\":2}}\n",
    );
    let (sender, mut events) = mpsc::channel(32);
    let telemetry = output::Publisher::new(&id, &crate::config::id());
    let diagnostics_sender = sender.clone();
    let diagnostics = tokio::spawn(async move {
        output::read_agent_diagnostics(stderr.as_bytes(), diagnostics_sender, telemetry).await;
        // A child writing stderr before its first stdout frame must not be held
        // behind manager-side credential work by stderr pipe backpressure.
        output::read_output(stdout.as_bytes(), false, sender, Vec::new()).await;
    });
    // Give the pipe reader a chance to fill the reference queue before draining.
    while events.len() < 32 && !diagnostics.is_finished() {
        tokio::task::yield_now().await;
    }
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let wall = std::time::Instant::now();
    let exit = execution
        .wait(
            &mut Supervised::exited_fixture(),
            &mut events,
            &mut 0,
            false,
        )
        .await
        .unwrap();
    diagnostics.await.unwrap();
    let controlled_elapsed_ms = started.elapsed().as_millis();
    tokio::time::resume();
    let first = probe.before_canonical.load(Ordering::Relaxed);
    let refreshes = probe.refreshes.load(Ordering::Relaxed);
    println!(
        "stream_fixture records=12180 canonical=3 refreshes={refreshes} first_canonical_refreshes={first} controlled_elapsed_ms={controlled_elapsed_ms} wall_us={}",
        wall.elapsed().as_micros()
    );
    assert_eq!(exit.code, Some(0));
    let stored = service
        .store
        .read(move |db| db.events_fixture(&id, 0, 100))
        .await
        .unwrap();
    let kinds: Vec<_> = stored.iter().map(|event| text(event, "type")).collect();
    assert_eq!(kinds, ["turn.started", "item.completed", "turn.completed"]);
    assert!(
        stored[1]["text"]
            .as_str()
            .unwrap()
            .contains("[redacted] answer")
    );
    assert!(
        !serde_json::to_string(&stored)
            .unwrap()
            .contains("rotated-test-secret")
    );
    assert!(checkpoint.read(|saved| saved.completed == Some(true)).await);
    assert!(
        first < 10,
        "routine telemetry performed {first} redaction refreshes before canonical output"
    );
    assert_eq!(refreshes, 3, "only canonical output needs account refresh");
    service.shutdown.cancel();
}

#[tokio::test]
async fn canonical_refresh_preserves_account_rotations_on_retained_output_path() {
    let (root, service) = service_fixture().await;
    let id = crate::config::id();
    let lease = Lease {
        account_id: crate::config::id(),
        run_id: id.clone(),
        provider: Provider::Codex,
        home: root.path().into(),
        model: "fixture".into(),
    };
    let key = format!("codex-account:{}", lease.account_id);
    let mut account = Some(lease);
    let mut sensitive = Vec::new();
    let checkpoint = Checkpoint::new(
        service.store.clone(),
        id.clone(),
        RunCheckpoint::fresh(None),
    );
    let cancel = CancellationToken::new();
    let mut run = json!({ "id": id });
    let mut execution = Execution {
        s: &service,
        id: id.clone(),
        run: &mut run,
        account: &mut account,
        preparation: None,
        cancel: &cancel,
        checkpoint: &checkpoint,
        sensitive: &mut sensitive,
        provider: Provider::Codex,
        directory: root.path().into(),
        attempt_id: crate::config::id(),
        reservation: None,
        output_probe: None,
    };
    for value in ["8471", "17381", "19873"] {
        service
            .vault
            .set(&key, &json!({ "tokens": { "access_token": value } }))
            .await
            .unwrap();
        execution.refresh_redactions().await.unwrap();
    }
    let observed: Vec<_> = execution.sensitive.iter().map(String::as_str).collect();
    assert_eq!(observed, ["8471", "17381", "19873"]);
    let retained = run_output::redact("8471 17381 19873", execution.sensitive);
    assert_eq!(retained, "[redacted] [redacted] [redacted]");
    service.shutdown.cancel();
}

#[test]
fn saturated_summary_sink_does_not_hold_canonical_output() {
    crate::test_process::run_ignored("worker::launch::stream_tests::saturated_summary_sink_child");
}

#[tokio::test]
#[ignore = "process-global diagnostic sink saturation, invoked by its wrapper"]
async fn saturated_summary_sink_child() {
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::{Layer, layer::SubscriberExt};

    #[derive(Default)]
    struct Gate {
        entered: CancellationToken,
        released: std::sync::Mutex<bool>,
        changed: std::sync::Condvar,
    }

    struct Release(Arc<Gate>);
    impl Drop for Release {
        fn drop(&mut self) {
            *self.0.released.lock().unwrap() = true;
            self.0.changed.notify_all();
        }
    }

    struct Operation(bool);
    impl tracing::field::Visit for Operation {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "operation" && value == "agent_rpc_summary" {
                self.0 = true;
            }
        }

        fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
    }

    struct Sink(Arc<Gate>);
    impl<S: tracing::Subscriber> Layer<S> for Sink {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut operation = Operation(false);
            event.record(&mut operation);
            if !operation.0 {
                return;
            }
            self.0.entered.cancel();
            let mut released = self.0.released.lock().unwrap();
            while !*released {
                released = self.0.changed.wait(released).unwrap();
            }
        }
    }

    let _registration = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let gate = Arc::new(Gate::default());
    let release = Release(gate.clone());
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(Sink(gate.clone())));
    async {
        assert!(crate::performance::try_record(|_| {
            tracing::info!(target: "cairn_performance", operation = "agent_rpc_summary");
        }));
        gate.entered.cancelled().await;
        let mut rejected = 0;
        for _ in 0..32 {
            if !crate::performance::try_record(|_| {}) {
                rejected += 1;
            }
        }
        assert!(rejected > 0);
        // Same real consumer, Store/checkpoint and exact canonical assertions.
        rpc_flood_fixture().await;
    }
    .with_subscriber(dispatch)
    .await;
    drop(release);
    assert!(crate::performance::flush(Duration::from_secs(5)));
}

#[path = "launch_credential_history_tests.rs"]
mod credential_history;
