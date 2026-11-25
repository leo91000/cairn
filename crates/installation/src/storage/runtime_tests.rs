use super::*;
use crate::{microvm::suspension, storage::environment};
use serde_json::json;

fn prepared_manifest() -> Value {
    json!({
        "version": 1, "size": 4096, "blockSize": 4_194_304,
        "blocks": [{ "offset": 0, "size": 4096, "hash": null }],
    })
}

fn prepared_context() -> Value {
    json!({ "unassigned": true, "policy": { "reserveMiB": 64, "reservePercent": 1 } })
}

#[tokio::test]
async fn isolated_native_workers_do_not_consume_shared_io_permits() {
    let (entered, received) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let released = Arc::new(Mutex::new(released));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let entered = entered.clone();
        let released = released.clone();
        workers.push(std::thread::spawn(move || {
            isolated_native_io();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime
                .block_on(blocking(move || {
                    entered.send(()).unwrap();
                    released.lock().unwrap().recv().unwrap();
                    Ok(())
                }))
                .unwrap();
        }));
    }
    for _ in 0..8 {
        received
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
    }
    let healthy =
        tokio::time::timeout(std::time::Duration::from_secs(1), blocking(|| Ok(42))).await;
    for _ in 0..8 {
        release.send(()).unwrap();
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(healthy.unwrap().unwrap(), 42);
}

#[tokio::test]
async fn retained_legacy_context_preserves_local_policy_without_controller_file() {
    let state = tempfile::tempdir().unwrap();
    let directory = state.path().join("environments/legacy");
    let context = json!({
        "unassigned": true,
        "policy": {
            "cacheMiB": 4096,
            "backupSeconds": 60,
            "maxDirtySeconds": 300,
            "memoryCacheMiB": 512,
            "reserveMiB": 1024,
            "reservePercent": 7,
        },
    });
    let disk = create_prepared(&directory.join("lazy"), &prepared_manifest(), &context)
        .await
        .unwrap();
    disk.write_at(0, b"retained").unwrap();
    drop(disk);
    assert!(!state.path().join("storage-policy.json").exists());

    let reopened = load(&directory).await.unwrap();
    assert_eq!(
        reopened.policy().unwrap(),
        Policy {
            memory_cache_mi_b: 512,
            reserve_mi_b: 1024,
            reserve_percent: 7,
        },
    );
    let mut bytes = [0; 8];
    reopened.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"retained");
}

#[tokio::test]
async fn slow_disk_open_does_not_block_another_conversation_or_fill_the_io_pool() {
    let root = tempfile::tempdir().unwrap();
    let slow = root.path().join("disks/slow");
    let other_root = tempfile::tempdir().unwrap();
    let (other, _, _api) = controller_fixture(other_root.path()).await;
    let (entered, opening) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let opening_path = slow.clone();
    let worker = std::thread::spawn(move || {
        try_open_with(&opening_path, |_| {
            entered.send(()).unwrap();
            wait.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            Err(Error::unavailable("Synthetic slow opening finished."))
        })
    });
    opening.await.unwrap();

    let mut waiters = Vec::new();
    for _ in 0..16 {
        let path = slow.clone();
        waiters.push(tokio::spawn(async move {
            load_with_wait(&path, std::time::Duration::from_millis(100)).await
        }));
    }
    let loaded = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        load(&other_root.path().join("disks/conversation")),
    )
    .await;
    for waiter in waiters {
        assert!(waiter.await.unwrap().err().unwrap().is_unavailable());
    }
    release.send(()).unwrap();
    assert!(worker.join().unwrap().err().unwrap().is_unavailable());
    assert!(Arc::ptr_eq(&loaded.unwrap().unwrap(), &other));
}

#[tokio::test]
async fn expired_attempt_stops_a_volume_acquired_after_expiry() {
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _api) = controller_fixture(root.path()).await;
    let attempt = AttemptStop::new(CancellationToken::new());
    attempt.cancel();

    assert!(attempt.bind(&volume).unwrap_err().is_unavailable());
    assert!(volume.stop.is_cancelled());
    assert_eq!(
        volume.write_at(0, b"late").unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
}

#[tokio::test]
async fn completed_attempt_preserves_the_disk_until_the_next_attempt_expires() {
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _api) = controller_fixture(root.path()).await;
    let completed = AttemptStop::new(CancellationToken::new());
    completed.bind(&volume).unwrap();
    completed.execution.cancel();
    drop(completed);
    volume.write_at(0, b"retained").unwrap();

    let next = AttemptStop::new(CancellationToken::new());
    next.bind(&volume).unwrap();
    next.cancel();
    assert_eq!(
        volume.write_at(0, b"expired").unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
}

#[tokio::test]
async fn reopened_conversation_never_returns_a_stopped_disk() {
    let root = tempfile::tempdir().unwrap();
    let (previous, _, _api) = controller_fixture(root.path()).await;
    previous.write_at(0, b"BEFORE").unwrap();
    previous.sync().unwrap();

    // VM shutdown cancels this token. An inspector/publication task can
    // retain the previous Volume while another VM loads the same disk.
    previous.stop.cancel();
    let directory = root.path().join("disks/conversation");
    let reopening = load(&directory);
    tokio::pin!(reopening);
    if let Ok(early) =
        tokio::time::timeout(std::time::Duration::from_millis(40), &mut reopening).await
    {
        let volume = early.unwrap();
        panic!(
            "VM boot received a disk before its stopped reader drained: same_volume={}, stopped={}, write={:?}",
            Arc::ptr_eq(&previous, &volume),
            volume.stop.is_cancelled(),
            volume.write_at(8, b"AFTER!"),
        );
    }
    assert!(previous.stop.is_cancelled());
    drop(previous);
    let reopened = tokio::time::timeout(std::time::Duration::from_secs(1), reopening)
        .await
        .unwrap()
        .unwrap();
    reopened.write_at(8, b"AFTER!").unwrap();
    let mut bytes = [0; 6];
    reopened.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"BEFORE");
}

#[tokio::test]
async fn stopped_conversation_reopens_after_last_owner_is_released() {
    let root = tempfile::tempdir().unwrap();
    let (previous, _, _api) = controller_fixture(root.path()).await;
    previous.write_at(0, b"BEFORE").unwrap();
    previous.sync().unwrap();
    previous.stop.cancel();
    drop(previous);

    let reopened = load(&root.path().join("disks/conversation")).await.unwrap();
    reopened.write_at(8, b"AFTER!").unwrap();
    let mut bytes = [0; 6];
    reopened.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"BEFORE");
    reopened.read_at(8, &mut bytes).unwrap();
    assert_eq!(&bytes, b"AFTER!");
}

#[tokio::test]
async fn stopped_journal_wait_is_bounded_and_preserves_other_disk_admission() {
    let root = tempfile::tempdir().unwrap();
    let (previous, _, _api) = controller_fixture(root.path()).await;
    previous.write_at(0, b"BEFORE").unwrap();
    let journal = previous.disk.clone();
    previous.stop.cancel();
    drop(previous);
    let directory = root.path().join("disks/conversation");
    assert!(replacement(&directory).await.err().unwrap().is_conflict());

    let waiting = load_with_wait(&directory, std::time::Duration::from_millis(80));
    let other_root = tempfile::tempdir().unwrap();
    let (other, _, _other_api) = controller_fixture(other_root.path()).await;
    let other_directory = other_root.path().join("disks/conversation");
    let other_open = async {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let opened = load(&other_directory).await.unwrap();
        assert!(Arc::ptr_eq(&opened, &other));
        opened.write_at(0, b"OTHER").unwrap();
    };
    let (result, ()) = tokio::join!(waiting, other_open);
    assert!(result.err().unwrap().is_unavailable());
    let mut bytes = [0; 6];
    journal.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"BEFORE");
    drop(journal);

    let reopened = load(&directory).await.unwrap();
    reopened.write_at(8, b"AFTER!").unwrap();
    reopened.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"BEFORE");
}

#[tokio::test]
#[ignore = "explicit local stopped-volume read/write measurement; run alone"]
async fn stopped_volume_reopen_measurement() {
    fn cpu_micros() -> u64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
            0
        );
        let usage = unsafe { usage.assume_init() };
        ((usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) * 1_000_000
            + usage.ru_utime.tv_usec
            + usage.ru_stime.tv_usec) as u64
    }

    for cancelled in [false, true] {
        let root = tempfile::tempdir_in("/var/tmp").unwrap();
        let (previous, _, _api) = controller_fixture(root.path()).await;
        let bytes = [7; 4096];
        previous.write_at(0, &bytes).unwrap();
        previous.sync().unwrap();
        let directory = root.path().join("disks/conversation");
        let attempt = AttemptStop::new(CancellationToken::new());
        attempt.bind(&previous).unwrap();
        let cpu = cpu_micros();
        let opening = std::time::Instant::now();
        let releasing = if cancelled {
            attempt.cancel();
            Some(tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                drop(previous);
            }))
        } else {
            drop(previous);
            None
        };
        let reopened = load(&directory).await.unwrap();
        let open_us = opening.elapsed().as_micros();
        if let Some(releasing) = releasing {
            releasing.await.unwrap();
        }
        let mut output = [0; 4096];
        let reading = std::time::Instant::now();
        reopened.read_at(0, &mut output).unwrap();
        assert_eq!(output, bytes);
        let cold_read_us = reading.elapsed().as_micros();
        let reading = std::time::Instant::now();
        for _ in 0..256 {
            reopened.read_at(0, &mut output).unwrap();
            assert_eq!(output, bytes);
        }
        let warm_reads_us = reading.elapsed().as_micros();
        let writing = std::time::Instant::now();
        for _ in 0..16 {
            reopened.write_at(0, &bytes).unwrap();
        }
        reopened.sync().unwrap();
        let writes_us = writing.elapsed().as_micros();
        let cpu_us = cpu_micros() - cpu;
        let journal_bytes: u64 = std::fs::read_dir(directory.join("lazy"))
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap())
            .filter(std::fs::Metadata::is_file)
            .map(|metadata| metadata.len())
            .sum();
        println!(
            "{}",
            json!({
                "cancelled": cancelled,
                "openUs": open_us,
                "coldReaderReadUs": cold_read_us,
                "warmReads256Us": warm_reads_us,
                "writes16AndSyncUs": writes_us,
                "cpuUs": cpu_us,
                "journalFileBytes": journal_bytes,
                "remoteEncodedBytes": 0,
            })
        );
    }
}

#[tokio::test]
async fn adopted_disk_binds_authority_without_reopening_journal_and_survives_restart() {
    let state = tempfile::tempdir().unwrap();
    let id = crate::config::id();
    let directory = state.path().join("environments").join(&id);
    let context = prepared_context();
    let disk = create_prepared(&directory.join("lazy"), &prepared_manifest(), &context)
        .await
        .unwrap();
    disk.write_at(0, b"BOOT").unwrap();
    drop(disk);
    let volume = load(&directory).await.unwrap();
    let live = volume.disk.clone();
    assert!(volume.inspect().await.unwrap()["grantId"].is_null());
    assert!(volume.assignment.assigned().unwrap().is_none());
    let wrong = environment::ownership(state.path(), &crate::config::id(), "busy")
        .await
        .unwrap();
    let authorized = json!({
        "master": "http://127.0.0.1:1/",
        "grant": "owner-grant",
        "policy": context["policy"],
    });
    assert!(volume.authorize(&wrong, &authorized).await.is_err());
    assert!(volume.assignment.assigned().unwrap().is_none());
    drop(wrong);
    let physical = crate::file_lock::exclusive(&directory.join("lock"), "busy").unwrap();
    let run = crate::config::id();
    let owner = environment::assign(state.path(), &id, &run).await.unwrap();
    volume.authorize(&owner, &authorized).await.unwrap();
    assert!(Arc::ptr_eq(&live, &volume.disk));
    assert_eq!(
        volume.assignment.assigned().unwrap().unwrap(),
        crate::auth::digest("owner-grant")
    );
    assert!(volume.authorize(&owner, &authorized).await.is_err());
    volume.write_at(4, b"USER").unwrap();
    let mut bytes = [0; 8];
    volume.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"BOOTUSER");
    drop((live, volume, physical, owner));
    let physical = environment::directory(state.path(), &run).unwrap();
    let reopened = load(&physical).await.unwrap();
    assert_eq!(
        reopened.assignment.assigned().unwrap().unwrap(),
        crate::auth::digest("owner-grant")
    );
    reopened.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"BOOTUSER");
}

#[tokio::test]
async fn unassigned_journal_cannot_hide_a_remote_base_or_replace_a_legacy_disk() {
    let state = tempfile::tempdir().unwrap();
    let context = prepared_context();
    let mut remote = prepared_manifest();
    remote["blocks"][0]["hash"] = format!("b3-{}", "a".repeat(64)).into();
    let physical = state.path().join("environments").join(crate::config::id());
    assert!(
        create_prepared(&physical.join("lazy"), &remote, &context)
            .await
            .is_err()
    );
    let authorized = json!({
        "master": "http://127.0.0.1:1/",
        "grant": "fixture",
        "policy": context["policy"],
    });
    assert!(
        create(&physical.join("lazy"), &remote, &authorized)
            .await
            .is_err()
    );
    let legacy = state.path().join("disks").join(crate::config::id());
    drop(
        create_prepared(&legacy.join("lazy"), &prepared_manifest(), &context)
            .await
            .unwrap(),
    );
    assert!(load(&legacy).await.is_err());
}

#[tokio::test]
async fn native_activation_and_control_preserve_cpu_protection_without_a_vm_record() {
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _) = controller_fixture(root.path()).await;
    volume.disk.write_at(0, b"durable native backlog").unwrap();
    let before = crate::config::now();
    volume.activate_attempt("native-first").unwrap();
    let first = volume.active_since.load(Ordering::Acquire);
    assert!(first >= before);
    volume.activate_attempt("native-first").unwrap();
    assert_eq!(volume.active_since.load(Ordering::Acquire), first);
    // Retrying the same attempt keeps its original activation.
    volume.active_since.store(1, Ordering::Release);
    volume.activate_attempt("native-first").unwrap();
    assert_eq!(volume.active_since.load(Ordering::Acquire), 1);
    volume.activate_attempt("native-resumed").unwrap();
    assert!(volume.active_since.load(Ordering::Acquire) >= first);
    volume.fault.store(true, Ordering::Release);
    assert_eq!(volume.cpu_pause_reason().unwrap(), Some("integrity"));
    volume.fault.store(false, Ordering::Release);
    volume.space_pressure.store(true, Ordering::Release);
    assert_eq!(volume.cpu_pause_reason().unwrap(), Some("disk-space"));
}

#[tokio::test]
async fn local_journal_backlog_never_pauses_work_but_disk_faults_still_do() {
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _) = controller_fixture(root.path()).await;
    volume.disk.write_at(0, b"unconsolidated work").unwrap();
    assert_eq!(volume.control_reason().unwrap(), None);
    let status = volume.inspect().await.unwrap();
    assert!(status["dirtyBytes"].as_u64().unwrap() > 0);
    assert_eq!(status["waitingFor"], Value::Null);
    for removed in [
        "backupDegraded",
        "backupUrgent",
        "backupSeconds",
        "maxDirtySeconds",
    ] {
        assert!(status.get(removed).is_none(), "{removed} is still reported");
    }
    volume.pressure.store(true, Ordering::SeqCst);
    assert_eq!(volume.control_reason().unwrap(), Some("disk-space"));
    volume.pressure.store(false, Ordering::SeqCst);
    volume.fault.store(true, Ordering::SeqCst);
    assert_eq!(volume.control_reason().unwrap(), Some("integrity"));
    volume.fault.store(false, Ordering::SeqCst);
    assert_eq!(volume.control_reason().unwrap(), None);
}

#[tokio::test]
async fn slow_policy_files_do_not_suspend_a_safe_vm() {
    use std::os::unix::ffi::OsStrExt;
    let root = tempfile::tempdir().unwrap();
    let (volume, attempt, _) = controller_fixture(root.path()).await;
    let path = root.path().join("storage-policy.json");
    std::fs::remove_file(&path).unwrap();
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    // Any policy-file read blocks indefinitely. CPU safety must rely on the
    // configured policy, atomics and locally enforced write admission.
    let stop = CancellationToken::new();
    let started = std::time::Instant::now();
    volume
        .enforce_limits(root.path(), &attempt, &stop)
        .await
        .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_millis(200));
    assert!(!volume.paused());
    assert!(!stop.is_cancelled());
}

async fn controller_fixture(root: &Path) -> (Arc<Volume>, String, tokio::net::UnixListener) {
    let directory = root.join("disks/conversation");
    let context = json!({"master":"http://127.0.0.1:1/","grant":"fixture",
        "policy":{"reserveMiB":64,"reservePercent":1}});
    std::fs::write(
        root.join("storage-policy.json"),
        serde_json::to_vec(&context["policy"]).unwrap(),
    )
    .unwrap();
    drop(
        create(
            &directory.join("lazy"),
            &json!({"version":1,"size":4096,"blockSize":4194304,
        "blocks":[{"offset":0,"size":4096,"hash":null}]}),
            &context,
        )
        .await
        .unwrap(),
    );
    let volume = load(&directory).await.unwrap();
    let attempt = "attempt".to_owned();
    let vm = uuid::Uuid::new_v4().to_string();
    let jail = root.join("jails/firecracker").join(&vm).join("root");
    std::fs::create_dir_all(&jail).unwrap();
    suspension::prepare_attempt(root, &attempt, &vm)
        .await
        .unwrap();
    std::fs::write(
        root.join("attempt.vm.json"),
        json!({"vmId":vm,"runId":"run"}).to_string(),
    )
    .unwrap();
    suspension::bind_attempt(root, &attempt, &vm).await.unwrap();
    (
        volume,
        attempt,
        tokio::net::UnixListener::bind(jail.join("api.sock")).unwrap(),
    )
}

async fn request_state(
    stream: tokio::net::UnixStream,
) -> (tokio::io::BufReader<tokio::net::UnixStream>, String) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut stream = tokio::io::BufReader::new(stream);
    let mut length = 0;
    loop {
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    (stream, value["state"].as_str().unwrap().to_owned())
}

#[tokio::test]
async fn lost_pause_response_retries_latest_state_without_restarting() {
    use tokio::io::AsyncWriteExt;
    let root = tempfile::tempdir().unwrap();
    let (volume, attempt, listener) = controller_fixture(root.path()).await;
    let server = tokio::spawn(async move {
        let (_, state) = request_state(listener.accept().await.unwrap().0).await;
        assert_eq!(state, "Paused"); // applied, but the connection loses its reply
        let (mut stream, state) = request_state(listener.accept().await.unwrap().0).await;
        assert_eq!(state, "Resumed");
        stream
            .get_mut()
            .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
            .await
            .unwrap();
    });
    let stop = CancellationToken::new();
    volume.pressure.store(true, Ordering::SeqCst);
    volume
        .enforce_limits(root.path(), &attempt, &stop)
        .await
        .unwrap();
    assert!(!stop.is_cancelled());
    assert!(volume.transition.lock().unwrap().is_some());
    volume.pressure.store(false, Ordering::SeqCst);
    volume
        .enforce_limits(root.path(), &attempt, &stop)
        .await
        .unwrap();
    assert!(!stop.is_cancelled());
    assert!(!volume.paused());
    assert!(volume.transition.lock().unwrap().is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn sustained_unknown_transition_still_fences_the_attempt() {
    let root = tempfile::tempdir().unwrap();
    let (volume, attempt, listener) = controller_fixture(root.path()).await;
    let server = tokio::spawn(async move {
        let (_, state) = request_state(listener.accept().await.unwrap().0).await;
        assert_eq!(state, "Paused");
    });
    let stop = CancellationToken::new();
    volume.pressure.store(true, Ordering::SeqCst);
    *volume.transition.lock().unwrap() =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(31));
    assert!(
        volume
            .enforce_limits(root.path(), &attempt, &stop)
            .await
            .is_err()
    );
    assert!(stop.is_cancelled());
    assert!(volume.stop.is_cancelled());
    server.await.unwrap();
}

#[tokio::test]
async fn delayed_pause_response_is_retried_and_explicit_refusal_still_stops() {
    use tokio::io::AsyncWriteExt;
    let root = tempfile::tempdir().unwrap();
    let (volume, attempt, listener) = controller_fixture(root.path()).await;
    let server = tokio::spawn(async move {
        let (stream, state) = request_state(listener.accept().await.unwrap().0).await;
        assert_eq!(state, "Paused");
        let delayed = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            let mut stream = stream;
            let _ = stream
                .get_mut()
                .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
                .await;
        });
        let (mut stream, state) = request_state(listener.accept().await.unwrap().0).await;
        assert_eq!(state, "Paused");
        stream
            .get_mut()
            .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
            .await
            .unwrap();
        let (mut stream, state) = request_state(listener.accept().await.unwrap().0).await;
        assert_eq!(state, "Resumed");
        stream
            .get_mut()
            .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
            .await
            .unwrap();
        delayed.await.unwrap();
    });
    let stop = CancellationToken::new();
    volume.pressure.store(true, Ordering::SeqCst);
    volume
        .enforce_limits(root.path(), &attempt, &stop)
        .await
        .unwrap();
    assert!(!stop.is_cancelled());
    assert!(
        !volume.paused(),
        "an unconfirmed pause must not be marked complete"
    );
    volume
        .enforce_limits(root.path(), &attempt, &stop)
        .await
        .unwrap();
    assert!(volume.paused());
    volume.pressure.store(false, Ordering::SeqCst);
    assert!(
        volume
            .enforce_limits(root.path(), &attempt, &stop)
            .await
            .is_err()
    );
    assert!(stop.is_cancelled());
    assert!(volume.stop.is_cancelled());
    server.await.unwrap();
}

#[tokio::test]
async fn cache_admission_contention_does_not_interrupt_the_vm() {
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _api) = controller_fixture(root.path()).await;
    let (ready, acquired) = std::sync::mpsc::channel();
    let admission = std::thread::spawn(move || {
        let _guard = super::super::cache::admission().unwrap();
        ready.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1300));
    });
    acquired.recv().unwrap();
    let stop = CancellationToken::new();
    let start = std::time::Instant::now();
    let result = volume.enforce_limits(root.path(), "attempt", &stop).await;
    let elapsed = start.elapsed();
    admission.join().unwrap();
    assert!(
        result.is_ok(),
        "contention was treated as a VM failure: {result:?}"
    );
    assert!(!stop.is_cancelled());
    assert!(!volume.paused());
    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "health waited on maintenance: {elapsed:?}"
    );
}

#[tokio::test]
async fn corrupt_local_base_pauses_io_and_preserves_newer_journal_writes() {
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _) = controller_fixture(root.path()).await;
    volume.disk.write_at(0, &[7; 4096]).unwrap();
    volume.disk.seal().unwrap();
    volume.disk.consolidate().unwrap();
    volume.disk.write_at(0, b"local work").unwrap();
    let cache = volume.directory.join("lazy/cache");
    let block = std::fs::read_dir(&cache)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(block, [8; 4096]).unwrap();
    assert_eq!(
        volume.read_at(1024, &mut [0; 8]).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(volume.status().unwrap()["waitingFor"], "integrity");
    assert!(volume.needs_pause().await.unwrap());
    let mut saved = [0; 10];
    volume.read_at(0, &mut saved).unwrap();
    assert_eq!(&saved, b"local work");
    assert_eq!(volume.status().unwrap()["waitingFor"], "integrity");
}

#[tokio::test]
async fn running_journal_pressure_consolidates_without_guest_control_or_turn_completion() {
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _) = controller_fixture(root.path()).await;
    let mut initial = [0; 4096];
    initial[..7].copy_from_slice(b"running");
    volume.disk.write_at(0, &initial).unwrap();
    monitor_consolidation(&volume, 4096);
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while volume.disk.accounting().unwrap()["dirtyBytes"] != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!volume.paused());
    assert!(!volume.stop.is_cancelled());
    assert!(volume.disk.completed().unwrap().is_none());
    volume.write_at(7, b"new").unwrap();
    let mut bytes = [0; 10];
    volume.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"runningnew");
}

#[tokio::test]
async fn small_task_consolidates_journal_history_before_its_allowance_is_full() {
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _) = controller_fixture(root.path()).await;
    volume.limit_backing(2 * 1024 * 1024).unwrap();
    let bytes = [7; 4096];
    let writes: Vec<_> = (0..128)
        .map(|_| DiskWrite {
            offset: 0,
            bytes: &bytes,
        })
        .collect();
    volume.disk.write_batch(&writes).unwrap();
    assert!(volume.task_has_room(4096).unwrap());
    assert!(!volume.pressure.load(Ordering::Acquire));
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while volume.disk.accounting().unwrap()["dirtyBytes"] != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!volume.pressure.load(Ordering::Acquire));
    assert!(!volume.paused());
    assert_eq!(volume.disk.completed().unwrap(), None);
}

#[tokio::test]
async fn quota_admission_tolerates_segments_reclaimed_during_accounting() {
    use std::sync::atomic::AtomicBool;
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _) = controller_fixture(root.path()).await;
    volume.limit_backing(1024 * 1024 * 1024).unwrap();
    let directory = volume.directory.join("lazy");
    for index in 0..128 {
        std::fs::write(
            directory.join(format!("reclaimed-{index}.segment")),
            b"retired",
        )
        .unwrap();
    }
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    let reclaimer = std::thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            for index in 0..128 {
                let path = directory.join(format!("reclaimed-{index}.segment"));
                std::fs::remove_file(&path).unwrap();
                std::fs::write(path, b"retired").unwrap();
            }
        }
    });
    let mut errors = Vec::new();
    for index in 0..2048 {
        *volume.task_usage.lock().unwrap() = None;
        if let Err(error) = volume.task_has_room(4096) {
            errors.push(error.kind());
        }
        if index % 128 == 0 {
            *volume.task_usage.lock().unwrap() = None;
            if let Err(error) = volume.write_at(0, b"x") {
                errors.push(error.kind());
            }
        }
    }
    stopped.store(true, Ordering::Release);
    reclaimer.join().unwrap();
    assert!(
        errors.is_empty(),
        "reclamation failed healthy admission: {errors:?}"
    );
    assert!(!volume.fault.load(Ordering::Acquire));
}

#[tokio::test]
async fn consolidation_scratch_is_excluded_for_vm_and_native_but_work_and_tmp_stay_capped() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("storage-policy.json"),
        br#"{"reserveMiB":64,"reservePercent":1}"#,
    )
    .unwrap();
    let directory = root.path().join("disks/conversation");
    let block = 4 * 1024 * 1024u64;
    let manifest = json!({
        "version": 1, "size": 2 * block, "blockSize": block,
        "blocks": (0..2).map(|index| json!({ "offset": index * block, "size": block, "hash": null })).collect::<Vec<_>>()
    });
    let disk = create(
        &directory.join("lazy"),
        &manifest,
        &json!({
            "master": "http://127.0.0.1:1/", "grant": "bounded-scratch",
            "policy": { "reserveMiB": 64, "reservePercent": 1 }
        }),
    )
    .await
    .unwrap();
    disk.write_at(0, &vec![7; block as usize]).unwrap();
    disk.write_at(block, &vec![9; block as usize]).unwrap();
    drop(disk);
    let volume = load(&directory).await.unwrap();
    volume.disk.seal().unwrap();
    let old_segments: Vec<_> = std::fs::read_dir(directory.join("lazy"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "segment")
        })
        .collect();
    assert!(!old_segments.is_empty());
    let before = allocated(&directory).unwrap();
    let limit = before + 2 * 1024 * 1024;
    volume.limit_backing(limit).unwrap();
    let copied = Arc::new(AtomicBool::new(false));
    let observed = copied.clone();
    let writer = volume.clone();
    let state = root.path().to_owned();
    volume.disk.on_consolidation_copy_ready(Box::new(move || {
        let physical = allocated(&writer.directory).unwrap();
        assert!(
            physical > limit,
            "fixture must cross the task limit with its copy"
        );
        assert!(
            crate::microvm::budget::disk_bytes(&state).unwrap() >= physical,
            "the shared node budget includes the complete internal copy"
        );
        let retained = retained_backing_bytes(&writer.directory).unwrap();
        assert!(retained <= before + 64 * 1024);
        assert!(physical - retained <= 2 * block);
        *writer.task_usage.lock().unwrap() = None;
        assert!(
            writer.task_has_room(4096).unwrap(),
            "VM admission counts only retained data"
        );
        let temporary = writer.directory.join("tmp");
        std::fs::create_dir_all(&temporary).unwrap();
        let user_file = temporary.join("working-files");
        std::fs::write(&user_file, vec![3; 4 * 1024 * 1024]).unwrap();
        assert!(
            retained_backing_bytes(&writer.directory).unwrap() > limit,
            "native accounting includes /tmp"
        );
        *writer.task_usage.lock().unwrap() = None;
        assert!(
            !writer.task_has_room(4096).unwrap(),
            "copy credit must not hide user files"
        );
        std::fs::remove_file(user_file).unwrap();
        *writer.task_usage.lock().unwrap() = None;
        writer.write_at(0, b"next").unwrap();
        observed.store(true, Ordering::Release);
    }));
    let committed = Arc::new(AtomicBool::new(false));
    let observed = committed.clone();
    let writer = volume.clone();
    volume.disk.on_consolidation_committed(Box::new(move || {
        // Simulate reclamation before the maintenance future returns. A
        // newly created file at an old name must never inherit its credit.
        for path in old_segments {
            let held = path.with_extension("retired-test");
            std::fs::rename(&path, &held).unwrap();
            std::fs::write(&path, vec![1; 4096]).unwrap();
            std::fs::remove_file(held).unwrap();
        }
        assert_eq!(
            retained_backing_bytes(&writer.directory).unwrap(),
            allocated(&writer.directory).unwrap()
        );
        *writer.task_usage.lock().unwrap() = None;
        assert!(
            !writer.task_has_room(4 * 1024 * 1024).unwrap(),
            "the new base counts immediately"
        );
        observed.store(true, Ordering::Release);
    }));
    volume.disk.consolidate().unwrap().unwrap();
    assert!(copied.load(Ordering::Acquire));
    assert!(committed.load(Ordering::Acquire));
    assert_eq!(
        retained_backing_bytes(&directory).unwrap(),
        allocated(&directory).unwrap()
    );
    let mut bytes = [0; 4];
    volume.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"next");
    assert!(
        volume.disk.accounting().unwrap()["dirtyBytes"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[tokio::test]
async fn exhausted_backing_allowance_blocks_writes_until_capacity_returns() {
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _) = controller_fixture(root.path()).await;
    volume.limit_backing(0).unwrap();
    let writing = volume.clone();
    let mut writer = tokio::task::spawn_blocking(move || writing.write_at(0, b"retained"));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let blocked = !writer.is_finished();
    volume.limit_backing(64 * 1024 * 1024).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), &mut writer).await;
    if result.is_err() {
        volume.stop.cancel();
        let _ = writer.await;
    }
    assert!(
        blocked,
        "managed /tmp can consume all remaining task headroom"
    );
    result.unwrap().unwrap().unwrap();
    let mut bytes = [0; 8];
    volume.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"retained");
}

#[tokio::test]
async fn task_backing_pressure_counts_journal_history_and_consolidates_before_resuming_writes() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("storage-policy.json"),
        br#"{"reserveMiB":64,"reservePercent":1}"#,
    )
    .unwrap();
    let directory = root.path().join("disks/conversation");
    let block = 4 * 1024 * 1024u64;
    let manifest = json!({
        "version": 1, "size": 4 * block, "blockSize": block,
        "blocks": (0..4).map(|index| json!({ "offset": index * block, "size": block, "hash": null })).collect::<Vec<_>>()
    });
    let disk = create(
        &directory.join("lazy"),
        &manifest,
        &json!({
            "master": "http://127.0.0.1:1/", "grant": "local-task",
            "policy": { "reserveMiB": 64, "reservePercent": 1 }
        }),
    )
    .await
    .unwrap();
    for value in 1..=3 {
        disk.write_at(0, &vec![value; block as usize]).unwrap();
    }
    drop(disk);
    let volume = load(&directory).await.unwrap();
    volume.limit_backing(8 * 1024 * 1024).unwrap();
    let writer = volume.clone();
    let writer = tokio::task::spawn_blocking(move || writer.write_at(0, b"new"));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !volume.pressure.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!writer.is_finished());
    assert!(volume.disk.journal_bytes() < CONSOLIDATE_JOURNAL_BYTES);
    tokio::time::timeout(std::time::Duration::from_secs(15), writer)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(allocated(&directory).unwrap() < 8 * 1024 * 1024);
    let mut bytes = [0; 3];
    volume.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"new");
}

fn hold_consolidation(
    disk: &LazyDisk,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (entered, wait) = tokio::sync::oneshot::channel();
    let (release, held) = std::sync::mpsc::channel();
    disk.on_consolidation_start(Box::new(move || {
        let _ = entered.send(());
        held.recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
    }));
    (wait, release)
}

#[tokio::test]
async fn stopped_vm_consolidates_shutdown_writes_after_its_completed_seal() {
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _) = controller_fixture(root.path()).await;
    let disk = volume.disk.clone();
    volume.write_at(0, b"completed").unwrap();
    let completed = volume.seal_completed().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while disk.accounting().unwrap()["dirtyBytes"] != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Guest shutdown can write superblock/journal metadata after the
    // completed response was sealed, even though no user turn remains.
    volume.write_at(16, b"shutdown").unwrap();
    volume.stop.cancel();
    volume.seal_stopped().await.unwrap();
    drop(volume);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while disk.accounting().unwrap()["dirtyBytes"] != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("stopped VM journal tail was never consolidated");
    assert_eq!(disk.completed().unwrap().unwrap()["generation"], completed);
    assert!(
        disk.accounting().unwrap()["published"]["generation"]
            .as_i64()
            .unwrap()
            > completed
    );
    let mut bytes = [0; 8];
    disk.read_at(16, &mut bytes).unwrap();
    assert_eq!(&bytes, b"shutdown");
}

#[tokio::test]
async fn stopped_prepared_volume_remains_journal_backed() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("environments").join(crate::config::id());
    drop(
        create_prepared(
            &directory.join("lazy"),
            &json!({
                "version": 1, "size": 4096, "blockSize": 4194304,
                "blocks": [{ "offset": 0, "size": 4096, "hash": null }]
            }),
            &json!({
                "unassigned": true,
                "policy": { "reserveMiB": 64, "reservePercent": 1 }
            }),
        )
        .await
        .unwrap(),
    );
    let volume = load(&directory).await.unwrap();
    volume.write_at(0, b"seed").unwrap();
    volume.stop.cancel();
    volume.seal_stopped().await.unwrap();
    assert!(!volume.disk.sealed_pending());
    assert!(!volume.disk.has_base_blocks().unwrap());
    assert!(volume.disk.completed().unwrap().is_none());
}

#[tokio::test]
async fn a_completed_seal_during_consolidation_is_drained_after_volume_release() {
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _) = controller_fixture(root.path()).await;
    let disk = volume.disk.clone();
    disk.write_at(0, b"first").unwrap();
    disk.seal().unwrap();
    let (entered, release) = hold_consolidation(&disk);
    let active = tokio::spawn(consolidate(disk.clone()));
    entered.await.unwrap();
    disk.write_at(0, b"completed").unwrap();
    let generation = volume.seal_completed().await.unwrap();
    drop(volume);
    let mut queued = tokio::spawn(consolidate(disk.clone()));
    let completed_early = tokio::time::timeout(std::time::Duration::from_millis(500), &mut queued)
        .await
        .is_ok();
    release.send(()).unwrap();
    active.await.unwrap();
    if !completed_early {
        queued.await.unwrap();
    }
    assert!(
        !completed_early,
        "a busy journal was incorrectly reported consolidated"
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while disk.sealed_pending() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("completed generation was abandoned after contention");
    assert_eq!(disk.accounting().unwrap()["dirtyBytes"], 0);
    assert!(
        disk.accounting().unwrap()["published"]["generation"]
            .as_i64()
            .unwrap()
            >= generation
    );
}

#[tokio::test]
async fn status_inspection_never_cancels_detached_consolidation() {
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _) = controller_fixture(root.path()).await;
    let directory = volume.directory.clone();
    let disk = volume.disk.clone();
    disk.write_at(0, b"completed").unwrap();
    let (entered, release) = hold_consolidation(&disk);
    volume.seal_completed().await.unwrap();
    entered.await.unwrap();
    drop(volume);
    let inspected = try_inspect(&directory).unwrap();
    let cancelled = disk.consolidation_stop().is_cancelled();
    release.send(()).unwrap();
    assert!(
        inspected.is_none(),
        "inspection must wait for the detached owner"
    );
    assert!(
        !cancelled,
        "status observation cancelled local consolidation"
    );
    drop(disk);
    let volume = load_for_inspection(&directory).await.unwrap();
    assert_eq!(volume.inspect().await.unwrap()["dirtyBytes"], 0);
}

#[tokio::test]
async fn completed_turn_consolidates_after_the_volume_is_released() {
    let root = tempfile::tempdir().unwrap();
    let (volume, _, _) = controller_fixture(root.path()).await;
    volume.disk.write_at(0, b"completed").unwrap();
    let disk = volume.disk.clone();
    let generation = volume.seal_completed().await.unwrap();
    drop(volume);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while disk.sealed_pending() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(disk.accounting().unwrap()["dirtyBytes"], 0);
    assert_eq!(
        disk.accounting().unwrap()["published"]["generation"],
        generation
    );
    let mut saved = [0; 9];
    disk.read_at(0, &mut saved).unwrap();
    assert_eq!(&saved, b"completed");
}

#[tokio::test]
async fn replacement_excludes_openers_and_releases_the_disk_after_failure() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("disks/conversation");
    let manifest = json!({
        "version": 1,
        "size": 4096,
        "blockSize": 4194304,
        "blocks": [{"offset": 0,"size": 4096,"hash": null}]
    });
    let context = json!({
        "master": "http://127.0.0.1:1/",
        "grant": "fixture",
        "policy": Policy::default()
    });
    drop(
        create(&directory.join("lazy"), &manifest, &context)
            .await
            .unwrap(),
    );
    let volume = load(&directory).await.unwrap();
    assert_eq!(replacement(&directory).await.err().unwrap().status, 409);
    drop(volume);
    let guard = replacement(&directory).await.unwrap();
    assert_eq!(load(&directory).await.err().unwrap().status, 409);
    assert_eq!(replacement(&directory).await.err().unwrap().status, 409);
    // Returning an error during restoration drops the same guard.
    drop(guard);
    let reopened = load(&directory).await.unwrap();
    let mut bytes = [1; 3];
    reopened.read_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 3]);
}

#[tokio::test]
async fn sparse_discard_never_charges_quota_and_reaches_later_deleted_bytes() {
    const BLOCK: u64 = 4 * 1024 * 1024;
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("disks/conversation");
    // This fixture tests the shared quota, not the production 10 GiB
    // physical reserve, which can exceed the free space on a test tmpfs.
    let policy = Policy {
        reserve_mi_b: 64,
        reserve_percent: 1,
        ..Default::default()
    };
    std::fs::write(
        root.path().join("storage-policy.json"),
        serde_json::to_vec(&policy).unwrap(),
    )
    .unwrap();
    let manifest = json!({
        "version": 1,
        "size": 16 * BLOCK,
        "blockSize": BLOCK,
        "blocks": (0..16).map(|index| json!({
            "offset": index * BLOCK,
            "size": BLOCK,
            "hash": null
        })).collect::<Vec<_>>()
    });
    let context = json!({
        "master": "http://127.0.0.1:1/",
        "grant": "test",
        "policy": policy
    });
    let disk = create(&directory.join("lazy"), &manifest, &context)
        .await
        .unwrap();
    disk.write_at(15 * BLOCK + 1024, b"deleted").unwrap();
    drop(disk);
    let volume = load(&directory).await.unwrap();
    let budget = crate::microvm::budget::Budget {
        slots: 1,
        limits: crate::nodes::Resources {
            cpu: 1,
            memory_mi_b: 2048,
            disk_mi_b: 128,
        },
    };
    std::fs::write(
        root.path().join(crate::microvm::budget::FILE),
        serde_json::to_vec(&budget).unwrap(),
    )
    .unwrap();
    crate::microvm::budget::disk_bytes(root.path()).unwrap();
    // Charging the empty prefix as writes exhausts this quota before the
    // final block is reached. Only that block needs a zero frame.
    volume.discard(0, volume.size() as usize).unwrap();
    let mut bytes = [1; 7];
    volume.read_at(15 * BLOCK + 1024, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 7]);
    let accounting = volume.disk.accounting().unwrap();
    let charged = crate::microvm::budget::disk_bytes(root.path()).unwrap();
    for _ in 0..8 {
        volume.discard(0, volume.size() as usize).unwrap();
    }
    assert_eq!(volume.disk.accounting().unwrap(), accounting);
    assert!(crate::microvm::budget::disk_bytes(root.path()).unwrap() <= charged);
    assert!(!volume.fault.load(Ordering::SeqCst));
    assert!(!volume.pressure.load(Ordering::SeqCst));
    volume.stop.cancel();
    assert_eq!(
        volume.discard(0, 4096).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
}

#[test]
fn disk_pressure_waits_without_acknowledging_or_losing_writes_and_resumes() {
    // Other tests deliberately hold the process-global admission mutex.
    // Keep the discard deadline and pressure recovery in an isolated process.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "storage::runtime::tests::disk_pressure_recovery_child",
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
#[ignore = "isolated process-global admission and pressure recovery child"]
async fn disk_pressure_recovery_child() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("disks/conversation");
    let policy = Policy {
        reserve_mi_b: 16 * 1024 * 1024,
        ..Default::default()
    };
    let context = json!({
        "master": "http://127.0.0.1:1/",
        "grant": "test",
        "policy": policy
    });

    let disk = LazyDisk::create(
        &directory.join("lazy"),
        &json!({
            "version": 1,
            "size": 4096,
            "blockSize": 4194304,
            "blocks": [{"offset": 0,"size": 4096,"hash": null}]
        }),
    )
    .unwrap();
    disk.set_context(&context).unwrap();
    drop(disk);
    let volume = open(&directory).unwrap();
    let sparse = volume.disk.accounting().unwrap();
    volume.discard(0, 4096).unwrap();
    assert_eq!(volume.disk.accounting().unwrap(), sparse);
    volume.disk.write_at(0, b"deleted").unwrap();
    let dirty_before = volume.disk.accounting().unwrap()["dirtyBytes"].clone();
    let started = std::time::Instant::now();
    assert_eq!(
        volume.discard(0, 4096).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(!volume.fault.load(Ordering::SeqCst));
    assert!(!volume.pressure.load(Ordering::SeqCst));
    let writer = {
        let volume = volume.clone();
        tokio::task::spawn_blocking(move || volume.write_at(3, b"retained"))
    };
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(!writer.is_finished());
    assert_eq!(volume.status().unwrap()["waitingFor"], "disk-space");
    assert_eq!(
        volume.disk.accounting().unwrap()["dirtyBytes"],
        dirty_before
    );
    let policy = Policy {
        reserve_mi_b: 64,
        reserve_percent: 1,
        ..Default::default()
    };
    std::fs::write(
        root.path().join("storage-policy.json"),
        serde_json::to_vec(&policy).unwrap(),
    )
    .unwrap();
    // The controller applies changed limits to an already waiting write.
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), writer).await;
    volume.stop.cancel();
    result.unwrap().unwrap().unwrap();
    let mut data = [0; 8];
    volume.read_at(3, &mut data).unwrap();
    assert_eq!(&data, b"retained");
}
