//! Test-only manager for the disposable local native-runtime qualification.
//! JSONL stays on private pipes, with synthetic local ownership and credentials.
use run_fixture::DbRunFixture;

#[path = "../tests/common/agent_fixture.rs"]
mod agent_fixture;

use agent_fixture::{AgentFixture, StoreAgentFixture};
use cairn_contracts::{
    ExecutionNode, NativeMode, NativeModeObserved, NativeObservationStatus, NodeBackend,
    NodeCapabilities, NodeId, NodeStoragePolicy,
};
use cairn_installation::{
    config::{Config, id, now},
    error::{Error, Result},
    http, nodes,
    service::Service,
    storage::{self, Disk, LazyDisk},
};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::io::{AsyncBufReadExt, BufReader};

const TRUSTED: &str = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";

async fn inspect_stopped_journal<T: Send + 'static>(
    directory: PathBuf,
    inspect: impl FnOnce(Arc<LazyDisk>) -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(move || {
        // A Volume would start a consolidation monitor and retire the very
        // generation being audited. This stopped inspection owns only a journal,
        // with the normal cross-process lock and no possible remote reads.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let disk = loop {
            match LazyDisk::open(&directory.join("lazy")) {
                Ok(disk) => break disk,
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(error) => return Err(error.into()),
            }
        };
        inspect(Arc::new(disk))
    })
    .await
    .map_err(Error::internal)?
}

async fn authority(s: &Service) -> Result<()> {
    s.store.set("native-authority", json!({
        "observedAt": now(), "accounts": { TRUSTED: { "email": "fixture@example.test", "accessId": "fixture" } }
    }), None).await?;
    Ok(())
}

async fn dispatch(s: &Arc<Service>, requests: &AtomicU64, command: &Value) -> Result<Value> {
    let run_id = command["run"].as_str().unwrap_or("");
    match command["op"].as_str().unwrap_or("") {
        "create" => {
            cairn_installation::validation::uuid(run_id)?;
            let mut agent = s.agent_fixture(json!({ "name": "Fixture" }), None).await?;
            // The reserved local ID is a VM runner in production. Only this
            // disposable manager seeds it as native to route through the private
            // fixture bridge; do not weaken the real agent-save prohibition.
            agent["access"]["backend"] = "native".into();
            agent["access"]["nodes"] = json!([nodes::LOCAL_NODE_ID]);
            s.store.put_agent_fixture(agent.clone()).await?;
            let run = json!({
                "id": run_id, "taskId": id(), "projectId": "", "status": "succeeded", "createdAt": now(),
                "isolated": true, "nodeId": nodes::LOCAL_NODE_ID, "executionBackend": "native",
                "nativePrincipals": [TRUSTED], "nativeAuthorityNodes": [nodes::LOCAL_NODE_ID],
                "nativeProvenanceUnknown": false, "snapshot": { "agent": agent }, "storage": { "mode": "on-demand" }
            });
            s.store
                .transaction({
                    let run = run.clone();
                    move |db| db.add_run_fixture(&run, None)
                })
                .await?;
            Ok(json!({ "owner": run["id"], "policy": {
                "memoryCacheMiB": 4, "reserveMiB": 64, "reservePercent": 1
            } }))
        }
        "checkpoint" => {
            s.store.save_checkpoint_boundary(run_id, json!({
                "runnerId": command["attempt"], "nodeId": nodes::LOCAL_NODE_ID,
                "prepared": { "backend": "native", "isolated": true, "diskFormat": if command["vm"] == true { json!("vm") } else { Value::Null } }, "runtimeId": "synthetic"
            })).await?;
            Ok(json!({ "ok": true }))
        }
        "consolidate" => {
            let root = s.config.data_dir.parent().unwrap();
            let directory = storage::environment::directory(&root.join("state"), run_id)?;
            // The caller has already observed the controller's full physical
            // close. Its detached background consolidation may still own the
            // journal briefly; never bypass that exclusive file lock.
            let owned = directory.clone();
            inspect_stopped_journal(directory, move |disk| {
                let generation = disk.seal_completed()?;
                disk.consolidate()?;
                let secret = b"synthetic-generated-secret-never-persist";
                let mut offset = 0;
                let mut bytes = vec![0; nodes::snapshots::BLOCK as usize];
                while offset < disk.size() {
                    let length = (disk.size() - offset).min(bytes.len() as u64) as usize;
                    disk.read_at(offset, &mut bytes[..length])?;
                    if bytes[..length]
                        .windows(secret.len())
                        .any(|window| window == secret)
                    {
                        return Err(Error::bad("Generated secret reached a local disk block."));
                    }
                    offset += length as u64;
                }
                disk.consolidate()?;
                let mut status = disk.accounting()?;
                status["completedGeneration"] = disk
                    .completed()?
                    .map_or(Value::Null, |value| value["generation"].clone());
                Ok(json!({
                    "generation": generation, "logicalBytes": disk.size(), "status": status,
                    "allocatedBytes": cairn_installation::microvm::budget::allocated(&owned)?
                }))
            })
            .await
        }
        "restore" => Err(Error::conflict(
            "Conversation disks are local; remote restore is no longer supported.",
        )),
        "metrics" => Ok(json!({ "remoteRequests": requests.load(Ordering::SeqCst) })),
        "archive-hash" => {
            let path = PathBuf::from(
                command["path"]
                    .as_str()
                    .ok_or_else(|| Error::bad("Missing fixture archive."))?,
            );
            if tokio::fs::metadata(&path).await?.len() > 64 * 1024 * 1024 {
                return Err(Error::bad("Fixture archive is too large."));
            }
            Ok(json!({ "hash": blake3::hash(&tokio::fs::read(path).await?).to_hex().to_string() }))
        }
        "revoke" => {
            let mut node = s.node_boundary(nodes::LOCAL_NODE_ID).await?;
            node["nativeEnabled"] = false.into();
            s.store.save_node_boundary(node).await?;
            Ok(json!({ "ok": true }))
        }
        "purge" => {
            // Local fixture cleanup: no bucket or remote manifest exists.
            s.store
                .change_run(
                    run_id,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_contracts::RunStatus::Succeeded),
                        ..Default::default()
                    },
                )
                .await?;
            Ok(json!({ "ok": true }))
        }
        _ => Err(Error::bad("Unknown fixture command.")),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // This executable never discovers a user's installation or cloud configuration.
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or_else(|| Error::bad("Missing private fixture."))?,
    );
    let bridge = std::env::args()
        .nth(2)
        .ok_or_else(|| Error::bad("Missing loopback controller."))?;
    let loopback = |value: &str| {
        url::Url::parse(value)
            .is_ok_and(|url| url.scheme() == "http" && url.host_str() == Some("127.0.0.1"))
    };
    if std::env::var("CAIRN_NATIVE_DISPOSABLE_HOST").as_deref() != Ok("1")
        || !root.join("disposable-fixture").is_file()
        || !loopback(&bridge)
    {
        return Err(Error::forbidden(
            "Requires an isolated disposable fixture and a loopback controller.",
        ));
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let data = root.join("manager");
    tokio::fs::create_dir_all(&data).await?;
    tokio::fs::copy(root.join("data/runner-secret"), data.join("runner-secret")).await?;
    let s = Service::new(Config {
        data_dir: data,
        home: root.join("manager-home"),
        workspace_roots: vec![root.clone()],
        public_url: origin,
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "unused".into(),
        claude_bin: "unused".into(),
        opencode_bin: "unused".into(),
        opencode_go_usage_url: cairn_installation::accounts::opencode::USAGE_URL.into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: bridge,
    })
    .await?;
    authority(&s).await?;
    s.store
        .transaction(|db| {
            db.save_node(&ExecutionNode {
                id: NodeId(uuid::Uuid::parse_str(nodes::LOCAL_NODE_ID).map_err(Error::internal)?),
                revision: id(),
                local: true,
                name: "Disposable fixture".into(),
                accepting: true,
                native_enabled: true,
                native_accounts: vec![TRUSTED.into()],
                last_seen: Some(now()),
                created_at: now(),
                capabilities: NodeCapabilities {
                    backend: NodeBackend::Native,
                    ublk: true,
                    native_mode_protocol: 1,
                    os: "linux".into(),
                    arch: "x86_64".into(),
                    ..Default::default()
                },
                native_mode_observed: Some(NativeModeObserved {
                    mode: NativeMode::Protected,
                    generation: None,
                    status: NativeObservationStatus::Applied,
                    error: None,
                    target: None,
                }),
                storage: NodeStoragePolicy {
                    reserve_mi_b: 64,
                    reserve_percent: 1,
                    memory_cache_mi_b: 4,
                },
                ..Default::default()
            })
        })
        .await?;
    let requests = Arc::new(AtomicU64::new(0));
    let count = requests.clone();
    let router = http::router(s.clone())
        .await?
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let count = count.clone();
                async move {
                    if request.uri().path().starts_with("/internal/node-restore/") {
                        count.fetch_add(1, Ordering::SeqCst);
                    }
                    next.run(request).await
                }
            },
        ));
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let refresh = {
        let s = s.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                if authority(&s).await.is_err() {
                    break;
                }
            }
        })
    };
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let command: Value = serde_json::from_str(&line)?;
        let response = match dispatch(&s, &requests, &command).await {
            Ok(value) => json!({ "ok": true, "value": value }),
            Err(error) => json!({ "ok": false, "error": error.message, "status": error.status }),
        };
        println!("{response}");
        std::io::stdout().flush()?;
    }
    refresh.abort();
    server.abort();
    s.shutdown.cancel();
    Ok(())
}

#[path = "../tests/common/run_fixture.rs"]
mod run_fixture;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stopped_inspection_keeps_its_generation_across_a_monitor_period() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/run");
        let disk = LazyDisk::create(
            &directory.join("lazy"),
            &json!({
                "version": 1, "size": 4096, "blockSize": 4194304,
                "blocks": [{ "offset": 0, "size": 4096, "hash": null }]
            }),
        )
        .unwrap();
        disk.set_context(&json!({
            "master": "http://127.0.0.1:1/", "grant": "inspection",
            "policy": { "reserveMiB": 64, "reservePercent": 1 }
        }))
        .unwrap();
        disk.write_at(0, b"kept").unwrap();
        drop(disk);
        let inspected = inspect_stopped_journal(directory, |disk| {
            let generation = disk.seal_completed()?;
            // Real Docker inspections exceed the Volume monitor's five-second
            // interval. Keep the captured generation through that same window.
            std::thread::sleep(std::time::Duration::from_secs(7));
            if !disk.has_sealed(generation)? {
                return Err(Error::bad("Inspection generation was retired concurrently."));
            }
            let mut bytes = vec![0; 4];
            disk.read_at(0, &mut bytes)?;
            Ok(bytes)
        })
        .await;
        assert!(
            inspected.is_ok(),
            "inspection generation was retired concurrently: {inspected:?}"
        );
        assert_eq!(&inspected.unwrap()[..4], b"kept");
    }
}
