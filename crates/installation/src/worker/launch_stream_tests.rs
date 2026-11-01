//! The real pipe readers, worker select loop, redaction seam and durable recorder.
use super::*;
use crate::run_fixture::DbRunFixture;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone)]
pub(super) struct OutputProbe {
    refreshes: Arc<AtomicU64>,
    before_canonical: Arc<AtomicU64>,
}

impl OutputProbe {
    pub(super) async fn refresh(&self) {
        self.refreshes.fetch_add(1, Ordering::Relaxed);
        // Model repeated sub-threshold account/Store work deterministically.
        tokio::time::sleep(Duration::from_millis(12)).await;
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
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "unused".into(),
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
    let diagnostics_sender = sender.clone();
    let diagnostics = tokio::spawn(async move {
        output::read_output(stderr.as_bytes(), true, diagnostics_sender, Vec::new()).await;
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
