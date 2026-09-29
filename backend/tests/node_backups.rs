use leo_agent_manager::nodes::snapshots;
use tempfile::TempDir;

#[tokio::test]
async fn incremental_snapshots_reuse_unchanged_blocks_and_restore_exact_bytes() {
    let root = TempDir::new().unwrap();
    let disk = root.path().join("disk");
    let mut original = vec![0u8; 9 * 1024 * 1024];
    original[19] = 41;
    original[8 * 1024 * 1024 + 11] = 99;
    tokio::fs::write(&disk, &original).await.unwrap();
    let first = snapshots::index(&disk).await.unwrap();
    original[8 * 1024 * 1024 + 11] = 100;
    tokio::fs::write(&disk, &original).await.unwrap();
    let second = snapshots::index(&disk).await.unwrap();
    assert_eq!(first["blocks"][0], second["blocks"][0]);
    assert!(second["blocks"][1]["hash"].is_null());
    assert_ne!(first["blocks"][2], second["blocks"][2]);
    let output = root.path().join("restored");
    snapshots::restore(&output, &second, |hash| {
        let disk = disk.clone();
        let manifest = second.clone();
        async move { snapshots::block(&disk, &manifest, &hash).await }
    })
    .await
    .unwrap();
    assert_eq!(tokio::fs::read(output).await.unwrap(), original);
    let mut corrupt = second.clone();
    corrupt["blocks"][0]["hash"] = "00".repeat(32).into();
    assert!(
        snapshots::restore(&root.path().join("bad"), &corrupt, |_| async {
            Ok(vec![1; 4 * 1024 * 1024])
        })
        .await
        .is_err()
    );
    assert!(!root.path().join("bad").exists());
}

#[tokio::test]
async fn holes_are_indexed_as_zero_blocks_without_reading_them() {
    use std::io::{Seek, SeekFrom, Write};
    const MIB: u64 = 1024 * 1024;
    let root = TempDir::new().unwrap();
    let disk = root.path().join("disk");
    // A 1 GiB sparse disk with data in two places, like a mostly empty VM disk.
    let mut file = std::fs::File::create(&disk).unwrap();
    file.set_len(1024 * MIB).unwrap();
    file.seek(SeekFrom::Start(3 * MIB)).unwrap();
    file.write_all(b"workspace").unwrap();
    file.seek(SeekFrom::Start(700 * MIB)).unwrap();
    file.write_all(&[7; 4096]).unwrap();
    file.sync_all().unwrap();
    let manifest = snapshots::index(&disk).await.unwrap();
    snapshots::validate(&manifest).unwrap();
    let hashed = manifest["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["hash"].is_string())
        .map(|b| b["offset"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(hashed, vec![0, 700 * MIB / (4 * MIB) * 4 * MIB]);
    // Only allocated extents are read, not the whole logical size.
    assert!(
        manifest["localBytesRead"].as_u64().unwrap() <= 16 * MIB,
        "{}",
        manifest["localBytesRead"]
    );
    let restored = root.path().join("restored");
    snapshots::restore(&restored, &manifest, |hash| {
        let disk = disk.clone();
        let manifest = manifest.clone();
        async move { snapshots::block(&disk, &manifest, &hash).await }
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(&restored).unwrap(),
        std::fs::read(&disk).unwrap()
    );
}

#[tokio::test]
async fn an_on_demand_lost_pause_acknowledgement_resumes_and_thaws_before_returning_error() {
    exercise_vm_control(ControlScenario::DemandCapture).await;
}

#[tokio::test]
async fn an_emergency_capture_with_unknown_pause_state_stops_the_attempt() {
    exercise_vm_control(ControlScenario::EmergencyCapture).await;
}

#[derive(Clone, Copy)]
enum ControlScenario {
    DemandCapture,
    EmergencyCapture,
    MonitorPause,
    MonitorResume,
    MonitorStarting,
    MonitorHealthy,
}

#[tokio::test]
async fn monitor_stops_unknown_cpu_state_after_a_lost_pause_acknowledgement() {
    exercise_vm_control(ControlScenario::MonitorPause).await;
}

#[tokio::test]
async fn monitor_stops_unknown_cpu_state_after_a_lost_resume_acknowledgement() {
    exercise_vm_control(ControlScenario::MonitorResume).await;
}

#[tokio::test]
async fn monitor_waits_for_vm_identity_during_boot() {
    exercise_vm_control(ControlScenario::MonitorStarting).await;
}

#[tokio::test]
async fn monitor_applies_and_releases_pressure_with_acknowledged_commands() {
    exercise_vm_control(ControlScenario::MonitorHealthy).await;
}

async fn exercise_vm_control(case: ControlScenario) {
    let emergency = matches!(
        case,
        ControlScenario::EmergencyCapture
            | ControlScenario::MonitorPause
            | ControlScenario::MonitorStarting
            | ControlScenario::MonitorHealthy
    );
    let resume_ack = matches!(case, ControlScenario::MonitorResume);
    let lose_ack = !matches!(case, ControlScenario::MonitorHealthy);
    let monitor = matches!(
        case,
        ControlScenario::MonitorPause
            | ControlScenario::MonitorResume
            | ControlScenario::MonitorStarting
            | ControlScenario::MonitorHealthy
    );
    use leo_agent_manager::{config::id, nodes::checkpoint};
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::UnixListener,
    };
    let root = tempfile::TempDir::new().unwrap();
    let run = id();
    let attempt = id();
    let vm = id();
    let disk = root.path().join("disks").join(&run);
    std::fs::create_dir_all(&disk).unwrap();
    std::fs::write(
        root.path().join(format!("{attempt}.vm.json")),
        json!({"vmId": vm}).to_string(),
    )
    .unwrap();
    {
        use leo_agent_manager::storage::{LazyDisk, policy::Policy, remote::RemoteSource};
        let context = json!({
            "master": "http://127.0.0.1:1/",
            "grant": "fixture",
            "policy": Policy { reserve_mi_b:if emergency { 16 * 1024 * 1024 } else { 64 }, reserve_percent:1, ..Default::default() }
        });
        let source = Arc::new(
            RemoteSource::new(
                &context,
                tokio::runtime::Handle::current(),
                Default::default(),
            )
            .unwrap(),
        );
        let journal = LazyDisk::create(
            &disk.join("lazy"),
            &json!({
                "version": 1,
                "size": 4096,
                "blockSize": 4194304,
                "blocks": [{"offset": 0,"size": 4096,"hash": null}]
            }),
            source,
        )
        .unwrap();
        journal.set_context(&context).unwrap();
    }
    let api = root.path().join("jails/firecracker").join(vm).join("root");
    std::fs::create_dir_all(&api).unwrap();
    let controller = UnixListener::bind(api.join("api.sock")).unwrap();
    let paused = Arc::new(AtomicBool::new(resume_ack));
    let state = paused.clone();
    let controller_task = tokio::spawn(async move {
        loop {
            let (socket, _) = controller.accept().await.unwrap();
            let state = state.clone();
            tokio::spawn(async move {
                let mut socket = BufReader::new(socket);
                let mut line = String::new();
                let mut size = 0;
                loop {
                    line.clear();
                    socket.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(n) = line.strip_prefix("Content-Length: ") {
                        size = n.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; size];
                tokio::io::AsyncReadExt::read_exact(&mut socket, &mut body)
                    .await
                    .unwrap();
                let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let pause = value["state"] == "Paused";
                state.store(pause, Ordering::SeqCst);
                if lose_ack && pause != resume_ack {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                } else {
                    let _ = socket
                        .get_mut()
                        .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                        .await;
                }
            });
        }
    });
    let guest_path = root.path().join("guest.sock");
    let guest = UnixListener::bind(&guest_path).unwrap();
    let thawed = Arc::new(AtomicBool::new(false));
    let thaw = thawed.clone();
    let guest_task = tokio::spawn(async move {
        loop {
            let (socket, _) = guest.accept().await.unwrap();
            let mut socket = BufReader::new(socket);
            let mut line = String::new();
            socket.read_line(&mut line).await.unwrap();
            socket.get_mut().write_all(b"OK 5200\n").await.unwrap();
            line.clear();
            socket.read_line(&mut line).await.unwrap();
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            if value["op"] == "thaw" {
                thaw.store(true, Ordering::SeqCst);
            }
            socket
                .get_mut()
                .write_all(b"{\"ok\":true,\"filesystemSnapshots\":true}\n")
                .await
                .unwrap();
        }
    });
    let stop = tokio_util::sync::CancellationToken::new();
    if monitor {
        use leo_agent_manager::storage::{Disk, runtime};
        let volume = runtime::load(&disk).await.unwrap();
        volume.disk.write_at(0, b"unsaved").unwrap();
        volume.set_paused(resume_ack);
        if matches!(case, ControlScenario::MonitorStarting) {
            std::fs::remove_file(root.path().join(format!("{attempt}.vm.json"))).unwrap();
        }
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            volume.enforce_limits(root.path(), &attempt, &stop),
        )
        .await
        .unwrap();
        match case {
            ControlScenario::MonitorStarting => {
                result.unwrap();
                assert!(!stop.is_cancelled());
                assert!(!paused.load(Ordering::SeqCst));
                assert!(!volume.paused());
            }
            ControlScenario::MonitorHealthy => {
                result.unwrap();
                assert!(volume.paused());
                assert!(paused.load(Ordering::SeqCst));
                std::fs::write(
                    root.path().join("storage-policy.json"),
                    serde_json::to_vec(&leo_agent_manager::storage::policy::Policy {
                        reserve_mi_b: 64,
                        reserve_percent: 1,
                        ..Default::default()
                    })
                    .unwrap(),
                )
                .unwrap();
                volume
                    .enforce_limits(root.path(), &attempt, &stop)
                    .await
                    .unwrap();
                assert!(!volume.paused());
                assert!(!paused.load(Ordering::SeqCst));
                assert!(!stop.is_cancelled());
                assert!(!volume.stop.is_cancelled());
            }
            _ => {
                assert!(result.is_err());
                assert_eq!(
                    paused.load(Ordering::SeqCst),
                    !resume_ack,
                    "the VM applied the command even though its acknowledgement was lost"
                );
                assert!(
                    stop.is_cancelled(),
                    "an ambiguous monitor command must stop execution"
                );
                assert!(
                    volume.stop.is_cancelled(),
                    "blocked disk reads must be released for shutdown"
                );
            }
        }
        controller_task.abort();
        guest_task.abort();
        let mut saved = [0; 7];
        volume.read_at(0, &mut saved).unwrap();
        assert_eq!(&saved, b"unsaved");
        return;
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        checkpoint::capture(
            root.path(),
            &run,
            Some(guest_path),
            Arc::new(tokio::sync::Mutex::new(())),
            stop.clone(),
            &attempt,
            None,
        ),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    if emergency {
        assert!(
            stop.is_cancelled(),
            "an unconfirmed emergency pause must stop execution"
        );
        assert!(!thawed.load(Ordering::SeqCst));
    } else {
        assert!(!paused.load(Ordering::SeqCst));
        assert!(thawed.load(Ordering::SeqCst));
        assert!(!stop.is_cancelled());
    }
    controller_task.abort();
    guest_task.abort();
}

#[tokio::test]
async fn restore_does_not_replace_a_journal_still_in_use() {
    use leo_agent_manager::{
        config::id,
        nodes::restore,
        storage::{Disk, LazyDisk, policy::Policy, remote::RemoteSource, runtime},
    };
    use serde_json::json;
    use std::sync::Arc;
    if !std::path::Path::new("/dev/fuse").exists() {
        eprintln!("skipping on-demand controller restore: /dev/fuse is unavailable");
        return;
    }
    let root = TempDir::new().unwrap();
    let run = id();
    let directory = root.path().join("disks").join(&run);
    std::fs::create_dir_all(&directory).unwrap();
    let image = root.path().join("images/fixture");
    std::fs::create_dir_all(&image).unwrap();
    std::fs::write(image.join("root.ext4"), []).unwrap();
    std::fs::write(image.join("vmlinux"), []).unwrap();
    let manifest = json!({
        "version": 1,
        "size": 4096,
        "blockSize": 4194304,
        "runtime": {"runtimeId": "fixture"},
        "blocks": [{"offset": 0,"size": 4096,"hash": null}]
    });
    let context = json!({
        "master": "http://127.0.0.1:1/",
        "grant": "old-grant",
        "policy": Policy {reserve_mi_b:64,reserve_percent:1,..Default::default()}
    });
    let source = Arc::new(
        RemoteSource::new(
            &context,
            tokio::runtime::Handle::current(),
            Default::default(),
        )
        .unwrap(),
    );
    let disk = LazyDisk::create(&directory.join("lazy"), &manifest, source).unwrap();
    disk.set_context(&context).unwrap();
    disk.write_at(0, b"OLD").unwrap();
    drop(disk);
    let old = runtime::load(&directory).await.unwrap();
    let replacement = json!({
        "onDemand": true,
        "manifest": manifest,
        "backupId": id(),
        "master": context["master"],
        "grant": "new-grant",
        "policy": context["policy"]
    });
    let error = restore::controller(root.path(), &run, replacement.clone())
        .await
        .expect_err("an open journal must not be replaced");
    assert_eq!(error.status, 409);
    assert!(!directory.join("restore.pending").exists());
    let mut bytes = [0; 3];
    old.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"OLD");
    drop(old);
    restore::controller(root.path(), &run, replacement)
        .await
        .unwrap();
    let restored = runtime::load(&directory).await.unwrap();
    restored.read_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 3]);
    assert_eq!(
        restored.source.grant_id(),
        leo_agent_manager::auth::digest("new-grant")
    );
}
