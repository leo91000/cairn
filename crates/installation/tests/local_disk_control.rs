use cairn_installation::{
    config::id,
    nodes::snapshots,
    storage::{Disk, LazyDisk, environment, policy::Policy, runtime},
};
use serde_json::{Value, json};
use std::{
    io::{Seek, SeekFrom, Write},
    path::Path,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const MIB: u64 = 1024 * 1024;

/// The manifest of a 4 KiB disk that was never written.
static EMPTY_DISK: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "version": 1,
        "size": 4096,
        "blockSize": 4_194_304,
        "blocks": [{ "offset": 0, "size": 4096, "hash": null }],
    })
});

#[tokio::test]
async fn holes_are_indexed_as_zero_blocks_without_reading_them() {
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
}

/// How a VM controller command is exercised, and which acknowledgement is lost.
#[derive(Clone, Copy)]
enum ControlScenario {
    Pause,
    Resume,
    Starting,
    Healthy,
}

impl ControlScenario {
    /// Whether the disk is below its reserve, so the VM must be paused.
    fn emergency(self) -> bool {
        matches!(self, Self::Pause | Self::Starting | Self::Healthy)
    }

    /// Whether the VM starts paused, so the lost acknowledgement is a resume.
    fn resume_ack(self) -> bool {
        matches!(self, Self::Resume)
    }

    fn lose_ack(self) -> bool {
        !matches!(self, Self::Healthy)
    }
}

#[tokio::test]
async fn monitor_retries_unknown_cpu_state_after_a_lost_pause_acknowledgement() {
    exercise_vm_control(ControlScenario::Pause).await;
}

#[tokio::test]
async fn monitor_retries_unknown_cpu_state_after_a_lost_resume_acknowledgement() {
    exercise_vm_control(ControlScenario::Resume).await;
}

#[tokio::test]
async fn monitor_waits_for_vm_identity_during_boot() {
    exercise_vm_control(ControlScenario::Starting).await;
}

#[tokio::test]
async fn monitor_applies_and_releases_pressure_with_acknowledged_commands() {
    exercise_vm_control(ControlScenario::Healthy).await;
}

fn reserve(reserve_mi_b: u64) -> Policy {
    Policy {
        reserve_mi_b,
        reserve_percent: 1,
        ..Policy::default()
    }
}

/// Creates a lazily restored disk whose reserve is exhausted in an `emergency`.
fn create_lazy_disk(disk: &Path, emergency: bool) {
    let policy = reserve(if emergency { 16 * 1024 * 1024 } else { 64 });
    let context = json!({ "master": "http://127.0.0.1:1/", "grant": "fixture", "policy": policy });
    let journal = LazyDisk::create(&disk.join("lazy"), &EMPTY_DISK).unwrap();
    journal.set_context(&context).unwrap();
}

/// Serves the Firecracker API: records the requested VM state, and never
/// acknowledges the first transition whose acknowledgement the scenario loses.
fn spawn_firecracker_api(
    controller: UnixListener,
    paused: Arc<AtomicBool>,
    case: ControlScenario,
) -> JoinHandle<()> {
    let lost = Arc::new(AtomicBool::new(false));
    tokio::spawn(async move {
        loop {
            let (socket, _) = controller.accept().await.unwrap();
            let paused = paused.clone();
            let lost = lost.clone();
            tokio::spawn(async move {
                let mut socket = BufReader::new(socket);
                let body = read_http_body(&mut socket).await;
                let value: Value = serde_json::from_slice(&body).unwrap();
                let pause = value["state"] == "Paused";
                paused.store(pause, Ordering::SeqCst);
                if case.lose_ack()
                    && pause != case.resume_ack()
                    && !lost.swap(true, Ordering::SeqCst)
                {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    return;
                }
                let _ = socket
                    .get_mut()
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                    .await;
            });
        }
    })
}

async fn read_http_body(socket: &mut BufReader<UnixStream>) -> Vec<u8> {
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
    socket.read_exact(&mut body).await.unwrap();
    body
}

/// The storage monitor enforces the reserve through the Firecracker API.
async fn check_monitor(
    case: ControlScenario,
    root: &Path,
    disk: &Path,
    attempt: &str,
    paused: &AtomicBool,
) {
    let stop = CancellationToken::new();
    let volume = runtime::load(disk).await.unwrap();
    volume.disk.write_at(0, b"unsaved").unwrap();
    volume.set_paused(case.resume_ack());
    if matches!(case, ControlScenario::Starting) {
        std::fs::remove_file(root.join(format!("{attempt}.vm.json"))).unwrap();
    }
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        volume.enforce_limits(root, attempt, &stop),
    )
    .await
    .unwrap();
    match case {
        ControlScenario::Starting => {
            result.unwrap();
            assert!(!stop.is_cancelled());
            assert!(!paused.load(Ordering::SeqCst));
            assert!(!volume.paused());
        }
        ControlScenario::Healthy => {
            result.unwrap();
            assert!(volume.paused());
            assert!(paused.load(Ordering::SeqCst));
            std::fs::write(
                root.join("storage-policy.json"),
                serde_json::to_vec(&reserve(64)).unwrap(),
            )
            .unwrap();
            // Policy files are applied by the settings endpoint and inspection,
            // never reread by the controller's CPU safety decision.
            volume.inspect().await.unwrap();
            volume.enforce_limits(root, attempt, &stop).await.unwrap();
            assert!(!volume.paused());
            assert!(!paused.load(Ordering::SeqCst));
            assert!(!stop.is_cancelled());
            assert!(!volume.stop.is_cancelled());
        }
        _ => {
            result.unwrap();
            assert_eq!(
                paused.load(Ordering::SeqCst),
                !case.resume_ack(),
                "the VM applied the command even though its acknowledgement was lost"
            );
            assert!(
                !stop.is_cancelled(),
                "a lost reply must not restart the attempt"
            );
            assert!(!volume.stop.is_cancelled());
            volume.enforce_limits(root, attempt, &stop).await.unwrap();
            assert_eq!(volume.paused(), !case.resume_ack());
            assert!(!stop.is_cancelled());
        }
    }
    let mut saved = [0; 7];
    volume.read_at(0, &mut saved).unwrap();
    assert_eq!(&saved, b"unsaved");
}

async fn exercise_vm_control(case: ControlScenario) {
    let root = TempDir::new().unwrap();
    let (run, attempt, vm) = (id(), id(), id());
    let disk = root.path().join("disks").join(&run);
    std::fs::create_dir_all(&disk).unwrap();
    let _physical = cairn_installation::file_lock::exclusive(&disk.join("lock"), "busy").unwrap();
    std::fs::write(
        root.path().join(format!("{attempt}.vm.json")),
        json!({ "vmId": vm, "runId": run }).to_string(),
    )
    .unwrap();
    create_lazy_disk(&disk, case.emergency());
    let live = runtime::load(&disk).await.unwrap();
    let resolved = environment::directory(root.path(), &run).unwrap();
    assert!(Arc::ptr_eq(&live, &runtime::load(&resolved).await.unwrap()));
    std::fs::write(
        disk.join("runtime.json"),
        json!({"runtimeId":"development"}).to_string(),
    )
    .unwrap();
    let api = root.path().join("jails/firecracker").join(&vm).join("root");
    std::fs::create_dir_all(&api).unwrap();
    std::fs::write(
        root.path().join(format!("{attempt}.suspension.json")),
        json!({
            "vmId": vm,
            "ready": true,
            "reasons": if case.resume_ack() { json!([{ "type": "storage" }]) } else { json!([]) },
            "confirmedPaused": case.resume_ack(),
            "administrativeSince": null,
            "administrativeSuspendedMs": 0,
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        api.join("suspension-owner.json"),
        json!({ "attempt": attempt }).to_string(),
    )
    .unwrap();
    let paused = Arc::new(AtomicBool::new(case.resume_ack()));
    let controller_task = spawn_firecracker_api(
        UnixListener::bind(api.join("api.sock")).unwrap(),
        paused.clone(),
        case,
    );
    check_monitor(case, root.path(), &disk, &attempt, &paused).await;
    controller_task.abort();
}
