//! Test-only manager for the disposable native-runtime/S3 qualification.
//! JSONL stays on private pipes: responses include synthetic scoped read grants.
use cairn_installation::{
    config::{Config, id, now},
    error::{Error, Result},
    http, nodes,
    nodes::{disk_grants, publication, restore},
    service::Service,
};
use serde_json::{Value, json};
use std::{
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::io::{AsyncBufReadExt, BufReader};

const TRUSTED: &str = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";

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
            let mut agent = s.agent(json!({ "name": "Fixture" }), None).await?;
            // The reserved local ID is a VM runner in production. Only this
            // disposable manager seeds it as native to route through the private
            // fixture bridge; do not weaken the real agent-save prohibition.
            agent["access"]["backend"] = "native".into();
            agent["access"]["nodes"] = json!([nodes::LOCAL_NODE_ID]);
            s.store.put("agents", agent.clone()).await?;
            let run = json!({
                "id": run_id, "taskId": id(), "projectId": "", "status": "succeeded", "createdAt": now(),
                "isolated": true, "nodeId": nodes::LOCAL_NODE_ID, "executionBackend": "native",
                "nativePrincipals": [TRUSTED], "nativeAuthorityNodes": [nodes::LOCAL_NODE_ID],
                "nativeProvenanceUnknown": false, "snapshot": { "agent": agent }, "storage": { "mode": "on-demand" }
            });
            s.store
                .transaction({
                    let run = run.clone();
                    move |db| db.add_run(&run, None)
                })
                .await?;
            let grant = disk_grants::new_disk(s, &run, nodes::LOCAL_NODE_ID).await?;
            Ok(
                json!({ "master": s.config.public_url, "grant": grant, "policy": {
                "cacheMiB": 128, "memoryCacheMiB": 4, "reserveMiB": 64, "reservePercent": 1,
                "backupSeconds": 60, "maxDirtySeconds": 300
            } }),
            )
        }
        "checkpoint" => {
            s.store.set(&format!("run-checkpoint:{run_id}"), json!({
                "runnerId": command["attempt"], "nodeId": nodes::LOCAL_NODE_ID,
                "prepared": { "backend": "native", "isolated": true }, "runtimeId": "synthetic"
            }), None).await?;
            Ok(json!({ "ok": true }))
        }
        "publish" | "live-publish" => {
            if command["op"] == "live-publish" {
                s.store
                    .patch_run(run_id, json!({ "status": "running" }))
                    .await?;
            } else {
                s.store
                    .patch_run(run_id, json!({ "status": "succeeded" }))
                    .await?;
                publication::request(s, run_id).await?;
            }
            let point = publication::capture(s, &s.store.run(run_id).await?).await?;
            let manifest = publication::manifest(s, &point).await?;
            if manifest["kind"] != "native-ext4-v1" || manifest["onDemand"] != true {
                return Err(Error::bad("Expected a native journal recovery point."));
            }
            // Inspect decoded real S3 blocks, not just the file view: generated
            // credentials must never appear in a persistent journal generation.
            let secret = b"synthetic-generated-secret-never-persist";
            for block in manifest["blocks"].as_array().into_iter().flatten() {
                let Some(hash) = block["hash"].as_str() else {
                    continue;
                };
                let bytes = publication::read_block(s, &point, hash).await?;
                if bytes.windows(secret.len()).any(|window| window == secret) {
                    return Err(Error::bad("Generated secret reached persistent blocks."));
                }
            }
            Ok(
                json!({ "backupId": point["id"], "generation": point["diskGeneration"],
                "manifestBytes": serde_json::to_vec(&manifest)?.len(), "point": point }),
            )
        }
        "restore" => {
            let run = s.store.run(run_id).await?;
            let point = s
                .get("node-backups", command["backupId"].as_str().unwrap_or(""))
                .await?;
            restore::start(s, &run, nodes::LOCAL_NODE_ID, &point).await?;
            Ok(json!({ "ok": true }))
        }
        "metrics" => Ok(json!({ "remoteRequests": requests.load(Ordering::SeqCst) })),
        "revoke" => {
            let mut node = s.get("nodes", nodes::LOCAL_NODE_ID).await?;
            node["nativeEnabled"] = false.into();
            s.store.put("nodes", node).await?;
            Ok(json!({ "ok": true }))
        }
        "purge" => {
            publication::purge(s, run_id).await?;
            // Only the isolated fixture skips the normal orphan grace period.
            s.store
                .transaction(|db| {
                    db.0.execute(
                        "UPDATE shared_objects SET unused_at=0 WHERE unused_at IS NOT NULL",
                        [],
                    )?;
                    Ok(())
                })
                .await?;
            while nodes::shared_blocks::collect(s).await? > 0 {}
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
    let endpoint = std::env::var("CAIRN_NODE_TEST_S3_ENDPOINT").map_err(Error::internal)?;
    let loopback = |value: &str| {
        url::Url::parse(value)
            .is_ok_and(|url| url.scheme() == "http" && url.host_str() == Some("127.0.0.1"))
    };
    if std::env::var("CAIRN_NATIVE_DISPOSABLE_HOST").as_deref() != Ok("1")
        || !root.join("disposable-fixture").is_file()
        || !loopback(&bridge)
        || !loopback(&endpoint)
        || std::env::var("AWS_ACCESS_KEY_ID").as_deref() != Ok("node-fixture")
    {
        return Err(Error::forbidden(
            "Requires an isolated disposable fixture and synthetic loopback S3.",
        ));
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let data = root.join("manager");
    tokio::fs::create_dir_all(&data).await?;
    tokio::fs::copy(root.join("data/runner-secret"), data.join("runner-secret")).await?;
    // The CLI transport also uses only the fixture endpoint. The hot SDK
    // transport reads AWS_ENDPOINT_URL_S3 from the isolated parent environment.
    let wrapper = root.join("aws-fixture");
    let script = format!(
        "#!/usr/bin/env python3\nimport subprocess,sys\nsys.exit(subprocess.run(['aws','--endpoint-url',{}]+sys.argv[1:]).returncode)\n",
        serde_json::to_string(&endpoint)?
    );
    tokio::fs::write(&wrapper, script).await?;
    tokio::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).await?;
    tokio::fs::write(
        data.join("archive-s3.json"),
        serde_json::to_vec(&json!({ "bucket": "cairn-node-test", "awsBinary": wrapper }))?,
    )
    .await?;
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
        gh_bin: "false".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: bridge,
    })
    .await?;
    authority(&s).await?;
    s.store.put("nodes", json!({
        "id": nodes::LOCAL_NODE_ID, "local": true, "name": "Disposable fixture", "accepting": true,
        "nativeEnabled": true, "nativeAccounts": [TRUSTED], "lastSeen": now(),
        "capabilities": { "backend": "native", "ublk": true, "os": "linux", "arch": "x86_64" },
        "storage": { "reserveMiB": 64, "reservePercent": 1, "memoryCacheMiB": 4, "cacheMiB": 128 }
    })).await?;
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
