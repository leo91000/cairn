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
