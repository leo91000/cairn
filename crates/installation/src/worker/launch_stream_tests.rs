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
    rpc_flood_on_service(root, service, None).await;
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

async fn rpc_flood_on_service(
    root: tempfile::TempDir,
    service: Arc<Service>,
    report_probe: Option<Arc<output::Probe>>,
) {
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
    let sensitive = History::default();
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
        sensitive: &sensitive,
        observation: None,
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
    let (telemetry, reports) = match report_probe {
        Some(probe) => output::Publisher::channel_fixture(&id, &crate::config::id(), probe),
        None => output::Publisher::channel(&id, &crate::config::id()),
    };
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
            Some(reports),
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
    assert_eq!(
        stored[2]["payload"]["usage"],
        json!({ "input_tokens": 4, "output_tokens": 2 })
    );
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
    let sensitive = History::default();
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
        sensitive: &sensitive,
        observation: None,
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
    let snapshot = execution.sensitive.snapshot();
    let observed: Vec<_> = snapshot.iter().map(String::as_str).collect();
    assert_eq!(observed, ["8471", "17381", "19873"]);
    let retained = run_output::redact("8471 17381 19873", &execution.sensitive.snapshot());
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

#[tokio::test]
async fn output_reader_failure_and_cancelled_join_leave_no_detached_reader() {
    struct Closed(CancellationToken);
    impl Drop for Closed {
        fn drop(&mut self) {
            self.0.cancel();
        }
    }
    for abort_join in [false, true] {
        let (stdout, stdout_reader) = tokio::io::duplex(128);
        let (stderr, stderr_reader) = tokio::io::duplex(128);
        let (sender, _events) = mpsc::channel(32);
        let out_started = CancellationToken::new();
        let err_started = CancellationToken::new();
        let out_closed = CancellationToken::new();
        let err_closed = CancellationToken::new();
        let start = out_started.clone();
        let closed = out_closed.clone();
        let out_sender = sender.clone();
        let out = AbortOnDropHandle::new(tokio::spawn(async move {
            let _closed = Closed(closed);
            start.cancel();
            output::read_output(
                stdout_reader,
                false,
                out_sender,
                vec!["synthetic-history".into()],
            )
            .await;
        }));
        let start = err_started.clone();
        let closed = err_closed.clone();
        let err = AbortOnDropHandle::new(tokio::spawn(async move {
            let _closed = Closed(closed);
            start.cancel();
            output::read_output(stderr_reader, true, sender, Vec::new()).await;
        }));
        out_started.cancelled().await;
        err_started.cancelled().await;
        if abort_join {
            // Cleanup's caller is cancelled while normal EOF is still pending.
            let join = tokio::spawn(close_output_readers(out, err, false));
            join.abort();
            assert!(join.await.unwrap_err().is_cancelled());
        } else {
            close_output_readers(out, err, true).await;
        }
        out_closed.cancelled().await;
        err_closed.cancelled().await;
        // Both writers are deliberately still open. Reader termination came
        // from ownership cleanup, not a lucky EOF or a timeout.
        drop((stdout, stderr));
    }
}

#[test]
fn v2_worker_reports_final_cumulative_flood_coverage() {
    crate::test_process::run_ignored("worker::launch::stream_tests::v2_worker_reporting_child");
}

#[tokio::test]
#[ignore = "process-global summary capture, invoked by its isolated wrapper"]
async fn v2_worker_reporting_child() {
    use tracing::instrument::WithSubscriber;
    let probe = Arc::new(output::Probe::default());
    let (capture, dispatch) = output::SummaryCapture::with_dispatch(Some(probe.clone()));
    let (root, service) = service_fixture().await;
    rpc_flood_on_service(root, service, Some(probe.clone()))
        .with_subscriber(dispatch)
        .await;
    assert!(
        crate::performance::flush(Duration::from_secs(5)),
        "fixed_reporting_stages={}",
        probe.snapshot()
    );
    let reports = capture.records();
    // Intermediate windows may coalesce. Require the final cumulative result,
    // not an exact global report count or a fortunate scheduling interleave.
    let final_coverage = reports.iter().any(|report| {
        let snapshot = &report["snapshot"];
        snapshot["recognized_records"] == 12180
            && snapshot["items"]["started_records"] == 6090
            && snapshot["items"]["completed_records"] == 6090
            && snapshot["reader_closed"] == true
            && snapshot["eof"] == true
    });
    assert!(
        final_coverage,
        "fixed_reporting_stages={}",
        probe.snapshot()
    );
}

#[tokio::test]
async fn actual_attempt_cleanup_precedes_same_account_refresh_and_real_account_switch() {
    use crate::accounts::broker::tests::{active_codex, add_account, pending_once, seed_codex};
    let mut active = active_codex().await;
    let s = &active.fixture.service;
    let old = &active.fixture.lease;
    let second = add_account(s, Provider::Codex, "Second account").await;
    seed_codex(s, &second, "C", "second-identity").await;
    // Exclude the previous account deterministically even if the fixture's
    // wall-clock reset timestamp advances during the metadata refresh.
    let mut previous = s.accounts.account(s, &old.account_id).await.unwrap();
    previous.enabled = false;
    s.store.save_coding_account(&previous).await.unwrap();
    let id = old.run_id.clone();
    let row = json!({ "id": id, "taskId": id, "status": "running" });
    s.store
        .transaction(move |db| db.add_run_fixture(&row, None).map(|_| ()))
        .await
        .unwrap();
    let mut run = s.store.run(&old.run_id).await.unwrap();
    run["snapshot"]["agent"]["model"] = "fixture-fast".into();
    let mut account = Some(old.clone());
    let cancel = CancellationToken::new();
    let checkpoint = Checkpoint::new(
        s.store.clone(),
        old.run_id.clone(),
        RunCheckpoint::fresh(None),
    );
    let mut execution = Execution {
        s,
        id: old.run_id.clone(),
        run: &mut run,
        account: &mut account,
        preparation: None,
        cancel: &cancel,
        checkpoint: &checkpoint,
        sensitive: &active.history,
        observation: Some(active.observation),
        provider: Provider::Codex,
        directory: s.config.data_dir.join("fixture-run"),
        attempt_id: crate::config::id(),
        reservation: None,
        output_probe: None,
    };
    let mut cleanup = Box::pin(execution.close_account_output(Some(active.broker)));
    assert!(pending_once(cleanup.as_mut()).await);
    active.stopped.cancelled().await;
    active.refresh_gate.release().await;
    let closing = active.close_gate.event().await;
    assert!(closing["event"] == "terminating" && closing["pid"] == active.pid);
    assert!(
        pending_once(cleanup.as_mut()).await,
        "actual attempt cleanup must wait for actual session close"
    );
    assert_eq!(active.materialize.entries.load(Ordering::Relaxed), 1);
    active.close_gate.release().await;
    cleanup.await;
    assert!(unsafe { libc::kill(active.pid as libc::pid_t, 0) } != 0);
    assert!(execution.observation.is_none());
    assert!(!old.home.join(crate::accounts::broker::SOCKET).exists());
    let workspace = Workspace {
        prepared: json!({ "isolated": false }),
        state_home: old.home.clone(),
        env: Environment::new(),
        mcp: json!({}),
        authorization: crate::plugins::run_credentials::Authorization::new(
            s,
            crate::capabilities::plugin_packages::Authority::default(),
            &crate::plugins::run_credentials::RunCredentials::default(),
        ),
    };
    // The real switch releases the old lease, refreshes its codex-monitor home,
    // acquires another eligible account and runs the actual adapter transition.
    execution.switch_account(&workspace).await.unwrap();
    assert!(!active.materialize.overlapped.load(Ordering::Relaxed));
    assert!(active.materialize.entries.load(Ordering::Relaxed) >= 2);
    assert!(
        execution
            .account
            .as_ref()
            .is_some_and(|lease| lease.account_id == second)
    );
    let current = execution.account.as_ref().unwrap().clone();
    let expected = s.accounts.redactions(s, &current).await.unwrap();
    assert!(
        expected
            .iter()
            .all(|value| active.history.snapshot().contains(value))
    );
    drop(execution);
    s.accounts.release(s, &current).await.unwrap();
    assert!(active.request.await.unwrap().is_err());
    s.shutdown.cancel();
}
