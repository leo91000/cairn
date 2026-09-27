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
async fn a_lost_pause_acknowledgement_resumes_and_thaws_before_returning_error() {
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
    std::fs::write(disk.join("data.ext4"), b"coherent disk").unwrap();
    std::fs::write(
        root.path().join(format!("{attempt}.vm.json")),
        json!({"vmId":vm}).to_string(),
    )
    .unwrap();
    let api = root.path().join("jails/firecracker").join(vm).join("root");
    std::fs::create_dir_all(&api).unwrap();
    let controller = UnixListener::bind(api.join("api.sock")).unwrap();
    let paused = Arc::new(AtomicBool::new(false));
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
                if value["state"] == "Paused" {
                    state.store(true, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                } else {
                    state.store(false, Ordering::SeqCst);
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
    assert!(!paused.load(Ordering::SeqCst));
    assert!(thawed.load(Ordering::SeqCst));
    assert!(!stop.is_cancelled());
    controller_task.abort();
    guest_task.abort();
}

#[tokio::test]
async fn stopped_disk_capture_reclaims_abandoned_transfers_and_failed_indexing() {
    use leo_agent_manager::{config::id, nodes::checkpoint};
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;
    let root = TempDir::new().unwrap();
    let run = id();
    let disk = root.path().join("disks").join(&run);
    std::fs::create_dir_all(&disk).unwrap();
    std::fs::write(disk.join("data.ext4"), b"retained environment").unwrap();
    std::fs::write(
        disk.join("runtime.json"),
        json!({"runtimeId":"fixture"}).to_string(),
    )
    .unwrap();
    let capture = || {
        checkpoint::capture(
            root.path(),
            &run,
            None,
            Arc::new(Mutex::new(())),
            CancellationToken::new(),
            &run,
            None,
        )
    };
    let first = capture().await.unwrap();
    let second = capture().await.unwrap();
    assert!(
        !root
            .path()
            .join("snapshots")
            .join(first["id"].as_str().unwrap())
            .exists()
    );
    assert!(
        root.path()
            .join("snapshots")
            .join(second["id"].as_str().unwrap())
            .exists()
    );
    std::fs::remove_file(disk.join("runtime.json")).unwrap();
    assert!(capture().await.is_err());
    assert_eq!(
        std::fs::read_dir(root.path().join("snapshots"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        std::fs::read(disk.join("data.ext4")).unwrap(),
        b"retained environment"
    );
}

#[tokio::test]
async fn older_guest_runtimes_refuse_active_capture_without_interrupting_the_vm() {
    use leo_agent_manager::{config::id, nodes::checkpoint};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::UnixListener,
    };
    let root = TempDir::new().unwrap();
    let run = id();
    std::fs::create_dir_all(root.path().join("disks").join(&run)).unwrap();
    let socket = root.path().join("guest.sock");
    let guest = UnixListener::bind(&socket).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = guest.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            stream.get_mut().write_all(b"OK 5200\n").await.unwrap();
            line.clear();
            stream.read_line(&mut line).await.unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
            // The pre-node guest supports status but has no filesystem capture protocol.
            stream
                .get_mut()
                .write_all(b"{\"version\":1,\"binaryImports\":true}\n")
                .await
                .unwrap();
        }
    });
    let stop = tokio_util::sync::CancellationToken::new();
    let result = checkpoint::capture(
        root.path(),
        &run,
        Some(socket),
        Arc::new(tokio::sync::Mutex::new(())),
        stop.clone(),
        &id(),
        None,
    )
    .await;
    assert!(result.is_err());
    assert!(
        !stop.is_cancelled(),
        "A retained older runtime must keep executing when active capture is unsupported"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "Reject before freezing or thawing the old guest"
    );
    task.abort();
}

#[tokio::test]
async fn active_captures_read_only_the_blocks_the_guest_reports_as_written() {
    use leo_agent_manager::{config::id, nodes::checkpoint};
    use serde_json::{Value, json};
    use std::{
        io::{Seek, SeekFrom, Write},
        sync::{Arc, Mutex},
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::UnixListener,
    };
    const BLOCK: u64 = snapshots::BLOCK;
    let root = TempDir::new().unwrap();
    let (run, attempt, vm) = (id(), id(), id());
    let disk = root.path().join("disks").join(&run);
    std::fs::create_dir_all(&disk).unwrap();
    let data = disk.join("data.ext4");
    let write = |block: u64, byte: u8| {
        let mut file = std::fs::OpenOptions::new().write(true).open(&data).unwrap();
        file.seek(SeekFrom::Start(block * BLOCK + 17)).unwrap();
        file.write_all(&[byte; 4096]).unwrap();
    };
    std::fs::File::create(&data)
        .unwrap()
        .set_len(16 * BLOCK)
        .unwrap();
    for block in [0, 3, 9] {
        write(block, 1);
    }
    std::fs::write(
        disk.join("runtime.json"),
        json!({"runtimeId":"fixture"}).to_string(),
    )
    .unwrap();
    std::fs::write(
        root.path().join(format!("{attempt}.vm.json")),
        json!({"vmId":vm}).to_string(),
    )
    .unwrap();
    // Firecracker's API: pause and resume always succeed, and a paused VM cannot answer.
    let api = root.path().join("jails/firecracker").join(&vm).join("root");
    std::fs::create_dir_all(&api).unwrap();
    let controller = UnixListener::bind(api.join("api.sock")).unwrap();
    let paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let vcpus = paused.clone();
    let controller_task = tokio::spawn(async move {
        loop {
            let (socket, _) = controller.accept().await.unwrap();
            let vcpus = vcpus.clone();
            tokio::spawn(async move {
                let mut socket = BufReader::new(socket);
                let (mut line, mut size) = (String::new(), 0);
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
                socket.read_exact(&mut body).await.unwrap();
                let state: Value = serde_json::from_slice(&body).unwrap();
                vcpus.store(
                    state["state"] == "Paused",
                    std::sync::atomic::Ordering::SeqCst,
                );
                let _ = socket
                    .get_mut()
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                    .await;
            });
        }
    });
    // The guest answers with whatever write list the test sets and records what it was asked.
    let reply = Arc::new(Mutex::new(Value::Null));
    let asked = Arc::new(Mutex::new(Vec::<Value>::new()));
    let guest_path = root.path().join("guest.sock");
    let guest = UnixListener::bind(&guest_path).unwrap();
    let (answer, record, stopped) = (reply.clone(), asked.clone(), paused.clone());
    let guest_task = tokio::spawn(async move {
        loop {
            let (socket, _) = guest.accept().await.unwrap();
            // A guest whose vCPUs are paused never answers.
            if stopped.load(std::sync::atomic::Ordering::SeqCst) {
                continue;
            }
            let mut socket = BufReader::new(socket);
            let mut line = String::new();
            socket.read_line(&mut line).await.unwrap();
            socket.get_mut().write_all(b"OK 5200\n").await.unwrap();
            line.clear();
            socket.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let response = match request["op"].as_str().unwrap() {
                "status" => json!({"version":1,"filesystemSnapshots":true,"writeTracking":true}),
                "written" => {
                    record.lock().unwrap().push(request.clone());
                    answer.lock().unwrap().clone()
                }
                _ => json!({"ok":true}),
            };
            socket
                .get_mut()
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let capture = |baseline: Option<String>| {
        let (root, run, attempt, guest_path) = (
            root.path().to_owned(),
            run.clone(),
            attempt.clone(),
            guest_path.clone(),
        );
        async move {
            checkpoint::capture(
                &root,
                &run,
                Some(guest_path),
                Arc::new(tokio::sync::Mutex::new(())),
                tokio_util::sync::CancellationToken::new(),
                &attempt,
                baseline.as_deref(),
            )
            .await
            .unwrap()
        }
    };
    let blocks = |manifest: &Value| manifest["blocks"].clone();

    // First capture of this boot: full, and it records era 7 as the baseline.
    *reply.lock().unwrap() = json!({"ok":true,"era":7,"blockSize":BLOCK,"blocks":[]});
    let first = capture(None).await;
    assert_eq!(asked.lock().unwrap()[0]["since"], Value::Null);
    assert_eq!(
        blocks(&first["manifest"]),
        blocks(&snapshots::index(&data).await.unwrap())
    );

    // The guest writes blocks 3 and 9; dm-era reports exactly those since era 7.
    write(3, 2);
    write(9, 3);
    *reply.lock().unwrap() = json!({"ok":true,"era":9,"blockSize":BLOCK,"blocks":[[3,4],[9,10]]});
    let second = capture(Some(first["id"].as_str().unwrap().to_owned())).await;
    assert_eq!(asked.lock().unwrap()[1]["since"], 7);
    assert_eq!(
        blocks(&second["manifest"]),
        blocks(&snapshots::index(&data).await.unwrap())
    );
    assert_eq!(second["manifest"]["localBytesRead"], 2 * BLOCK);
    // The snapshot serves the changed blocks it holds.
    let snapshot = root
        .path()
        .join("snapshots")
        .join(second["id"].as_str().unwrap());
    let hash = second["manifest"]["blocks"][9]["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        snapshots::served(&snapshot, &hash).await.unwrap().len() as u64,
        BLOCK
    );

    // A baseline this node does not know (another boot, lost record) falls back to a full capture.
    let third = capture(Some(id())).await;
    assert_eq!(asked.lock().unwrap()[2]["since"], Value::Null);
    assert_eq!(
        blocks(&third["manifest"]),
        blocks(&snapshots::index(&data).await.unwrap())
    );
    assert_eq!(third["manifest"]["localBytesRead"], 3 * BLOCK);
    controller_task.abort();
    guest_task.abort();
}

#[tokio::test]
async fn a_sealed_stopped_disk_copies_only_blocks_its_metadata_lists() {
    use leo_agent_manager::{
        config::{id, now},
        nodes::{checkpoint, tracking},
    };
    use serde_json::json;
    use std::{
        io::{Seek, SeekFrom, Write},
        sync::Arc,
    };
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;
    if std::process::Command::new("era_invalidate")
        .arg("--version")
        .output()
        .is_err()
    {
        assert!(
            std::env::var_os("LEO_REQUIRE_ERA_TOOLS").is_none(),
            "era_invalidate (thin-provisioning-tools) is required"
        );
        eprintln!("skipping: era_invalidate is not installed");
        return;
    }
    const BLOCK: u64 = snapshots::BLOCK;
    let root = TempDir::new().unwrap();
    let run = id();
    let disk = root.path().join("disks").join(&run);
    std::fs::create_dir_all(&disk).unwrap();
    let data = disk.join("data.ext4");
    let write = |block: u64, byte: u8| {
        let mut file = std::fs::OpenOptions::new().write(true).open(&data).unwrap();
        file.seek(SeekFrom::Start(block * BLOCK + 7 * 4096))
            .unwrap();
        file.write_all(&[byte; 4096]).unwrap();
    };
    // The fixture's disk: 16 blocks of 4 MiB.
    std::fs::File::create(&data)
        .unwrap()
        .set_len(16 * BLOCK)
        .unwrap();
    for block in [0, 2, 9] {
        write(block, 1);
    }
    std::fs::write(
        disk.join("runtime.json"),
        json!({"runtimeId":"fixture"}).to_string(),
    )
    .unwrap();
    // A baseline at era 1, then the guest writes blocks 2, 5 and 6 and seals era 2.
    let baseline = id();
    let manifest = snapshots::index(&data).await.unwrap();
    tracking::remember(&disk, &baseline, 1, &manifest, now())
        .await
        .unwrap();
    let metadata = std::process::Command::new("gzip")
        .args([
            "-dc",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/dm-era-sealed.meta.gz"
            ),
        ])
        .output()
        .unwrap()
        .stdout;
    std::fs::write(tracking::metadata(&disk), metadata).unwrap();
    for block in [2, 5, 6] {
        write(block, 2);
    }
    tracking::seal(&disk, 2).await.unwrap();

    let capture = |baseline: Option<String>| {
        let (root, run) = (root.path().to_owned(), run.clone());
        async move {
            checkpoint::capture(
                &root,
                &run,
                None,
                Arc::new(Mutex::new(())),
                CancellationToken::new(),
                &run,
                baseline.as_deref(),
            )
            .await
            .unwrap()
        }
    };
    let point = capture(Some(baseline)).await;
    assert_eq!(point["manifest"]["incremental"], true);
    assert_eq!(point["manifest"]["localBytesRead"], 3 * BLOCK);
    assert_eq!(
        point["manifest"]["blocks"],
        snapshots::index(&data).await.unwrap()["blocks"]
    );
    // The point becomes the next baseline at the sealed era: nothing was written since.
    let next = capture(Some(point["id"].as_str().unwrap().to_owned())).await;
    assert_eq!(next["manifest"]["incremental"], true);
    assert_eq!(next["manifest"]["localBytesRead"], 0);
    assert_eq!(next["manifest"]["blocks"], point["manifest"]["blocks"]);
    // After a boot, the seal no longer describes the disk: copy everything.
    tracking::prepare_boot(&disk, unsafe { libc::getuid() })
        .await
        .unwrap();
    let full = capture(Some(next["id"].as_str().unwrap().to_owned())).await;
    assert_eq!(full["manifest"]["incremental"], false);
}
