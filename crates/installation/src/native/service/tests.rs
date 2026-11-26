use super::*;

#[tokio::test]
async fn inherited_output_writer_cannot_hold_finalization_open() {
    let (mut input, mut output) = tokio::io::duplex(16);
    let mut reader = tokio::spawn(async move {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).await?;
        Ok(())
    });
    assert!(
        settle_output(&mut reader, Duration::from_millis(20))
            .await
            .is_err()
    );
    assert!(reader.is_finished());
    assert!(output.write_all(b"closed").await.is_err());
}

#[tokio::test]
async fn concurrent_copy_reservations_never_spend_the_same_pool_capacity() {
    let (_root, controller) = fixture().await;
    let budget = controller.disk_growth().await.unwrap();
    let requested = budget.saturating_mul(2) / 3;
    let policy = storage::policy::Policy {
        reserve_mi_b: 64,
        ..Default::default()
    };
    let (first, first_bytes) = controller
        .reserve_copy(requested, &controller.state, &policy)
        .await
        .unwrap();
    let (second, second_bytes) = controller
        .reserve_copy(requested, &controller.state, &policy)
        .await
        .unwrap();
    assert_eq!(first_bytes, requested);
    assert!(first_bytes + second_bytes <= budget);
    assert!(second_bytes < requested);
    drop((first, second));
    assert_eq!(controller.disk_growth().await.unwrap(), budget);
}

#[test]
fn interrupted_admission_recovers_without_workspace_attempt_and_preserves_quarantine() {
    let run = crate::config::id();
    let attempt = crate::config::id();
    let mut pending = HashMap::new();
    interrupted_attempt(&mut pending, run.clone(), attempt.clone());
    assert_eq!(pending[&run], attempt);
    interrupted_attempt(&mut pending, run.clone(), crate::config::id());
    assert_eq!(pending[&run], attempt);
}

#[tokio::test]
async fn fenced_task_pressure_settles_an_admitted_write_before_draining_its_access() {
    use crate::storage::Disk;
    use std::sync::atomic::{AtomicBool, Ordering};
    let (_root, controller) = fixture().await;
    let directory = controller.state.join("disks").join(crate::config::id());
    drop(
        storage::runtime::create(
            &directory.join("lazy"),
            &json!({
                "version": 1, "size": 4096, "blockSize": 4194304,
                "blocks": [{ "offset": 0, "size": 4096, "hash": null }]
            }),
            &json!({
                "master": "http://127.0.0.1:1/", "grant": "accepted-native-write",
                "policy": { "reserveMiB": 64, "reservePercent": 1 }
            }),
        )
        .await
        .unwrap(),
    );
    let volume = storage::runtime::load(&directory).await.unwrap();
    volume.write_at(0, b"acknowledged").unwrap();
    volume.limit_backing(0).unwrap();
    let pool = disk::lock(&controller.state).await.unwrap();
    let live_access = Arc::new(Mutex::new(()));
    let admitted = live_access.clone().lock_owned().await;
    let mut storage_close = StorageClose::new(live_access);
    let settled = Arc::new(AtomicBool::new(false));
    let observed = settled.clone();
    let writing = volume.clone();
    let accepted = tokio::spawn(async move {
        let _pool = pool;
        let _admitted = admitted;
        let result = tokio::task::spawn_blocking(move || writing.write_at(0, b"pending"))
            .await
            .unwrap();
        observed.store(true, Ordering::Release);
        result
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while volume.status().unwrap()["performance"]["writeAdmission"]["inFlight"] != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let finalizing = volume.clone();
    let mut finish = tokio::spawn(async move {
        storage_close
            .finish_budgeted(&finalizing, async move {
                assert!(
                    settled.load(Ordering::Acquire),
                    "accepted access must actually drain"
                );
                Ok(block::Completion::Flushed)
            })
            .await
    });
    let result = tokio::time::timeout(Duration::from_secs(3), &mut finish).await;
    if result.is_err() {
        volume.stop.cancel();
        let _ = finish.await;
    }
    let written = accepted.await.unwrap();
    assert!(
        result.is_ok(),
        "quota handling must precede accepted-access drainage"
    );
    assert_eq!(
        result.unwrap().unwrap().unwrap(),
        block::Completion::Flushed
    );
    assert_eq!(written.unwrap_err().raw_os_error(), Some(libc::EIO));
    let mut bytes = [0; 12];
    volume.disk.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"acknowledged");
}

pub(crate) async fn validate_staged_plan(data: &Path, id: &str) -> Result<Value> {
    let (_root, mut controller) = fixture().await;
    controller.data = data.to_owned();
    controller.plan(id).await
}

/// Real output guard, completion-error branch and authenticated wait/client.
/// Sparse bytes model an exhausted journal without storing child payloads.
pub(crate) async fn output_limit_completion(attempt: &str) -> crate::runner::ControllerResult {
    let (_root, controller) = fixture().await;
    let log = controller.file(attempt, "log");
    let file = tokio::fs::File::create(&log).await.unwrap();
    file.set_len(MAX_LOG + 1).await.unwrap();
    drop(file);
    let output = b"{\"type\":\"turn.started\"}\n";
    let result = controller.output(attempt, false, output.as_slice()).await;
    let error = result.as_ref().unwrap_err();
    assert_eq!(error.status, 400);
    assert_eq!(
        error.message,
        "Native output exceeded its bounded log budget."
    );
    let code = controller
        .execution_status(attempt, result.map(|()| 0))
        .await;
    let terminal = tokio::fs::read(controller.file(attempt, "terminal"))
        .await
        .unwrap();
    assert_eq!(terminal, br#"{"reason":"native_output_limit"}"#);
    assert!(terminal.len() as u64 <= MAX_TERMINAL_METADATA);
    assert_eq!(code, crate::runner::CONTROLLER_FAILED);
    assert_eq!(tokio::fs::metadata(log).await.unwrap().len(), MAX_LOG + 1);
    atomic_write(
        &controller.file(attempt, "exit"),
        exit_record(code, false, controller.terminal_reason(attempt).await).as_bytes(),
    )
    .await
    .unwrap();
    authenticated_wait_result(controller, attempt, false).await
}

pub(crate) async fn retired_cleanup_completion(attempt: &str) -> crate::runner::ControllerResult {
    let (_root, controller) = fixture().await;
    let run = crate::config::id();
    private_dir(&controller.state.join("native-cleanup"))
        .await
        .unwrap();
    atomic_write(&controller.file(attempt, "run"), run.as_bytes())
        .await
        .unwrap();
    controller
        .record_cleanup_pending(attempt, "Native cleanup was pending.")
        .await
        .unwrap();
    atomic_write(
        &controller.file(attempt, "cleanup-result"),
        br#"{"completion":"Flushed"}"#,
    )
    .await
    .unwrap();
    authenticated_wait_result(controller, attempt, true).await
}

async fn authenticated_wait_result(
    controller: Controller,
    attempt: &str,
    observe_retirement: bool,
) -> crate::runner::ControllerResult {
    let router = Router::new().fallback(any(handler)).with_state(controller);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let url = format!("http://{address}/runs/{attempt}");
    let http = reqwest::Client::new();
    let unauthorized = http.post(format!("{url}/wait")).send().await.unwrap();
    assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);
    let token = "synthetic-private-controller-token";
    let initial = crate::runner::wait_for_exit(&http, &url, token)
        .await
        .unwrap();
    let result = if observe_retirement {
        assert!(initial.finalization_failed);
        assert!(!initial.physical_finalized);
        assert!(initial.native_cleanup.is_some());
        crate::runner::observe_cleanup_retirement(&http, &url, token)
            .await
            .unwrap()
    } else {
        initial
    };
    server.abort();
    let _ = server.await;
    result
}

#[tokio::test]
async fn native_budget_validates_persisted_protocol_and_keeps_fresh_bootstrap() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(crate::microvm::budget::FILE);
    assert_eq!(load_budget(&path).await.unwrap().slots, 1);
    let request = json!({
        "slots": 6,
        "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 },
    });
    assert_eq!(budget_request(&request).unwrap().slots, 6);
    for protocol in [None, Some(json!(null)), Some(json!(2)), Some(json!("1"))] {
        let mut record = request.clone();
        if let Some(protocol) = protocol {
            record["nativeModeProtocol"] = protocol;
        }
        let bytes = serde_json::to_vec(&record).unwrap();
        atomic_write(&path, &bytes).await.unwrap();
        let error = load_budget(&path).await.unwrap_err();
        assert!(error.message.contains("native mode storage protocol"));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), bytes);
    }
    atomic_write(
        &path,
        &serde_json::to_vec(&budget_record(&request)).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(load_budget(&path).await.unwrap().slots, 6);
}

#[tokio::test]
async fn native_startup_reclaims_interrupted_preparations_and_preserves_committed_disks() {
    let (_fixture, controller) = fixture().await;
    let run = crate::config::id();
    let attempt = crate::config::id();
    let disk = controller.state.join("disks").join(&run);
    let abandoned = controller
        .state
        .join("environments")
        .join(crate::config::id());
    private_dir(&disk.join("lazy")).await.unwrap();
    atomic_write(&disk.join("lazy/preserved"), b"committed payload")
        .await
        .unwrap();
    private_dir(&abandoned.join("lazy")).await.unwrap();
    atomic_write(&abandoned.join("lazy/partial"), b"unclaimed")
        .await
        .unwrap();
    let stage = controller.data.join("runs").join(&run);
    private_dir(&stage).await.unwrap();
    let pin = super::super::bases::pin_path(&stage, &attempt).unwrap();
    atomic_write(&pin, b"").await.unwrap();
    recover_preparations(&controller.data, &controller.state)
        .await
        .unwrap();
    assert!(!abandoned.exists());
    assert!(!pin.exists());
    assert_eq!(
        storage::environment::directory(&controller.state, &run).unwrap(),
        disk
    );
    assert_eq!(
        tokio::fs::read(disk.join("lazy/preserved")).await.unwrap(),
        b"committed payload"
    );
}

#[tokio::test]
async fn optional_base_limits_do_not_reject_valid_zip_preparation() {
    let (_fixture, controller) = fixture().await;
    let run = crate::config::id();
    let project_root = controller.data.join("runs").join(&run).join("workspace");
    let archive = json!({
        "projectId": crate::config::id(),
        "revision": "a".repeat(40),
        "key": "b".repeat(64),
        "hash": "c".repeat(64),
        "bytes": 100,
        "logicalBytes": 15
    });
    for (size, count) in [(2_097_152, 1), (128, 65)] {
        let mut archives = Vec::new();
        for _ in 0..count {
            let mut value = archive.clone();
            value["projectId"] = crate::config::id().into();
            archives.push(value);
        }
        let plan = json!({
            "id": crate::config::id(),
            "runId": run,
            "projectRoot": project_root,
            "resources": { "cpu": 1, "memoryMiB": 512, "diskMiB": size },
            "imports": [{ "source": project_root, "target": project_root }],
            "nativeProjectArchives": archives
        });
        controller.check_preparation_space(&plan).await.unwrap();
        assert!(
            !controller
                .bases()
                .initialize(&plan, &CancellationToken::new())
                .await
                .unwrap()
        );
    }
}

#[tokio::test]
async fn preparation_space_check_scans_usage_once_unless_it_reclaims() {
    let (_fixture, controller) = fixture().await;
    let run = crate::config::id();
    let project_root = controller.data.join("runs").join(&run).join("workspace");
    let plan = |logical_bytes: u64| {
        json!({
            "id": crate::config::id(),
            "runId": run,
            "projectRoot": project_root,
            "resources": { "cpu": 1, "memoryMiB": 512, "diskMiB": 128 },
            "imports": [{ "source": project_root, "target": project_root }],
            "nativeProjectArchives": [{
                "projectId": crate::config::id(),
                "revision": "a".repeat(40),
                "key": "b".repeat(64),
                "hash": "c".repeat(64),
                "bytes": 100,
                "logicalBytes": logical_bytes
            }]
        })
    };

    let before = disk::usage_scans_of(&controller.state);
    controller.check_preparation_space(&plan(15)).await.unwrap();
    assert_eq!(disk::usage_scans_of(&controller.state) - before, 1);

    // Reclamation changes the pool, so exactly one more sample follows it.
    let before = disk::usage_scans_of(&controller.state);
    let huge = controller.disk_limit().await.unwrap() * 2;
    assert!(
        controller
            .check_preparation_space(&plan(huge))
            .await
            .is_err()
    );
    assert!(disk::usage_scans_of(&controller.state) - before <= 2);
}

#[tokio::test]
async fn physical_reserve_pressure_reclaims_unpinned_bases_with_logical_budget_to_spare() {
    let (_fixture, controller) = fixture().await;
    let state = &controller.state;
    let pool = workspace::data_root(state).join(super::super::bases::POOL_DIRECTORY);
    let evictable = "a".repeat(64);
    let pinned = "b".repeat(64);
    for key in [&evictable, &pinned] {
        let contents = pool.join(key);
        private_dir(&contents).await.unwrap();
        std::fs::write(contents.join("cached"), b"cached work").unwrap();
        let control = state.join("native-bases").join(key);
        private_dir(&control).await.unwrap();
        std::fs::write(control.join("pin"), b"").unwrap();
    }
    std::fs::hard_link(
        state.join("native-bases").join(&pinned).join("pin"),
        state.join("pinned-source"),
    )
    .unwrap();
    let run = crate::config::id();
    let project = controller.data.join("runs").join(&run).join("workspace");
    private_dir(&project).await.unwrap();
    std::fs::write(project.join("seed"), b"new work").unwrap();
    let plan = json!({
        "runId": run,
        "projectRoot": project,
        "resources": { "diskMiB": 128 },
        "imports": [{ "source": project, "target": project }],
        "storage": { "policy": { "reserveMiB": 16 * 1024 * 1024 } },
    });
    assert!(controller.disk_growth().await.unwrap() > disk::MIB);
    let error = controller.check_preparation_space(&plan).await.unwrap_err();
    assert_eq!(error.status, 507);
    assert!(!pool.join(evictable).exists());
    assert!(pool.join(pinned).is_dir());
}

#[tokio::test]
async fn budget_update_does_not_block_budget_reads_while_waiting_for_storage() {
    let (_fixture, controller) = fixture().await;
    let owner =
        crate::file_lock::exclusive(&controller.state.join("storage.lock"), "fixture owner")
            .unwrap();
    let mut value = controller.budget.lock().await.clone();
    value["slots"] = 2.into();
    let updating = controller.clone();
    let pending =
        tokio::spawn(async move { route(updating, request("/node-budget", &value)).await });
    let waiting = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if crate::file_lock::exclusive(
                &controller.state.join("storage-foreground.lock"),
                "priority probe",
            )
            .is_err()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok();
    let readable = controller.budget.try_lock().is_ok();
    // Abort this direct route before releasing the fixture disk; no real
    // systemd budget operation is admitted by this concurrency test.
    pending.abort();
    let _ = pending.await;
    drop(owner);
    assert!(waiting);
    assert!(readable);
}

#[tokio::test]
async fn mixed_storage_policy_changes_still_wait_for_the_disk_gate() {
    use crate::storage::policy::{Configuration, Policy};

    let (_fixture, controller) = fixture().await;
    let first = Configuration {
        generation: 1,
        policy: Policy::default(),
    };
    controller
        .storage_policy
        .apply(&controller.state, first.clone())
        .await
        .unwrap();
    let owner = disk::lock(&controller.state).await.unwrap();
    let mixed = Configuration {
        generation: 2,
        policy: Policy {
            reserve_mi_b: 64,
            memory_cache_mi_b: 128,
            ..first.policy.clone()
        },
    };
    let updating = controller.clone();
    let pending =
        tokio::spawn(
            async move { route(updating, request("/storage-policy", &json!(mixed))).await },
        );
    let waiting = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if crate::file_lock::exclusive(
                &controller.state.join("storage-foreground.lock"),
                "priority probe",
            )
            .is_err()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok();
    let current = controller.storage_policy.current().await;
    // A queued general update must not retain Control's mutex and prevent
    // a later committed reserve reduction from reaching blocked writers.
    let relief = Configuration {
        generation: 3,
        policy: Policy {
            reserve_mi_b: 64,
            ..first.policy.clone()
        },
    };
    let applied = tokio::time::timeout(
        Duration::from_secs(2),
        route(
            controller.clone(),
            request("/storage-policy", &json!(relief)),
        ),
    )
    .await;
    drop(owner);
    let result = tokio::time::timeout(Duration::from_secs(2), pending).await;
    assert!(waiting);
    assert_eq!(current, Some(first));
    applied.unwrap().unwrap();
    result.unwrap().unwrap().unwrap();
    assert_eq!(controller.storage_policy.current().await, Some(relief));
}

#[test]
fn storage_policy_relief_reaches_a_writer_while_the_disk_gate_is_held() {
    // Other storage tests deliberately hold process-global write admission.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native::service::tests::storage_policy_relief_child",
            "--ignored",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[ignore = "isolated process-global write admission and pressure relief child"]
async fn storage_policy_relief_child() {
    use crate::storage::{
        Disk,
        policy::{Configuration, Policy},
    };

    let (_fixture, controller) = fixture().await;
    let directory = controller
        .state
        .join("environments")
        .join(crate::config::id());
    let policy = Policy {
        reserve_mi_b: 64,
        reserve_percent: 1,
        ..Policy::default()
    };
    drop(
        storage::runtime::create_prepared(
            &directory.join("lazy"),
            &json!({
                "version": 1,
                "size": 4096,
                "blockSize": 4194304,
                "blocks": [{ "offset": 0, "size": 4096, "hash": null }]
            }),
            &json!({ "unassigned": true, "policy": policy }),
        )
        .await
        .unwrap(),
    );
    let volume = storage::runtime::load(&directory).await.unwrap();
    let blocked = Configuration {
        generation: 1,
        policy: Policy {
            reserve_mi_b: 16 * 1024 * 1024,
            ..policy.clone()
        },
    };
    route(
        controller.clone(),
        request("/storage-policy", &json!(blocked)),
    )
    .await
    .unwrap();

    // Filesystem close holds this gate while syncfs waits for outstanding
    // block writes. Keep its ownership until the route has relieved I/O.
    let owner = disk::lock(&controller.state).await.unwrap();
    let writing = volume.clone();
    let mut writer = tokio::task::spawn_blocking(move || writing.write_at(0, b"preserved"));
    let pressure = tokio::time::timeout(Duration::from_secs(5), async {
        while volume.status().unwrap()["performance"]["writeAdmission"]["inFlight"] != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let blocked = !writer.is_finished();

    let relief = Configuration {
        generation: 2,
        policy,
    };
    let updating = controller.clone();
    let value = json!(relief);
    let mut applying =
        tokio::spawn(async move { route(updating, request("/storage-policy", &value)).await });
    let applied = tokio::time::timeout(Duration::from_secs(2), &mut applying).await;
    // A broken route must fail without retaining a blocked writer/thread.
    if applied.is_err() {
        applying.abort();
        let _ = applying.await;
        controller
            .storage_policy
            .apply(&controller.state, relief.clone())
            .await
            .unwrap();
    }
    let written = tokio::time::timeout(Duration::from_secs(5), &mut writer).await;
    assert!(
        crate::file_lock::exclusive(&controller.state.join("storage.lock"), "other owner").is_err()
    );
    drop(owner);
    pressure.unwrap();
    assert!(blocked, "the reserve must still protect the journal");
    applied.unwrap().unwrap().unwrap();
    written.unwrap().unwrap().unwrap();
    assert_eq!(controller.storage_policy.current().await, Some(relief));
    let mut bytes = [0; 9];
    volume.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"preserved");
}

pub(super) async fn fixture() -> (tempfile::TempDir, Controller) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let state = root.path().join("state");
    private_dir(&data).await.unwrap();
    private_dir(&state).await.unwrap();
    atomic_write(
        &data.join("runner-secret"),
        b"synthetic-private-controller-token",
    )
    .await
    .unwrap();
    let controller = Controller {
        bootstrap: bootstrap::Registry::new("fixture-runtime".into(), Some(crate::config::id())),
        storage_policy: crate::storage::policy::Control::default(),
        data,
        state,
        active: Arc::default(),
        capacity: Arc::default(),
        mounted: Arc::default(),
        leases: Arc::default(),
        admission: Arc::default(),
        exclusive: Arc::default(),
        base_warmer: Arc::default(),
        mode: Arc::new(Mutex::new(
            json!({ "mode": "protected", "generation": null, "status": "applied" }),
        )),
        budget: Arc::new(Mutex::new(
            json!({ "slots": 1, "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 } }),
        )),
        changed: Arc::default(),
        log_lock: Arc::default(),
        telemetry: health::Telemetry::default(),
        stop: CancellationToken::new(),
        cleanup: crate::native::cleanup::Registry::default(),
        environments: Arc::default(),
        identity: Arc::new(systemd::Identity {
            user: "synthetic-workload".into(),
            uid: 65534,
            gid: 65534,
        }),
    };
    (root, controller)
}

#[tokio::test]
async fn bootstrap_phase_is_visible_while_real_admission_waits() {
    let (_root, controller) = fixture().await;
    let id = crate::config::id();
    let admission = controller.admission.lock().await;
    let (starting, attempt) = (controller.clone(), id.clone());
    let start = tokio::spawn(async move { starting.start(&attempt).await });
    tokio::task::yield_now().await;

    let response = tokio::time::timeout(
        Duration::from_secs(1),
        handler(
            State(controller.clone()),
            Request::builder()
                .uri(format!("/runs/{id}/bootstrap"))
                .header("authorization", "Bearer synthetic-private-controller-token")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("bootstrap observation cannot wait for admission")
    .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 32_768)
        .await
        .unwrap();
    let health = tokio::time::timeout(Duration::from_secs(1), controller.health())
        .await
        .expect("observation cannot make health wait for admission");
    start.abort();
    let _ = start.await;
    drop(admission);

    assert_eq!(status, 200, "missing bootstrap observation: {body:?}");
    let report: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(report["bootstrap"]["attemptId"], id);
    assert_eq!(report["bootstrap"]["status"], "preparing");
    assert_eq!(report["bootstrap"]["phase"], "admission_wait");
    assert_eq!(health["activeRuns"], 0);
}

#[tokio::test]
async fn bootstrap_lookup_requires_authentication_and_reports_retention_loss() {
    let (_root, controller) = fixture().await;
    let id = crate::config::id();
    let observation = controller.bootstrap.begin(&id);
    observation.context(&crate::config::id(), Some("fixture-runtime"));
    observation.submitted();
    let unauthorized = route(
        controller.clone(),
        Request::builder()
            .uri(format!("/runs/{id}/bootstrap"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(unauthorized.status, 401);
    let unknown = route(
        controller.clone(),
        Request::builder()
            .uri(format!("/runs/{}/bootstrap", crate::config::id()))
            .header("authorization", "Bearer synthetic-private-controller-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(unknown.status, 404);
    assert!(controller.bootstrap.get(&id).is_some());
}

#[tokio::test]
async fn actual_home_reconciliation_exposes_its_held_phase_without_blocking_controls() {
    let module = module_path!().split_once("::").unwrap().1;
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            &format!("{module}::held_home_reconciliation_child"),
            "--ignored",
        ])
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(15), child.output())
        .await
        .expect("isolated blocked-I/O regression must finish")
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed;"));
}

#[tokio::test]
#[ignore = "isolated FIFO regression, executed by its parent"]
async fn held_home_reconciliation_child() {
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;

    let (root, controller) = fixture().await;
    let data = root.path().join("durable");
    let source = root.path().join("staging");
    private_dir(&data).await.unwrap();
    private_dir(&source).await.unwrap();
    let metadata = std::fs::metadata(&data).unwrap();
    let identity = super::super::identity::Identity {
        user: "fixture".into(),
        root_uid: metadata.uid().saturating_add(131072),
        root_gid: metadata.gid().saturating_add(131072),
        subuid: 100000,
        subgid: 200000,
        host_engine: Some((metadata.uid(), metadata.gid())),
    };
    std::fs::write(
        data.join(".cairn-identity.json"),
        serde_json::to_vec(&identity).unwrap(),
    )
    .unwrap();
    let path = data.join(".cairn-identity.transition.json");
    let fifo = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let transition = serde_json::to_vec(&json!({
        "previous": &identity,
        "target": &identity,
    }))
    .unwrap();
    let (opened, opening) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        opened.send(()).unwrap();
        blocked.recv_timeout(Duration::from_secs(5)).unwrap();
        file.write_all(&transition).unwrap();
    });
    let id = crate::config::id();
    let scope = controller.bootstrap.begin(&id);
    let trace = scope.trace();
    let preparing = tokio::spawn(async move {
        super::super::home::prepare_for_identity(&source, &data, &identity, &trace).await
    });
    let arrived = tokio::task::spawn_blocking(move || opening.recv_timeout(Duration::from_secs(3)))
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(1), controller.health()).await;
    let phase = controller.bootstrap.get(&id).unwrap();
    let lease = tokio::time::timeout(
        Duration::from_secs(1),
        handler(
            State(controller.clone()),
            request(
                &format!("/runs/{id}/lease"),
                &json!({ "remainingMs": 60_000 }),
            ),
        ),
    )
    .await;
    release.send(()).unwrap();
    writer.join().unwrap();
    preparing.await.unwrap().unwrap();
    arrived.expect("the real reconciler must reach the held metadata read");
    response.expect("health must remain responsive during actual reconciliation");
    lease
        .expect("lease renewal must remain responsive")
        .unwrap();
    assert!(controller.leased(&id).await);
    let phase = serde_json::to_value(phase).unwrap();
    assert_eq!(phase["phase"], "identity_reconcile");
    assert_eq!(phase["status"], "preparing");
    assert_eq!(controller.health().await["activeRuns"], 0);
    drop(scope);
}

async fn busy_disk_fixture(controller: &Controller) -> (String, String, Value) {
    let run = crate::config::id();
    let id = crate::config::id();
    let root = workspace::directory(&controller.state, &run).unwrap();
    private_dir(&root).await.unwrap();
    // Stop before any privileged mount after ownership becomes available.
    // This permanent missing-journal conflict must never be retried.
    atomic_write(&root.join("initialized"), b"1").await.unwrap();
    atomic_write(&root.join("block-initialized"), b"1")
        .await
        .unwrap();
    atomic_write(&controller.file(&id, "log"), b"")
        .await
        .unwrap();
    let plan = json!({
        "runId": run,
        "resources": { "diskMiB": 128 },
    });
    controller
        .leases
        .lock()
        .await
        .insert(id.clone(), crate::nodes::boot_ms() + 60_000);
    (run, id, plan)
}

#[tokio::test]
async fn activation_fences_retirement_until_the_attempt_is_registered() {
    let (_fixture, controller) = fixture().await;
    let (run, id, _) = busy_disk_fixture(&controller).await;
    let stage = controller.data.join("runs").join(&run);
    let workspace = stage.join("workspace");
    let home = stage.join("home");
    let plans = controller.data.join("runner-plans");
    for directory in [&workspace, &home, &plans] {
        private_dir(directory).await.unwrap();
    }
    let plan = json!({
        "id": id, "runId": run, "backend": "native", "nodeLeaseRequired": true,
        "cwd": workspace, "projectRoot": workspace, "sandbox": "workspace-write",
        "args": [], "prompt": "fixture", "expires": null,
        "resources": { "cpu": 1, "memoryMiB": 128, "diskMiB": 128 },
        "imports": [
            { "source": workspace, "target": workspace },
            { "source": home, "target": "/home/node" },
        ],
    });
    atomic_write(
        &plans.join(format!("{id}.json")),
        &serde_json::to_vec(&plan).unwrap(),
    )
    .await
    .unwrap();
    let capacity = controller.capacity.lock().await;
    let mut starting = {
        let controller = controller.clone();
        let id = id.clone();
        tokio::spawn(async move { controller.start(&id).await })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut starting => panic!("Start exited before activation: {result:?}"),
            () = async {
                while !controller.active.lock().await.contains_key(&id) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            } => {},
        }
    })
    .await
    .unwrap();

    let gate = crate::file_lock::exclusive(
        &controller.state.join("storage.lock"),
        "activation owns storage",
    );
    assert!(gate.err().unwrap().is_conflict());
    starting.abort();
    assert!(starting.await.unwrap_err().is_cancelled());
    drop(capacity);
}

#[tokio::test]
async fn start_path_observes_deadline_and_cancellation_before_storage_admission() {
    let (_fixture, controller) = fixture().await;
    let (run, id, _) = busy_disk_fixture(&controller).await;
    let stage = controller.data.join("runs").join(&run);
    for directory in [
        stage.join("workspace"),
        stage.join("home"),
        controller.data.join("runner-plans"),
    ] {
        private_dir(&directory).await.unwrap();
    }
    let mut plan = json!({
        "id": id,
        "runId": run,
        "backend": "native",
        "nodeLeaseRequired": true,
        "cwd": stage.join("workspace"),
        "projectRoot": stage.join("workspace"),
        "sandbox": "workspace-write",
        "args": [],
        "prompt": "fixture",
        "resources": { "cpu": 1, "memoryMiB": 128, "diskMiB": 128 },
        "imports": [
            { "source": stage.join("workspace"), "target": stage.join("workspace") },
            { "source": stage.join("home"), "target": "/home/node" }
        ],
    });
    let now = Arc::new(std::sync::atomic::AtomicI64::new(crate::config::now()));
    let expires = now.load(std::sync::atomic::Ordering::SeqCst) + 1000;
    plan["expires"] = expires.into();
    atomic_write(
        &controller
            .data
            .join("runner-plans")
            .join(format!("{id}.json")),
        plan.to_string().as_bytes(),
    )
    .await
    .unwrap();
    // Hold storage without foreground intent, so a foreground lock proves
    // start has validated the plan and reached its disk admission wait.
    let held = crate::file_lock::exclusive(&controller.state.join("storage.lock"), "fixture owner")
        .unwrap();
    controller
        .leases
        .lock()
        .await
        .insert(id.clone(), crate::nodes::boot_ms() + 10_000);
    let error = tokio::time::timeout(Duration::from_secs(5), async {
        let starting = crate::config::TEST_NOW.scope(now.clone(), controller.start(&id));
        tokio::pin!(starting);
        tokio::select! {
            result = &mut starting => result,
            () = async {
                while crate::file_lock::exclusive(
                    &controller.state.join("storage-foreground.lock"),
                    "fixture admission probe",
                )
                .is_ok()
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            } => {
                // Fixture fsyncs cannot consume the execution deadline.
                // Advance only start's wall clock once it waits on disk.
                now.store(expires, std::sync::atomic::Ordering::SeqCst);
                starting.await
            }
        }
    })
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.status, 408, "{}", error.message);
    let saved: Value = serde_json::from_slice(
        &tokio::fs::read(controller.file(&id, "stop-reason.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved["reason"], "time_limit");
    assert!(controller.active.lock().await.is_empty());

    let id = crate::config::id();
    plan["id"] = id.clone().into();
    plan["expires"] = Value::Null;
    atomic_write(
        &controller
            .data
            .join("runner-plans")
            .join(format!("{id}.json")),
        plan.to_string().as_bytes(),
    )
    .await
    .unwrap();
    controller
        .leases
        .lock()
        .await
        .insert(id.clone(), crate::nodes::boot_ms() + 10_000);
    let starting = tokio::spawn({
        let controller = controller.clone();
        let id = id.clone();
        async move { controller.start(&id).await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while controller.admission.try_lock().is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // DELETE publishes this marker before it waits for admission/fencing.
    atomic_write(&controller.file(&id, "stopped"), b"1")
        .await
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(2), starting)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.is_conflict());
    assert!(controller.admission.try_lock().is_ok());
    assert!(controller.active.lock().await.is_empty());
    drop(held);
}

#[tokio::test]
async fn native_disk_admission_waits_without_holding_the_global_storage_lock() {
    let (_fixture, controller) = fixture().await;
    let (run, id, plan) = busy_disk_fixture(&controller).await;
    let held = storage::environment::ownership(&controller.state, &run, "fixture owner")
        .await
        .unwrap();
    let waiting = tokio::spawn({
        let controller = controller.clone();
        let id = id.clone();
        async move {
            controller
                .mount_after_release(
                    &id,
                    &plan,
                    &CancellationToken::new(),
                    Duration::from_secs(5),
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if tokio::fs::metadata(controller.file(&id, "log"))
                .await
                .unwrap()
                .len()
                > 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!waiting.is_finished());
    // Shutdown of the previous writer must be able to take this lock.
    let global = tokio::time::timeout(Duration::from_secs(1), disk::lock(&controller.state))
        .await
        .unwrap()
        .unwrap();
    drop(global);
    drop(held);
    let error = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .err()
        .unwrap();
    assert_eq!(error.status, 409);
    assert!(error.message.starts_with("Native journal is missing"));
    assert!(!controller.file(&id, "exit").exists());
    assert!(
        storage::environment::ownership(&controller.state, &run, "probe")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn native_disk_admission_retries_physical_contention_during_bootstrap() {
    let (_fixture, controller) = fixture().await;
    let (run, id, plan) = busy_disk_fixture(&controller).await;
    let root = workspace::directory(&controller.state, &run).unwrap();
    tokio::fs::remove_file(root.join("block-initialized"))
        .await
        .unwrap();
    let lease = storage::environment::ownership(&controller.state, &run, "fixture")
        .await
        .unwrap();
    let held =
        crate::file_lock::exclusive(&lease.directory.join("lock"), "physical owner").unwrap();
    drop(lease);
    let waiting = tokio::spawn({
        let controller = controller.clone();
        let id = id.clone();
        async move {
            controller
                .mount_after_release(
                    &id,
                    &plan,
                    &CancellationToken::new(),
                    Duration::from_secs(5),
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if tokio::fs::metadata(controller.file(&id, "log"))
                .await
                .unwrap()
                .len()
                > 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!waiting.is_finished());
    let global = tokio::time::timeout(Duration::from_secs(1), disk::lock(&controller.state))
        .await
        .unwrap()
        .unwrap();
    drop(global);
    drop(held);
    // The intentionally absent storage credentials reject before privileged
    // image preparation, proving that bootstrap retried after lock release.
    let error = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .err()
        .unwrap();
    assert!(
        error.is_internal(),
        "invalid storage fixture must reject before image preparation"
    );
    assert_ne!(error.message, block::DISK_BUSY);
}

#[tokio::test]
async fn lease_expiry_during_disk_admission_records_the_initiating_cause() {
    let (_fixture, controller) = fixture().await;
    let (run, id, plan) = busy_disk_fixture(&controller).await;
    let held = storage::environment::ownership(&controller.state, &run, "fixture")
        .await
        .unwrap();
    let stop = CancellationToken::new();
    controller
        .leases
        .lock()
        .await
        .insert(id.clone(), crate::nodes::boot_ms() + 150);
    let watching = tokio::spawn({
        let controller = controller.clone();
        let id = id.clone();
        let stop = stop.clone();
        async move { controller.watch_lease(&id, stop).await }
    });
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        controller.mount_after_release(&id, &plan, &stop, Duration::from_secs(5)),
    )
    .await
    .unwrap()
    .err()
    .unwrap();
    assert_eq!(error.message, "Native disk preparation stopped.");
    watching.await.unwrap();
    let path = controller.file(&id, "stop-reason.json");
    // Fencing only waits a bounded time for diagnostics. Its blocking
    // writer retains ownership and may commit after both callers return.
    let saved = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match tokio::fs::read(&path).await {
                Ok(bytes) => break serde_json::from_slice::<Value>(&bytes).unwrap(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("Could not read native stop reason: {error}"),
            }
        }
    })
    .await
    .expect("Native stop diagnostics must finish committing after fencing");
    assert_eq!(saved["reason"], "lease_expired");
    controller.record_stop(&id, "cancelled", Value::Null).await;
    assert_eq!(
        serde_json::from_slice::<Value>(&tokio::fs::read(path).await.unwrap()).unwrap(),
        saved
    );
    assert!(
        storage::environment::ownership(&controller.state, &run, "probe")
            .await
            .is_err()
    );
    drop(held);
}

#[tokio::test]
async fn lease_expiry_on_global_lock_release_prevents_preparation_without_a_watcher() {
    let (_fixture, controller) = fixture().await;
    let (_, id, plan) = busy_disk_fixture(&controller).await;
    let held = disk::lock(&controller.state).await.unwrap();
    controller
        .leases
        .lock()
        .await
        .insert(id.clone(), crate::nodes::boot_ms() + 50);
    let waiting = tokio::spawn({
        let controller = controller.clone();
        let id = id.clone();
        async move {
            controller
                .mount_after_release(
                    &id,
                    &plan,
                    &CancellationToken::new(),
                    Duration::from_secs(5),
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(held);
    let error = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .err()
        .unwrap();
    assert_eq!(error.message, "Native disk preparation stopped.");
    let saved: Value = serde_json::from_slice(
        &tokio::fs::read(controller.file(&id, "stop-reason.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved["reason"], "lease_expired");
}

#[tokio::test]
async fn controller_shutdown_during_preparation_retains_its_specific_reason() {
    let (_fixture, controller) = fixture().await;
    let (_, id, plan) = busy_disk_fixture(&controller).await;
    let held = disk::lock(&controller.state).await.unwrap();
    let waiting = tokio::spawn({
        let controller = controller.clone();
        let id = id.clone();
        async move {
            let stop = controller.stop.child_token();
            controller
                .mount_after_release(&id, &plan, &stop, Duration::from_secs(5))
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    controller.stop.cancel();
    let error = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .err()
        .unwrap();
    assert_eq!(error.message, "Native disk preparation stopped.");
    let saved: Value = serde_json::from_slice(
        &tokio::fs::read(controller.file(&id, "stop-reason.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved["reason"], "controller_shutdown");
    drop(held);
}

#[tokio::test]
async fn native_disk_admission_deadline_keeps_the_existing_owner_fenced() {
    let (_fixture, controller) = fixture().await;
    let (run, id, plan) = busy_disk_fixture(&controller).await;
    let held = storage::environment::ownership(&controller.state, &run, "fixture owner")
        .await
        .unwrap();
    let error = controller
        .mount_after_release(
            &id,
            &plan,
            &CancellationToken::new(),
            Duration::from_millis(50),
        )
        .await
        .err()
        .unwrap();
    assert!(error.is_unavailable());
    assert!(error.message.contains("deadline"));
    assert!(
        storage::environment::ownership(&controller.state, &run, "probe")
            .await
            .is_err()
    );
    drop(held);
    assert!(
        storage::environment::ownership(&controller.state, &run, "probe")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn native_disk_admission_cancels_while_waiting_for_global_storage() {
    let (_fixture, controller) = fixture().await;
    let (_, id, plan) = busy_disk_fixture(&controller).await;
    let held = disk::lock(&controller.state).await.unwrap();
    let stop = CancellationToken::new();
    let waiting = tokio::spawn({
        let controller = controller.clone();
        let stop = stop.clone();
        async move {
            controller
                .mount_after_release(&id, &plan, &stop, Duration::from_secs(5))
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    stop.cancel();
    let error = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap()
        .err()
        .unwrap();
    assert_eq!(error.message, "Native disk preparation stopped.");
    drop(held);
    assert!(disk::lock(&controller.state).await.is_ok());
}

#[tokio::test]
async fn execution_deadline_bounds_global_disk_wait_and_records_the_cause() {
    let (_fixture, controller) = fixture().await;
    let (_, id, mut plan) = busy_disk_fixture(&controller).await;
    let held = disk::lock(&controller.state).await.unwrap();
    plan["expires"] = (crate::config::now() + 50).into();
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        controller.mount_after_release(
            &id,
            &plan,
            &CancellationToken::new(),
            Duration::from_secs(5),
        ),
    )
    .await
    .unwrap()
    .err()
    .unwrap();
    assert_eq!(error.status, 408);
    let saved: Value = serde_json::from_slice(
        &tokio::fs::read(controller.file(&id, "stop-reason.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved["reason"], "time_limit");
    drop(held);
}

#[tokio::test]
async fn native_disk_admission_does_not_retry_permanent_conflicts_or_expired_plans() {
    let (_fixture, controller) = fixture().await;
    let (_, id, mut plan) = busy_disk_fixture(&controller).await;
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        controller.mount_after_release(
            &id,
            &plan,
            &CancellationToken::new(),
            Duration::from_secs(5),
        ),
    )
    .await
    .unwrap()
    .err()
    .unwrap();
    assert!(error.message.starts_with("Native journal is missing"));
    assert_eq!(
        tokio::fs::metadata(controller.file(&id, "log"))
            .await
            .unwrap()
            .len(),
        0
    );
    plan["expires"] = 1.into();
    let error = controller
        .mount_after_release(
            &id,
            &plan,
            &CancellationToken::new(),
            Duration::from_secs(5),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.status, 408);
}

#[tokio::test]
async fn native_supervision_records_the_trigger_and_both_disk_budgets() {
    let (_fixture, controller) = fixture().await;
    let id = crate::config::id();
    atomic_write(&controller.file(&id, "log"), b"")
        .await
        .unwrap();
    let usage = json!({
        "workspaceBytes": 24,
        "temporaryBytes": 24,
        "conversationBytes": 48,
        "conversationLimitBytes": 32,
        "nodeBytes": 100,
        "nodeLimitBytes": 512,
    });
    let reason = supervision_stop_reason(true, false, 48, 32, 100, 512).unwrap();
    let details = json!({ "reason": reason, "at": crate::config::now(), "usage": usage });
    controller.write_stop_diagnostics(&id, &details).await;
    let saved: Value = serde_json::from_slice(
        &tokio::fs::read(controller.file(&id, "stop-reason.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved["reason"], "conversation_disk_limit");
    assert_eq!(saved["usage"], details["usage"]);
    assert!(saved["at"].as_i64().unwrap() > 0);
    assert!(
        tokio::fs::metadata(controller.file(&id, "log"))
            .await
            .unwrap()
            .len()
            > 0
    );
    assert_eq!(
        supervision_stop_reason(false, false, 0, 32, 0, 512),
        Some("lease_expired")
    );
    assert_eq!(
        supervision_stop_reason(true, true, 0, 32, 0, 512),
        Some("time_limit")
    );
    assert_eq!(
        supervision_stop_reason(true, false, 32, 32, 513, 512),
        Some("node_disk_limit")
    );
    assert_eq!(supervision_stop_reason(true, false, 32, 32, 512, 512), None);
}

#[tokio::test]
async fn native_stop_fences_after_the_log_lock_remains_held() {
    let (_fixture, controller) = fixture().await;
    let id = crate::config::id();
    atomic_write(&controller.file(&id, "log"), b"")
        .await
        .unwrap();
    let details = json!({ "reason": "time_limit", "at": crate::config::now(), "usage": null });
    super::super::record_stop_once(&controller.file(&id, "stop-reason.json"), &details)
        .await
        .unwrap();
    let held = controller.log_lock.lock().await;
    tokio::time::timeout(
        Duration::from_secs(5),
        controller.record_stop(&id, "time_limit", Value::Null),
    )
    .await
    .expect("Stop diagnostics must not prevent fencing when the log lock is held");
    assert_eq!(
        tokio::fs::metadata(controller.file(&id, "log"))
            .await
            .unwrap()
            .len(),
        0
    );
    let marker: Value = serde_json::from_slice(
        &tokio::fs::read(controller.file(&id, "stop-reason.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(marker, details);
    drop(held);
}

fn request(path: &str, body: &Value) -> Request {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("authorization", "Bearer synthetic-private-controller-token")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn native_project_import_uses_the_current_generation_and_preserves_retained_files() {
    let (fixture, controller) = fixture().await;
    let (run, attempt, project) = (
        crate::config::id(),
        crate::config::id(),
        crate::config::id(),
    );
    let manager = fixture.path().join("manager");
    let project_root = manager.join("runs").join(&run).join("workspace-fdeab5b6");
    let stage = controller.data.join("runs").join(&run);
    let source = stage.join("workspace-fdeab5b6").join(&project);
    for directory in [
        &source,
        &stage.join("home"),
        &controller.data.join("runner-plans"),
    ] {
        private_dir(directory).await.unwrap();
    }
    tokio::fs::write(source.join("file"), b"original seed")
        .await
        .unwrap();
    let plan = json!({
        "id": attempt,
        "runId": run,
        "backend": "native",
        "expires": crate::config::now() + 60_000,
        "nodeLeaseRequired": true,
        "guestDataRoot": manager,
        "cwd": project_root,
        "projectRoot": project_root,
        "sandbox": "workspace-write",
        "args": [],
        "prompt": "fixture",
        "resources": { "cpu": 1, "memoryMiB": 128, "diskMiB": 128 },
        "imports": [
            { "source": stage.join("workspace-fdeab5b6"), "target": project_root },
            { "source": stage.join("home"), "target": "/home/node" }
        ],
    });
    atomic_write(
        &controller
            .data
            .join("runner-plans")
            .join(format!("{attempt}.json")),
        plan.to_string().as_bytes(),
    )
    .await
    .unwrap();
    super::super::atomic_write(
        &controller.state.join("storage-policy.json"),
        &serde_json::to_vec(
            &storage::policy::for_node(&json!({"reserveMiB": 64, "reservePercent": 1})).unwrap(),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    controller.active.lock().await.insert(
        attempt.clone(),
        Attempt {
            live_access: Arc::default(),
            run: run.clone(),
            expires: None,
            stop: CancellationToken::new(),
            started: true,
            protection: None,
            suspension: Arc::new(super::super::suspension::Suspension::new(
                &controller.state,
                &attempt,
            )),
        },
    );
    let view = block::view(&controller.state, &run).await.unwrap();
    let retained = view.workspace.join(&project);
    private_dir(&retained).await.unwrap();
    tokio::fs::write(retained.join("file"), b"private edits")
        .await
        .unwrap();
    let path = format!("/runs/{attempt}/projects/{project}");
    let body = json!({
        "runId": run,
        "source": project_root.join(&project),
        "target": project_root.join(&project),
    });
    route(controller.clone(), request(&path, &body))
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(retained.join("file")).await.unwrap(),
        b"private edits"
    );
    let mut wrong = body;
    wrong["source"] = manager
        .join("runs")
        .join(&run)
        .join("workspace")
        .join(&project)
        .to_string_lossy()
        .into_owned()
        .into();
    assert_eq!(
        route(controller, request(&path, &wrong))
            .await
            .unwrap_err()
            .status,
        403
    );
}

#[tokio::test]
async fn scoped_close_contention_measurement() {
    use std::os::fd::AsRawFd;

    fn cpu_us() -> u64 {
        let mut time: libc::timespec = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) },
            0
        );
        time.tv_sec as u64 * 1_000_000 + time.tv_nsec as u64 / 1_000
    }

    let (_fixture, controller) = fixture().await;
    let bytes = vec![0x5a; 4096];
    let path = controller.state.join("measurement-file");
    tokio::fs::write(&path, &bytes).await.unwrap();
    let mut samples = Vec::new();
    for trial in 0..8 {
        for legacy in if trial % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            let held = disk::lock(&controller.state).await.unwrap();
            let unrelated = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                drop(held);
            });
            let started = std::time::Instant::now();
            let cpu = cpu_us();
            let mut sample = Value::Null;
            let close_io = async {
                let admission_us = started.elapsed().as_micros() as u64;
                let writing = std::time::Instant::now();
                let mut file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .await
                    .unwrap();
                file.write_all(&bytes).await.unwrap();
                file.sync_all().await.unwrap();
                let write_us = writing.elapsed().as_micros() as u64;
                let advice = unsafe {
                    libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED)
                };
                let reading = std::time::Instant::now();
                let cold_hinted = tokio::fs::read(&path).await.unwrap();
                let cold_hint_us = reading.elapsed().as_micros() as u64;
                let reading = std::time::Instant::now();
                let warm = tokio::fs::read(&path).await.unwrap();
                let warm_us = reading.elapsed().as_micros() as u64;
                assert_eq!(cold_hinted, bytes);
                assert_eq!(warm, bytes);
                sample = json!({
                    "legacyGlobalDrain": legacy,
                    "trial": trial,
                    "admissionUs": admission_us,
                    "writeAndFsyncUs": write_us,
                    "coldHintReadUs": cold_hint_us,
                    "warmReadUs": warm_us,
                    "coldHintStatus": advice,
                    "fileBytes": tokio::fs::metadata(&path).await.unwrap().len(),
                    "processCpuUs": cpu_us().saturating_sub(cpu),
                    "hash": blake3::hash(&warm).to_hex().to_string(),
                });
                Ok(block::Completion::Flushed)
            };
            if legacy {
                // Exact admission sequence of ADR0077, under the same real
                // pool lock as the candidate. No network latency model.
                drop(disk::lock(&controller.state).await.unwrap());
                close_io.await.unwrap();
            } else {
                StorageClose::default().finish(close_io).await.unwrap();
            }
            unrelated.await.unwrap();
            samples.push(sample);
        }
    }
    eprintln!(
        "NATIVE_CLOSE_GATE_MEASUREMENT {}",
        json!({
            "samples": samples,
            "unrelatedPoolHoldMs": 50,
            "fixture": "4KiB raw host file, same bytes; no journal or object encoding changed",
            "coldRead": "per-file DONTNEED hint after fsync; residency not asserted",
            "host": std::env::var("ImageVersion").unwrap_or_else(|_| "unspecified".into()),
            "cpus": std::thread::available_parallelism().unwrap().get(),
            "kernel": std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap().trim(),
            "cpuMeasurement": "process CPU; run this test alone (nextest or --exact)",
            "scope": "controlled production-gate contention fixture, not production/ublk latency",
        })
    );
}

#[tokio::test]
async fn first_close_drain_does_not_wait_for_an_unrelated_pool_operation() {
    let (_fixture, controller) = fixture().await;
    // This is the production pool gate used by seed transfers and policy
    // updates, not a simulated delay in the close operation itself.
    let unrelated = disk::lock(&controller.state).await.unwrap();
    let mut close = StorageClose::default();
    let result = tokio::time::timeout(
        Duration::from_millis(250),
        close.finish(async { Ok(block::Completion::Flushed) }),
    )
    .await;
    drop(unrelated);
    assert!(
        result.is_ok(),
        "unrelated pool ownership blocked the first close drain"
    );
    assert_eq!(result.unwrap().unwrap(), block::Completion::Flushed);
}

#[tokio::test]
async fn close_drains_admitted_mutations_then_allows_policy_changes_and_refuses_late_mutations() {
    let (_fixture, controller) = fixture().await;
    let (run, id, _) = busy_disk_fixture(&controller).await;
    let stop = CancellationToken::new();
    stop.cancel();
    controller.active.lock().await.insert(
        id.clone(),
        Attempt {
            live_access: Arc::default(),
            run: run.clone(),
            expires: None,
            stop,
            started: true,
            protection: None,
            suspension: Arc::new(super::super::suspension::Suspension::new(
                &controller.state,
                &id,
            )),
        },
    );
    let live_access = controller
        .active
        .lock()
        .await
        .get(&id)
        .unwrap()
        .live_access
        .clone();
    let admitted = live_access.lock().await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let closing = tokio::spawn({
        let live_access = live_access.clone();
        let entered = entered.clone();
        let release = release.clone();
        async move {
            StorageClose::new(live_access)
                .finish(async {
                    entered.notify_one();
                    release.notified().await;
                    Ok(block::Completion::Flushed)
                })
                .await
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), entered.notified())
            .await
            .is_err()
    );
    assert!(!closing.is_finished());
    drop(admitted);
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let result = async {
        let policy = storage::policy::Configuration {
            generation: 1,
            policy: storage::policy::Policy {
                reserve_mi_b: 64,
                reserve_percent: 1,
                memory_cache_mi_b: 128,
            },
        };
        route(
            controller.clone(),
            request("/storage-policy", &json!(policy)),
        )
        .await?;
        let mut budget = controller.budget.lock().await.clone();
        budget["slots"] = 2.into();
        route(controller.clone(), request("/node-budget", &budget)).await?;
        for path in [
            format!("/runs/{id}/artifact"),
            format!("/runs/{id}/projects/{}", crate::config::id()),
        ] {
            let error = route(controller.clone(), request(&path, &json!({"runId":run})))
                .await
                .unwrap_err();
            assert_eq!(error.status, 403);
        }
        assert!(controller.active.lock().await.contains_key(&id));
        assert!(controller.leases.lock().await.contains_key(&id));
        assert!(!controller.file(&id, "exit").exists());
        Ok::<_, Error>(())
    };
    let result = tokio::time::timeout(Duration::from_secs(2), result).await;
    release.notify_one();
    assert_eq!(closing.await.unwrap().unwrap(), block::Completion::Flushed);
    result.unwrap().unwrap();
}

#[tokio::test]
async fn native_wait_streams_heartbeats_until_physical_ownership_is_released() {
    use futures_util::StreamExt;

    let (_fixture, controller) = fixture().await;
    let attempt = crate::config::id();
    controller.active.lock().await.insert(
        attempt.clone(),
        Attempt {
            live_access: Arc::default(),
            run: crate::config::id(),
            expires: None,
            stop: CancellationToken::new(),
            started: true,
            protection: None,
            suspension: Arc::new(super::super::suspension::Suspension::new(
                &controller.state,
                &attempt,
            )),
        },
    );
    atomic_write(&controller.file(&attempt, "exit"), b"17")
        .await
        .unwrap();

    let response = tokio::time::timeout(
        Duration::from_millis(250),
        handler(
            State(controller.clone()),
            request(&format!("/runs/{attempt}/wait"), &json!({})),
        ),
    )
    .await
    .expect("Native wait must send response headers before the attempt finishes")
    .unwrap();
    assert_eq!(response.headers()["content-type"], "application/json");
    let mut body = response.into_body().into_data_stream();
    let mut received = Vec::new();
    for _ in 0..2 {
        let bytes = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("An unfinished native attempt must keep its wait connection alive")
            .unwrap()
            .unwrap();
        assert!(bytes.iter().all(u8::is_ascii_whitespace));
        received.extend_from_slice(&bytes);
    }

    controller.active.lock().await.remove(&attempt);
    while let Some(bytes) = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .unwrap()
    {
        received.extend_from_slice(&bytes.unwrap());
    }
    let result: Value = serde_json::from_slice(&received).unwrap();
    assert_eq!(result, json!({ "StatusCode": 17 }));
}

#[tokio::test]
async fn failed_native_flush_survives_read_only_final_write_after_cleanup() {
    use futures_util::StreamExt;
    use std::os::unix::fs::PermissionsExt;
    let (_fixture, controller) = fixture().await;
    let attempt = crate::config::id();
    let (code, failed) = completion_status(1, block::Completion::FailedFlush);
    let record = exit_record(code, failed, None);
    controller.active.lock().await.insert(
        attempt.clone(),
        Attempt {
            live_access: Arc::default(),
            run: crate::config::id(),
            expires: None,
            stop: CancellationToken::new(),
            started: true,
            protection: None,
            suspension: Arc::new(super::super::suspension::Suspension::new(
                &controller.state,
                &attempt,
            )),
        },
    );
    atomic_write(&controller.file(&attempt, "exit"), record.as_bytes())
        .await
        .unwrap();
    let marker = controller.file(&attempt, "active");
    atomic_write(&marker, b"run").await.unwrap();
    preserve_interrupted_exit(&marker).await.unwrap();
    let response = handler(
        State(controller.clone()),
        request(&format!("/runs/{attempt}/wait"), &json!({})),
    )
    .await
    .unwrap();
    let mut body = response.into_body().into_data_stream();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), body.next())
            .await
            .is_err()
    );
    let other_path = controller.file(&crate::config::id(), "exit");
    std::os::unix::fs::symlink(controller.file(&attempt, "exit"), &other_path).unwrap();
    std::fs::set_permissions(&controller.state, std::fs::Permissions::from_mode(0o500)).unwrap();
    let written = crate::runner::write_final_result(
        &controller.file(&attempt, "exit"),
        record.as_bytes(),
        true,
    )
    .await;
    let other = crate::runner::write_final_result(&other_path, record.as_bytes(), true).await;
    std::fs::write(controller.file(&attempt, "exit"), b"{broken").unwrap();
    let corrupt = crate::runner::write_final_result(
        &controller.file(&attempt, "exit"),
        record.as_bytes(),
        true,
    )
    .await;
    std::fs::write(controller.file(&attempt, "exit"), record.as_bytes()).unwrap();
    std::fs::set_permissions(&controller.state, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        written.is_ok(),
        "a committed failure must not require another write: {written:?}"
    );
    assert!(other.is_err() && corrupt.is_err());
    let hardlink = controller.file(&crate::config::id(), "exit");
    std::fs::hard_link(controller.file(&attempt, "exit"), &hardlink).unwrap();
    std::fs::set_permissions(&controller.state, std::fs::Permissions::from_mode(0o500)).unwrap();
    let other = crate::runner::write_final_result(&hardlink, record.as_bytes(), true).await;
    std::fs::set_permissions(&controller.state, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_file(hardlink).unwrap();
    assert!(other.is_err());
    controller.active.lock().await.remove(&attempt);
    let mut received = Vec::new();
    while let Some(bytes) = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .unwrap()
    {
        received.extend_from_slice(&bytes.unwrap());
    }
    assert_eq!(
        serde_json::from_slice::<Value>(&received).unwrap(),
        json!({ "StatusCode": 70, "FinalizationFailed": true })
    );
    for _ in 0..2 {
        let response = handler(
            State(controller.clone()),
            request(&format!("/runs/{attempt}/wait"), &json!({})),
        )
        .await
        .unwrap();
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            result,
            json!({ "StatusCode": 70, "FinalizationFailed": true })
        );
    }
    let (code, failed) = completion_status(1, block::Completion::Flushed);
    assert_eq!(
        serde_json::from_str::<Value>(&exit_record(code, failed, None)).unwrap(),
        json!({ "StatusCode": 1, "PhysicalFinalized": true })
    );
}

#[tokio::test]
async fn native_stop_cancels_workload_when_stop_marker_cannot_be_written() {
    let (_root, controller) = fixture().await;
    let id = crate::config::id();
    let stop = CancellationToken::new();
    controller.active.lock().await.insert(
        id.clone(),
        Attempt {
            live_access: Arc::default(),
            run: crate::config::id(),
            expires: None,
            stop: stop.clone(),
            started: true,
            protection: None,
            suspension: Arc::new(super::super::suspension::Suspension::new(
                &controller.state,
                &id,
            )),
        },
    );
    tokio::fs::create_dir(controller.file(&id, "stopped"))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), controller.stop_attempt(&id))
            .await
            .unwrap()
            .is_err()
    );
    assert!(stop.is_cancelled());
    assert!(controller.active.lock().await.contains_key(&id));
}

#[tokio::test]
async fn native_logs_keep_quiet_attempts_alive_without_splitting_json_records() {
    use futures_util::StreamExt;

    let (_fixture, controller) = fixture().await;
    let attempt = crate::config::id();
    controller.active.lock().await.insert(
        attempt.clone(),
        Attempt {
            live_access: Arc::default(),
            run: crate::config::id(),
            expires: None,
            stop: CancellationToken::new(),
            started: true,
            protection: None,
            suspension: Arc::new(super::super::suspension::Suspension::new(
                &controller.state,
                &attempt,
            )),
        },
    );
    let log = controller.file(&attempt, "log");
    atomic_write(&log, b"").await.unwrap();
    let request = Request::builder()
        .method("GET")
        .uri(format!("/runs/{attempt}/logs"))
        .header("authorization", "Bearer synthetic-private-controller-token")
        .body(Body::empty())
        .unwrap();
    let response = handler(State(controller.clone()), request).await.unwrap();
    let mut body = response.into_body().into_data_stream();
    let heartbeat = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .expect("A quiet native attempt must keep its logs connection alive")
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&heartbeat).unwrap(),
        json!({ "type": "heartbeat" }),
    );

    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .await
        .unwrap();
    file.write_all(b"{\"type\":\"agent_").await.unwrap();
    file.flush().await.unwrap();
    let prefix = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(&prefix[..], b"{\"type\":\"agent_");
    assert!(
        tokio::time::timeout(Duration::from_millis(1200), body.next())
            .await
            .is_err(),
        "Heartbeats must not be inserted inside a partially written JSON record",
    );
    // Queue the reader's EOF lifecycle check behind the final append and retirement.
    let mut active = controller.active.lock().await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    file.write_all(b"message\",\"text\":\"ok\"}\n")
        .await
        .unwrap();
    file.flush().await.unwrap();
    assert_eq!(
        tokio::fs::read(&log).await.unwrap(),
        b"{\"type\":\"agent_message\",\"text\":\"ok\"}\n"
    );
    active.remove(&attempt);
    drop(active);
    let mut record = prefix.to_vec();
    while let Some(bytes) = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .unwrap()
    {
        record.extend_from_slice(&bytes.unwrap());
    }
    assert_eq!(
        serde_json::from_slice::<Value>(&record).unwrap(),
        json!({ "type": "agent_message", "text": "ok" }),
    );
}

async fn sparse(path: &Path, size: u64) {
    private_dir(path.parent().unwrap()).await.unwrap();
    tokio::fs::File::create(path)
        .await
        .unwrap()
        .set_len(size)
        .await
        .unwrap();
}

async fn artifact_fixture() -> (tempfile::TempDir, Controller, String, String, PathBuf) {
    let (fixture, controller) = fixture().await;
    let run = crate::config::id();
    let attempt = crate::config::id();
    let stage = controller.data.join("runs").join(&run);
    let environment = workspace::directory(&controller.state, &run).unwrap();
    for directory in [
        stage.join("workspace"),
        stage.join("home"),
        environment.join("tmp"),
        environment.join("data/tmp"),
        controller.data.join("runner-plans"),
    ] {
        private_dir(&directory).await.unwrap();
    }
    let plan = json!({
        "id": attempt, "runId": run, "backend": "native", "nodeLeaseRequired": true, "expires": null,
        "cwd": stage.join("workspace"), "projectRoot": stage.join("workspace"),
        "sandbox": "workspace-write", "args": [], "prompt": "fixture",
        "resources": { "cpu": 1, "memoryMiB": 128, "diskMiB": 128 },
        "imports": [
            { "source": stage.join("workspace"), "target": stage.join("workspace") },
            { "source": stage.join("home"), "target": "/home/node" }
        ]
    });
    atomic_write(
        &controller
            .data
            .join("runner-plans")
            .join(format!("{attempt}.json")),
        &serde_json::to_vec(&plan).unwrap(),
    )
    .await
    .unwrap();
    atomic_write(
        &controller.state.join("storage-policy.json"),
        &serde_json::to_vec(
            &storage::policy::for_node(&json!({"reserveMiB":64,"reservePercent":1})).unwrap(),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    controller.active.lock().await.insert(
        attempt.clone(),
        Attempt {
            live_access: Arc::default(),
            run: run.clone(),
            expires: None,
            stop: CancellationToken::new(),
            started: true,
            protection: None,
            suspension: Arc::new(super::super::suspension::Suspension::new(
                &controller.state,
                &attempt,
            )),
        },
    );
    (fixture, controller, run, attempt, environment)
}

#[tokio::test]
async fn artifact_refuses_controller_filesystem_reserve_without_leaving_exports() {
    let (_fixture, controller, run, attempt, environment) = artifact_fixture().await;
    let source = environment.join("tmp/result");
    tokio::fs::write(&source, b"retained artifact")
        .await
        .unwrap();
    let (total, _) = storage::policy::physical_space(&controller.state).unwrap();
    let mut policy =
        storage::policy::for_node(&json!({"reserveMiB":64,"reservePercent":1})).unwrap();
    policy.reserve_mi_b = total / disk::MIB + 1;
    atomic_write(
        &controller.state.join("storage-policy.json"),
        &serde_json::to_vec(&policy).unwrap(),
    )
    .await
    .unwrap();
    let error = controller
        .artifact_snapshot(&attempt, json!({"runId":run,"path":"/tmp/result"}))
        .await
        .unwrap_err();
    assert_eq!(error.status, 507);
    assert!(!controller.state.join("exports").exists());
    assert_eq!(tokio::fs::read(source).await.unwrap(), b"retained artifact");
}

#[tokio::test]
async fn queued_live_routes_do_not_delay_close_or_admit_after_cancellation() {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };

    for artifact in [true, false] {
        let (_fixture, controller, run, id, environment) = artifact_fixture().await;
        let project = crate::config::id();
        let target = environment.join("data/workspaces").join(&project);
        private_dir(&target).await.unwrap();
        tokio::fs::write(target.join("preserved"), b"existing project bytes")
            .await
            .unwrap();
        let plan = controller.plan(&id).await.unwrap();
        let (path, body) = if artifact {
            (
                format!("/runs/{id}/artifact"),
                json!({ "runId": run, "path": "/tmp/result" }),
            )
        } else {
            (
                format!("/runs/{id}/projects/{project}"),
                json!({
                    "runId": run,
                    "source": guest_data(&plan, &controller.data).join("runs").join(&run).join("workspace").join(&project),
                    "target": Path::new(text(&plan, "projectRoot")).join(&project),
                }),
            )
        };
        let unrelated = disk::lock(&controller.state).await.unwrap();
        let pending = route(controller.clone(), request(&path, &body));
        tokio::pin!(pending);
        // Poll the actual route into the global gate before closing. It
        // must not acquire this attempt's live-access guard while queued.
        let queued = poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx))).await;
        assert!(queued.is_pending());
        let live_access = {
            let active = controller.active.lock().await;
            let attempt = active.get(&id).unwrap();
            attempt.stop.cancel();
            attempt.live_access.clone()
        };
        let mut close = StorageClose::new(live_access);
        let closed = tokio::time::timeout(
            Duration::from_millis(250),
            close.finish(async { Ok(block::Completion::Flushed) }),
        )
        .await;
        let still_held = crate::file_lock::exclusive(
            &controller.state.join("storage.lock"),
            "unrelated pool operation is still active",
        )
        .is_err();
        drop(unrelated);
        let rejected = tokio::time::timeout(Duration::from_secs(2), pending).await;
        assert_eq!(closed.unwrap().unwrap(), block::Completion::Flushed);
        assert!(still_held);
        assert_eq!(rejected.unwrap().unwrap_err().status, 403);
        assert!(!controller.state.join("exports").exists());
        assert_eq!(
            tokio::fs::read(target.join("preserved")).await.unwrap(),
            b"existing project bytes"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_artifact_request_drains_its_admitted_io_before_close() {
    assert_interrupted_artifact_drain(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timed_out_artifact_request_drains_its_admitted_io_before_close() {
    assert_interrupted_artifact_drain(true).await;
}

async fn assert_interrupted_artifact_drain(request_timeout: bool) {
    let (_fixture, controller, run, id, environment) = artifact_fixture().await;
    let path = environment.join("tmp/result");
    tokio::fs::write(&path, b"preserved artifact bytes")
        .await
        .unwrap();
    let (opening, release) = crate::artifacts::file::tests::delay_open(&path);
    let expire_request = Arc::new(Notify::new());
    let export = tokio::spawn({
        let controller = controller.clone();
        let id = id.clone();
        let expire_request = expire_request.clone();
        async move {
            let exporting = handler(
                State(controller),
                request(
                    &format!("/runs/{id}/artifact"),
                    &json!({ "runId": run, "path": "/tmp/result" }),
                ),
            );
            tokio::pin!(exporting);
            tokio::select! {
                result = &mut exporting => result,
                () = expire_request.notified() => {
                    // The real open is already blocked before this short
                    // HTTP deadline starts, so no scheduler timing is assumed.
                    tokio::time::timeout(Duration::from_millis(1), &mut exporting)
                        .await
                        .unwrap_or_else(|_| Err(Error::timeout("Synthetic request timeout.")))
                }
            }
        }
    });
    let opened = tokio::task::spawn_blocking(move || opening.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap();
    if opened.is_err() {
        let _ = release.send(());
    }
    opened.expect("artifact must reach its real blocking filesystem open");
    if request_timeout {
        expire_request.notify_one();
        assert_eq!(
            export.await.unwrap().unwrap_err().message,
            "Synthetic request timeout."
        );
    } else {
        export.abort();
        assert!(export.await.unwrap_err().is_cancelled());
    }
    controller
        .active
        .lock()
        .await
        .get(&id)
        .unwrap()
        .stop
        .cancel();
    let pool_available = crate::file_lock::exclusive(
        &controller.state.join("storage.lock"),
        "admitted filesystem work must not block other disks",
    )
    .is_ok();
    let entered = Arc::new(Notify::new());
    let live_access = controller
        .active
        .lock()
        .await
        .get(&id)
        .unwrap()
        .live_access
        .clone();
    let closing = tokio::spawn({
        let entered = entered.clone();
        async move {
            StorageClose::new(live_access)
                .finish(async {
                    entered.notify_one();
                    Ok(block::Completion::Flushed)
                })
                .await
        }
    });
    let drained_too_early = tokio::time::timeout(Duration::from_millis(100), entered.notified())
        .await
        .is_ok();
    // Always release the actual blocking operation before an assertion.
    let _ = release.send(());
    let closed = tokio::time::timeout(Duration::from_secs(5), closing).await;
    assert!(
        pool_available,
        "request cancellation blocked the global pool despite isolated IO ownership"
    );
    assert!(
        !drained_too_early,
        "close raced an admitted filesystem open"
    );
    assert_eq!(
        closed.unwrap().unwrap().unwrap(),
        block::Completion::Flushed
    );
    assert_eq!(
        tokio::fs::read(path).await.unwrap(),
        b"preserved artifact bytes"
    );
    assert_eq!(
        std::fs::read_dir(controller.state.join("exports"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn tmp_artifact_uses_the_ephemeral_path_and_rejects_redirects() {
    let (_fixture, controller, run, attempt, environment) = artifact_fixture().await;
    tokio::fs::write(environment.join("tmp/result"), b"actual temporary artifact")
        .await
        .unwrap();
    tokio::fs::write(environment.join("data/tmp/result"), b"wrong stale bytes")
        .await
        .unwrap();
    let response = route(
        controller.clone(),
        request(
            &format!("/runs/{attempt}/artifact"),
            &json!({ "runId": run, "path": "/tmp/result" }),
        ),
    )
    .await
    .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), b"actual temporary artifact");
    std::os::unix::fs::symlink(
        environment.join("data/tmp/result"),
        environment.join("tmp/redirect"),
    )
    .unwrap();
    assert!(
        route(
            controller,
            request(
                &format!("/runs/{attempt}/artifact"),
                &json!({ "runId": run, "path": "/tmp/redirect" })
            )
        )
        .await
        .is_err()
    );
}

#[test]
fn delayed_artifact_keeps_health_leases_and_cancellation_responsive() {
    // Isolate the native tool bundle environment from parallel unit tests.
    let tools = tempfile::tempdir().unwrap();
    let bundle = tools.path().join("tools");
    std::fs::create_dir(&bundle).unwrap();
    std::fs::write(bundle.join("manifest.json"), b"{}").unwrap();
    std::fs::write(
        tools.path().join("release.json"),
        serde_json::to_vec(&json!({
            "runtimeId": crate::nodes::component_runtime(),
            "os": "linux", "arch": "x86_64"
        }))
        .unwrap(),
    )
    .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native::service::tests::delayed_artifact_control_child",
            "--ignored",
        ])
        .env("CAIRN_NATIVE_TOOLS", bundle)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "isolated native tool bundle environment"]
fn delayed_artifact_control_child() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (_fixture, controller, run, attempt, environment) = runtime.block_on(artifact_fixture());
    let path = environment.join("tmp/result");
    std::fs::write(&path, b"completed artifact").unwrap();
    let (opening, release) = crate::artifacts::file::tests::delay_open(&path);
    let exporting = controller.clone();
    let id = attempt.clone();
    let export = runtime.spawn(async move {
        handler(
            State(exporting),
            request(
                &format!("/runs/{id}/artifact"),
                &json!({ "runId": run, "path": "/tmp/result" }),
            ),
        )
        .await
    });
    let opened = opening.recv_timeout(Duration::from_secs(5));
    let physical_owner = crate::file_lock::exclusive(
        &controller.state.join("storage.lock"),
        "An isolated export must release the global pool",
    );
    let controls = controller.clone();
    let (progress, observed) = std::sync::mpsc::channel();
    let control = runtime.spawn(async move {
        let health = handler(
            State(controls.clone()),
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(health.status(), 200);
        let health: Value = serde_json::from_slice(
            &axum::body::to_bytes(health.into_body(), 100_000)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(health["activeRuns"], 1);
        let lease = handler(
            State(controls.clone()),
            request(
                &format!("/runs/{attempt}/lease"),
                &json!({ "remainingMs": 60_000 }),
            ),
        )
        .await
        .unwrap();
        assert_eq!(lease.status(), 200);
        assert!(controls.leased(&attempt).await);
        // Exercise the same active-map cancellation and disk-abort path
        // without invoking privileged host systemd operations in a unit test.
        controls
            .active
            .lock()
            .await
            .get(&attempt)
            .unwrap()
            .stop
            .cancel();
        controls.abort_disk(&attempt).await.unwrap();
        let _ = progress.send(());
    });
    let responsive = observed.recv_timeout(Duration::from_secs(2));
    // Release the filesystem even when the old controller holds active.
    let _ = release.send(());
    let exported = runtime.block_on(export).unwrap();
    runtime.block_on(control).unwrap();
    opened.expect("The native artifact must reach its filesystem open");
    assert!(physical_owner.is_ok());
    drop(physical_owner);
    responsive.expect("Health, lease renewal and cancellation must finish during the delayed open");
    assert_eq!(exported.unwrap_err().status, 403);
    assert!(
        crate::file_lock::exclusive(&controller.state.join("storage.lock"), "Export finished",)
            .is_ok()
    );
    assert_eq!(
        std::fs::read_dir(controller.state.join("exports"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn native_disk_pool_charges_retained_conversations_archives_and_staging() {
    let (fixture, controller) = fixture().await;
    controller.budget.lock().await["limits"]["diskMiB"] = 32768.into();
    let first = crate::config::id();
    let second = crate::config::id();
    let gib = 1024 * disk::MIB;
    let retained = workspace::directory(&controller.state, &first)
        .unwrap()
        .join("data/workspaces/model");
    sparse(&retained, 20 * gib).await;
    let archive = controller.state.join("snapshots").join(crate::config::id());
    sparse(&archive.join("disk"), gib).await;
    atomic_write(&archive.join("run"), first.as_bytes())
        .await
        .unwrap();
    sparse(
        &controller.data.join("runs").join(&second).join("staging"),
        2 * gib,
    )
    .await;
    let usage = controller.usage().await.unwrap();
    assert!(usage["diskBytes"].as_u64().unwrap() > 23 * gib);
    assert_eq!(usage["diskMiB"], 23 * 1024 + 1);

    // A second 20 GiB conversation cannot consume the remaining shared pool.
    // The transfer checks the size frame before reading/writing its contents.
    let incoming = fixture.path().join("remote-source");
    sparse(&incoming.join("model"), 20 * gib).await;
    let receiving = tempfile::tempdir_in(controller.data.join("runs")).unwrap();
    let _owner = disk::lock(&controller.state).await.unwrap();
    let error = workspace::copy(
        &incoming,
        receiving.path(),
        controller.disk_growth().await.unwrap(),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("disk budget"), "{}", error.message);
    assert!(!receiving.path().join("model").exists());
    assert_eq!(tokio::fs::metadata(retained).await.unwrap().len(), 20 * gib);
    assert_eq!(controller.usage().await.unwrap(), usage);
}

#[tokio::test]
async fn private_socket_routes_ignore_a_spoofed_tcp_controller() {
    let (_root, controller) = fixture().await;
    let socket = controller.state.join("controller.sock");
    let listener = bind_controller(&socket).await.unwrap();
    let spoof = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", spoof.local_addr().unwrap());
    let router = Router::new().fallback(any(handler)).with_state(controller);
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let response = super::super::controller_client(&socket)
        .unwrap()
        .post(format!("{origin}/node-budget"))
        .bearer_auth("synthetic-private-controller-token")
        .json(&json!({
            "slots": 1,
            "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 }
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(
        std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), spoof.accept())
            .await
            .is_err()
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn controller_socket_never_replaces_an_unrelated_file() {
    let (_root, controller) = fixture().await;
    let socket = controller.state.join("controller.sock");
    atomic_write(&socket, b"preserved owner data")
        .await
        .unwrap();
    assert!(bind_controller(&socket).await.is_err());
    assert_eq!(
        tokio::fs::read(socket).await.unwrap(),
        b"preserved owner data"
    );
}

#[tokio::test]
async fn frozen_skill_copy_preserves_approved_executable_bits() {
    let root = tempfile::tempdir().unwrap();
    let (source, target) = (root.path().join("seed"), root.path().join("home"));
    private_dir(&source.join(".agents/skills/game/scripts"))
        .await
        .unwrap();
    private_dir(&target).await.unwrap();
    let relative = ".agents/skills/game/scripts/build";
    atomic_write(&source.join(relative), b"#!/bin/sh\nexit 0\n")
        .await
        .unwrap();
    tokio::fs::set_permissions(
        source.join(relative),
        std::fs::Permissions::from_mode(0o700),
    )
    .await
    .unwrap();
    atomic_write(
        &source.join(".agents/skills/game/SKILL.md"),
        b"Use the build script",
    )
    .await
    .unwrap();
    install_home(&source, &target).await.unwrap();
    assert_eq!(
        tokio::fs::metadata(target.join(relative))
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        tokio::fs::metadata(target.join(".agents/skills/game/SKILL.md"))
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        tokio::process::Command::new(target.join(relative))
            .status()
            .await
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn private_controller_routes_require_the_node_credential() {
    let (_root, controller) = fixture().await;
    for (method, path) in [
        ("POST", "/node-budget"),
        ("GET", "/exclusive"),
        ("POST", "/exclusive/acquire"),
        ("POST", "/exclusive/release"),
    ] {
        let response = route(
            controller.clone(),
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(response.status, 401);
    }
}

#[tokio::test]
async fn native_mode_final_write_failure_reports_unconfirmed_privileges() {
    let (_root, controller) = fixture().await;
    let target = json!({ "mode": "administrator", "generation": crate::config::id() });
    {
        let mut pending = controller.mode.lock().await;
        pending["status"] = "pending".into();
        pending["target"] = target.clone();
    }
    private_dir(&controller.state.join(mode::FILE))
        .await
        .unwrap();
    let applied =
        json!({ "mode": target["mode"], "generation": target["generation"], "status": "applied" });
    // Exercise final acknowledgement persistence without invoking host sudo.
    assert!(controller.persist_mode(&applied).await.is_err());
    let observed = controller.mode.lock().await.clone();
    assert_eq!(observed["status"], "failed");
    assert_eq!(
        observed["mode"], "protected",
        "Never publish an undurable applied mode"
    );
    assert_eq!(observed["target"], target);
    assert!(!text(&observed, "error").is_empty());
    assert!(mode::check_plan(&observed, &json!({})).is_err());
    assert!(mode::check_plan(&observed, &json!({ "nativeMode": target })).is_err());
}

#[tokio::test]
async fn native_mode_persistence_failure_blocks_previously_leased_plans() {
    let (_root, controller) = fixture().await;
    let run = crate::config::id();
    let attempt = crate::config::id();
    let stage = controller.data.join("runs").join(&run);
    for directory in [
        stage.join("workspace"),
        stage.join("home"),
        controller.data.join("runner-plans"),
    ] {
        private_dir(&directory).await.unwrap();
    }
    let plan = json!({
        "id": attempt, "runId": run, "backend": "native", "nodeLeaseRequired": true, "expires": null,
        "cwd": stage.join("workspace"), "projectRoot": stage.join("workspace"),
        "sandbox": "workspace-write", "args": [], "prompt": "fixture",
        "resources": { "cpu": 1, "memoryMiB": 128, "diskMiB": 128 },
        "imports": [
            { "source": stage.join("workspace"), "target": stage.join("workspace") },
            { "source": stage.join("home"), "target": "/home/node" }
        ]
    });
    atomic_write(
        &controller
            .data
            .join("runner-plans")
            .join(format!("{attempt}.json")),
        &serde_json::to_vec(&plan).unwrap(),
    )
    .await
    .unwrap();
    controller
        .leases
        .lock()
        .await
        .insert(attempt.clone(), crate::nodes::boot_ms() + 60000);
    controller.plan(&attempt).await.unwrap();
    mode::check_plan(&*controller.mode.lock().await, &plan).unwrap();
    // A directory at the state-file destination guarantees an atomic rename
    // failure on this disposable fixture, before any host sudo operation.
    private_dir(&controller.state.join(mode::FILE))
        .await
        .unwrap();
    let request = json!({ "mode": "administrator", "generation": crate::config::id() });
    assert!(controller.change_mode(request.clone()).await.is_err());
    let state = controller.mode.lock().await.clone();
    assert_eq!(state["status"], "failed");
    assert!(!text(&state, "error").is_empty());
    assert_eq!(state["target"], request);
    let error = controller.start(&attempt).await.unwrap_err();
    assert_eq!(error.status, 409);
    assert!(error.message.contains("privilege generation"), "{error:?}");
    assert!(controller.active.lock().await.is_empty());
}

#[tokio::test]
async fn expired_or_stopped_attempts_cannot_start_or_regain_a_lease() {
    let (_root, controller) = fixture().await;
    let attempt = crate::config::id();
    assert_eq!(controller.start(&attempt).await.unwrap_err().status, 409);
    atomic_write(&controller.file(&attempt, "stopped"), b"1")
        .await
        .unwrap();
    let error = route(
        controller.clone(),
        request(
            &format!("/runs/{attempt}/lease"),
            &json!({ "remainingMs": 60000 }),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, 409);
    assert!(!controller.leased(&attempt).await);
}

#[tokio::test]
async fn native_retirement_retries_attempt_cleanup_before_persisting_its_receipt() {
    for bad_log in [true, false] {
        let (_fixture, controller) = fixture().await;
        let (run, attempt, retirement) = (
            crate::config::id(),
            crate::config::id(),
            crate::config::id(),
        );
        tokio::fs::write(controller.file(&attempt, "run"), run.as_bytes())
            .await
            .unwrap();
        for extension in ["active", "vm.json", "lease", "terminal"] {
            tokio::fs::write(
                controller.file(&attempt, extension),
                b"private attempt metadata",
            )
            .await
            .unwrap();
        }
        let roots = controller.state.join("roots");
        private_dir(&roots).await.unwrap();
        let directory = roots.join(&attempt);
        if bad_log {
            private_dir(&directory).await.unwrap();
            tokio::fs::write(directory.join("provider-home"), b"old private home")
                .await
                .unwrap();
            private_dir(&controller.file(&attempt, "log"))
                .await
                .unwrap();
        } else {
            tokio::fs::write(&directory, b"transient wrong file type")
                .await
                .unwrap();
        }
        let path = format!("/disks/{run}/delete");
        let body = json!({ "retirementId":retirement });
        assert!(
            route(controller.clone(), request(&path, &body))
                .await
                .is_err()
        );
        let receipt = controller
            .state
            .join("disk-retirements")
            .join(&run)
            .join(&retirement);
        assert!(
            !receipt.exists(),
            "failed cleanup was permanently acknowledged"
        );
        assert_eq!(
            tokio::fs::read_to_string(controller.file(&attempt, "run"))
                .await
                .unwrap(),
            run,
            "failed cleanup lost its retry ownership"
        );
        if bad_log {
            tokio::fs::remove_dir(controller.file(&attempt, "log"))
                .await
                .unwrap();
        } else {
            tokio::fs::remove_file(&directory).await.unwrap();
        }
        assert_eq!(
            route(controller.clone(), request(&path, &body))
                .await
                .unwrap()
                .status(),
            200
        );
        assert!(receipt.exists());
        assert!(!directory.exists());
        for extension in ["active", "vm.json", "run", "log", "lease", "terminal"] {
            assert!(
                !controller.file(&attempt, extension).exists(),
                "retired attempt retained {extension}"
            );
        }
        assert!(controller.file(&attempt, "stopped").exists());
    }
}

#[tokio::test]
async fn identified_native_retirement_preserves_a_shared_manager_namespace() {
    let (_fixture, controller) = fixture().await;
    let run = crate::config::id();
    let old = workspace::directory(&controller.state, &run).unwrap();
    private_dir(&old.join("data")).await.unwrap();
    tokio::fs::write(old.join("data/old.txt"), b"old disk")
        .await
        .unwrap();
    let manager = controller.data.join("runs").join(&run);
    private_dir(&manager).await.unwrap();
    tokio::fs::write(
        manager.join(crate::conversation_deletion::retired::MANAGER_NAMESPACE),
        b"manager",
    )
    .await
    .unwrap();
    tokio::fs::write(manager.join("new.txt"), b"new remote environment")
        .await
        .unwrap();
    let path = format!("/disks/{run}/delete");
    assert_eq!(
        route(controller.clone(), request(&path, &json!({})))
            .await
            .unwrap_err()
            .status,
        409
    );
    assert!(old.join("data/old.txt").exists());
    route(
        controller,
        request(&path, &json!({ "retirementId":crate::config::id() })),
    )
    .await
    .unwrap();
    assert!(!old.join("data").exists());
    assert_eq!(
        tokio::fs::read(manager.join("new.txt")).await.unwrap(),
        b"new remote environment"
    );
}

#[tokio::test]
async fn legacy_native_cleanup_hints_preserve_the_completed_replacement() {
    let (_fixture, controller) = fixture().await;
    let (run, old, fresh, retirement) = (
        crate::config::id(),
        crate::config::id(),
        crate::config::id(),
        crate::config::id(),
    );
    tokio::fs::write(controller.file(&old, "log"), b"legacy private output")
        .await
        .unwrap();
    let old_root = controller.state.join("roots").join(&old);
    private_dir(&old_root).await.unwrap();
    tokio::fs::write(old_root.join("home"), b"legacy private home")
        .await
        .unwrap();
    let endpoint = format!("/disks/{run}/delete");
    let body = json!({ "retirementId":retirement,"attempts":[old] });
    assert_eq!(
        route(controller.clone(), request(&endpoint, &body))
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(!controller.file(&old, "log").exists());
    assert!(!old_root.exists());
    let disk = workspace::directory(&controller.state, &run).unwrap();
    tokio::fs::write(disk.join("replacement"), b"new disk")
        .await
        .unwrap();
    tokio::fs::write(controller.file(&fresh, "run"), run.as_bytes())
        .await
        .unwrap();
    tokio::fs::write(controller.file(&fresh, "log"), b"new private output")
        .await
        .unwrap();
    let fresh_root = controller.state.join("roots").join(&fresh);
    private_dir(&fresh_root).await.unwrap();
    tokio::fs::write(fresh_root.join("home"), b"new private home")
        .await
        .unwrap();
    assert_eq!(
        route(controller.clone(), request(&endpoint, &body))
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        tokio::fs::read(disk.join("replacement")).await.unwrap(),
        b"new disk"
    );
    assert_eq!(
        tokio::fs::read(controller.file(&fresh, "log"))
            .await
            .unwrap(),
        b"new private output"
    );
    assert_eq!(
        tokio::fs::read(fresh_root.join("home")).await.unwrap(),
        b"new private home"
    );
}

#[tokio::test]
async fn late_retirement_retry_preserves_the_next_native_environment() {
    let (_fixture, controller) = fixture().await;
    let run = crate::config::id();
    let retirement = crate::config::id();
    let root = workspace::directory(&controller.state, &run).unwrap();
    private_dir(&root.join("data")).await.unwrap();
    tokio::fs::write(root.join("data/old"), b"old environment")
        .await
        .unwrap();
    let path = format!("/disks/{run}/delete");
    let body = json!({ "retirementId": retirement });
    let held = disk::lock(&controller.state).await.unwrap();
    let mut late = Box::pin(route(controller.clone(), request(&path, &body)));
    assert!(futures_util::poll!(&mut late).is_pending());
    drop(held);
    // Leave the original waiter descheduled while its retry overtakes it.
    let acknowledged = route(controller.clone(), request(&path, &body))
        .await
        .unwrap();
    assert_eq!(acknowledged.status(), 200);
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(root.join("data/new"), b"next environment").unwrap();
    assert_eq!(late.await.unwrap().status(), 200);
    assert_eq!(
        std::fs::read(root.join("data/new")).unwrap(),
        b"next environment"
    );
    // Reopen the durable receipt before attempting ownership of an
    // environment that is now executing.
    let _active = crate::file_lock::exclusive(&root.join("owner.lock"), "active").unwrap();
    let repeated = route(controller.clone(), request(&path, &body))
        .await
        .unwrap();
    assert_eq!(repeated.status(), 200);
    assert_eq!(
        std::fs::read(root.join("data/new")).unwrap(),
        b"next environment"
    );
}

#[tokio::test]
async fn retirement_waits_for_the_conversations_staging_writer() {
    let (_root, controller) = fixture().await;
    let run = crate::config::id();
    let stage = controller.data.join("runs").join(&run);
    private_dir(&stage).await.unwrap();
    tokio::fs::write(stage.join("seed"), b"pending staging")
        .await
        .unwrap();
    let writer = disk::lock_staging(&controller.state, &run).await.unwrap();
    let deletion = || {
        request(
            &format!("/disks/{run}/delete"),
            &json!({ "permanentlyDeleted": true }),
        )
    };

    let error = route(controller.clone(), deletion()).await.unwrap_err();
    assert!(error.is_conflict());
    assert!(stage.join("seed").is_file());

    drop(writer);
    route(controller, deletion()).await.unwrap();
    assert!(!stage.exists());
}

#[tokio::test]
async fn deleting_a_managed_workspace_does_not_follow_its_symlink_or_remove_unrelated_files() {
    let (_root, controller) = fixture().await;
    let outside = tempfile::tempdir().unwrap();
    tokio::fs::write(outside.path().join("keep"), b"owner files")
        .await
        .unwrap();
    let run = crate::config::id();
    let directory = workspace::directory(&controller.state, &run).unwrap();
    private_dir(&directory).await.unwrap();
    std::os::unix::fs::symlink(outside.path(), directory.join("data")).unwrap();
    let docker_runtime = directory.join("docker-runtime");
    let mirror = docker_runtime.join("runtime/mounts/fixture/source");
    tokio::fs::create_dir_all(&mirror).await.unwrap();
    tokio::fs::create_dir_all(docker_runtime.join("rootlesskit"))
        .await
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), mirror.join("outside")).unwrap();
    tokio::fs::write(docker_runtime.join("rootlesskit/child_pid"), b"123")
        .await
        .unwrap();
    tokio::fs::write(directory.join("unrelated"), b"preserved")
        .await
        .unwrap();
    route(
        controller,
        request(
            &format!("/disks/{run}/delete"),
            &json!({ "permanentlyDeleted": true }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read(outside.path().join("keep")).await.unwrap(),
        b"owner files"
    );
    assert!(!directory.join("data").exists());
    assert!(!docker_runtime.exists());
    assert!(directory.join("unrelated").exists());
}

#[tokio::test]
async fn native_budget_supports_six_slots_and_rejects_invalid_capacities() {
    let (_root, controller) = fixture().await;
    route(
        controller.clone(),
        request(
            "/node-budget",
            &json!({ "slots": 6, "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 } }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(controller.budget.lock().await["slots"], 6);
    for slots in [json!(0), json!(4097), json!(1.5), json!(null)] {
        assert!(
            route(
                controller.clone(),
                request(
                    "/node-budget",
                    &json!({
                        "slots": slots, "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 }
                    })
                )
            )
            .await
            .is_err()
        );
    }
    assert_eq!(controller.budget.lock().await["slots"], 6);
}

#[tokio::test]
async fn controller_enforces_all_configured_slots_before_preparing_an_attempt() {
    let (_root, controller) = fixture().await;
    controller.budget.lock().await["slots"] = 6.into();
    let attempt = crate::config::id();
    controller
        .leases
        .lock()
        .await
        .insert(attempt.clone(), crate::nodes::boot_ms() + 60000);
    for _ in 0..5 {
        controller.active.lock().await.insert(
            crate::config::id(),
            Attempt {
                live_access: Arc::default(),
                run: crate::config::id(),
                expires: None,
                stop: CancellationToken::new(),
                started: true,
                protection: None,
                suspension: Arc::new(super::super::suspension::Suspension::new(
                    &controller.state,
                    &attempt,
                )),
            },
        );
    }
    // Five occupied slots still admit a sixth attempt as far as plan loading.
    assert_ne!(controller.start(&attempt).await.unwrap_err().status, 503);
    let existing = crate::config::id();
    controller.active.lock().await.insert(
        existing.clone(),
        Attempt {
            live_access: Arc::default(),
            run: crate::config::id(),
            expires: None,
            stop: CancellationToken::new(),
            started: true,
            protection: None,
            suspension: Arc::new(super::super::suspension::Suspension::new(
                &controller.state,
                &attempt,
            )),
        },
    );
    assert_eq!(controller.start(&attempt).await.unwrap_err().status, 503);
    // Retrying an existing attempt never consumes a second slot.
    controller.start(&existing).await.unwrap();
    controller.active.lock().await.remove(&existing);
    assert_ne!(controller.start(&attempt).await.unwrap_err().status, 503);
}
