//! These process-isolated regressions can also be applied to an unchanged
//! reference runtime: they call the actual seed/archive APIs and private socket.
use super::*;
use axum::routing::post;
use tokio::sync::{mpsc, watch};

#[tokio::test]
#[ignore = "owned native environment child of the network preparation regressions"]
async fn prepare_child() {
    let input: Value =
        serde_json::from_str(&std::env::var("CAIRN_TEST_PREPARATION_CHILD").unwrap()).unwrap();
    assert!(crate::native::enabled());
    let session = crate::nodes::executor::Session {
        client: reqwest::Client::new(),
        master: text(&input, "master").parse().unwrap(),
        token: "synthetic-node".into(),
        attempt: text(&input, "attempt").into(),
    };
    let target = Path::new(text(&input, "target"));
    if input["archive"] == true {
        let archive = serde_json::from_value(input["descriptor"].clone()).unwrap();
        crate::project_archives::stage(&session, target, &[archive], None)
            .await
            .unwrap();
    } else {
        crate::nodes::workspace::seed(
            &session.client,
            &session.master,
            &session.token,
            &session.attempt,
            json!({ "kind": "initial" }),
            target,
        )
        .await
        .unwrap();
    }
}

async fn controlled_network_preparation(archive: bool) {
    let (fixture, controller) = super::tests::fixture().await;
    let run = crate::config::id();
    let id = crate::config::id();
    let target = controller.data.join("runs").join(&run);
    private_dir(&target).await.unwrap();
    let source = fixture.path().join("source");
    private_dir(&source).await.unwrap();
    let payload = vec![37; 128 * 1024];
    tokio::fs::write(source.join("asset"), &payload)
        .await
        .unwrap();
    let descriptor = json!({
        "projectId": crate::config::id(), "key": "a".repeat(64), "revision": "b".repeat(40),
        "hash": blake3::hash(&payload).to_hex().to_string(),
        "bytes": payload.len(), "logicalBytes": payload.len()
    });
    let mut wire = Vec::new();
    if archive {
        wire.extend_from_slice(&payload);
    } else {
        crate::nodes::files::send(&source, &mut wire).await.unwrap();
    }
    let encoded_bytes = wire.len();
    let (entered, mut observed) = mpsc::unbounded_channel();
    let (release, released) = watch::channel(false);
    let app = Router::new().route(
        "/internal/node-workspace/{attempt}/{operation}",
        post(move || {
            let (wire, entered, released) = (wire.clone(), entered.clone(), released.clone());
            async move {
                let stream = futures_util::stream::unfold(
                    (0, wire, entered, released),
                    |(part, wire, entered, mut released)| async move {
                        match part {
                            0 => Some((
                                Ok::<_, std::io::Error>(Bytes::copy_from_slice(&wire[..65536])),
                                (1, wire, entered, released),
                            )),
                            1 => {
                                entered.send(()).unwrap();
                                released.wait_for(|value| *value).await.unwrap();
                                Some((
                                    Ok(Bytes::copy_from_slice(&wire[65536..])),
                                    (2, wire, entered, released),
                                ))
                            }
                            _ => None,
                        }
                    },
                );
                Body::from_stream(stream)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let master = format!("http://{}", listener.local_addr().unwrap());
    let manager = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let socket = tokio::net::UnixListener::bind(controller.state.join("controller.sock")).unwrap();
    let router = Router::new()
        .fallback(any(handler))
        .with_state(controller.clone());
    let local = tokio::spawn(async move { axum::serve(socket, router).await.unwrap() });
    let cpu_before = cpu_ms();
    let child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "native::service::seed_process_tests::prepare_child", "--ignored", "--nocapture"])
        .env("CAIRN_NODE_BACKEND", "native").env("DATA_DIR", &controller.data)
        .env("RUNNER_STATE_DIR", &controller.state)
        .env("CAIRN_TEST_PREPARATION_CHILD", json!({
            "master": master, "attempt": id, "target": target, "archive": archive, "descriptor": descriptor
        }).to_string())
        .kill_on_drop(true).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().unwrap();
    tokio::time::timeout(Duration::from_secs(5), observed.recv())
        .await
        .unwrap()
        .unwrap();
    controller
        .leases
        .lock()
        .await
        .insert(id.clone(), crate::nodes::boot_ms() + 60_000);
    let started = std::time::Instant::now();
    // Identical artificial network hold on reference and candidate. Admission
    // must proceed before this gate opens; no throughput timing gate is used.
    let release_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        release.send(true).unwrap();
        std::time::Instant::now()
    });
    let plan = json!({});
    let admission = controller.start_storage(&id, &plan);
    tokio::pin!(admission);
    let early = tokio::time::timeout(Duration::from_secs(1), &mut admission).await;
    let before_release = early.is_ok();
    let owner = match early {
        Ok(owner) => owner.unwrap(),
        Err(_) => tokio::time::timeout(Duration::from_secs(5), admission)
            .await
            .unwrap()
            .unwrap(),
    };
    let admitted_ms = started.elapsed().as_secs_f64() * 1000.0;
    drop(owner);
    let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let released_at = release_task.await.unwrap();
    let commit_after_release_ms = released_at.elapsed().as_secs_f64() * 1000.0;
    manager.abort();
    local.abort();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let file = if archive {
        target
            .join("project-archives")
            .join(format!("{}.zip", descriptor["projectId"].as_str().unwrap()))
    } else {
        target.join("asset")
    };
    assert_eq!(tokio::fs::read(&file).await.unwrap(), payload);
    let file_metrics = if std::env::var_os("CAIRN_TEST_STORAGE_MEASURE").is_some() {
        use std::{io::Write, os::fd::AsRawFd};
        let handle = std::fs::File::open(&file).unwrap();
        assert_eq!(
            unsafe { libc::posix_fadvise(handle.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) },
            0
        );
        let cold = std::time::Instant::now();
        let read = std::fs::read(&file).unwrap();
        let cold_ms = cold.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(read, payload);
        let warm = std::time::Instant::now();
        for _ in 0..16 {
            assert_eq!(std::fs::read(&file).unwrap(), payload);
        }
        let warm_ms = warm.elapsed().as_secs_f64() * 1000.0 / 16.0;
        let write = std::time::Instant::now();
        let mut probe = std::fs::File::create(fixture.path().join("write-probe")).unwrap();
        probe.write_all(&payload).unwrap();
        probe.sync_all().unwrap();
        json!({ "coldRegularReadMs": cold_ms, "warmRegularReadMeanMs": warm_ms,
            "rawWriteSyncMs": write.elapsed().as_secs_f64() * 1000.0,
            "coldMethod": "private file POSIX_FADV_DONTNEED, not node/S3 cache eviction" })
    } else {
        Value::Null
    };
    let cpu_after = cpu_ms();
    eprintln!(
        "NATIVE_PREPARATION_NETWORK {}",
        json!({
            "archive": archive, "payloadBytes": payload.len(), "encodedWireBytes": encoded_bytes,
            "controlledHoldMs": 1200, "admissionMs": admitted_ms,
            "commitAfterReleaseMs": commit_after_release_ms,
            "parentCpuMs": cpu_after.0 - cpu_before.0, "childCpuMs": cpu_after.1 - cpu_before.1,
            "fileMetrics": file_metrics,
            "scope": "loopback synthetic transfer, ordinary private files, no WAN or physical workload launch"
        })
    );
    assert!(
        before_release,
        "native admission waited behind the controlled network transfer"
    );
}

#[tokio::test]
async fn network_seed_does_not_block_real_native_admission() {
    controlled_network_preparation(false).await;
}

#[tokio::test]
async fn network_archive_does_not_block_real_native_admission() {
    controlled_network_preparation(true).await;
}

fn cpu_ms() -> (f64, f64) {
    fn usage(who: libc::c_int) -> f64 {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::getrusage(who, &mut usage) }, 0);
        let milliseconds =
            |time: libc::timeval| time.tv_sec as f64 * 1000.0 + time.tv_usec as f64 / 1000.0;
        milliseconds(usage.ru_utime) + milliseconds(usage.ru_stime)
    }
    (usage(libc::RUSAGE_SELF), usage(libc::RUSAGE_CHILDREN))
}
