mod common;

use common::relay_fixture::router;

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, request::Builder},
    response::IntoResponse,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use cairn_installation::{
    auth,
    config::{Config, MAIN_AGENT_ID, id, now},
    microvm::wire,
    nodes::{
        LOCAL_NODE_ID, alerts, checkpoint, disk_grants, executor, files, moves, placement,
        publication, relay, restore, shared_blocks, snapshots, workspace,
    },
    object_storage::Storage,
    recovery,
    run_status::RunStatus,
    service::Service,
    storage::{Disk, LazyDisk, bootstrap, policy::Policy, remote::RemoteSource, runtime},
    validation::parse,
};
use common::RelayContext;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};
use tokio_util::sync::CancellationToken;

const ENROLL: &str = "/internal/nodes/enroll";
const HEARTBEAT: &str = "/internal/nodes/heartbeat";
const REPLY: &str = "/internal/nodes/reply";
const NODE_SETTINGS: &str = "/api/nodes/settings";
/// Written by `publication::capture` but never readable in a disk block.
const PRIVATE_CONTENTS: &[u8] = b"private-untracked-contents!";
const MIB: usize = 1024 * 1024;

/// A master service, its router and an authenticated owner session.
struct Owner {
    service: Arc<Service>,
    root: TempDir,
    app: Router,
    session: RelayContext,
    host: String,
}

impl Owner {
    async fn new() -> Self {
        Self::at(common::HOST.into()).await
    }

    async fn at(host: String) -> Self {
        Self::with_runner(host, String::new()).await
    }

    async fn with_runner(host: String, runner_url: String) -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let service = Service::new(Config {
            public_url: format!("http://{host}"),
            gh_bin: "false".into(),
            concurrency: 4,
            runner_url,
            ..common::config(root.path())
        })
        .await
        .unwrap();
        if std::env::var_os("CAIRN_NODE_TEST_S3_ENDPOINT").is_some() {
            std::fs::write(
                service.config.data_dir.join("archive-s3.json"),
                json!({ "bucket": "cairn-node-test" }).to_string(),
            )
            .unwrap();
        }
        let session = RelayContext::new(&common::relay_fixture::context(&service).await);
        Self {
            app: router(service.clone()).await.unwrap(),
            service,
            host,
            root,
            session,
        }
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    /// A request to the master, addressed to its public host.
    fn request(&self, method: &str, path: &str) -> Builder {
        Request::builder()
            .method(method)
            .uri(path)
            .header("host", &self.host)
    }

    /// Calls the master with the owner session on `/api/` paths, and the node
    /// credential `token` when given.
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Value,
        token: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = self
            .request(method, path)
            .header("content-type", "application/json");
        if path.starts_with("/api/") {
            request = self.session.authorize(request);
        }
        if let Some(token) = token {
            request = request.header("authorization", bearer(token));
        }
        let response = common::send(
            &self.app,
            request.body(Body::from(body.to_string())).unwrap(),
        )
        .await;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Calls an owner endpoint.
    async fn send(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.call(method, path, body, None).await
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.send("GET", path, Value::Null).await
    }

    async fn invite(&self, name: &str) -> Value {
        let (status, invitation) = self
            .send("POST", "/api/nodes/enrollments", json!({ "name": name }))
            .await;
        assert_eq!(status, StatusCode::OK, "{invitation}");
        invitation
    }

    /// Invites and enrolls a node, and returns its identity.
    async fn enroll(&self, name: &str, capabilities: &Value) -> Value {
        let invitation = self.invite(name).await;
        self.call(
            "POST",
            ENROLL,
            enrollment(&invitation, name, capabilities),
            None,
        )
        .await
        .1
    }

    async fn put(&self, kind: &str, document: Value) {
        self.service.store.put(kind, document).await.unwrap();
    }

    async fn stored(&self, kind: &str, id: &str) -> Option<Value> {
        self.service.store.get(kind, id).await.unwrap()
    }

    async fn run(&self, id: &str) -> Value {
        self.service.store.run(id).await.unwrap()
    }

    async fn add_run(&self, record: &Value) {
        common::add_run(&self.service.store, record).await;
    }

    async fn set_checkpoint(&self, run: &str, checkpoint: Value) {
        common::set_checkpoint(&self.service.store, run, checkpoint).await;
    }

    /// Lets `token` authenticate as `node`.
    async fn authorize_node(&self, node: &str, token: &str) {
        self.service
            .store
            .set(
                &format!("node-token:{}", auth::digest(token)),
                json!(node),
                None,
            )
            .await
            .unwrap();
    }

    /// Records an enrolled node reachable with `token`.
    async fn register_node(&self, node: &str, token: &str) {
        self.put("nodes", json!({ "id": node, "revoked": false }))
            .await;
        self.authorize_node(node, token).await;
    }

    async fn grant_nodes(&self, agent: &str, nodes: Value) {
        self.put(
            "agents",
            json!({ "id": agent, "access": { "nodes": nodes } }),
        )
        .await;
    }

    /// Encrypts a recovery point manifest as the master stores it.
    fn encrypt_manifest(&self, point: &str, manifest: &Value) -> Value {
        self.service
            .vault
            .encrypt(&format!("backup:{point}"), manifest)
            .unwrap()
    }

    /// Where the master keeps the recovery points of `run`.
    fn backup_directory(&self, run: &str) -> PathBuf {
        self.service.config.data_dir.join("node-backups").join(run)
    }

    /// Serves the router on `listener` until the task is aborted.
    fn serve(&self, listener: tokio::net::TcpListener) -> tokio::task::JoinHandle<()> {
        common::serve(listener, self.app.clone())
    }
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn capabilities(kvm: bool, cpu: u32, memory: u32, disk: u32) -> Value {
    json!({
        "os": "linux",
        "arch": "x86_64",
        "kvm": kvm,
        "cpu": cpu,
        "memoryMiB": memory,
        "diskMiB": disk,
    })
}

fn enrollment(invitation: &Value, name: &str, capabilities: &Value) -> Value {
    json!({
        "code": invitation["code"],
        "name": name,
        "protocol": 1,
        "capabilities": capabilities,
        "runtimeId": "fixture",
    })
}

fn limits(cpu: u32, memory: u32, disk: u32) -> Value {
    json!({ "cpu": cpu, "memoryMiB": memory, "diskMiB": disk })
}

/// A connected node that accepts VM work within `limits`.
fn schedulable_node(id: &str, limits: &Value) -> Value {
    json!({
        "id": id,
        "accepting": true,
        "executionReady": true,
        "sharedResources": true,
        "lastSeen": now(),
        "capabilities": { "kvm": true, "fuse": true },
        "limits": limits,
    })
}

/// A run owned by `agent`, as seen by placement.
fn agent_run(run: &str, agent: &str) -> Value {
    json!({ "id": run, "snapshot": { "agent": { "id": agent } } })
}

/// A persisted run record in `status`.
fn run_record(run: &str, status: RunStatus) -> Value {
    json!({ "id": run, "taskId": "fixture", "createdAt": 0, "status": status })
}

/// A storage policy with a small reserve, so fixtures fit on any disk.
fn small_reserve() -> Policy {
    Policy {
        reserve_mi_b: 64,
        reserve_percent: 1,
        ..Policy::default()
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn find_by_id(list: &Value, id: &str) -> Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id)
        .unwrap()
        .clone()
}

/// Indexes `disk` as a controller snapshot of the fixture runtime.
async fn controller_manifest(disk: &Path) -> Value {
    let mut manifest = snapshots::index(disk).await.unwrap();
    manifest["capturedAt"] = now().into();
    manifest["runtime"] = json!({ "runtimeId": "fixture" });
    manifest
}

async fn wait_for_path(path: &Path) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

/// Makes every unused shared object collectable and collects them all.
async fn drain_remote_deletions(service: &Service) {
    service
        .store
        .transaction(|db| {
            db.0.execute(
                "UPDATE shared_objects SET unused_at=0 WHERE unused_at IS NOT NULL",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    while shared_blocks::collect(service).await.unwrap() > 0 {}
}

/// The S3 key of the shared object holding block `hash` of `manifest`.
fn shared_object_key(manifest: &Value, hash: &str) -> String {
    let block = manifest["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["hash"] == hash)
        .unwrap();
    shared_blocks::key(hash, block["object"].as_str().unwrap())
}

#[tokio::test]
async fn shared_budget_must_be_applied_before_admission_and_reconfiguration_pauses_admission() {
    let owner = Owner::new().await;
    let node = id();
    let credential = auth::token();
    let token = credential.as_str();
    let shared_limits = limits(7, 7680, 52428);
    let mut record = schedulable_node(&node, &shared_limits);
    record["name"] = "Shared worker".into();
    record["slots"] = 4.into();
    record["capabilities"] = capabilities(true, 8, 8192, 65536);
    record["capabilities"]["fuse"] = true.into();
    owner.put("nodes", record).await;
    owner.authorize_node(&node, token).await;
    let mut heartbeat = json!({
        "runtimeId": "fixture",
        "executionReady": true,
        "dataRoot": owner.service.config.data_dir,
        "budget": { "slots": 4, "limits": shared_limits },
    });
    for (shared, applied_slots, ready) in [(false, 4, false), (true, 3, false), (true, 4, true)] {
        heartbeat["sharedResources"] = shared.into();
        heartbeat["budget"]["slots"] = applied_slots.into();
        assert_eq!(
            owner
                .call("POST", HEARTBEAT, heartbeat.clone(), Some(token))
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            owner.stored("nodes", &node).await.unwrap()["executionReady"],
            ready
        );
    }
    let (status, configured) = owner
        .send(
            "PUT",
            &format!("/api/nodes/{node}"),
            json!({
                "name": "Shared worker", "tags": [], "accepting": true,
                "slots": 12, "limits": shared_limits,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{configured}");
    assert_eq!(
        owner.stored("nodes", &node).await.unwrap()["executionReady"],
        false
    );
    owner
        .call("POST", HEARTBEAT, heartbeat.clone(), Some(token))
        .await;
    assert_eq!(
        owner.stored("nodes", &node).await.unwrap()["executionReady"],
        false
    );
    heartbeat["budget"]["slots"] = 12.into();
    owner.call("POST", HEARTBEAT, heartbeat, Some(token)).await;
    assert_eq!(
        owner.stored("nodes", &node).await.unwrap()["executionReady"],
        true
    );
}

#[tokio::test]
async fn enrollment_is_single_use_and_revocation_removes_node_access() {
    let owner = Owner::new().await;
    let invitation = owner.invite("Desktop").await;
    let input = enrollment(
        &invitation,
        "Desktop",
        &capabilities(true, 16, 32768, 131_072),
    );
    let (status, identity) = owner.call("POST", ENROLL, input.clone(), None).await;
    assert_eq!(status, StatusCode::OK, "{identity}");
    let token = identity["token"].as_str().unwrap();
    assert_eq!(
        owner.call("POST", ENROLL, input, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, nodes) = owner.get("/api/nodes").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!nodes.to_string().contains(token));
    let (status, _) = owner
        .call(
            "POST",
            HEARTBEAT,
            json!({ "runtimeId": "fixture" }),
            Some(token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let node = identity["nodeId"].as_str().unwrap();
    let (status, _) = owner
        .send("POST", &format!("/api/nodes/{node}/revoke"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        owner
            .call("POST", HEARTBEAT, json!({}), Some(token))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn enrollment_uses_a_configurable_direct_manager_origin() {
    let owner = Owner::new().await;
    for origin in [
        "https://192.168.1.20:4310",
        "https://manager.vpn.example",
        "https://manager.example.test",
    ] {
        let (status, invitation) = owner
            .send(
                "POST",
                "/api/nodes/enrollments",
                json!({ "name": "Direct node", "managerUrl": origin }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{invitation}");
        assert_eq!(invitation["managerUrl"], origin);
        let (status, identity) = owner
            .call(
                "POST",
                ENROLL,
                enrollment(
                    &invitation,
                    "Direct node",
                    &capabilities(true, 2, 4096, 32768),
                ),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{identity}");
        assert_eq!(
            owner
                .call("POST", HEARTBEAT, json!({}), identity["token"].as_str())
                .await
                .0,
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn direct_node_channel_accepts_private_proxy_hosts_and_keeps_credentials_required() {
    let owner = Owner::new().await;
    let (listener, address) = common::bind().await;
    let router = cairn_installation::http::router(owner.service.clone())
        .await
        .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let base = format!("http://{address}");

    // TLS proxies preserve Host while forwarding to the private HTTP listener.
    for host in ["manager.vpn.example:443", "192.168.1.20"] {
        let invitation = owner
            .send(
                "POST",
                "/api/nodes/enrollments",
                json!({ "name": "Direct node", "managerUrl": format!("https://{host}") }),
            )
            .await
            .1;
        let response = client
            .post(format!("{base}{ENROLL}"))
            .header("host", host)
            .json(&enrollment(
                &invitation,
                "Direct node",
                &capabilities(true, 2, 4096, 32768),
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let identity: Value = response.json().await.unwrap();
        let token = identity["token"].as_str().unwrap();

        for credential in [None, Some("invalid"), Some(token)] {
            let mut request = client
                .post(format!("{base}{HEARTBEAT}"))
                .header("host", host)
                .json(&json!({}));
            if let Some(credential) = credential {
                request = request.bearer_auth(credential);
            }
            let expected = if credential == Some(token) {
                StatusCode::OK
            } else {
                StatusCode::UNAUTHORIZED
            };
            assert_eq!(request.send().await.unwrap().status(), expected);
        }

        let node = identity["nodeId"].as_str().unwrap();
        assert_eq!(
            owner
                .send("POST", &format!("/api/nodes/{node}/revoke"), json!({}))
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            client
                .post(format!("{base}{HEARTBEAT}"))
                .header("host", host)
                .bearer_auth(token)
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(format!("{base}/api/nodes"))
                .header("host", host)
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    server.abort();
}

#[tokio::test]
async fn enrollment_rejects_insecure_or_non_origin_manager_addresses() {
    let owner = Owner::new().await;
    for origin in [
        "http://192.168.1.20:4310",
        "http://manager.vpn.example",
        "https://user:password@manager.vpn.example",
        "https://manager.vpn.example/path",
        "https://manager.vpn.example?query=1",
        "https://manager.vpn.example#fragment",
        "",
    ] {
        let (status, _) = owner
            .send(
                "POST",
                "/api/nodes/enrollments",
                json!({ "name": "Direct node", "managerUrl": origin }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{origin}");
    }
    let (status, invitation) = owner
        .send(
            "POST",
            "/api/nodes/enrollments",
            json!({ "name": "Local test node", "managerUrl": "http://127.0.0.1:4310" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(invitation["managerUrl"], "http://127.0.0.1:4310");
}

#[tokio::test]
async fn node_installer_cannot_hide_a_different_download_origin_in_its_url() {
    for (configured, fallback) in [
        ("http://localhost:4310", "http://localhost:4310/"),
        ("http://manager:4310", ""),
    ] {
        let owner = Owner::new().await;
        let (listener, address) = common::bind().await;
        drop(listener);
        let mut config = owner.service.config.clone();
        config.port = address.port();
        config.public_url = configured.into();
        let config_file = owner.root().join("manager-config.json");
        std::fs::write(&config_file, serde_json::to_vec(&config).unwrap()).unwrap();
        let mut manager = tokio::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
            .env("CAIRN_CONFIG", &config_file)
            .env(
                "CAIRN_NODE_IMAGE",
                format!("registry.example/cairn@sha256:{}", "1".repeat(64)),
            )
            .env_remove("CAIRN_BEACON_ORIGIN")
            .env_remove("CAIRN_INSTALLATION_CLAIM_CODE")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let client = reqwest::Client::new();
        let base = format!("http://{address}");
        common::eventually(
            Duration::from_secs(10),
            Duration::from_millis(20),
            async || {
                client
                    .get(format!("{base}/health"))
                    .send()
                    .await
                    .ok()
                    .filter(|response| response.status().is_success())
            },
        )
        .await;

        let response = client
            .get(format!(
                "{base}/internal/nodes/install.sh?managerUrl=https%3A%2F%2Fattacker.example"
            ))
            .header("host", "manager.vpn.example")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = client
            .get(format!("{base}/internal/nodes/install.sh"))
            .header("host", "manager.vpn.example")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let script = response.text().await.unwrap();
        assert!(script.contains(&format!("CAIRN_MASTER=${{1:-'{fallback}'}}")));
        assert!(!script.contains("attacker.example"));

        // Validate the served installer itself, before privileged host changes
        // or downloading the supervisor from an argument supplied by the user.
        let script_path = owner.root().join("install.sh");
        std::fs::write(&script_path, script).unwrap();
        for unsafe_origin in [
            "http://192.168.1.20:4310",
            "https://user:password@manager.vpn.example",
            "https://manager.vpn.example/path",
        ] {
            let output = tokio::process::Command::new("bash")
                .arg(&script_path)
                .arg(unsafe_origin)
                .output()
                .await
                .unwrap();
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("Use an HTTPS manager origin")
            );
        }
        manager.kill().await.unwrap();
    }
}

#[tokio::test]
async fn node_configuration_validates_capacity_and_never_grants_agent_access() {
    let owner = Owner::new().await;
    let invitation = owner.invite("Small node").await;
    let input = enrollment(&invitation, "ignored", &capabilities(true, 8, 8192, 65536));
    let (_, identity) = owner.call("POST", ENROLL, input, None).await;
    let node = identity["nodeId"].as_str().unwrap();
    let path = format!("/api/nodes/{node}");
    let config = json!({
        "name": "My node",
        "tags": ["fast"],
        "accepting": true,
        "limits": limits(4, 4096, 32768),
    });
    let (status, saved) = owner.send("PUT", &path, config.clone()).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let mut invalid = config;
    invalid["limits"]["cpu"] = 9.into();
    assert_eq!(
        owner.send("PUT", &path, invalid).await.0,
        StatusCode::BAD_REQUEST
    );
    let (_, beat) = owner
        .call("POST", HEARTBEAT, json!({}), identity["token"].as_str())
        .await;
    assert_eq!(beat["limits"]["cpu"], 4);
    let (_, agents) = owner.get("/api/agents").await;
    for agent in agents.as_array().unwrap() {
        assert_eq!(agent["access"]["nodes"], json!([LOCAL_NODE_ID]));
    }
}

#[tokio::test]
async fn disk_budget_uses_total_capacity_while_free_space_is_already_allocated() {
    let owner = Owner::new().await;
    let invitation = owner.invite("Allocated disk").await;
    let mut detected = capabilities(true, 8, 8192, 8192);
    detected["diskTotalMiB"] = 65536.into();
    let input = enrollment(&invitation, "ignored", &detected);
    let (_, identity) = owner.call("POST", ENROLL, input, None).await;
    let path = format!("/api/nodes/{}", identity["nodeId"].as_str().unwrap());
    let mut config = json!({
        "name": "Allocated disk",
        "tags": [],
        "accepting": true,
        "limits": limits(4, 4096, 32768),
    });
    let (status, saved) = owner.send("PUT", &path, config.clone()).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["limits"]["diskMiB"], 32768);

    config["limits"]["diskMiB"] = 65537.into();
    assert_eq!(
        owner.send("PUT", &path, config).await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn main_node_policy_survives_an_update_from_an_older_client() {
    let owner = Owner::new().await;
    let path = format!("/api/agents/{MAIN_AGENT_ID}");
    let restricted = json!({ "name": "Main", "access": { "nodes": [] } });
    let (status, restricted) = owner.send("PUT", &path, restricted).await;
    assert_eq!(status, StatusCode::OK, "{restricted}");
    let older_client = json!({
        "name": "Renamed main",
        "access": { "projects": null, "skills": null, "mcps": null, "github": true },
    });
    let (status, saved) = owner.send("PUT", &path, older_client).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["access"]["nodes"], json!([]));
    let unknown_node = json!({
        "name": "Main",
        "access": { "nodes": ["10000000-0000-4000-8000-000000000000"] },
    });
    let (status, _) = owner.send("PUT", &path, unknown_node).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn concurrent_enrollment_has_exactly_one_winner() {
    let owner = Owner::new().await;
    let invitation = owner.invite("Race").await;
    let input = enrollment(&invitation, "Race", &capabilities(true, 2, 4096, 32768));
    let (first, second) = tokio::join!(
        owner.call("POST", ENROLL, input.clone(), None),
        owner.call("POST", ENROLL, input, None)
    );
    let mut statuses = [first.0, second.0];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::UNAUTHORIZED]);
}

#[tokio::test]
async fn connector_enrolls_over_http_without_printing_or_exposing_its_token() {
    let (listener, address) = common::bind().await;
    let host = address.to_string();
    let owner = Owner::at(host.clone()).await;
    let server = owner.serve(listener);
    let invitation = owner.invite("Linux connector").await;
    let state = owner.root().join("node");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
        .args([
            "node-enroll",
            &format!("http://{host}"),
            state.to_str().unwrap(),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(invitation["code"].as_str().unwrap().as_bytes())
        .await
        .unwrap();
    let output = child.wait_with_output().await.unwrap();
    server.abort();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let identity_file = state.join("identity.json");
    let identity: Value = serde_json::from_slice(&std::fs::read(&identity_file).unwrap()).unwrap();
    let token = identity["token"].as_str().unwrap();
    assert!(!stdout.contains(token));
    assert!(!stderr.contains(token));
    assert_eq!(mode(&identity_file), 0o600);
    let (_, nodes) = owner.get("/api/nodes").await;
    assert!(
        nodes
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["name"] == "Linux connector" && node["capabilities"]["os"] == "linux")
    );
}

#[tokio::test]
async fn a_task_cannot_use_the_local_runner_without_node_permission() {
    let owner = Owner::new().await;
    let (status, _) = owner
        .send(
            "PUT",
            &format!("/api/agents/{MAIN_AGENT_ID}"),
            json!({ "name": "Main", "access": { "nodes": [] } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let task = json!({
        "name": "Restricted node task",
        "prompt": "Do nothing",
        "agentId": MAIN_AGENT_ID,
        "enabled": false,
    });
    let (status, task) = owner.send("POST", "/api/tasks", task).await;
    assert_eq!(status, StatusCode::OK, "{task}");
    let path = format!("/api/tasks/{}/run", task["id"].as_str().unwrap());
    let (status, error) = owner.send("POST", &path, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{error}");
}

#[tokio::test]
async fn node_credentials_cannot_administer_nodes_and_owner_sessions_cannot_impersonate_a_node() {
    let owner = Owner::new().await;
    let identity = owner
        .enroll("Scoped", &capabilities(false, 2, 4096, 32768))
        .await;
    let request = owner
        .request("GET", "/api/nodes")
        .header("authorization", bearer(identity["token"].as_str().unwrap()))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        common::send(&owner.app, request).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        owner.call("POST", HEARTBEAT, json!({}), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        owner
            .call("POST", HEARTBEAT, json!({}), Some("not-a-node-token"))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn revocation_removes_node_grants_without_blocking_later_agent_edits() {
    let owner = Owner::new().await;
    let identity = owner
        .enroll("Revocation", &capabilities(true, 2, 4096, 32768))
        .await;
    let node = identity["nodeId"].as_str().unwrap();
    let main = format!("/api/agents/{MAIN_AGENT_ID}");
    let grant = json!({ "name": "Main", "access": { "nodes": [LOCAL_NODE_ID, node] } });
    assert_eq!(owner.send("PUT", &main, grant).await.0, StatusCode::OK);
    let (status, _) = owner
        .send("POST", &format!("/api/nodes/{node}/revoke"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, agents) = owner.get("/api/agents").await;
    let mut main_agent = find_by_id(&agents, MAIN_AGENT_ID);
    assert_eq!(main_agent["access"]["nodes"], json!([LOCAL_NODE_ID]));
    main_agent["name"] = "Renamed after revocation".into();
    assert_eq!(owner.send("PUT", &main, main_agent).await.0, StatusCode::OK);
}

#[test]
fn mcp_agent_updates_preserve_omitted_node_permissions() {
    let update = parse(
        "mcp:update_agent",
        json!({ "id": MAIN_AGENT_ID, "agent": { "access": { "mcps": [] } } }),
    )
    .unwrap();
    assert!(update["agent"]["access"].get("nodes").is_none());
    let restricted = parse(
        "mcp:update_agent",
        json!({ "id": MAIN_AGENT_ID, "agent": { "access": { "nodes": [] } } }),
    )
    .unwrap();
    assert_eq!(restricted["agent"]["access"]["nodes"], json!([]));
    let created = parse("mcp:save_agent", json!({ "name": "Default" })).unwrap();
    assert_eq!(created["access"]["nodes"], json!([LOCAL_NODE_ID]));
}

#[tokio::test]
async fn prepared_node_disks_use_the_enrolled_manager_origin() {
    let owner = Owner::new().await;
    let identity = owner
        .enroll("VPN node", &capabilities(true, 4, 8192, 65536))
        .await;
    let node_id = identity["nodeId"].as_str().unwrap();
    let (run, attempt) = (id(), id());
    owner.grant_nodes(MAIN_AGENT_ID, json!([node_id])).await;
    owner
        .put(
            "node-attempts",
            json!({
                "id": attempt,
                "runId": run,
                "nodeId": node_id,
                "released": false,
            }),
        )
        .await;
    let mut record = run_record(&run, RunStatus::Running);
    record["snapshot"] = json!({ "agent": { "id": MAIN_AGENT_ID } });
    owner.add_run(&record).await;
    owner
        .set_checkpoint(&run, json!({ "nodeId": node_id, "runnerId": attempt }))
        .await;
    let data = &owner.service.config.data_dir;
    std::fs::create_dir_all(data.join("runs").join(&run)).unwrap();
    std::fs::create_dir_all(data.join("runner-plans")).unwrap();
    let plan_path = data.join("runner-plans").join(format!("{attempt}.json"));
    std::fs::write(
        &plan_path,
        json!({
            "id": attempt,
            "runId": run,
            "chat": { "provider": "codex" },
            "storage": {
                "master": owner.service.config.public_url,
                "grant": "disk-grant-fixture",
            },
        })
        .to_string(),
    )
    .unwrap();

    // The controller consumes the prepared plan through its existing start
    // contract; this fixture returns the disk configuration it would mount.
    let controller = Router::new()
        .route("/health", axum::routing::get(async || Json(json!({}))))
        .route(
            "/runs/{id}/lease",
            axum::routing::post(async || Json(json!({}))),
        )
        .route(
            "/runs/{id}",
            axum::routing::post(move || {
                let plan_path = plan_path.clone();
                async move {
                    let plan: Value =
                        serde_json::from_slice(&tokio::fs::read(plan_path).await.unwrap()).unwrap();
                    Json(plan["storage"].clone())
                }
            }),
        );
    let (runner, runner_server) = common::serve_locally(controller).await;
    let (listener, address) = common::bind().await;
    let manager_url = format!("http://{address}/");
    let manager = owner.serve(listener);
    let node_directory = owner.root().join("node");
    std::fs::create_dir(&node_directory).unwrap();
    std::fs::write(
        node_directory.join("identity.json"),
        json!({
            "master": manager_url,
            "nodeId": node_id,
            "token": identity["token"],
        })
        .to_string(),
    )
    .unwrap();
    let mut connector = tokio::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
        .args(["node-connect", node_directory.to_str().unwrap()])
        .env("DATA_DIR", data)
        .env("RUNNER_URL", runner)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let prepared = tokio::time::timeout(
        Duration::from_secs(10),
        owner.service.node_transport.request(
            node_id,
            "POST",
            &format!("/prepare/{attempt}"),
            vec![],
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(prepared.status(), StatusCode::OK);
    let response = owner
        .service
        .node_transport
        .request(node_id, "POST", &format!("/runs/{attempt}"), b"{}".to_vec())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let storage: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(storage["master"], manager_url);
    assert_eq!(storage["grant"], "disk-grant-fixture");

    connector.kill().await.unwrap();
    manager.abort();
    runner_server.abort();
}

#[tokio::test]
async fn remote_disk_restores_use_the_nodes_direct_manager_origin() {
    let owner = Owner::new().await;
    let identity = owner
        .enroll("VPN node", &capabilities(true, 4, 8192, 65536))
        .await;
    let (listener, address) = common::bind().await;
    let manager_url = format!("http://{address}/");
    let manager = owner.serve(listener);
    let controller = Router::new().route(
        "/disks/{id}/restore",
        axum::routing::post(async |Json(body): Json<Value>| Json(body)),
    );
    let (runner, runner_server) = common::serve_locally(controller).await;
    let stop = CancellationToken::new();
    let node = tokio::spawn(relay::run(
        manager_url.parse().unwrap(),
        identity["token"].as_str().unwrap().into(),
        runner,
        "controller-fixture".into(),
        stop.clone(),
    ));
    let response = owner
        .service
        .node_transport
        .request(
            identity["nodeId"].as_str().unwrap(),
            "POST",
            &format!("/disks/{}/restore", id()),
            serde_json::to_vec(&json!({
                "master": owner.service.config.public_url,
                "grant": "disk-grant-fixture",
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let restored: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(restored["master"], manager_url);
    assert_eq!(restored["grant"], "disk-grant-fixture");
    stop.cancel();
    node.await.unwrap().unwrap();
    manager.abort();
    runner_server.abort();
}

#[tokio::test]
async fn outbound_transport_streams_only_to_the_authenticated_node() {
    let owner = Owner::new().await;
    let identity = owner
        .enroll("Worker", &capabilities(true, 4, 8192, 65536))
        .await;
    let credential =
        cairn_installation::execution::secret(&owner.root().join("data"), "runner-secret")
            .await
            .unwrap();
    let request = owner
        .request(
            "GET",
            &format!(
                "/internal/execution/{}/health",
                identity["nodeId"].as_str().unwrap()
            ),
        )
        .header("authorization", bearer(&credential))
        .body(Body::empty())
        .unwrap();
    let app = owner.app.clone();
    let waiting = tokio::spawn(async move {
        let response = common::send(&app, request).await;
        let status = response.status();
        (status, to_bytes(response.into_body(), 1024).await.unwrap())
    });
    let token = identity["token"].as_str().unwrap();
    let (status, command) = owner
        .call("POST", "/internal/nodes/poll", json!({}), Some(token))
        .await;
    assert_eq!(status, StatusCode::OK, "{command}");
    assert_eq!(command["path"], "/health");
    let reply = json!({ "id": command["id"], "status": 200, "data": "b2s=", "done": true });
    let forged = "x".repeat(43);
    assert_eq!(
        owner
            .call("POST", REPLY, reply.clone(), Some(&forged))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        owner.call("POST", REPLY, reply, Some(token)).await.0,
        StatusCode::OK
    );
    let (status, bytes) = waiting.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&bytes[..], b"ok");
}

#[tokio::test]
async fn node_responses_keep_their_length_when_a_proxy_asks_for_compression() {
    // Remote artifact exports reach the manager through its public origin, where a proxy may add
    // Accept-Encoding. Publication requires the declared length, so it must survive.
    let owner = Owner::new().await;
    let identity = owner
        .enroll("Worker", &capabilities(true, 4, 8192, 65536))
        .await;
    let credential =
        cairn_installation::execution::secret(&owner.root().join("data"), "runner-secret")
            .await
            .unwrap();

    let request = owner
        .request(
            "POST",
            &format!(
                "/internal/execution/{}/runs/{}/artifact",
                identity["nodeId"].as_str().unwrap(),
                id()
            ),
        )
        .header("authorization", bearer(&credential))
        .header("accept-encoding", "gzip, br")
        .body(Body::from(
            json!({ "runId": id(), "path": "/tmp/report.md" }).to_string(),
        ))
        .unwrap();
    let app = owner.app.clone();
    let waiting = tokio::spawn(async move { common::send(&app, request).await });

    let token = identity["token"].as_str().unwrap();
    let (status, command) = owner
        .call("POST", "/internal/nodes/poll", json!({}), Some(token))
        .await;
    assert_eq!(status, StatusCode::OK, "{command}");
    assert!(command["path"].as_str().unwrap().ends_with("/artifact"));

    // Like the runner's export, the reply declares no content type: an image type would skip
    // compression and hide the bug. 100 bytes exceed the 32-byte compression minimum.
    let file = vec![b'x'; 100];
    let reply = json!({
        "id": command["id"],
        "status": 200,
        "length": file.len(),
        "data": STANDARD.encode(&file),
        "done": true
    });
    assert_eq!(
        owner.call("POST", REPLY, reply, Some(token)).await.0,
        StatusCode::OK
    );

    let response = waiting.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-encoding"), None);
    assert_eq!(response.headers()["content-length"], "100");
    assert_eq!(to_bytes(response.into_body(), 1024).await.unwrap(), file);

    // Browser API responses stay compressed.
    let api = owner
        .session
        .authorize(owner.request("GET", "/api/agents"))
        .header("accept-encoding", "gzip, br")
        .body(Body::empty())
        .unwrap();
    let response = common::send(&owner.app, api).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("content-encoding"));
}

#[tokio::test]
async fn workspace_transfer_preserves_files_and_links_without_following_them() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    let tool = b"#!/bin/sh\nexit 0\n";
    std::fs::create_dir_all(source.join("nested")).unwrap();
    std::fs::write(source.join("nested/tool"), tool).unwrap();
    std::fs::set_permissions(
        source.join("nested/tool"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::os::unix::fs::symlink("/etc", source.join("external")).unwrap();
    let mut bytes = Vec::new();
    files::send(&source, &mut bytes).await.unwrap();
    files::receive(&mut bytes.as_slice(), &target, 1024)
        .await
        .unwrap();
    assert_eq!(std::fs::read(target.join("nested/tool")).unwrap(), tool);
    assert_eq!(
        std::fs::read_link(target.join("external")).unwrap(),
        Path::new("/etc")
    );
    assert_eq!(mode(&target.join("nested/tool")), 0o755);
    let malicious =
        b"{\"path\":\"external/passwd\",\"kind\":\"file\",\"size\":0}\n{\"complete\":true}\n";
    assert!(
        files::receive(&mut malicious.as_slice(), &target, 1024)
            .await
            .is_err()
    );
    let incomplete = root.path().join("incomplete");
    assert!(
        files::receive(&mut &bytes[..bytes.len() - 20], &incomplete, 1024)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn binary_node_responses_are_scoped_and_never_hide_truncation() {
    let owner = Owner::new().await;
    let (node, token, stranger, stranger_token) = (id(), auth::token(), id(), auth::token());
    owner.register_node(&node, &token).await;
    owner.register_node(&stranger, &stranger_token).await;
    for (broken, unknown_length) in [(false, false), (true, false), (true, true)] {
        let (hub, target) = (owner.service.node_transport.clone(), node.clone());
        let waiting = tokio::spawn(async move {
            let response = hub
                .request(&target, "GET", "/health", vec![])
                .await
                .unwrap();
            to_bytes(response.into_body(), MIB).await
        });
        let command = owner.service.node_transport.poll(&node).await.unwrap();
        assert_eq!(
            command["streamBody"], true,
            "Master must advertise continuous body support"
        );
        let call = command["id"].as_str().unwrap();
        let length = if unknown_length {
            Value::Null
        } else {
            json!(262_144)
        };
        let head = json!({ "id": call, "sequence": 0, "status": 200, "length": length });
        assert_eq!(
            owner.call("POST", REPLY, head, Some(&token)).await.0,
            StatusCode::OK
        );
        let stream = |token: &str, body: Body| {
            owner
                .request("POST", &format!("/internal/nodes/stream/{call}"))
                .header("authorization", bearer(token))
                .body(body)
                .unwrap()
        };
        // Even another valid node identity cannot write this response.
        assert_eq!(
            common::send(&owner.app, stream(&stranger_token, Body::empty()))
                .await
                .status(),
            StatusCode::CONFLICT
        );
        let bytes = vec![53u8; if broken { 1024 } else { 262_144 }];
        let body = if unknown_length {
            Body::from_stream(futures_util::stream::iter([
                Ok(bytes::Bytes::copy_from_slice(&bytes)),
                Err(std::io::Error::other("fixture connection lost")),
            ]))
        } else {
            Body::from(bytes.clone())
        };
        let response = common::send(&owner.app, stream(&token, body)).await;
        assert_eq!(response.status().is_success(), !broken);
        let result = tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap();
        if broken {
            assert!(result.is_err(), "Truncation must fail the manager reader");
        } else {
            assert_eq!(result.unwrap(), bytes);
        }
        assert_eq!(
            common::send(&owner.app, stream(&token, Body::empty()))
                .await
                .status(),
            StatusCode::CONFLICT
        );
    }
}

#[tokio::test]
async fn bulk_stream_backpressure_and_reader_cancellation_bound_the_upload() {
    let owner = Owner::new().await;
    let (node, token) = (id(), auth::token());
    owner.put("nodes", json!({ "id": node })).await;
    owner.authorize_node(&node, &token).await;
    let (hub, target) = (owner.service.node_transport.clone(), node.clone());
    let waiting = tokio::spawn(async move {
        hub.request(&target, "GET", "/health", vec![])
            .await
            .unwrap()
    });
    let call = owner.service.node_transport.poll(&node).await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let head = json!({ "id": call, "sequence": 0, "status": 200 });
    assert_eq!(
        owner.call("POST", REPLY, head, Some(&token)).await.0,
        StatusCode::OK
    );
    let response = waiting.await.unwrap();
    let produced = Arc::new(AtomicUsize::new(0));
    let counter = produced.clone();
    let data = bytes::Bytes::from(vec![17u8; 65536]);
    let stream = futures_util::stream::unfold(0, move |count| {
        let (counter, data) = (counter.clone(), data.clone());
        async move {
            if count == 512 {
                return None;
            }
            counter.fetch_add(1, Ordering::SeqCst);
            Some((Ok::<_, std::io::Error>(data), count + 1))
        }
    });
    let request = owner
        .request("POST", &format!("/internal/nodes/stream/{call}"))
        .header("authorization", bearer(&token))
        .body(Body::from_stream(stream))
        .unwrap();
    let app = owner.app.clone();
    let upload = tokio::spawn(async move { common::send(&app, request).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while produced.load(Ordering::SeqCst) < 9 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        produced.load(Ordering::SeqCst),
        9,
        "Only eight queued frames plus the pending frame may be read"
    );
    drop(response);
    let upload = tokio::time::timeout(Duration::from_secs(2), upload)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(upload.status(), StatusCode::CONFLICT);
    assert!(
        owner
            .service
            .node_transport
            .reply(&node, json!({ "id": call, "sequence": 1, "done": true }))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn concurrent_admission_reserves_capacity_once_and_preserves_agent_grants() {
    let owner = Owner::new().await;
    let (node, agent) = (id(), id());
    let mut record = schedulable_node(&node, &limits(2, 4096, 65536));
    record["local"] = false.into();
    record["slots"] = 1.into();
    owner.put("nodes", record).await;
    owner.grant_nodes(&agent, json!([node])).await;
    let a = agent_run(&id(), &agent);
    let b = agent_run(&id(), &agent);
    let (attempt_a, attempt_b) = (id(), id());
    let (first, second) = tokio::join!(
        placement::reserve(&owner.service, &a, &attempt_a),
        placement::reserve(&owner.service, &b, &attempt_b)
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let (winner, loser) = if first.is_ok() {
        (&attempt_a, &b)
    } else {
        (&attempt_b, &a)
    };
    placement::release(&owner.service, winner).await.unwrap();
    assert_eq!(
        placement::reserve(&owner.service, loser, &id())
            .await
            .unwrap()["nodeId"],
        node
    );
    owner.grant_nodes(&agent, json!([])).await;
    let mut local = agent_run(&id(), &agent);
    local["preferredNodeId"] = LOCAL_NODE_ID.into();
    assert!(
        placement::reserve(&owner.service, &local, &id())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn queued_automatic_conversations_reserve_another_node_without_discarding_the_source() {
    let owner = Owner::new().await;
    let (run, agent, source, destination) = (id(), id(), id(), id());
    for (node, cpu) in [(&source, 1), (&destination, 12)] {
        let mut record = schedulable_node(node, &limits(cpu, 32768, 131072));
        record["runtimes"] = json!(["fixture"]);
        if node == &source {
            record["pressure"] = "memory".into();
        }
        owner.put("nodes", record).await;
    }
    owner
        .grant_nodes(&agent, json!([source, destination]))
        .await;
    let mut record = run_record(&run, RunStatus::Queued);
    record["snapshot"] = json!({ "agent": { "id": agent } });
    record["resources"] = limits(4, 12288, 65536);
    record["nodeId"] = source.clone().into();
    owner.add_run(&record).await;
    owner
        .set_checkpoint(&run, json!({ "nodeId": source, "runtimeId": "fixture" }))
        .await;
    assert!(placement::is_no_capacity(
        &placement::check(&owner.service, &record).await.unwrap_err()
    ));
    assert!(
        moves::queue_capacity_move(&owner.service, &record)
            .await
            .unwrap()
    );
    let pending = owner.run(&run).await;
    assert_eq!(
        pending["nodeId"], source,
        "source stays authoritative until transfer completes"
    );
    assert_eq!(pending["moveRequest"]["nodeId"], destination);
    assert_eq!(
        pending["moveRequest"]["automatic"], false,
        "capture the latest source disk, never use a stale backup"
    );
    assert!(pending["moveRequest"]["backupId"].is_null());
    assert_eq!(pending["recoveryPending"], true);
    let reservation = pending["moveRequest"]["reservation"].as_str().unwrap();
    assert_eq!(
        owner.stored("node-attempts", reservation).await.unwrap()["released"],
        false
    );
    assert!(
        !moves::queue_capacity_move(&owner.service, &pending)
            .await
            .unwrap()
    );
    let mut cancelled = pending;
    cancelled["cancelRequestedAt"] = now().into();
    assert!(moves::advance(&owner.service, &cancelled).await.unwrap());
}

#[tokio::test]
async fn capacity_moves_keep_pinned_unauthorized_and_running_conversations_in_place() {
    let owner = Owner::new().await;
    let (run, agent, source, destination) = (id(), id(), id(), id());
    owner
        .put(
            "nodes",
            schedulable_node(&source, &limits(1, 32768, 131072)),
        )
        .await;
    owner
        .put(
            "nodes",
            schedulable_node(&destination, &limits(12, 32768, 131072)),
        )
        .await;
    owner.grant_nodes(&agent, json!([source])).await;
    let mut record = run_record(&run, RunStatus::Queued);
    record["snapshot"] = json!({ "agent": { "id": agent } });
    record["resources"] = limits(4, 12288, 65536);
    owner.add_run(&record).await;
    owner
        .set_checkpoint(&run, json!({ "nodeId": source }))
        .await;
    assert!(
        !moves::queue_capacity_move(&owner.service, &record)
            .await
            .unwrap()
    );
    owner
        .grant_nodes(&agent, json!([source, destination]))
        .await;
    record["pinnedNodeId"] = source.clone().into();
    assert!(
        !moves::queue_capacity_move(&owner.service, &record)
            .await
            .unwrap()
    );
    record["pinnedNodeId"] = Value::Null;
    record["status"] = RunStatus::Running.into();
    assert!(
        !moves::queue_capacity_move(&owner.service, &record)
            .await
            .unwrap()
    );
    assert!(
        owner
            .service
            .store
            .list("node-attempts")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn retained_s3_disks_charge_local_cache_and_cancellation_releases_destination() {
    let owner = Owner::new().await;
    let (node, agent, run, attempt) = (id(), id(), id(), id());
    owner
        .put("nodes", schedulable_node(&node, &limits(4, 8192, 32768)))
        .await;
    owner.grant_nodes(&agent, json!([node])).await;
    let execution = agent_run(&run, &agent);
    placement::reserve(&owner.service, &execution, &attempt)
        .await
        .unwrap();
    placement::materialize(&owner.service, &attempt)
        .await
        .unwrap();
    placement::release(&owner.service, &attempt).await.unwrap();
    let retained = owner
        .stored("node-volumes", &format!("{run}:{node}"))
        .await
        .unwrap();
    assert_eq!(retained["diskMiB"], 128);
    let other = id();
    placement::reserve(&owner.service, &agent_run(&id(), &agent), &other)
        .await
        .expect("An idle S3 disk should leave capacity for another conversation");
    placement::release(&owner.service, &other).await.unwrap();
    let retry = id();
    placement::reserve(&owner.service, &execution, &retry)
        .await
        .unwrap();
    placement::release(&owner.service, &retry).await.unwrap();
    let pending = id();
    owner
        .put(
            "node-attempts",
            json!({
                "id": pending,
                "nodeId": node,
                "runId": run,
                "role": "destination",
                "released": false,
            }),
        )
        .await;
    let cancelled = json!({
        "id": run,
        "cancelRequestedAt": now(),
        "moveRequest": { "reservation": pending },
    });
    assert!(moves::advance(&owner.service, &cancelled).await.unwrap());
    assert_eq!(
        owner.stored("node-attempts", &pending).await.unwrap()["released"],
        true
    );
}

#[tokio::test]
async fn explicit_resource_requests_are_rejected_before_moving() {
    let owner = Owner::new().await;
    let run = json!({ "id": id(), "resources": limits(2, 4096, 32768) });
    let error = moves::request(&owner.service, &run, &limits(2, 4096, 128))
        .await
        .unwrap_err();
    assert_eq!(error.status, 400);
    assert!(
        error
            .message
            .contains("resource requests are no longer supported")
    );
}

#[tokio::test]
async fn a_moved_disk_requires_a_node_with_fuse() {
    let owner = Owner::new().await;
    let (agent, local_only, unknown, s3_ready) = (id(), id(), id(), id());
    for (node, fuse) in [(&local_only, false), (&s3_ready, true)] {
        let mut record = schedulable_node(node, &limits(4, 8192, 32768));
        record["storage"] = json!({ "enabled": false, "reserveMiB": 64, "reservePercent": 1 });
        record["capabilities"] = json!({
            "kvm": true,
            "fuse": fuse,
            "diskMiB": 32768,
            "diskTotalMiB": 32768,
        });
        owner.put("nodes", record).await;
    }
    let mut record = schedulable_node(&unknown, &limits(4, 8192, 32768));
    record["capabilities"] = json!({ "kvm": true });
    owner.put("nodes", record).await;
    owner
        .grant_nodes(&agent, json!([local_only, unknown, s3_ready]))
        .await;
    let mut run = agent_run(&id(), &agent);
    run["placementTransition"] = true.into();
    for target in [&local_only, &unknown] {
        run["targetNodeId"] = target.clone().into();
        assert_eq!(
            placement::reserve(&owner.service, &run, &id())
                .await
                .unwrap_err()
                .status,
            503
        );
    }
    run["targetNodeId"] = s3_ready.clone().into();
    assert_eq!(
        placement::reserve(&owner.service, &run, &id())
            .await
            .unwrap()["nodeId"],
        s3_ready
    );
}

#[tokio::test]
async fn recovery_settings_validate_and_preserve_the_last_good_configuration() {
    let owner = Owner::new().await;
    let (status, initial) = owner.get(NODE_SETTINGS).await;
    assert_eq!(status, StatusCode::OK);
    assert!(initial.get("destination").is_none());
    assert!(initial.get("retention").is_none());
    let settings = json!({
        "intervalSeconds": 30,
        "budgetMiB": 4096,
        "disconnectTimeoutSeconds": 60,
        "shutdownTimeoutSeconds": 300,
        "maxCapacityWaitSeconds": 3600,
    });
    assert_eq!(
        owner.send("PUT", NODE_SETTINGS, settings.clone()).await.0,
        StatusCode::OK
    );
    let mut bad = settings.clone();
    bad["intervalSeconds"] = 0.into();
    assert_eq!(
        owner.send("PUT", NODE_SETTINGS, bad).await.0,
        StatusCode::BAD_REQUEST
    );
    // The S3 status is reported alongside the settings but never stored.
    let mut expected = settings;
    expected["s3Configured"] = false.into();
    assert_eq!(owner.get(NODE_SETTINGS).await.1, expected);
}

#[tokio::test]
async fn tags_filter_authorized_nodes_without_granting_access() {
    let owner = Owner::new().await;
    let (fast, other, agent) = (id(), id(), id());
    for (node, tags) in [(&fast, json!(["fast"])), (&other, json!(["slow"]))] {
        let mut record = schedulable_node(node, &limits(4, 8192, 65536));
        record["tags"] = tags;
        owner.put("nodes", record).await;
    }
    owner.grant_nodes(&agent, json!([other])).await;
    let mut run = agent_run(&id(), &agent);
    run["requiredTags"] = json!(["fast"]);
    assert!(
        placement::reserve(&owner.service, &run, &id())
            .await
            .is_err()
    );
    owner.grant_nodes(&agent, json!([fast, other])).await;
    assert_eq!(
        placement::reserve(&owner.service, &run, &id())
            .await
            .unwrap()["nodeId"],
        fast
    );
}

#[tokio::test]
async fn a_remote_only_agent_prepares_a_private_vm_without_a_local_controller() {
    let owner = Owner::new().await;
    common::managed_codex_home(&owner.service.config.home);
    let run = json!({
        "id": "e2000000-0000-4000-8000-000000000001",
        "snapshot": {
            "agent": {
                "id": MAIN_AGENT_ID,
                "access": { "nodes": ["e2000000-0000-4000-8000-000000000002"] },
            },
            "projects": [],
            "skills": [],
            "task": {},
        },
    });
    owner.put("agents", run["snapshot"]["agent"].clone()).await;
    let prepared =
        cairn_installation::execution::prepare(&run, &owner.service.config, None, None, None)
            .await
            .unwrap();
    assert_eq!(prepared["backend"], "firecracker");
    assert_eq!(prepared["isolated"], true);
    let placement =
        placement::reserve(&owner.service, &run, "e2000000-0000-4000-8000-000000000003").await;
    assert!(
        placement.is_err(),
        "An unavailable VM must never fall back to the shared host"
    );
}

#[tokio::test]
async fn owner_placement_obeys_agent_grants_and_preserves_last_good_selection() {
    let owner = Owner::new().await;
    let online = id();
    let offline = id();
    let revoked = id();
    let denied = id();
    for (node, local, last_seen, is_revoked) in [
        (LOCAL_NODE_ID, true, 0, false),
        (online.as_str(), false, now(), false),
        (offline.as_str(), false, 0, false),
        (revoked.as_str(), false, now(), true),
        (denied.as_str(), false, now(), false),
    ] {
        owner
            .put(
                "nodes",
                json!({
                    "id": node,
                    "name": "Placement fixture",
                    "local": local,
                    "lastSeen": last_seen,
                    "revoked": is_revoked,
                }),
            )
            .await;
    }
    owner
        .grant_nodes(
            MAIN_AGENT_ID,
            json!([LOCAL_NODE_ID, online, offline, revoked]),
        )
        .await;

    let assert_statuses = |value: &Value| {
        let nodes = value["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 3);
        for (node, expected) in [
            (LOCAL_NODE_ID, "local"),
            (online.as_str(), "online"),
            (offline.as_str(), "offline"),
        ] {
            let listed = nodes.iter().find(|n| n["id"] == node).unwrap();
            assert_eq!(listed["status"], expected);
        }
    };

    let run = id();
    let mut record = run_record(&run, RunStatus::Succeeded);
    record["snapshot"] = json!({ "agent": { "id": MAIN_AGENT_ID } });
    owner.add_run(&record).await;
    let path = format!("/api/nodes/placement/{run}");
    let pin = |node: Value| json!({ "pinnedNodeId": node, "preferredNodeId": null });
    let (status, value) = owner.send("PUT", &path, pin(LOCAL_NODE_ID.into())).await;
    assert_eq!(status, StatusCode::OK);
    assert_statuses(&value);
    let (status, _) = owner.send("PUT", &path, pin(id().into())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, value) = owner.get(&path).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["pinnedNodeId"], LOCAL_NODE_ID);
    assert_statuses(&value);
    let prefer = json!({ "pinnedNodeId": null, "preferredNodeId": LOCAL_NODE_ID });
    let (status, _) = owner.send("PUT", &path, prefer).await;
    assert_eq!(status, StatusCode::OK);
    let (_, value) = owner.get(&path).await;
    assert!(value["pinnedNodeId"].is_null());
    assert_eq!(value["preferredNodeId"], LOCAL_NODE_ID);
}

/// Sends a refresh request through the relayed authentication socket.
async fn refresh_through(socket: &Path) -> Option<Value> {
    let mut stream = BufReader::new(UnixStream::connect(socket).await.unwrap());
    wire::write(stream.get_mut(), &json!({ "method": "refresh" }))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), wire::read(&mut stream))
        .await
        .unwrap()
        .unwrap()
}

/// Answers two refresh requests on the run's native authentication socket.
fn serve_native_refreshes(provider: UnixListener) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        for generation in 1..=2 {
            let (stream, _) = provider.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            assert_eq!(
                wire::read(&mut stream).await.unwrap().unwrap()["method"],
                "refresh"
            );
            wire::write(
                stream.get_mut(),
                &json!({ "accessToken": format!("synthetic-{generation}") }),
            )
            .await
            .unwrap();
        }
    })
}

#[tokio::test]
async fn native_auth_relays_refresh_both_protocols_and_stop_after_grant_revocation() {
    for claude in [false, true] {
        let (listener, address) = common::bind().await;
        let owner = Owner::at(address.to_string()).await;
        let (node, attempt, run, token) = (id(), id(), id(), auth::token());
        owner.register_node(&node, &token).await;
        owner.grant_nodes(MAIN_AGENT_ID, json!([node])).await;
        owner
            .put(
                "node-attempts",
                json!({ "id": attempt, "runId": run, "nodeId": node, "released": false }),
            )
            .await;
        let mut record = run_record(&run, RunStatus::Running);
        record["snapshot"] = json!({ "agent": { "id": MAIN_AGENT_ID } });
        owner.add_run(&record).await;
        owner
            .set_checkpoint(&run, json!({ "nodeId": node, "runnerId": attempt }))
            .await;
        let plans = owner.service.config.data_dir.join("runner-plans");
        std::fs::create_dir_all(&plans).unwrap();
        let plan = json!({
            "runId": run,
            "chat": {
                "provider": if claude { "claude" } else { "codex" },
                "claudeManagedAuth": claude,
            },
        });
        std::fs::write(plans.join(format!("{attempt}.json")), plan.to_string()).unwrap();
        let home = owner
            .service
            .config
            .data_dir
            .join("runs")
            .join(&run)
            .join("home")
            .join(if claude { ".claude" } else { ".codex" });
        std::fs::create_dir_all(&home).unwrap();
        let native =
            serve_native_refreshes(UnixListener::bind(home.join("cairn-auth.sock")).unwrap());
        let server = owner.serve(listener);
        let stop = CancellationToken::new();
        let socket = owner.root().join("remote.sock");
        let relay = tokio::spawn(workspace::auth_listener(
            socket.clone(),
            reqwest::Client::new(),
            format!("http://{address}").parse().unwrap(),
            token,
            attempt,
            stop.clone(),
        ));
        wait_for_path(&socket).await;
        for generation in 1..=2 {
            let value = refresh_through(&socket).await.unwrap();
            assert_eq!(value["accessToken"], format!("synthetic-{generation}"));
        }
        native.await.unwrap();
        owner.grant_nodes(MAIN_AGENT_ID, json!([])).await;
        assert!(refresh_through(&socket).await.is_none());
        stop.cancel();
        relay.await.unwrap().unwrap();
        server.abort();
    }
}

/// Sends a JSON-RPC request through a run's MCP channel socket, as the VM
/// controller relays its agent's calls to `http://127.0.0.1:5202`.
async fn mcp_through(
    socket: &Path,
    path: &str,
    bearer: &str,
    method: &str,
    params: Value,
) -> (u16, Value) {
    let client = reqwest::Client::builder()
        .unix_socket(socket)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let response = client
        .post(format!("http://127.0.0.1:5202{path}"))
        .bearer_auth(bearer)
        .header("mcp-protocol-version", "2025-11-25")
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn remote_vm_agents_reach_workspace_tools_only_through_their_node_session() {
    let (listener, address) = common::bind().await;
    // An installation with its local runner and a node enrolled at a private origin.
    let owner = Owner::with_runner(address.to_string(), "http://runner:4311".into()).await;
    let (node, attempt, run, token) = (id(), id(), id(), auth::token());
    owner.register_node(&node, &token).await;
    owner.grant_nodes(MAIN_AGENT_ID, json!([node])).await;
    owner
        .put(
            "node-attempts",
            json!({ "id": attempt, "runId": run, "nodeId": node, "released": false }),
        )
        .await;
    let snapshot = json!({ "agent": { "id": MAIN_AGENT_ID, "access": { "nodes": [node] } } });
    let mut record = run_record(&run, RunStatus::Running);
    record["snapshot"] = snapshot.clone();
    owner.add_run(&record).await;
    owner
        .set_checkpoint(&run, json!({ "nodeId": node, "runnerId": attempt }))
        .await;
    let plans = owner.service.config.data_dir.join("runner-plans");
    std::fs::create_dir_all(&plans).unwrap();
    std::fs::write(
        plans.join(format!("{attempt}.json")),
        json!({ "runId": run }).to_string(),
    )
    .unwrap();
    // An HTTP MCP connection the agent reaches through the manager's gateway.
    let (upstream, upstream_address) = common::bind().await;
    let answer = |Json(body): Json<Value>| async move {
        let result = match body["method"].as_str() {
            Some("server/discover") => json!({
                "supportedVersions": [cairn_installation::mcp_client::MODERN],
                "capabilities": { "tools": {} },
            }),
            _ => json!({ "content": [{ "type": "text", "text": "connection-ok" }] }),
        };
        Json(json!({ "jsonrpc": "2.0", "id": body["id"], "result": result }))
    };
    let upstream = common::serve(
        upstream,
        Router::new().route("/mcp", axum::routing::post(answer)),
    );
    let connection = json!({
        "name": "Upstream",
        "url": format!("http://{upstream_address}/mcp"),
        "auth": "none",
        "allowPrivateNetwork": true,
    });
    let connection = owner
        .service
        .mcps
        .save(&owner.service, connection, None)
        .await
        .unwrap();
    let connection = connection["id"].as_str().unwrap();
    let configuration = owner
        .service
        .mcps
        .run_configuration(&owner.service, &record)
        .await
        .unwrap();
    let run_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let gateway = format!("cairn_{}", connection.replace('-', "_"));
    // Claude Code and the resident Codex thread receive the VM-local origin.
    for (name, path) in [
        ("cairn_workspace", "/mcp-workspace".to_owned()),
        (gateway.as_str(), format!("/mcp-gateway/{connection}")),
    ] {
        let url = format!("http://127.0.0.1:5202{path}");
        assert_eq!(configuration["claudeMcps"]["mcpServers"][name]["url"], url);
        assert_eq!(configuration["codexConfig"][name]["url"], url);
    }
    // Another run in progress on the same installation.
    let mut other_run = run_record(&id(), RunStatus::Running);
    other_run["taskId"] = "other".into();
    other_run["snapshot"] = snapshot;
    owner.add_run(&other_run).await;
    let other = owner
        .service
        .mcps
        .run_configuration(&owner.service, &other_run)
        .await
        .unwrap();
    let other_token = other["env"]["CAIRN_MCP_RUN_TOKEN"].as_str().unwrap();
    let server = owner.serve(listener);
    let stop = CancellationToken::new();
    let socket = owner.root().join("node/runs/home").join("cairn-mcp.sock");
    let session = executor::Session {
        client: reqwest::Client::new(),
        master: format!("http://{address}").parse().unwrap(),
        token,
        attempt,
    };
    let channel = tokio::spawn(cairn_installation::mcps::channel::forward(
        socket.clone(),
        session,
        stop.clone(),
    ));
    wait_for_path(&socket).await;

    let (status, listed) = mcp_through(
        &socket,
        "/mcp-workspace",
        run_token,
        "tools/list",
        json!({}),
    )
    .await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(
        listed["result"]["tools"][0]["name"], "open_project",
        "{listed}"
    );
    let call = json!({ "name": "list_nodes", "arguments": {} });
    let (status, called) = mcp_through(
        &socket,
        "/mcp-workspace",
        run_token,
        "tools/call",
        call.clone(),
    )
    .await;
    assert_eq!(status, 200, "{called}");
    assert!(called["error"].is_null(), "{called}");
    assert_ne!(called["result"]["isError"], true, "{called}");
    let echo = json!({ "name": "echo", "arguments": {} });
    let gateway = format!("/mcp-gateway/{connection}");
    let (status, proxied) = mcp_through(&socket, &gateway, run_token, "tools/call", echo).await;
    assert_eq!(status, 200, "{proxied}");
    assert_eq!(
        proxied["result"]["content"][0]["text"], "connection-ok",
        "{proxied}"
    );

    // The node relays only this attempt's run-scoped MCP.
    let (status, _) = mcp_through(
        &socket,
        "/mcp-workspace",
        other_token,
        "tools/list",
        json!({}),
    )
    .await;
    assert_eq!(status, 401);
    for path in ["/api/mcp", "/internal/nodes/heartbeat", "/api/agents"] {
        let (status, _) = mcp_through(&socket, path, run_token, "tools/list", json!({})).await;
        assert_eq!(status, 404, "{path}");
    }
    owner.grant_nodes(MAIN_AGENT_ID, json!([])).await;
    let (status, _) = mcp_through(&socket, "/mcp-workspace", run_token, "tools/call", call).await;
    assert_eq!(status, 403);

    stop.cancel();
    channel.await.unwrap().unwrap();
    assert!(!socket.exists());
    server.abort();
    upstream.abort();
}

#[tokio::test]
async fn completed_run_storage_refresh_clears_stale_dirty_counts() {
    use cairn_installation::nodes::storage;

    let runner = Router::new().fallback(|| async {
        Json(json!({
            "mode": "on-demand",
            "dirtyBytes": 0,
            "dirtySince": null,
            "localBytes": 4096,
            "activeLocalBytes": 4096,
        }))
    });
    let (runner_url, server) = common::serve_locally(runner).await;
    let owner = Owner::with_runner(common::HOST.into(), runner_url).await;
    let run = id();
    let volume = format!("{run}:{LOCAL_NODE_ID}");
    let mut record = run_record(&run, RunStatus::Succeeded);
    record["nodeId"] = LOCAL_NODE_ID.into();
    record["storage"] = json!({ "mode": "on-demand", "dirtyBytes": 1_048_576, "dirtySince": 1 });
    owner.add_run(&record).await;
    owner
        .put(
            "node-volumes",
            json!({ "id": volume, "runId": run, "nodeId": LOCAL_NODE_ID, "diskMiB": 100 }),
        )
        .await;

    storage::refresh(&owner.service, &record).await.unwrap();
    let current = owner.run(&run).await;
    assert_eq!(current["status"], RunStatus::Succeeded);
    assert_eq!(current["storage"]["dirtyBytes"], 0);
    assert!(current["storage"]["dirtySince"].is_null());
    let volume = owner.stored("node-volumes", &volume).await.unwrap();
    assert_eq!(volume["diskMiB"], 1);

    // A late reply from the old owner cannot overwrite the destination's state.
    owner
        .service
        .store
        .patch_run(
            &run,
            json!({ "nodeId": id(), "storage": { "dirtyBytes": 42 } }),
        )
        .await
        .unwrap();
    storage::refresh(&owner.service, &record).await.unwrap();
    assert_eq!(owner.run(&run).await["storage"]["dirtyBytes"], 42);
    server.abort();
}

#[tokio::test]
async fn starting_vm_defers_publication_without_a_sync_failure() {
    let captures = Arc::new(tokio::sync::Notify::new());
    let captured = captures.clone();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let received = requests.clone();
    let runner = Router::new().fallback(move |request: Request<Body>| {
        let captured = captured.clone();
        let received = received.clone();
        async move {
            let body = to_bytes(request.into_body(), 1024).await.unwrap();
            received
                .lock()
                .unwrap()
                .push(serde_json::from_slice::<Value>(&body).unwrap());
            captured.notify_one();
            (
                StatusCode::CONFLICT,
                Json(json!({ "error": "VM is still starting." })),
            )
        }
    });
    let (runner_url, server) = common::serve_locally(runner).await;
    let owner = Owner::with_runner(common::HOST.into(), runner_url).await;
    std::fs::write(
        owner.service.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "fixture" }).to_string(),
    )
    .unwrap();
    let run = id();
    let previous_snapshot = id();
    let mut record = run_record(&run, RunStatus::Running);
    record["isolated"] = true.into();
    record["backup"] = json!({ "snapshotId": previous_snapshot });
    owner.add_run(&record).await;
    owner
        .set_checkpoint(&run, json!({ "runnerId": id() }))
        .await;

    publication::attempt(&owner.service, &record).await;
    let current = owner.run(&run).await;
    assert!(
        current["backup"]["error"].is_null(),
        "{}",
        current["backup"]
    );
    assert_eq!(current["backup"]["status"], "pending");
    assert_eq!(current["backup"]["snapshotId"], previous_snapshot);
    captures.notified().await;
    assert_eq!(requests.lock().unwrap()[0]["consistency"], "crash");

    owner
        .service
        .store
        .patch_run(
            &run,
            json!({
                "status": RunStatus::Succeeded,
                "storage": { "mode": "on-demand", "dirtyBytes": 0 },
            }),
        )
        .await
        .unwrap();
    let maintenance = tokio::spawn(publication::maintain(owner.service.clone()));
    tokio::time::timeout(Duration::from_secs(8), captures.notified())
        .await
        .expect("a deferred final capture must retry after the run has completed");
    assert_eq!(
        requests.lock().unwrap().last().unwrap()["consistency"],
        "filesystem"
    );
    maintenance.abort();
    assert!(
        owner
            .service
            .store
            .list("node-alerts")
            .await
            .unwrap()
            .is_empty()
    );
    server.abort();
}

#[tokio::test]
async fn retired_audit_errors_do_not_force_full_download_before_demand_resume() {
    let owner = Owner::new().await;
    let (run, point) = (id(), id());
    let mut record = run_record(&run, RunStatus::Queued);
    record["storage"] = json!({ "mode": "on-demand" });
    owner.add_run(&record).await;
    owner
        .service
        .store
        .set(
            &format!("node-backup-audit:{run}"),
            json!({ "error": "historical audit failure" }),
            None,
        )
        .await
        .unwrap();
    let manifest = json!({
        "version": 1,
        "size": 4096,
        "blockSize": snapshots::BLOCK,
        "blocks": [{ "offset": 0, "size": 4096, "hash": "a".repeat(64) }],
    });
    let encrypted = owner.encrypt_manifest(&point, &manifest);
    owner
        .put(
            "node-backups",
            json!({
                "id": point,
                "runId": run,
                "sessionId": "saved-session",
                "capturedAt": 1,
                "destination": "s3",
                "manifest": encrypted,
            }),
        )
        .await;
    // No S3 credentials or local payload: selecting verified metadata cannot
    // download the disk. Reads will validate the remote blocks when requested.
    owner
        .service
        .store
        .patch_run(&run, json!({ "backup": { "id": point } }))
        .await
        .unwrap();
    let selected = moves::latest(&owner.service, &run)
        .await
        .unwrap()
        .expect("published metadata remains eligible");
    assert_eq!(selected["id"], point);
    assert_eq!(
        publication::manifest(&owner.service, &selected)
            .await
            .unwrap()["size"],
        4096
    );
}

/// A runner controller that serves snapshots of `disk` and can corrupt blocks.
/// It sets `batched` once a publication reads blocks in a batch.
fn snapshot_controller(
    disk: PathBuf,
    manifest: Arc<tokio::sync::Mutex<Value>>,
    corrupt: Arc<AtomicBool>,
    batched: Arc<AtomicBool>,
    snapshot: String,
) -> Router {
    Router::new().fallback(axum::routing::any(move |request: Request<Body>| {
        let (disk, manifest, corrupt, batched, snapshot) = (
            disk.clone(),
            manifest.clone(),
            corrupt.clone(),
            batched.clone(),
            snapshot.clone(),
        );
        async move {
            assert_eq!(
                request.headers()["authorization"],
                "Bearer controller-fixture"
            );
            if request.method() == "DELETE" {
                return Json(json!({ "ok": true })).into_response();
            }
            if request.uri().path().ends_with("/snapshot") {
                let manifest = manifest.lock().await.clone();
                return Json(json!({ "id": snapshot, "manifest": manifest })).into_response();
            }
            if request.uri().path().ends_with("/blocks")
                || request.uri().path().ends_with("/publication")
            {
                batched.store(true, Ordering::SeqCst);
                let body = to_bytes(request.into_body(), 4096).await.unwrap();
                let batch: Value = serde_json::from_slice(&body).unwrap();
                let manifest = manifest.lock().await;
                let mut bytes = Vec::new();
                for hash in batch["hashes"].as_array().unwrap() {
                    bytes.extend(
                        snapshots::block(&disk, &manifest, hash.as_str().unwrap())
                            .await
                            .unwrap(),
                    );
                }
                if corrupt.load(Ordering::SeqCst) {
                    bytes[0] ^= 255;
                }
                return bytes.into_response();
            }
            let hash = request.uri().path().rsplit('/').next().unwrap();
            let mut bytes = snapshots::block(&disk, &*manifest.lock().await, hash)
                .await
                .unwrap();
            if corrupt.load(Ordering::SeqCst) {
                bytes[0] ^= 255;
            }
            bytes.into_response()
        }
    }))
}

/// Points the archive at `endpoint` through a wrapper that isolates the AWS CLI.
async fn configure_loopback_s3(owner: &Owner, endpoint: &str) {
    let parsed: url::Url = endpoint.parse().unwrap();
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    assert_eq!(parsed.scheme(), "http");
    let wrapper = owner.root().join("aws-fixture-client");
    let script = format!(
        "#!/usr/bin/env python3\nimport os,subprocess,sys\nenv={{k:v for k,v in os.environ.items() if not k.startswith('AWS_')}}\nenv.update(AWS_ACCESS_KEY_ID='node-fixture',AWS_SECRET_ACCESS_KEY='node-fixture-secret',AWS_DEFAULT_REGION='us-east-1',AWS_EC2_METADATA_DISABLED='true',AWS_CONFIG_FILE='/dev/null',AWS_SHARED_CREDENTIALS_FILE='/dev/null')\nsys.exit(subprocess.run(['aws','--endpoint-url',{}]+sys.argv[1:],env=env).returncode)\n",
        serde_json::to_string(endpoint).unwrap()
    );
    std::fs::write(&wrapper, script).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        owner.service.config.data_dir.join("archive-s3.json"),
        json!({ "bucket": "cairn-node-test", "awsBinary": wrapper }).to_string(),
    )
    .unwrap();
    let (status, value) = owner
        .send(
            "PUT",
            NODE_SETTINGS,
            json!({ "intervalSeconds": 60, "budgetMiB": 128 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
}

/// An S3 outage fails reads with a retryable status, without dropping the
/// upload receipts nor the incremental baseline of the first recovery point.
async fn assert_s3_outage_is_transient(
    owner: &Owner,
    run: &str,
    blocks: &Path,
    retained: &Value,
    first: &Value,
    hash: &str,
) {
    std::fs::remove_dir_all(blocks).unwrap();
    let config_path = owner.service.config.data_dir.join("archive-s3.json");
    let original_config = std::fs::read(&config_path).unwrap();
    let mut disconnected_config: Value = serde_json::from_slice(&original_config).unwrap();
    disconnected_config["endpoint"] = json!("https://127.0.0.1:1");
    std::fs::write(&config_path, disconnected_config.to_string()).unwrap();
    let mut disconnected_point = retained.clone();
    disconnected_point["endpoint"] = json!("https://127.0.0.1:1");
    let location = json!({
        "destination": "s3",
        "bucket": "cairn-node-test",
        "endpoint": "https://127.0.0.1:1",
    });
    let receipt = blocks.join(format!(
        "{hash}.s3-{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&location).unwrap()))
    ));
    std::fs::create_dir_all(receipt.parent().unwrap()).unwrap();
    std::fs::write(&receipt, b"verified before outage").unwrap();
    assert_eq!(
        publication::read_block(&owner.service, &disconnected_point, hash)
            .await
            .unwrap_err()
            .status,
        503
    );
    assert!(
        receipt.exists(),
        "a transient outage retains the upload receipt"
    );
    assert_eq!(
        owner.run(run).await["backup"]["snapshotId"],
        first["snapshotId"],
        "a transient outage keeps the incremental baseline"
    );
    std::fs::remove_file(receipt).unwrap();
    std::fs::write(&config_path, original_config).unwrap();
}

/// The previous JSON/base64 envelope and the binary format can coexist: a
/// recovery point without a block format reads the run-scoped `key`.
async fn assert_legacy_block_envelope_is_readable(
    owner: &Owner,
    key: &str,
    retained: &Value,
    block: &str,
    original: &[u8],
) {
    let legacy = owner
        .service
        .vault
        .encrypt(key, &json!(STANDARD.encode(&original[..4 * MIB])))
        .unwrap();
    let legacy_file = owner.root().join("legacy-block.json");
    std::fs::write(&legacy_file, serde_json::to_vec(&legacy).unwrap()).unwrap();
    Storage::configured(&owner.service)
        .unwrap()
        .upload_file_verified(&legacy_file, key)
        .await
        .unwrap();
    let mut legacy_point = retained.clone();
    legacy_point.as_object_mut().unwrap().remove("blockFormat");
    assert_eq!(
        publication::read_block(&owner.service, &legacy_point, block)
            .await
            .unwrap(),
        original[..4 * MIB]
    );
}

#[tokio::test]
#[ignore = "requires loopback S3; run scripts/tests/node_s3_test.py"]
async fn encrypted_recovery_points_cross_the_outbound_relay_and_reject_incomplete_publication() {
    let (listener, address) = common::bind().await;
    let owner = Owner::at(address.to_string()).await;
    let (node, attempt, run, token) = (id(), id(), id(), auth::token());
    owner.register_node(&node, &token).await;
    let mut record = run_record(&run, RunStatus::Running);
    record["sessionId"] = "synthetic-native-session".into();
    owner.add_run(&record).await;
    owner
        .set_checkpoint(&run, json!({ "nodeId": node, "runnerId": attempt }))
        .await;
    let source = owner.root().join("source-disk");
    let mut original = vec![0u8; 4 * MIB + 128];
    original[..PRIVATE_CONTENTS.len()].copy_from_slice(PRIVATE_CONTENTS);
    original[4 * MIB] = 1;
    std::fs::write(&source, &original).unwrap();
    let state = Arc::new(tokio::sync::Mutex::new(controller_manifest(&source).await));
    let corrupt = Arc::new(AtomicBool::new(false));
    let batched = Arc::new(AtomicBool::new(false));
    let (fixture, controller) = common::bind().await;
    let controller_task = common::serve(
        fixture,
        snapshot_controller(
            source.clone(),
            state.clone(),
            corrupt.clone(),
            batched.clone(),
            id(),
        ),
    );
    let master_task = owner.serve(listener);
    let stop = CancellationToken::new();
    let relay_task = tokio::spawn(relay::run(
        format!("http://{address}").parse().unwrap(),
        token,
        format!("http://{controller}"),
        "controller-fixture".into(),
        stop.clone(),
    ));
    // Optional external integration: the real AWS CLI talks only to the selected
    // loopback S3 fixture, with synthetic credentials and no inherited AWS config.
    let s3 = std::env::var("CAIRN_NODE_TEST_S3_ENDPOINT").ok();
    if let Some(endpoint) = &s3 {
        configure_loopback_s3(&owner, endpoint).await;
    }
    let blocks = owner.backup_directory(&run).join("blocks");
    let first = publication::capture(&owner.service, &record).await.unwrap();
    assert!(
        batched.load(Ordering::SeqCst),
        "remote publication must exercise batching"
    );
    assert_eq!(first["uploadedBytes"], 4 * MIB + 128);
    let retained = owner
        .stored("node-backups", first["id"].as_str().unwrap())
        .await
        .unwrap();
    let first_manifest = publication::manifest(&owner.service, &retained)
        .await
        .unwrap();
    if s3.is_some() {
        let hash = first_manifest["blocks"][0]["hash"].as_str().unwrap();
        assert_s3_outage_is_transient(&owner, &run, &blocks, &retained, &first, hash).await;
    }
    let restored = owner.root().join("restored-disk");
    snapshots::restore(&restored, &first_manifest, |hash| {
        let (service, backup) = (owner.service.clone(), retained.clone());
        async move { publication::read_block(&service, &backup, &hash).await }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(restored).unwrap(), original);
    let block = first_manifest["blocks"][0]["hash"].as_str().unwrap();
    let encrypted = Storage::configured(&owner.service)
        .unwrap()
        .download_bytes(
            &shared_object_key(&first_manifest, block),
            snapshots::BLOCK + 4096,
        )
        .await
        .unwrap();
    assert!(
        !encrypted
            .windows(PRIVATE_CONTENTS.len())
            .any(|window| window == PRIVATE_CONTENTS)
    );
    assert!(
        encrypted.len() <= 4 * MIB + 64,
        "A 4 MiB backup block occupies {} bytes",
        encrypted.len()
    );
    assert_eq!(
        publication::read_block(&owner.service, &retained, block)
            .await
            .unwrap(),
        original[..4 * MIB],
    );
    let legacy_key = format!("node-backups/{run}/blocks/{block}");
    assert_legacy_block_envelope_is_readable(&owner, &legacy_key, &retained, block, &original)
        .await;
    original[4 * MIB] = 2;
    std::fs::write(&source, &original).unwrap();
    *state.lock().await = controller_manifest(&source).await;
    corrupt.store(true, Ordering::SeqCst);
    assert!(publication::capture(&owner.service, &record).await.is_err());
    assert_eq!(
        owner
            .service
            .store
            .list("node-backups")
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(owner.run(&run).await["backup"]["id"], first["id"]);
    corrupt.store(false, Ordering::SeqCst);
    let second = publication::capture(&owner.service, &record).await.unwrap();
    assert_eq!(second["uploadedBytes"], 128);
    assert!(
        owner
            .stored("node-backups", first["id"].as_str().unwrap())
            .await
            .is_none()
    );
    assert_eq!(
        owner
            .service
            .store
            .node_backups_for_run(&run)
            .await
            .unwrap()
            .len(),
        1
    );

    let filler = owner
        .service
        .config
        .data_dir
        .join("node-backups/quota-fixture");
    std::fs::File::create(&filler)
        .unwrap()
        .set_len(128 * 1024 * 1024)
        .unwrap();
    let settings = json!({
        "destination": if s3.is_some() { "s3" } else { "master" },
        "intervalSeconds": 60,
        "retention": 2,
        "budgetMiB": 128,
    });
    owner
        .service
        .store
        .set("node-backup-settings", settings, None)
        .await
        .unwrap();
    assert_eq!(
        publication::capture(&owner.service, &record)
            .await
            .unwrap_err()
            .status,
        507
    );
    assert_eq!(owner.run(&run).await["backup"]["id"], second["id"]);
    std::fs::remove_file(filler).unwrap();
    let mut third = publication::capture(&owner.service, &record).await.unwrap();
    assert_eq!(third["uploadedBytes"], 0);
    assert!(
        owner
            .stored("node-backups", first["id"].as_str().unwrap())
            .await
            .is_none(),
        "Retention removes the oldest manifest while keeping shared blocks"
    );
    if s3.is_some() {
        let storage = Storage::configured(&owner.service).unwrap();
        let point = owner
            .stored("node-backups", third["id"].as_str().unwrap())
            .await
            .unwrap();
        let current_manifest = publication::manifest(&owner.service, &point).await.unwrap();
        storage
            .purge_key(&shared_object_key(&current_manifest, block))
            .await
            .unwrap();
        assert_eq!(
            publication::read_block(&owner.service, &point, block)
                .await
                .unwrap_err()
                .status,
            409,
            "A missing S3 object requires repair, unlike a retryable storage outage"
        );
        assert!(
            owner.run(&run).await["backup"]["snapshotId"].is_null(),
            "A failed restore must request a full next capture"
        );
        let mut entries = std::fs::read_dir(&blocks).unwrap();
        assert!(
            !entries.any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{block}.s3-"))
            }),
            "A missing remote copy must lose its upload receipt"
        );
        third = publication::capture(&owner.service, &owner.run(&run).await)
            .await
            .unwrap();
        assert_eq!(
            third["uploadedBytes"],
            4 * MIB,
            "Only the invalidated shared object is uploaded again"
        );
        let repaired = owner
            .stored("node-backups", third["id"].as_str().unwrap())
            .await
            .unwrap();
        assert_eq!(
            publication::read_block(&owner.service, &repaired, block)
                .await
                .unwrap(),
            &original[..4 * MIB]
        );
        std::fs::write(blocks.join(block), b"damaged ciphertext").unwrap();
        assert_eq!(
            publication::read_block(&owner.service, &repaired, block)
                .await
                .unwrap(),
            &original[..4 * MIB],
            "Shared reads ignore obsolete run-scoped ciphertext"
        );
        assert_eq!(
            owner.run(&run).await["backup"]["snapshotId"],
            third["snapshotId"],
            "A successful remote recovery keeps the incremental baseline"
        );
        std::fs::remove_file(blocks.join(block)).unwrap();
        // A local enabled node has one clean disk cache (the controller). Even
        // below its normal reserve, verified publication uses bounded memory.
        let mut local = owner
            .stored("nodes", LOCAL_NODE_ID)
            .await
            .unwrap_or_else(|| json!({ "id": LOCAL_NODE_ID }));
        local["storage"] = json!(Policy {
            reserve_mi_b: 16_777_216,
            ..Policy::default()
        });
        owner.put("nodes", local).await;
        publication::maintain_local_cache(&owner.service)
            .await
            .unwrap();
        assert!(
            !blocks.join(block).exists(),
            "duplicate S3 cache is reclaimed"
        );
        original[4 * MIB] = 3;
        std::fs::write(&source, &original).unwrap();
        let memory = controller_manifest(&source).await;
        let changed = memory["blocks"][1]["hash"].as_str().unwrap().to_owned();
        *state.lock().await = memory;
        third = publication::capture(&owner.service, &record).await.unwrap();
        assert_eq!(third["uploadedBytes"], 128);
        assert!(
            !blocks.join(&changed).exists(),
            "memory publication retains no second cache"
        );
        let published = owner
            .stored("node-backups", third["id"].as_str().unwrap())
            .await
            .unwrap();
        assert_eq!(
            publication::read_block(&owner.service, &published, &changed)
                .await
                .unwrap(),
            original[4 * MIB..]
        );
        assert!(
            !blocks.join(changed).exists(),
            "reads also avoid duplicate cache"
        );
    }
    let mut damaged = owner
        .stored("node-backups", third["id"].as_str().unwrap())
        .await
        .unwrap();
    damaged["capturedAt"] = (now() + 1000).into();
    damaged["manifest"] = json!({ "corrupt": true });
    owner.put("node-backups", damaged).await;
    assert!(
        moves::latest(&owner.service, &run).await.unwrap().is_none(),
        "No rollback to a previous publication after the current manifest is damaged"
    );
    publication::purge(&owner.service, &run).await.unwrap();
    drain_remote_deletions(&owner.service).await;
    assert!(
        owner
            .service
            .store
            .list("node-backups")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!owner.backup_directory(&run).exists());
    stop.cancel();
    relay_task.await.unwrap().unwrap();
    controller_task.abort();
    master_task.abort();
}

#[tokio::test]
async fn an_unreachable_owner_is_fenced_only_after_its_last_lease_and_cannot_renew_after_release() {
    for node in [id(), LOCAL_NODE_ID.to_owned()] {
        let runner_url = if node == LOCAL_NODE_ID {
            "http://127.0.0.1:1".into()
        } else {
            String::new()
        };
        let owner = Owner::with_runner("127.0.0.1:1".into(), runner_url).await;
        let (run, attempt, token) = (id(), id(), auth::token());
        owner
            .put("nodes", json!({ "id": node, "revoked": false }))
            .await;
        owner.grant_nodes(MAIN_AGENT_ID, json!([node])).await;
        owner.authorize_node(&node, &token).await;
        owner
            .put(
                "node-attempts",
                json!({
                    "id": attempt,
                    "nodeId": node,
                    "runId": run,
                    "released": false,
                    "leaseRequired": true,
                }),
            )
            .await;
        let mut record = run_record(&run, RunStatus::Running);
        record["isolated"] = true.into();
        record["snapshot"] = json!({ "agent": { "id": MAIN_AGENT_ID } });
        owner.add_run(&record).await;
        owner
            .set_checkpoint(
                &run,
                json!({
                    "nodeId": node,
                    "runnerId": attempt,
                    "launched": true,
                    "prepared": { "isolated": true },
                }),
            )
            .await;
        let lease_deadline = async |deadline: tokio::time::Instant| {
            owner
                .service
                .node_lease_deadlines
                .lock()
                .await
                .insert(attempt.clone(), deadline);
        };
        lease_deadline(tokio::time::Instant::now() + Duration::from_secs(60)).await;
        assert_eq!(
            recovery::fence(&owner.service, &record)
                .await
                .unwrap_err()
                .status,
            503
        );
        assert_eq!(
            owner.stored("node-attempts", &attempt).await.unwrap()["released"],
            false
        );
        lease_deadline(tokio::time::Instant::now() - Duration::from_secs(21)).await;
        recovery::fence(&owner.service, &record).await.unwrap();
        let (status, heartbeat) = owner
            .call(
                "POST",
                HEARTBEAT,
                json!({ "runtimeId": "fixture" }),
                Some(&token),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(heartbeat["leases"].as_array().unwrap().is_empty());
    }
}

#[tokio::test]
async fn abandoned_return_to_a_node_releases_only_the_unmaterialized_disk_reservation() {
    let owner = Owner::new().await;
    let (node, agent, run) = (id(), id(), id());
    owner
        .put("nodes", schedulable_node(&node, &limits(4, 8192, 65536)))
        .await;
    owner.grant_nodes(&agent, json!([node])).await;
    let original = agent_run(&run, &agent);
    let attempt = id();
    placement::reserve(&owner.service, &original, &attempt)
        .await
        .unwrap();
    placement::materialize(&owner.service, &attempt)
        .await
        .unwrap();
    placement::release(&owner.service, &attempt).await.unwrap();
    owner.set_checkpoint(&run, json!({ "nodeId": id() })).await;
    let mut returning = original.clone();
    returning["placementTransition"] = true.into();
    let pending = id();
    placement::reserve(&owner.service, &returning, &pending)
        .await
        .unwrap();
    let other = agent_run(&id(), &agent);
    let parallel = id();
    placement::reserve(&owner.service, &other, &parallel)
        .await
        .expect("An S3 disk should not reserve its logical size on a node");
    placement::release(&owner.service, &parallel).await.unwrap();
    placement::release(&owner.service, &pending).await.unwrap();
    placement::release(&owner.service, &pending).await.unwrap();
    let replacement = id();
    placement::reserve(&owner.service, &other, &replacement)
        .await
        .expect("Cancellation must return the unused destination space");
    let thin = id();
    placement::reserve(&owner.service, &agent_run(&id(), &agent), &thin)
        .await
        .expect("A retained S3 disk should consume only local cache headroom");
    placement::release(&owner.service, &thin).await.unwrap();
    placement::release(&owner.service, &replacement)
        .await
        .unwrap();
    let restored = id();
    placement::reserve(&owner.service, &returning, &restored)
        .await
        .unwrap();
    placement::materialize(&owner.service, &restored)
        .await
        .unwrap();
    placement::release(&owner.service, &restored).await.unwrap();
    owner.set_checkpoint(&run, json!({ "nodeId": node })).await;
}

#[tokio::test]
async fn legacy_disks_keep_their_size_and_resource_requests_do_not_reserve_capacity() {
    let owner = Owner::new().await;
    let (node, agent, run) = (id(), id(), id());
    owner
        .put("nodes", schedulable_node(&node, &limits(1, 1024, 8192)))
        .await;
    owner.grant_nodes(&agent, json!([node])).await;
    let mut record = agent_run(&run, &agent);
    record["resources"] = limits(8, 16384, 32768);
    record["requestedResources"] = limits(16, 32768, 65536);
    let selected = placement::reserve(&owner.service, &record, &id())
        .await
        .unwrap();
    assert_eq!(selected["resources"], limits(1, 1024, 32768));
    // All four slots share the one CPU and one GiB host budget.
    for _ in 0..3 {
        let another = agent_run(&id(), &agent);
        assert_eq!(
            placement::reserve(&owner.service, &another, &id())
                .await
                .unwrap()["resources"],
            limits(1, 1024, 8192)
        );
    }
    assert!(placement::is_no_capacity(
        &placement::reserve(&owner.service, &agent_run(&id(), &agent), &id())
            .await
            .unwrap_err()
    ));
}

/// Contents written to the source disk that every move must carry.
const MOVED_CONTENTS: &[u8] = b"complete environment, untracked files and native session";

/// Flags that make the fixture controllers fail or cancel a movement.
#[derive(Clone)]
struct MoveFaults {
    failed: Arc<AtomicBool>,
    cancel_during_capture: Arc<AtomicBool>,
}

/// A node controller that captures, publishes and restores the disk of `run`.
fn movement_controller(
    state: PathBuf,
    run: String,
    faults: MoveFaults,
    service: Arc<Service>,
) -> Router {
    Router::new().fallback(axum::routing::any(move |request: Request<Body>| {
        let (state, run, faults, service) =
            (state.clone(), run.clone(), faults.clone(), service.clone());
        async move {
            assert_eq!(
                request.headers()["authorization"],
                "Bearer controller-fixture"
            );
            let route = request.uri().path().to_owned();
            if request.method() == "DELETE" {
                return Json(json!({ "ok": true })).into_response();
            }
            if route.ends_with("/storage-status") {
                let volume = runtime::load(&state.join("disks").join(&run))
                    .await
                    .unwrap();
                return Json(volume.status().unwrap()).into_response();
            }
            if route.ends_with("/snapshot") {
                assert_eq!(
                    route,
                    format!("/disks/{run}/snapshot"),
                    "Idle movement must capture by disk, without destination attempt history"
                );
                let body = to_bytes(request.into_body(), 1024).await.unwrap();
                let value: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(
                    value["consistency"], "filesystem",
                    "Movement retains coherent capture"
                );
                if faults.cancel_during_capture.load(Ordering::SeqCst) {
                    service
                        .store
                        .patch_run(
                            &run,
                            json!({ "cancelRequestedAt": now(), "status": RunStatus::Cancelled }),
                        )
                        .await
                        .unwrap();
                }
                if faults.failed.load(Ordering::SeqCst) {
                    return StatusCode::PRECONDITION_FAILED.into_response();
                }
                let snapshot = checkpoint::capture(
                    &state,
                    &run,
                    None,
                    Arc::new(tokio::sync::Mutex::new(())),
                    CancellationToken::new(),
                    &run,
                    cairn_installation::storage::checkpoint::Consistency::Filesystem,
                )
                .await
                .unwrap();
                return Json(snapshot).into_response();
            }
            if route.ends_with("/published") {
                let body = to_bytes(request.into_body(), 1_000_000).await.unwrap();
                let value: Value = serde_json::from_slice(&body).unwrap();
                let volume = runtime::load(&state.join("disks").join(&run))
                    .await
                    .unwrap();
                volume
                    .disk
                    .commit_published(
                        value["generation"].as_i64().unwrap(),
                        value["backupId"].as_str().unwrap(),
                    )
                    .unwrap();
                return Json(json!({ "committed": true })).into_response();
            }
            if route.ends_with("/restore") {
                let body = to_bytes(request.into_body(), 1_000_000).await.unwrap();
                let value = serde_json::from_slice(&body).unwrap();
                let restored = restore::controller(&state, &run, value).await.unwrap();
                return Json(restored).into_response();
            }
            if route.starts_with("/snapshots/") {
                let parts = route.split('/').collect::<Vec<_>>();
                let directory = state.join("snapshots").join(parts[2]);
                if request.method() == "DELETE" {
                    tokio::fs::remove_dir_all(directory).await.unwrap();
                    return Json(json!({ "ok": true })).into_response();
                }
                if request.method() == "POST" && matches!(parts[3], "blocks" | "publication") {
                    let body = to_bytes(request.into_body(), 4096).await.unwrap();
                    let value: Value = serde_json::from_slice(&body).unwrap();
                    let hashes = serde_json::from_value(value["hashes"].clone()).unwrap();
                    let (length, body) = if parts[3] == "publication" {
                        snapshots::served_publication(&directory, hashes)
                            .await
                            .unwrap()
                    } else {
                        snapshots::served_batch(&directory, hashes).await.unwrap()
                    };
                    return ([("content-length", length.to_string())], body).into_response();
                }
                return snapshots::served(&directory, parts[3])
                    .await
                    .unwrap()
                    .into_response();
            }
            panic!("Idle movement must not launch a provider: {route}");
        }
    }))
}

/// Creates the lazily restored disk that the source node holds for `record`.
async fn seed_source_disk(owner: &Owner, state: &Path, record: &Value, node: &str, master: &str) {
    let run = record["id"].as_str().unwrap();
    let disk = state.join("disks").join(run);
    std::fs::create_dir_all(&disk).unwrap();
    std::fs::write(disk.join("runtime.json"), br#"{"runtimeId":"fixture"}"#).unwrap();
    let grant = disk_grants::new_disk(&owner.service, record, node)
        .await
        .unwrap();
    let context = json!({ "master": master, "grant": grant, "policy": small_reserve() });
    let remote = Arc::new(
        RemoteSource::new(
            &context,
            tokio::runtime::Handle::current(),
            CancellationToken::default(),
        )
        .unwrap(),
    );
    let manifest = json!({
        "version": 1,
        "size": 4096,
        "blockSize": 4_194_304,
        "blocks": [{ "offset": 0, "size": 4096, "hash": null }],
    });
    let journal = LazyDisk::create(&disk.join("lazy"), &manifest, remote).unwrap();
    journal.set_context(&context).unwrap();
    journal.write_at(0, MOVED_CONTENTS).unwrap();
    journal.sync().unwrap();
}

/// Requests a move of `run` to `target` and lets it settle.
async fn move_to(owner: &Owner, run: &str, _resources: &Value, target: &str) {
    let args = json!({ "nodeId": target });
    moves::request(&owner.service, &owner.run(run).await, &args)
        .await
        .unwrap();
    moves::advance(&owner.service, &owner.run(run).await)
        .await
        .unwrap();
}

/// Prepares the runtime of `node`, enrolls it, and connects its fixture
/// controller to the master at `master` through an outbound relay.
async fn connect_node(
    owner: &Owner,
    record: &Value,
    node: &str,
    master: std::net::SocketAddr,
    faults: &MoveFaults,
    stop: &CancellationToken,
    seed: bool,
) -> [tokio::task::JoinHandle<()>; 2] {
    let run = record["id"].as_str().unwrap();
    let state = owner.root().join(node);
    let image = state.join("images/fixture");
    std::fs::create_dir_all(&image).unwrap();
    std::fs::write(image.join("root.ext4"), b"fixture runtime").unwrap();
    std::fs::write(image.join("vmlinux"), b"fixture kernel").unwrap();
    if seed {
        seed_source_disk(owner, &state, record, node, &format!("http://{master}/")).await;
    }
    let token = auth::token();
    let mut enrolled = schedulable_node(node, &limits(2, 1024, 1024));
    enrolled["revoked"] = false.into();
    enrolled["runtimeId"] = "fixture".into();
    enrolled["runtimeIds"] = json!(["fixture"]);
    enrolled["runtimes"] = json!(["fixture"]);
    enrolled["storage"] = json!(small_reserve());
    owner.put("nodes", enrolled).await;
    owner.authorize_node(node, &token).await;
    let (controller, controller_address) = common::bind().await;
    let app = movement_controller(state, run.to_owned(), faults.clone(), owner.service.clone());
    let controller_task = common::serve(controller, app);
    let stop = stop.clone();
    let relay_task = tokio::spawn(async move {
        relay::run(
            format!("http://{master}").parse().unwrap(),
            token,
            format!("http://{controller_address}"),
            "controller-fixture".into(),
            stop,
        )
        .await
        .unwrap();
    });
    [controller_task, relay_task]
}

/// The idle run settled on `target` with its session and disk contents.
async fn assert_moved_with_its_disk(owner: &Owner, run: &str, target: &str) {
    let settled = owner.run(run).await;
    assert_eq!(settled["status"], RunStatus::Succeeded);
    assert_eq!(settled["nodeId"], *target, "{settled}");
    assert_eq!(settled["recoveryPending"], false);
    assert_eq!(settled["sessionId"], "retained-session");
    let directory = owner.root().join(target).join("disks").join(run);
    let volume = runtime::load(&directory).await.unwrap();
    let bytes = tokio::task::spawn_blocking(move || {
        let mut bytes = vec![0; MOVED_CONTENTS.len()];
        volume.read_at(0, &mut bytes).unwrap();
        bytes
    })
    .await
    .unwrap();
    assert_eq!(bytes, MOVED_CONTENTS);
    assert!(
        owner
            .service
            .store
            .list("node-attempts")
            .await
            .unwrap()
            .iter()
            .all(|attempt| attempt["released"] == true)
    );
}

#[tokio::test]
#[ignore = "requires loopback S3; run scripts/tests/node_s3_test.py"]
async fn idle_conversations_move_twice_through_outbound_relays_and_failed_moves_stay_idle() {
    let (listener, address) = common::bind().await;
    let owner = Owner::at(address.to_string()).await;
    let (run, agent, source, destination, attempt) = (id(), id(), id(), id(), id());
    let resources = limits(1, 512, 128);
    owner
        .grant_nodes(&agent, json!([source, destination]))
        .await;
    let mut record = run_record(&run, RunStatus::Succeeded);
    record["isolated"] = true.into();
    record["sessionId"] = "retained-session".into();
    record["nodeId"] = source.clone().into();
    record["resources"] = resources.clone();
    record["snapshot"] = json!({ "agent": { "id": agent } });
    owner.add_run(&record).await;
    owner
        .set_checkpoint(
            &run,
            json!({
                "nodeId": source,
                "runnerId": attempt,
                "completed": true,
                "prepared": { "backend": "firecracker", "isolated": true },
            }),
        )
        .await;
    let stop = CancellationToken::new();
    let faults = MoveFaults {
        failed: Arc::new(AtomicBool::new(false)),
        cancel_during_capture: Arc::new(AtomicBool::new(false)),
    };
    let mut tasks = Vec::new();
    for node in [&source, &destination] {
        let seed = node == &source;
        tasks.extend(connect_node(&owner, &record, node, address, &faults, &stop, seed).await);
    }
    tasks.push(owner.serve(listener));
    for target in [&destination, &source] {
        let current = owner.run(&run).await;
        let args = json!({ "nodeId": target });
        moves::request(&owner.service, &current, &args)
            .await
            .unwrap();
        let pending = owner.run(&run).await;
        assert_eq!(pending["status"], RunStatus::Queued);
        assert_eq!(pending["moveRequest"]["idle"], true);
        moves::advance(&owner.service, &pending).await.unwrap();
        assert_moved_with_its_disk(&owner, &run, target).await;
    }
    faults.failed.store(true, Ordering::SeqCst);
    move_to(&owner, &run, &resources, &destination).await;
    let settled = owner.run(&run).await;
    assert_eq!(settled["status"], RunStatus::Succeeded);
    assert_eq!(settled["recoveryPending"], false);
    assert_eq!(
        settled["movementError"],
        "Unable to capture a coherent VM snapshot."
    );
    assert_eq!(settled["nodeId"], source);
    faults.cancel_during_capture.store(true, Ordering::SeqCst);
    move_to(&owner, &run, &resources, &destination).await;
    let cancelled = owner.run(&run).await;
    assert_eq!(cancelled["status"], RunStatus::Cancelled);
    assert_eq!(cancelled["recoveryPending"], false);
    stop.cancel();
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn pending_movement_retries_a_lost_stop_without_stopping_a_later_execution() {
    let (listener, address) = common::bind().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let app = Router::new().fallback(axum::routing::delete(move || {
        let counter = counter.clone();
        async move {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::NO_CONTENT
            }
        }
    }));
    let owner = Owner::with_runner(common::HOST.into(), format!("http://{address}")).await;
    let server = common::serve(listener, app);
    let run = id();
    let mut record = run_record(&run, RunStatus::Running);
    record["moveRequest"] = json!({ "requestedAt": now() - 3000 });
    owner.add_run(&record).await;
    owner
        .set_checkpoint(&run, json!({ "runnerId": id() }))
        .await;
    assert!(moves::pause_pending(&owner.service, &run).await.is_err());
    moves::pause_pending(&owner.service, &run).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    owner
        .service
        .store
        .patch_run(&run, json!({ "moveRequest": null }))
        .await
        .unwrap();
    moves::pause_pending(&owner.service, &run).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
#[ignore = "requires loopback S3; run scripts/tests/node_s3_test.py"]
async fn queued_capacity_transfer_carries_unpublished_writes_and_keeps_the_source_after_failure() {
    let (listener, address) = common::bind().await;
    let owner = Owner::at(address.to_string()).await;
    let (run, agent, source, destination, attempt) = (id(), id(), id(), id(), id());
    owner
        .grant_nodes(&agent, json!([source, destination]))
        .await;
    let mut record = run_record(&run, RunStatus::Queued);
    record["isolated"] = true.into();
    record["sessionId"] = "retained-session".into();
    record["nodeId"] = source.clone().into();
    record["resources"] = limits(2, 512, 128);
    record["snapshot"] = json!({ "agent": { "id": agent } });
    owner.add_run(&record).await;
    owner
        .set_checkpoint(
            &run,
            json!({
                "nodeId": source,
                "runnerId": attempt,
                "runtimeId": "fixture",
                "prepared": { "backend": "firecracker", "isolated": true },
            }),
        )
        .await;
    let stop = CancellationToken::new();
    let faults = MoveFaults {
        failed: Arc::new(AtomicBool::new(true)),
        cancel_during_capture: Arc::new(AtomicBool::new(false)),
    };
    let mut tasks = Vec::new();
    for node in [&source, &destination] {
        tasks.extend(
            connect_node(
                &owner,
                &record,
                node,
                address,
                &faults,
                &stop,
                node == &source,
            )
            .await,
        );
    }
    tasks.push(owner.serve(listener));
    let mut source_node = owner.stored("nodes", &source).await.unwrap();
    source_node["slots"] = 1.into();
    owner.put("nodes", source_node).await;
    let mut busy = agent_run(&id(), &agent);
    busy["pinnedNodeId"] = source.clone().into();
    busy["resources"] = limits(1, 128, 128);
    placement::reserve(&owner.service, &busy, &id())
        .await
        .unwrap();
    for fail in [true, false] {
        faults.failed.store(fail, Ordering::SeqCst);
        let current = owner.run(&run).await;
        assert!(
            moves::queue_capacity_move(&owner.service, &current)
                .await
                .unwrap()
        );
        assert!(
            !moves::advance(&owner.service, &owner.run(&run).await)
                .await
                .unwrap()
        );
        let current = owner.run(&run).await;
        assert_eq!(
            current["nodeId"],
            if fail {
                source.as_str()
            } else {
                destination.as_str()
            }
        );
        assert_eq!(current["status"], RunStatus::Queued);
        if fail {
            assert!(current["movementError"].is_string());
            assert!(current["moveRequest"].is_null());
        } else {
            assert!(current["movementError"].is_null());
            assert_eq!(current["sessionId"], "retained-session");
        }
        let node = if fail { &source } else { &destination };
        let directory = owner.root().join(node).join("disks").join(&run);
        let volume = runtime::load(&directory).await.unwrap();
        let bytes = tokio::task::spawn_blocking(move || {
            let mut bytes = vec![0; MOVED_CONTENTS.len()];
            volume.read_at(0, &mut bytes).unwrap();
            bytes
        })
        .await
        .unwrap();
        assert_eq!(
            bytes, MOVED_CONTENTS,
            "all previously unpublished data preserved on {node}"
        );
    }
    assert!(owner.root().join(&source).join("disks").join(&run).exists());
    stop.cancel();
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn new_disks_use_the_default_size_within_shared_headroom_and_retained_disks_keep_their_size()
{
    let owner = Owner::new().await;
    let (node, agent) = (id(), id());
    owner.grant_nodes(&agent, json!([node])).await;
    for (budget, expected) in [(400_844, 32_768), (8_192, 8_192)] {
        owner
            .put("nodes", schedulable_node(&node, &limits(7, 36_352, budget)))
            .await;
        let result = placement::reserve(&owner.service, &agent_run(&id(), &agent), &id())
            .await
            .unwrap();
        assert_eq!(result["resources"]["diskMiB"], expected);
        assert_eq!(result["resources"]["cpu"], 7);
        assert_eq!(result["resources"]["memoryMiB"], 36_352);
    }

    owner
        .put(
            "nodes",
            schedulable_node(&node, &limits(7, 36_352, 400_844)),
        )
        .await;
    let mut retained = agent_run(&id(), &agent);
    retained["resources"] = limits(2, 4_096, 400_844);
    let result = placement::reserve(&owner.service, &retained, &id())
        .await
        .unwrap();
    assert_eq!(result["resources"]["diskMiB"], 400_844);
}

#[tokio::test]
async fn automatic_placement_spreads_work_unless_a_node_is_preferred() {
    let owner = Owner::new().await;
    let (small, large, agent) = (id(), id(), id());
    for (node, cpu, memory) in [(&small, 4, 8192), (&large, 16, 65536)] {
        owner
            .put(
                "nodes",
                schedulable_node(node, &limits(cpu, memory, 262_144)),
            )
            .await;
    }
    owner.grant_nodes(&agent, json!([small, large])).await;
    let run = |preferred: Value| {
        let mut run = agent_run(&id(), &agent);
        run["preferredNodeId"] = preferred;
        run
    };
    let mut busy = run(json!(small));
    busy["pinnedNodeId"] = small.clone().into();
    placement::reserve(&owner.service, &busy, &id())
        .await
        .unwrap();
    assert_eq!(
        placement::reserve(&owner.service, &run(Value::Null), &id())
            .await
            .unwrap()["nodeId"],
        large
    );
    assert_eq!(
        placement::reserve(&owner.service, &run(json!(small)), &id())
            .await
            .unwrap()["nodeId"],
        small
    );
}

#[tokio::test]
async fn node_agent_grants_are_edited_from_the_node_without_narrowing_all_node_agents() {
    let owner = Owner::new().await;
    let node = id();
    let (explicit, everywhere) = (id(), id());
    owner
        .put(
            "nodes",
            json!({
                "id": node,
                "name": "Desktop",
                "local": false,
                "revoked": false,
                "accepting": true,
                "tags": [],
                "lastSeen": now(),
                "capabilities": { "kvm": true, "fuse": true },
                "limits": limits(4, 8192, 65536),
            }),
        )
        .await;
    for (agent, nodes) in [(&explicit, json!([])), (&everywhere, Value::Null)] {
        owner
            .put(
                "agents",
                json!({ "id": agent, "name": agent, "access": { "nodes": nodes } }),
            )
            .await;
    }
    let granted = async || {
        let (_, nodes) = owner.get("/api/nodes").await;
        let mut agents = find_by_id(&nodes, &node)["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|agent| agent["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        agents.sort();
        agents
    };
    assert_eq!(granted().await, vec![everywhere.clone()]);
    let path = format!("/api/nodes/{node}/agents");
    let (status, body) = owner
        .send("PUT", &path, json!({ "agentIds": [explicit] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut expected = vec![explicit.clone(), everywhere.clone()];
    expected.sort();
    assert_eq!(granted().await, expected);
    owner.send("PUT", &path, json!({ "agentIds": [] })).await;
    assert_eq!(granted().await, vec![everywhere.clone()]);
    let agent = owner.stored("agents", &everywhere).await.unwrap();
    assert!(agent["access"]["nodes"].is_null());
}

#[tokio::test]
async fn agents_choose_nodes_and_legacy_resource_limits_are_not_exposed() {
    let owner = Owner::new().await;
    let (status, agent) = owner
        .send(
            "POST",
            "/api/agents",
            json!({
                "name": "Agent", "access": { "nodes": null, "maxResources": limits(1, 128, 128) }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let run = agent_run(&id(), agent["id"].as_str().unwrap());
    let listed = moves::list(&owner.service, &run).await.unwrap();
    assert!(listed.get("maxResources").is_none());
    assert!(listed.get("currentResources").is_none());
    let tool = moves::tool();
    assert_eq!(tool["name"], "move_to_node");
    assert_eq!(tool["inputSchema"]["required"], json!(["nodeId"]));
    assert!(tool["inputSchema"]["properties"].get("cpu").is_none());
    let error = moves::request_by_agent(&owner.service, &run, &json!({ "nodeId": id(), "cpu": 8 }))
        .await
        .unwrap_err();
    assert_eq!(error.status, 400);
}

#[tokio::test]
async fn node_alerts_reach_the_conversation_and_clients_without_repeating() {
    let owner = Owner::new().await;
    let (run, chat) = (id(), id());
    let mut record = run_record(&run, RunStatus::Running);
    record["trigger"] = "chat".into();
    owner.add_run(&record).await;
    owner
        .put(
            "chats",
            json!({ "id": chat, "runId": run, "title": "Fixture" }),
        )
        .await;
    for _ in 0..2 {
        alerts::raise(
            &owner.service,
            &run,
            "waiting",
            "Conversation waiting for its node",
            "The node is unavailable.",
        )
        .await
        .unwrap();
    }
    let (status, listed) = owner.get("/api/nodes/alerts").await;
    assert_eq!(status, StatusCode::OK);
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["chatId"], chat);
    assert_eq!(listed[0]["title"], "Conversation waiting for its node");
    let events = owner
        .service
        .store
        .read(move |db| db.events(&run, 0, 100))
        .await
        .unwrap();
    assert!(
        json!(events)
            .to_string()
            .contains("The node is unavailable.")
    );
}

#[tokio::test]
async fn stale_node_disks_are_reported_and_freed_on_request() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = calls.clone();
    let runner = Router::new().fallback(move |request: Request<Body>| {
        let recorded = recorded.clone();
        async move {
            recorded
                .lock()
                .unwrap()
                .push(request.uri().path().to_owned());
            Json(json!({}))
        }
    });
    let (url, _runner) = common::serve_locally(runner).await;
    let owner = Owner::with_runner(common::HOST.into(), url).await;
    let other = id();
    owner
        .put(
            "nodes",
            json!({
                "id": LOCAL_NODE_ID,
                "name": "Current runner",
                "local": true,
                "revoked": false,
                "accepting": true,
                "tags": [],
                "capabilities": { "kvm": true, "fuse": true },
                "limits": limits(4, 8192, 65536),
            }),
        )
        .await;
    let (moved, current) = (id(), id());
    for (run, node, total) in [
        (&moved, other.as_str(), 1024),
        (&current, LOCAL_NODE_ID, 3072),
    ] {
        owner.add_run(&run_record(run, RunStatus::Succeeded)).await;
        owner.set_checkpoint(run, json!({ "nodeId": node })).await;
        owner
            .put(
                "node-volumes",
                json!({
                    "id": format!("{run}:{LOCAL_NODE_ID}"),
                    "runId": run,
                    "nodeId": LOCAL_NODE_ID,
                    "materialized": true,
                    "diskMiB": total,
                    "activeDiskMiB": 1024,
                }),
            )
            .await;
    }
    let stale_disks =
        async || find_by_id(&owner.get("/api/nodes").await.1, LOCAL_NODE_ID)["staleDisks"].clone();
    assert_eq!(stale_disks().await, json!({ "count": 2, "diskMiB": 3072 }));
    let (status, freed) = owner
        .send(
            "POST",
            &format!("/api/nodes/{LOCAL_NODE_ID}/stale-disks/delete"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{freed}");
    assert_eq!(freed, json!({ "freedMiB": 3072, "failed": 0 }));
    let calls = calls.lock().unwrap().clone();
    assert!(
        calls.contains(&format!("/disks/{moved}/delete")),
        "{calls:?}"
    );
    assert!(
        calls.contains(&format!("/disks/{current}/prune")),
        "{calls:?}"
    );
    assert_eq!(stale_disks().await, json!({ "count": 0, "diskMiB": 0 }));
}

/// A running isolated conversation placed on `node`.
fn isolated_run(run: &str, node: &str) -> Value {
    json!({
        "id": run,
        "taskId": run,
        "createdAt": 0,
        "status": RunStatus::Running,
        "isolated": true,
        "sessionId": "session",
        "nodeId": node,
    })
}

#[tokio::test]
async fn isolated_conversations_synchronize_on_local_and_remote_nodes() {
    let owner = Owner::new().await;
    let mut states = Vec::new();
    for node in [LOCAL_NODE_ID.to_owned(), id()] {
        let run = id();
        owner.add_run(&isolated_run(&run, &node)).await;
        publication::attempt(&owner.service, &owner.run(&run).await).await;
        states.push(owner.run(&run).await["backup"].clone());
    }
    // Both nodes try to publish, even before their first storage status report.
    // This fixture lacks a controller checkpoint, so both report the failure.
    for state in states {
        assert_eq!(state["status"], "error", "{state}");
    }
}

/// What the capture controller of `active_captures_…` observed and how it fails.
#[derive(Clone, Default)]
struct CaptureLog {
    bodies: Arc<Mutex<Vec<Value>>>,
    snapshots: Arc<Mutex<Vec<String>>>,
    block_reads: Arc<AtomicUsize>,
    failing: Arc<AtomicBool>,
    lost_ack: Arc<AtomicBool>,
}

#[tokio::test]
#[ignore = "requires loopback S3; run scripts/tests/node_s3_test.py"]
async fn coalesced_final_publication_survives_a_new_attempt_and_preserves_later_writes() {
    let root = TempDir::new().unwrap();
    let state = root.path().to_owned();
    let run = id();
    let disk_owner = cairn_installation::storage::environment::ownership(&state, &run, "busy")
        .await
        .unwrap();
    let manifest = json!({ "version": 1, "size": 4096, "blockSize": snapshots::BLOCK, "blocks": [{ "offset": 0, "size": 4096, "hash": null }] });
    let source = json!({ "master": "http://localhost:4310/", "grant": "synthetic-publication", "policy": { "reserveMiB": 64, "reservePercent": 1 } });
    let remote = RemoteSource::new(
        &source,
        tokio::runtime::Handle::current(),
        CancellationToken::new(),
    )
    .unwrap();
    let disk = LazyDisk::create(
        &disk_owner.directory.join("lazy"),
        &manifest,
        Arc::new(remote),
    )
    .unwrap();
    disk.set_context(&source).unwrap();
    drop(disk);
    let volume = runtime::load(&disk_owner.directory).await.unwrap();
    volume.disk.write_at(0, b"first").unwrap();
    std::fs::write(
        disk_owner.directory.join("runtime.json"),
        b"{\"runtimeId\":\"fixture\"}",
    )
    .unwrap();
    volume.seal_completed().await.unwrap();
    let grant = volume.source.grant_id().unwrap();
    let captures = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let acknowledgements = Arc::new(AtomicUsize::new(0));
    let transfer_entered = Arc::new(tokio::sync::Notify::new());
    let transfer_release = CancellationToken::new();
    let ack_entered = Arc::new(tokio::sync::Notify::new());
    let ack_release = CancellationToken::new();
    let controller = Router::new().fallback({
        let (
            state,
            run,
            volume,
            captures,
            reads,
            acknowledgements,
            transfer_entered,
            transfer_release,
            ack_entered,
            ack_release,
            grant,
            disk_owner,
        ) = (
            state.clone(),
            run.clone(),
            volume.clone(),
            captures.clone(),
            reads.clone(),
            acknowledgements.clone(),
            transfer_entered.clone(),
            transfer_release.clone(),
            ack_entered.clone(),
            ack_release.clone(),
            grant.clone(),
            disk_owner.clone(),
        );
        move |request: Request<Body>| {
            let (
                state,
                run,
                volume,
                captures,
                reads,
                acknowledgements,
                transfer_entered,
                transfer_release,
                ack_entered,
                ack_release,
                grant,
                _disk_owner,
            ) = (
                state.clone(),
                run.clone(),
                volume.clone(),
                captures.clone(),
                reads.clone(),
                acknowledgements.clone(),
                transfer_entered.clone(),
                transfer_release.clone(),
                ack_entered.clone(),
                ack_release.clone(),
                grant.clone(),
                disk_owner.clone(),
            );
            async move {
                let path = request.uri().path().to_owned();
                if request.method() == "DELETE" {
                    return Json(json!({})).into_response();
                }
                if path.ends_with("/storage-status") {
                    return Json(volume.inspect().await.unwrap()).into_response();
                }
                if path.ends_with("/snapshot-completed") {
                    captures.fetch_add(1, Ordering::SeqCst);
                    return Json(
                        checkpoint::capture_completed(&state, &run, CancellationToken::new())
                            .await
                            .unwrap(),
                    )
                    .into_response();
                }
                if path.ends_with("/snapshot") {
                    captures.fetch_add(1, Ordering::SeqCst);
                    let generation = volume.seal().await.unwrap();
                    let disk = volume.disk.clone();
                    let mut manifest =
                        tokio::task::spawn_blocking(move || disk.capture(generation))
                            .await
                            .unwrap()
                            .unwrap();
                    manifest["generation"] = generation.into();
                    manifest["onDemand"] = true.into();
                    manifest["capturedAt"] = now().into();
                    manifest["runtime"] = json!({ "runtimeId": "fixture" });
                    let snapshot = id();
                    let directory = state.join("snapshots").join(&snapshot);
                    tokio::fs::create_dir_all(&directory).await.unwrap();
                    tokio::fs::write(directory.join("run"), &run).await.unwrap();
                    tokio::fs::write(directory.join("manifest.json"), manifest.to_string())
                        .await
                        .unwrap();
                    return Json(json!({ "id": snapshot, "manifest": manifest, "grantId": grant }))
                        .into_response();
                }
                if path.ends_with("/published") {
                    let body = to_bytes(request.into_body(), 16384).await.unwrap();
                    let receipt: Value = serde_json::from_slice(&body).unwrap();
                    if acknowledgements.fetch_add(1, Ordering::SeqCst) == 0 {
                        ack_entered.notify_one();
                        ack_release.cancelled().await;
                    }
                    let disk = volume.disk.clone();
                    tokio::task::spawn_blocking(move || {
                        disk.commit_published(
                            receipt["generation"].as_i64().unwrap(),
                            receipt["backupId"].as_str().unwrap(),
                        )
                    })
                    .await
                    .unwrap()
                    .unwrap();
                    return Json(json!({ "committed": true })).into_response();
                }
                if path.ends_with("/blocks") || path.ends_with("/publication") {
                    return StatusCode::NOT_FOUND.into_response();
                }
                if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                    transfer_entered.notify_one();
                    transfer_release.cancelled().await;
                }
                let parts: Vec<_> = path.split('/').collect();
                snapshots::served(&state.join("snapshots").join(parts[2]), parts[3])
                    .await
                    .unwrap()
                    .into_response()
            }
        }
    });
    let (url, server) = common::serve_locally(controller).await;
    let owner = Owner::with_runner(common::HOST.into(), url).await;
    let mut record = isolated_run(&run, LOCAL_NODE_ID);
    record["status"] = json!(RunStatus::Succeeded);
    record["storage"] = json!({ "mode": "on-demand", "dirtyBytes": 5 });
    add_local_run(&owner, &record).await;
    owner
        .put(
            "node-disk-grants",
            json!({ "id": grant, "runId": run, "nodeId": LOCAL_NODE_ID, "backups": [] }),
        )
        .await;
    publication::request(&owner.service, &run).await.unwrap();
    let maintenance = tokio::spawn(publication::maintain(owner.service.clone()));
    tokio::time::timeout(Duration::from_secs(5), transfer_entered.notified())
        .await
        .unwrap();
    owner
        .set_checkpoint(&run, json!({ "nodeId": LOCAL_NODE_ID, "runnerId": id() }))
        .await;
    let mut later = vec![0; 4096];
    later[..5].copy_from_slice(b"later");
    volume.disk.write_at(0, &later).unwrap();
    volume.seal_completed().await.unwrap();
    publication::request(&owner.service, &run).await.unwrap();
    publication::request(&owner.service, &run).await.unwrap();
    assert_eq!(
        captures.load(Ordering::SeqCst),
        1,
        "Later turns coalesce behind the in-flight generation"
    );
    transfer_release.cancel();
    tokio::time::timeout(Duration::from_secs(5), ack_entered.notified())
        .await
        .unwrap();
    let current = owner.run(&run).await;
    assert_eq!(current["backup"]["requestedRevision"], 3);
    assert_eq!(
        current["backup"]["status"], "saving",
        "S3 publication is not a node acknowledgement"
    );
    assert!(current["backup"]["acknowledgedRevision"].is_null());
    ack_release.cancel();
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let current = owner.run(&run).await;
            if current["backup"]["acknowledgedRevision"] == 3
                && current["backup"]["status"] == "ready"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let current = owner.run(&run).await;
    assert!(
        result.is_ok(),
        "Coalesced publication did not drain: backup={} storage={} captures={} acks={}",
        current["backup"],
        current["storage"],
        captures.load(Ordering::SeqCst),
        acknowledgements.load(Ordering::SeqCst)
    );
    assert_eq!(
        captures.load(Ordering::SeqCst),
        2,
        "Three final turns require only two publications"
    );
    assert_eq!(volume.disk.accounting().unwrap()["dirtyBytes"], 0);
    let current = owner.run(&run).await;
    let point = owner
        .service
        .get("node-backups", current["backup"]["id"].as_str().unwrap())
        .await
        .unwrap();
    let manifest = publication::manifest(&owner.service, &point).await.unwrap();
    let bytes = publication::read_block(
        &owner.service,
        &point,
        manifest["blocks"][0]["hash"].as_str().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(&bytes[..5], b"later");
    owner
        .service
        .store
        .patch_run(&run, json!({ "status": RunStatus::Cancelled }))
        .await
        .unwrap();
    owner
        .put(
            "chats",
            json!({ "id": id(), "runId": run, "lifecycle": "trash", "trashedAt": now() }),
        )
        .await;
    publication::request(&owner.service, &run).await.unwrap();
    common::eventually(
        Duration::from_secs(15),
        Duration::from_millis(10),
        async || {
            let current = owner.run(&run).await;
            (current["backup"]["acknowledgedRevision"] == 4
                && current["backup"]["status"] == "ready")
                .then_some(())
        },
    )
    .await;
    assert_eq!(
        acknowledgements.load(Ordering::SeqCst),
        2,
        "An already published completed generation needs no new S3 publication or receipt"
    );
    owner.service.shutdown.cancel();
    maintenance.await.unwrap();
    server.abort();
}

/// A runner controller that records capture requests and never acknowledges publication.
fn capture_controller(disk: PathBuf, manifest: Value, grant: String, log: CaptureLog) -> Router {
    Router::new().fallback(move |request: Request<Body>| {
        let (disk, manifest, grant, log) =
            (disk.clone(), manifest.clone(), grant.clone(), log.clone());
        async move {
            let path = request.uri().path().to_owned();
            if request.method() == "DELETE" {
                return Json(json!({ "ok": true })).into_response();
            }
            if path.ends_with("/published") {
                return (StatusCode::SERVICE_UNAVAILABLE, "ack lost").into_response();
            }
            if path.ends_with("/snapshot") {
                let body = to_bytes(request.into_body(), 4096).await.unwrap();
                log.bodies
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
                if log.failing.load(Ordering::SeqCst) {
                    return (StatusCode::SERVICE_UNAVAILABLE, "down").into_response();
                }
                let snapshot = id();
                log.snapshots.lock().unwrap().push(snapshot.clone());
                let mut manifest = manifest;
                if log.lost_ack.load(Ordering::SeqCst) {
                    manifest["onDemand"] = true.into();
                    manifest["generation"] = 1.into();
                }
                return Json(json!({ "id": snapshot, "manifest": manifest, "grantId": grant }))
                    .into_response();
            }
            if request.method() != "GET" {
                return StatusCode::METHOD_NOT_ALLOWED.into_response();
            }
            let hash = path.rsplit('/').next().unwrap();
            log.block_reads.fetch_add(1, Ordering::SeqCst);
            snapshots::block(&disk, &manifest, hash)
                .await
                .unwrap()
                .into_response()
        }
    })
}

/// Losing the controller acknowledgement after S3 publication must preserve the new pointer.
async fn assert_lost_acknowledgement_keeps_the_new_pointer(
    owner: &Owner,
    run: &str,
    grant: &str,
    log: &CaptureLog,
    hash: &str,
) {
    let previous = owner.run(run).await["backup"]["id"].clone();
    owner
        .put(
            "node-disk-grants",
            json!({
                "id": grant,
                "runId": run,
                "nodeId": LOCAL_NODE_ID,
                "backups": [previous],
            }),
        )
        .await;
    owner
        .service
        .store
        .patch_run(run, json!({ "storage": { "mode": "on-demand" } }))
        .await
        .unwrap();
    log.lost_ack.store(true, Ordering::SeqCst);
    publication::attempt(&owner.service, &owner.run(run).await).await;
    let after = owner.run(run).await;
    assert_eq!(after["backup"]["status"], "error");
    assert!(
        after["backup"]["error"]
            .as_str()
            .unwrap()
            .contains("acknowledge")
    );
    assert_ne!(after["backup"]["id"], previous);
    let new_point = owner
        .service
        .get("node-backups", after["backup"]["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        publication::read_block(&owner.service, &new_point, hash)
            .await
            .unwrap(),
        b"workspace blocks"
    );
    publication::collect(&owner.service, run).await.unwrap();
    assert!(
        owner
            .stored("node-backups", after["backup"]["id"].as_str().unwrap())
            .await
            .is_some()
    );
}

#[tokio::test]
#[ignore = "requires loopback S3; run scripts/tests/node_s3_test.py"]
async fn active_captures_name_the_last_published_recovery_point_as_baseline() {
    let root = TempDir::new().unwrap();
    let disk = root.path().join("disk");
    std::fs::write(&disk, b"workspace blocks").unwrap();
    let manifest = controller_manifest(&disk).await;
    // The controller records each capture request body and can be told to fail.
    let log = CaptureLog::default();
    let grant_id = id();
    let runner = capture_controller(
        disk.clone(),
        manifest.clone(),
        grant_id.clone(),
        log.clone(),
    );
    let (url, _runner) = common::serve_locally(runner).await;
    let owner = Owner::with_runner(common::HOST.into(), url).await;
    let (run, attempt) = (id(), id());
    owner.add_run(&isolated_run(&run, LOCAL_NODE_ID)).await;
    owner
        .set_checkpoint(
            &run,
            json!({ "nodeId": LOCAL_NODE_ID, "runnerId": attempt }),
        )
        .await;
    let current = || owner.run(&run);
    let baseline_of = |index: usize| log.bodies.lock().unwrap()[index]["baseline"].clone();

    publication::capture(&owner.service, &current().await)
        .await
        .unwrap();
    let first = log.snapshots.lock().unwrap()[0].clone();
    assert_eq!(current().await["backup"]["snapshotId"], first);
    // An unchanged capture reuses the published S3 block without asking the
    // controller to transfer it again after its local receipt is evicted.
    let initial_reads = log.block_reads.load(Ordering::SeqCst);
    let hash = manifest["blocks"][0]["hash"].as_str().unwrap();
    let block = owner.backup_directory(&run).join("blocks").join(hash);
    std::fs::remove_dir_all(block.parent().unwrap()).unwrap();
    publication::capture(&owner.service, &current().await)
        .await
        .unwrap();
    assert_eq!(log.block_reads.load(Ordering::SeqCst), initial_reads);
    assert!(baseline_of(0).is_null());
    assert_eq!(baseline_of(1), first);

    // After a failed capture, the next one asks for a full copy.
    log.failing.store(true, Ordering::SeqCst);
    assert!(
        publication::capture(&owner.service, &current().await)
            .await
            .is_err()
    );
    log.failing.store(false, Ordering::SeqCst);
    publication::capture(&owner.service, &current().await)
        .await
        .unwrap();
    assert!(baseline_of(3).is_null(), "{:?}", log.bodies.lock().unwrap());

    // A damaged local cache is repaired from S3 without resetting the incremental baseline.
    let backup = owner
        .stored(
            "node-backups",
            current().await["backup"]["id"].as_str().unwrap(),
        )
        .await
        .unwrap();
    let baseline = current().await["backup"]["snapshotId"].clone();
    std::fs::create_dir_all(block.parent().unwrap()).unwrap();
    std::fs::write(&block, b"damaged ciphertext").unwrap();
    assert_eq!(
        publication::read_block(&owner.service, &backup, hash)
            .await
            .unwrap(),
        b"workspace blocks"
    );
    assert_eq!(current().await["backup"]["snapshotId"], baseline);
    publication::capture(&owner.service, &current().await)
        .await
        .unwrap();
    assert_eq!(baseline_of(4), baseline);
    let backup = owner
        .service
        .get(
            "node-backups",
            current().await["backup"]["id"].as_str().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        publication::read_block(&owner.service, &backup, hash)
            .await
            .unwrap(),
        b"workspace blocks"
    );
    assert_lost_acknowledgement_keeps_the_new_pointer(&owner, &run, &grant_id, &log, hash).await;
}

/// A recovery point of `run` published by `node` for disk `generation`.
fn published_point(run: &str, node: &str, generation: i64) -> Value {
    json!({
        "id": id(),
        "runId": run,
        "nodeId": node,
        "destination": "s3",
        "diskGeneration": generation,
    })
}

#[tokio::test]
async fn growing_a_disk_preserves_publication_identity_and_releases_its_previous_base() {
    let owner = Owner::new().await;
    let run = id();
    let record = json!({ "id": run });
    let token = disk_grants::new_disk(&owner.service, &record, LOCAL_NODE_ID)
        .await
        .unwrap();
    let directory = owner.root().join("disks").join(&run);
    let mut context = json!({
        "master": "http://127.0.0.1:1/",
        "grant": token,
        "policy": small_reserve(),
    });
    let stop = CancellationToken::new();
    bootstrap::prepare(&directory, 128 * 1024 * 1024, &context, &stop)
        .await
        .unwrap();
    let volume = runtime::load(&directory).await.unwrap();
    volume.seal().await.unwrap();
    let generation = volume.seal().await.unwrap();
    let previous = published_point(&run, LOCAL_NODE_ID, generation);
    disk_grants::acknowledged(&owner.service, &auth::digest(&token), &previous)
        .await
        .unwrap();
    drop(volume);
    // Each attempt supplies a fresh grant; a resize must retain the mounted
    // disk's identity so that publication retires the pins of its old base.
    context["grant"] = disk_grants::new_disk(&owner.service, &record, LOCAL_NODE_ID)
        .await
        .unwrap()
        .into();
    bootstrap::prepare(&directory, 256 * 1024 * 1024, &context, &stop)
        .await
        .unwrap();
    let volume = runtime::load(&directory).await.unwrap();
    assert_eq!(volume.disk.size(), 256 * 1024 * 1024);
    assert!(!directory.join("data.ext4").exists());
    assert!(!directory.join("resize-source").exists());
    assert_eq!(volume.source.grant_id().unwrap(), auth::digest(&token));
    let next = volume.seal().await.unwrap();
    assert!(
        next > generation,
        "publication generations must remain monotone"
    );
    let replacement = published_point(&run, LOCAL_NODE_ID, next);
    disk_grants::extend(
        &owner.service,
        &volume.source.grant_id().unwrap(),
        &replacement,
    )
    .await
    .unwrap();
    assert!(
        disk_grants::pinned(&owner.service, &run)
            .await
            .unwrap()
            .contains(previous["id"].as_str().unwrap())
    );
    disk_grants::acknowledged(
        &owner.service,
        &volume.source.grant_id().unwrap(),
        &replacement,
    )
    .await
    .unwrap();
    let pinned = disk_grants::pinned(&owner.service, &run).await.unwrap();
    assert_eq!(pinned.len(), 1);
    assert!(pinned.contains(replacement["id"].as_str().unwrap()));
}

/// Whether `credential` currently grants access to the recovery `point`.
async fn permits(owner: &Owner, credential: &str, point: &Value) -> bool {
    disk_grants::authorize(&owner.service, credential)
        .await
        .ok()
        .flatten()
        .and_then(|grant| grant["backups"].as_array().cloned())
        .is_some_and(|backups| backups.contains(&point["id"]))
}

/// Publishes three single-block S3 recovery points of `run`, one per disk generation.
async fn publish_s3_points(owner: &Owner, run: &str, node: &str) -> Vec<Value> {
    let mut points = Vec::new();
    for (generation, byte) in (1..).zip([7u8, 9u8, 11u8]) {
        let hash = hex::encode(Sha256::digest(vec![byte; 4096]));
        let mut point = published_point(run, node, generation);
        let manifest = json!({
            "version": 1,
            "size": 4096,
            "blockSize": snapshots::BLOCK,
            "blocks": [{ "offset": 0, "size": 4096, "hash": hash }],
        });
        point["bucket"] = "fixture".into();
        point["manifest"] = owner.encrypt_manifest(point["id"].as_str().unwrap(), &manifest);
        owner.put("node-backups", point.clone()).await;
        points.push(point);
    }
    points
}

#[tokio::test]
async fn mounted_disk_grants_pin_generations_and_reject_the_previous_owner() {
    let owner = Owner::new().await;
    let (run, node, next, agent) = (id(), id(), id(), id());
    for node in [&node, &next] {
        owner
            .put("nodes", json!({ "id": node, "revoked": false }))
            .await;
    }
    owner.grant_nodes(&agent, json!([node, next])).await;
    let mut record = run_record(&run, RunStatus::Succeeded);
    record["nodeId"] = node.clone().into();
    record["snapshot"] = json!({ "agent": { "id": agent } });
    owner.add_run(&record).await;
    let points = publish_s3_points(&owner, &run, &node).await;
    let credential = disk_grants::issue(&owner.service, &record, &node, &points[0])
        .await
        .unwrap();
    let grant = auth::digest(&credential);
    let pinned = async || {
        disk_grants::pinned(&owner.service, &run)
            .await
            .unwrap()
            .len()
    };
    assert!(permits(&owner, &credential, &points[0]).await);
    assert!(!permits(&owner, &credential, &points[1]).await);
    let _stale = disk_grants::issue(&owner.service, &record, &node, &points[0])
        .await
        .unwrap();
    disk_grants::extend(&owner.service, &grant, &points[1])
        .await
        .unwrap();
    assert_eq!(pinned().await, 2);
    assert!(permits(&owner, &credential, &points[1]).await);
    disk_grants::acknowledged(&owner.service, &grant, &points[1])
        .await
        .unwrap();
    assert_eq!(pinned().await, 2);
    assert!(!permits(&owner, &credential, &points[0]).await);
    // Reconciliation of a lost acknowledgement must retain an in-flight successor.
    let racing = disk_grants::issue(&owner.service, &record, &node, &points[0])
        .await
        .unwrap();
    let racing_id = auth::digest(&racing);
    disk_grants::extend(&owner.service, &racing_id, &points[1])
        .await
        .unwrap();
    disk_grants::extend(&owner.service, &racing_id, &points[2])
        .await
        .unwrap();
    disk_grants::acknowledged(&owner.service, &racing_id, &points[1])
        .await
        .unwrap();
    assert!(permits(&owner, &racing, &points[2]).await);
    disk_grants::acknowledged(&owner.service, &racing_id, &points[2])
        .await
        .unwrap();
    assert!(!permits(&owner, &racing, &points[1]).await);
    // A delayed monitor receipt must not roll authorization back behind publication.
    disk_grants::acknowledged(&owner.service, &grant, &points[0])
        .await
        .unwrap();
    disk_grants::extend(&owner.service, &grant, &points[0])
        .await
        .unwrap();
    assert!(permits(&owner, &credential, &points[1]).await);
    assert!(!permits(&owner, &credential, &points[0]).await);
    owner
        .service
        .store
        .patch_run(&run, json!({ "nodeId": next }))
        .await
        .unwrap();
    assert!(!permits(&owner, &credential, &points[1]).await);
    assert_eq!(
        owner
            .call(
                "POST",
                "/internal/node-restore/renew",
                json!({}),
                Some(&credential)
            )
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn corrupted_recovery_ciphertext_is_terminal_and_invalidates_the_baseline() {
    let owner = Owner::new().await;
    let run = id();
    let mut record = run_record(&run, RunStatus::Succeeded);
    record["backup"] = json!({ "snapshotId": "previous" });
    owner.add_run(&record).await;
    let data = b"durable conversation work";
    let hash = hex::encode(Sha256::digest(data));
    let key = format!("node-backups/{run}/blocks/{hash}");
    let mut encoded = b"CAIRNB\x01\0".to_vec();
    encoded.extend(owner.service.vault.encrypt_bytes(&key, data).unwrap());
    *encoded.last_mut().unwrap() ^= 1;
    let path = owner.service.config.data_dir.join(&key);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, encoded).unwrap();
    let error = publication::read_block(
        &owner.service,
        &json!({ "runId": run, "destination": "master" }),
        &hash,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.status, 409,
        "corrupt ciphertext must not trigger an endless 5xx retry"
    );
    assert!(!path.exists());
    assert!(owner.run(&run).await["backup"]["snapshotId"].is_null());
}

/// Publishes one master recovery point per generation: `shared` plus its own block.
/// Newer generations get older timestamps, so only the published pointer orders them.
async fn publish_master_points(
    owner: &Owner,
    run: &str,
    shared: &str,
    generations: [&String; 2],
) -> Vec<String> {
    let mut ids = Vec::new();
    for (generation, hash) in generations.into_iter().enumerate() {
        let point = id();
        let manifest = json!({
            "version": 1,
            "size": 2 * snapshots::BLOCK,
            "blockSize": snapshots::BLOCK,
            "blocks": [
                { "offset": 0, "size": snapshots::BLOCK, "hash": shared },
                { "offset": snapshots::BLOCK, "size": snapshots::BLOCK, "hash": hash },
            ],
        });
        owner
            .put(
                "node-backups",
                json!({
                    "id": point,
                    "runId": run,
                    "destination": "master",
                    "createdAt": 100 - generation,
                    "manifest": owner.encrypt_manifest(&point, &manifest),
                }),
            )
            .await;
        ids.push(point);
    }
    ids
}

#[tokio::test]
async fn publication_cleanup_preserves_current_and_in_use_blocks_then_reclaims_idle_generations() {
    let owner = Owner::new().await;
    let run = id();
    owner.add_run(&run_record(&run, RunStatus::Succeeded)).await;
    let directory = owner.backup_directory(&run).join("blocks");
    std::fs::create_dir_all(&directory).unwrap();
    let shared = hex::encode(Sha256::digest(b"shared"));
    let old = hex::encode(Sha256::digest(b"old"));
    let new = hex::encode(Sha256::digest(b"new"));
    for hash in [&shared, &old, &new] {
        std::fs::write(directory.join(hash), b"fixture").unwrap();
    }
    let ids = publish_master_points(&owner, &run, &shared, [&old, &new]).await;
    owner
        .service
        .store
        .patch_run(&run, json!({ "backup": { "id": ids[1] } }))
        .await
        .unwrap();
    let grant = id();
    owner
        .put(
            "node-disk-grants",
            json!({ "id": grant, "runId": run, "backups": [ids[0]] }),
        )
        .await;
    owner
        .service
        .store
        .set("node-backup-settings", json!({ "retention": 100 }), None)
        .await
        .unwrap();
    assert!(
        publication::settings(&owner.service)
            .await
            .unwrap()
            .get("retention")
            .is_none()
    );
    publication::collect(&owner.service, &run).await.unwrap();
    assert_eq!(
        owner
            .service
            .store
            .node_backups_for_run(&run)
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(directory.join(&old).exists());
    owner
        .service
        .store
        .write(move |db| db.remove("node-disk-grants", &grant))
        .await
        .unwrap();
    publication::collect(&owner.service, &run).await.unwrap();
    let points = owner
        .service
        .store
        .node_backups_for_run(&run)
        .await
        .unwrap();
    assert_eq!(points.len(), 1);
    assert_eq!(
        points[0]["id"], ids[1],
        "Published pointer wins over timestamp ordering"
    );
    assert!(directory.join(shared).exists());
    assert!(directory.join(new).exists());
    assert!(!directory.join(old).exists());
}

/// Runs `aws s3api` against the loopback bucket.
fn s3api(args: &[&str]) -> std::process::Output {
    std::process::Command::new("aws")
        .arg("s3api")
        .args(args)
        .output()
        .unwrap()
}

#[tokio::test]
#[ignore = "requires loopback S3; run scripts/tests/node_s3_test.py"]
async fn obsolete_object_collection_deletes_versions_without_deleting_prefix_neighbors() {
    let owner = Owner::new().await;
    assert!(std::env::var_os("CAIRN_NODE_TEST_S3_ENDPOINT").is_some());
    let storage = Storage::configured(&owner.service).unwrap();
    let versioning = s3api(&[
        "put-bucket-versioning",
        "--bucket",
        "cairn-node-test",
        "--versioning-configuration",
        "Status=Enabled",
    ]);
    assert!(versioning.status.success());
    let key = format!("collection/{}", id());
    let neighbor = format!("{key}-neighbor");
    storage.upload_bytes(b"first".to_vec(), &key).await.unwrap();
    storage
        .upload_bytes(b"second".to_vec(), &key)
        .await
        .unwrap();
    storage
        .upload_bytes(b"neighbor".to_vec(), &neighbor)
        .await
        .unwrap();
    let marker = s3api(&[
        "delete-object",
        "--bucket",
        "cairn-node-test",
        "--key",
        &key,
    ]);
    assert!(marker.status.success());
    storage.purge_key(&key).await.unwrap();
    let listing = s3api(&[
        "list-object-versions",
        "--bucket",
        "cairn-node-test",
        "--prefix",
        &key,
        "--output",
        "json",
    ]);
    assert!(listing.status.success());
    let listing: Value = serde_json::from_slice(&listing.stdout).unwrap();
    assert!(
        listing["DeleteMarkers"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    let versions = listing["Versions"].as_array().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0]["Key"], neighbor);
    assert_eq!(
        storage.download_bytes(&neighbor, 8).await.unwrap(),
        b"neighbor"
    );
    storage.purge_key(&neighbor).await.unwrap();
}

/// A runner controller whose snapshot waits for `resume` after signalling `captured`.
fn paused_capture_controller(
    disk: PathBuf,
    manifest: Value,
    captured: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
) -> Router {
    Router::new().fallback(move |request: Request<Body>| {
        let (captured, resume, disk, manifest) = (
            captured.clone(),
            resume.clone(),
            disk.clone(),
            manifest.clone(),
        );
        async move {
            if request.method() == "DELETE" {
                return Json(json!({})).into_response();
            }
            if request.uri().path().ends_with("/snapshot") {
                captured.notify_one();
                resume.notified().await;
                return Json(json!({ "id": id(), "manifest": manifest })).into_response();
            }
            if request.method() != "GET" {
                return StatusCode::METHOD_NOT_ALLOWED.into_response();
            }
            snapshots::block(
                &disk,
                &manifest,
                request.uri().path().rsplit('/').next().unwrap(),
            )
            .await
            .unwrap()
            .into_response()
        }
    })
}

#[tokio::test]
#[ignore = "requires loopback S3; run scripts/tests/node_s3_test.py"]
async fn interrupted_first_publication_is_collected_after_restart_without_another_write() {
    for explicit_purge in [false, true] {
        let root = TempDir::new().unwrap();
        let disk = root.path().join("disk");
        std::fs::write(&disk, b"uncommitted publication").unwrap();
        let manifest = snapshots::index(&disk).await.unwrap();
        let captured = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let runner =
            paused_capture_controller(disk, manifest.clone(), captured.clone(), resume.clone());
        let (url, server) = common::serve_locally(runner).await;
        let owner = Owner::with_runner(common::HOST.into(), url).await;
        let (run, attempt) = (id(), id());
        let record = json!({
            "id": run,
            "taskId": run,
            "createdAt": 0,
            "status": RunStatus::Succeeded,
            "sessionId": "session",
            "nodeId": LOCAL_NODE_ID,
        });
        owner.add_run(&record).await;
        owner
            .set_checkpoint(
                &run,
                json!({ "nodeId": LOCAL_NODE_ID, "runnerId": attempt }),
            )
            .await;
        let service = owner.service.clone();
        let capture = tokio::spawn(async move { publication::capture(&service, &record).await });
        captured.notified().await;
        // The S3 writes finish, but the ownership fence rejects the database commit.
        owner
            .set_checkpoint(&run, json!({ "nodeId": LOCAL_NODE_ID, "runnerId": id() }))
            .await;
        resume.notify_one();
        assert_eq!(capture.await.unwrap().unwrap_err().status, 409);
        assert!(
            owner
                .service
                .store
                .node_backups_for_run(&run)
                .await
                .unwrap()
                .is_empty()
        );
        let directory = owner.backup_directory(&run);
        let intent = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .unwrap()
            .path();
        let pending: Value = serde_json::from_slice(&std::fs::read(&intent).unwrap()).unwrap();
        let object = format!(
            "node-backups/{run}/{}.json",
            pending["id"].as_str().unwrap()
        );
        let pending_manifest = publication::manifest(&owner.service, &pending)
            .await
            .unwrap();
        let block = shared_object_key(
            &pending_manifest,
            manifest["blocks"][0]["hash"].as_str().unwrap(),
        );
        let storage = Storage::configured(&owner.service).unwrap();
        assert!(storage.download_bytes(&object, 65536).await.is_ok());
        assert!(storage.download_bytes(&block, 65536).await.is_ok());
        // Missing receipts model an interruption between remote PUT and local verification receipt.
        std::fs::remove_dir_all(directory.join("blocks")).unwrap();
        let restarted = Service::new(owner.service.config.clone()).await.unwrap();
        if explicit_purge {
            publication::purge(&restarted, &run).await.unwrap();
            assert!(!directory.exists());
        } else {
            let maintenance = tokio::spawn(publication::maintain(restarted.clone()));
            tokio::time::timeout(Duration::from_secs(30), async {
                while intent.exists() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .unwrap();
            restarted.shutdown.cancel();
            maintenance.await.unwrap();
        }
        drain_remote_deletions(&restarted).await;
        for key in [&object, &block] {
            assert_eq!(
                storage.download_bytes(key, 65536).await.unwrap_err().status,
                409
            );
        }
        server.abort();
    }
}

/// A succeeded run owned by the local node, whose task shares its id.
fn local_run(run: &str) -> Value {
    json!({
        "id": run,
        "taskId": run,
        "createdAt": 0,
        "status": RunStatus::Succeeded,
        "nodeId": LOCAL_NODE_ID,
    })
}

/// Stores `record` with a checkpoint that places it on the local node.
async fn add_local_run(owner: &Owner, record: &Value) {
    owner.add_run(record).await;
    owner
        .set_checkpoint(
            record["id"].as_str().unwrap(),
            json!({ "nodeId": LOCAL_NODE_ID, "runnerId": id() }),
        )
        .await;
}

/// Gates of the shared publication controller.
#[derive(Clone, Default)]
struct SharedPublicationGates {
    /// Block reads served from the source disk.
    requests: Arc<AtomicUsize>,
    /// Holds the next snapshot until `resume_capture` once set.
    pause: Arc<AtomicBool>,
    entered_capture: Arc<tokio::sync::Notify>,
    resume_capture: Arc<tokio::sync::Notify>,
    /// Makes two block reads wait for each other.
    racing: Arc<AtomicBool>,
}

/// A runner controller serving `disk` one block at a time, never in batches.
fn shared_publication_controller(
    disk: PathBuf,
    manifest: Arc<tokio::sync::Mutex<Value>>,
    gates: SharedPublicationGates,
) -> Router {
    let upload_barrier = Arc::new(tokio::sync::Barrier::new(2));
    Router::new().fallback(move |request: Request<Body>| {
        let (disk, manifest, gates, upload_barrier) = (
            disk.clone(),
            manifest.clone(),
            gates.clone(),
            upload_barrier.clone(),
        );
        async move {
            if request.method() == "DELETE" {
                return Json(json!({})).into_response();
            }
            if request.uri().path().ends_with("/snapshot") {
                if gates.pause.swap(false, Ordering::SeqCst) {
                    gates.entered_capture.notify_one();
                    gates.resume_capture.notified().await;
                }
                let manifest = manifest.lock().await.clone();
                return Json(json!({ "id": id(), "manifest": manifest })).into_response();
            }
            if request.uri().path().ends_with("/blocks")
                || request.uri().path().ends_with("/publication")
            {
                return StatusCode::NOT_FOUND.into_response();
            }
            gates.requests.fetch_add(1, Ordering::SeqCst);
            if gates.racing.load(Ordering::SeqCst) {
                upload_barrier.wait().await;
            }
            snapshots::block(
                &disk,
                &*manifest.lock().await,
                request.uri().path().rsplit('/').next().unwrap(),
            )
            .await
            .unwrap()
            .into_response()
        }
    })
}

#[tokio::test]
#[ignore = "requires loopback S3; run scripts/tests/node_s3_test.py"]
async fn shared_publications_reuse_across_runs_migrate_legacy_and_ignore_slow_cleanup() {
    let root = TempDir::new().unwrap();
    let disk = root.path().join("source");
    let original = vec![42u8; 4 * MIB];
    std::fs::write(&disk, &original).unwrap();
    let manifest = snapshots::index(&disk).await.unwrap();
    let state = Arc::new(tokio::sync::Mutex::new(manifest.clone()));
    let gates = SharedPublicationGates::default();
    let runner = shared_publication_controller(disk.clone(), state.clone(), gates.clone());
    let (url, server) = common::serve_locally(runner).await;
    let owner = Owner::with_runner(common::HOST.into(), url).await;
    let s = &owner.service;
    let new_run = async || {
        let record = local_run(&id());
        add_local_run(&owner, &record).await;
        record
    };

    let a = new_run().await;
    let b = new_run().await;
    let start = std::time::Instant::now();
    let first = publication::capture(s, &a).await.unwrap();
    let first_ms = start.elapsed().as_secs_f64() * 1000.;
    let start = std::time::Instant::now();
    let second = publication::capture(s, &b).await.unwrap();
    let reused_ms = start.elapsed().as_secs_f64() * 1000.;
    assert_eq!(first["uploadedBytes"], original.len());
    assert_eq!(second["uploadedBytes"], 0);
    assert_eq!(
        gates.requests.load(Ordering::SeqCst),
        1,
        "Cross-run reuse does not request source bytes"
    );
    let first = s
        .get("node-backups", first["id"].as_str().unwrap())
        .await
        .unwrap();
    let second = s
        .get("node-backups", second["id"].as_str().unwrap())
        .await
        .unwrap();
    let first_manifest = publication::manifest(s, &first).await.unwrap();
    let second_manifest = publication::manifest(s, &second).await.unwrap();
    assert_eq!(first_manifest["blocks"], second_manifest["blocks"]);
    let hash = manifest["blocks"][0]["hash"].as_str().unwrap();
    let object = shared_object_key(&first_manifest, hash);
    assert_eq!(
        publication::read_block(s, &second, hash).await.unwrap(),
        original
    );
    eprintln!(
        "Shared publication 4MiB: unique={first_ms:.2}ms reused={reused_ms:.2}ms; source requests=1"
    );

    // A blocked prefix purge runs on its own worker. It holds no publication or
    // disk-reader lock, and removing one owner cannot delete another owner's block.
    let entered = root.path().join("delete-entered");
    let release = root.path().join("delete-release");
    let wrapper = root.path().join("slow-aws");
    let script = format!(
        "#!/usr/bin/env python3
import pathlib,time,subprocess,sys
pathlib.Path({}).touch()
while not pathlib.Path({}).exists(): time.sleep(.01)
sys.exit(subprocess.call(['aws']+sys.argv[1:]))
",
        serde_json::to_string(&entered).unwrap(),
        serde_json::to_string(&release).unwrap(),
    );
    std::fs::write(&wrapper, script).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "cairn-node-test", "awsBinary": wrapper }).to_string(),
    )
    .unwrap();
    publication::purge(s, a["id"].as_str().unwrap())
        .await
        .unwrap();
    let service = s.clone();
    let gc = tokio::spawn(async move { shared_blocks::collect(&service).await });
    tokio::time::timeout(Duration::from_secs(10), async {
        while !entered.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let c = new_run().await;
    let third = tokio::time::timeout(Duration::from_secs(5), publication::capture(s, &c))
        .await
        .expect("Slow deletion must not block publication")
        .unwrap();
    assert_eq!(third["uploadedBytes"], 0);
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(5),
            publication::read_block(s, &second, hash)
        )
        .await
        .unwrap()
        .unwrap(),
        original
    );
    assert!(!gc.is_finished());
    std::fs::write(&release, b"resume").unwrap();
    gc.await.unwrap().unwrap();

    publication::purge(s, b["id"].as_str().unwrap())
        .await
        .unwrap();
    drain_remote_deletions(s).await;
    assert!(
        Storage::configured(s)
            .unwrap()
            .download_bytes(&object, snapshots::BLOCK + 36)
            .await
            .is_ok()
    );
    publication::purge(s, c["id"].as_str().unwrap())
        .await
        .unwrap();
    drain_remote_deletions(s).await;
    assert_eq!(
        Storage::configured(s)
            .unwrap()
            .download_bytes(&object, snapshots::BLOCK + 36)
            .await
            .unwrap_err()
            .status,
        409
    );
    std::fs::remove_file(s.config.data_dir.join("storage-s3.json")).unwrap();

    // A cancelled caller cannot release reservations while its owned capture is
    // running. The operation completes before another collector can retire it.
    let cancelled = new_run().await;
    let cancelled_id = cancelled["id"].as_str().unwrap();
    gates.pause.store(true, Ordering::SeqCst);
    let (service, record) = (s.clone(), cancelled.clone());
    let caller = tokio::spawn(async move { publication::capture(&service, &record).await });
    gates.entered_capture.notified().await;
    caller.abort();
    let _ = caller.await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            s.node_backup_operation.lock(cancelled_id)
        )
        .await
        .is_err()
    );
    gates.resume_capture.notify_one();
    let guard = tokio::time::timeout(
        Duration::from_secs(10),
        s.node_backup_operation.lock(cancelled_id),
    )
    .await
    .unwrap();
    drop(guard);
    let committed = s.store.run(cancelled_id).await.unwrap();
    let point = s
        .get("node-backups", committed["backup"]["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        publication::read_block(s, &point, hash).await.unwrap(),
        original
    );
    publication::purge(s, cancelled_id).await.unwrap();
    drain_remote_deletions(s).await;

    // Both disks reserve the same unseen hash before either source responds.
    // Their first uploads must converge while preserving independent disk locks.
    let parallel_a = new_run().await;
    let parallel_b = new_run().await;
    gates.racing.store(true, Ordering::SeqCst);
    let (a, b) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(
            publication::capture(s, &parallel_a),
            publication::capture(s, &parallel_b)
        )
    })
    .await
    .unwrap();
    gates.racing.store(false, Ordering::SeqCst);
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a["uploadedBytes"], original.len());
    assert_eq!(b["uploadedBytes"], original.len());
    let a = s
        .get("node-backups", a["id"].as_str().unwrap())
        .await
        .unwrap();
    let b = s
        .get("node-backups", b["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        publication::manifest(s, &a).await.unwrap()["blocks"],
        publication::manifest(s, &b).await.unwrap()["blocks"]
    );
    publication::purge(s, parallel_a["id"].as_str().unwrap())
        .await
        .unwrap();
    drain_remote_deletions(s).await;
    assert_eq!(
        publication::read_block(s, &b, hash).await.unwrap(),
        original
    );
    publication::purge(s, parallel_b["id"].as_str().unwrap())
        .await
        .unwrap();
    drain_remote_deletions(s).await;

    // Legacy incremental captures can omit unchanged bytes on the node. Migrate
    // them from their old authenticated object instead of requesting absent data.
    let legacy_run = new_run().await;
    let run_id = legacy_run["id"].as_str().unwrap();
    let point_id = id();
    let legacy_bytes = b"legacy unchanged block";
    std::fs::write(&disk, legacy_bytes).unwrap();
    let mut legacy_manifest = snapshots::index(&disk).await.unwrap();
    legacy_manifest["incremental"] = true.into();
    let hash = legacy_manifest["blocks"][0]["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let old_key = format!("node-backups/{run_id}/blocks/{hash}");
    let mut encoded = b"CAIRNB\x01\0".to_vec();
    encoded.extend(s.vault.encrypt_bytes(&old_key, legacy_bytes).unwrap());
    Storage::configured(s)
        .unwrap()
        .upload_bytes(encoded, &old_key)
        .await
        .unwrap();
    let legacy = json!({
        "id": point_id,
        "runId": run_id,
        "destination": "s3",
        "bucket": "cairn-node-test",
        "endpoint": null,
        "manifest": owner.encrypt_manifest(&point_id, &legacy_manifest),
    });
    owner.put("node-backups", legacy.clone()).await;
    s.store
        .patch_run(
            run_id,
            json!({ "backup": { "id": point_id, "snapshotId": id() } }),
        )
        .await
        .unwrap();
    let directory = owner.backup_directory(run_id);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(format!("{point_id}.json")),
        legacy.to_string(),
    )
    .unwrap();
    *state.lock().await = legacy_manifest;

    let before = gates.requests.load(Ordering::SeqCst);
    let migrated = publication::capture(s, &owner.run(run_id).await)
        .await
        .unwrap();
    assert_eq!(migrated["blockFormat"], "shared-v1");
    assert_eq!(gates.requests.load(Ordering::SeqCst), before);
    let migrated = s
        .get("node-backups", migrated["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        publication::read_block(s, &migrated, &hash).await.unwrap(),
        legacy_bytes
    );
    drain_remote_deletions(s).await;
    assert_eq!(
        Storage::configured(s)
            .unwrap()
            .download_bytes(&old_key, 65536)
            .await
            .unwrap_err()
            .status,
        409
    );
    publication::purge(s, run_id).await.unwrap();
    drain_remote_deletions(s).await;
    server.abort();
}

/// A local-node owner whose runner is `runner` and whose S3 is never reached.
async fn owner_without_reachable_s3(runner: Router) -> (Owner, tokio::task::JoinHandle<()>) {
    let (url, server) = common::serve_locally(runner).await;
    let owner = Owner::with_runner(common::HOST.into(), url).await;
    std::fs::write(
        owner.service.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "fixture-never-contacted" }).to_string(),
    )
    .unwrap();
    (owner, server)
}

#[tokio::test]
async fn stalled_sync_does_not_block_other_conversations_and_same_disk_stays_fenced() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (signal, wait) = (entered.clone(), release.clone());
    let runner = Router::new().fallback(move || {
        let (signal, wait) = (signal.clone(), wait.clone());
        async move {
            signal.notify_one();
            wait.notified().await;
            StatusCode::SERVICE_UNAVAILABLE
        }
    });
    let (owner, server) = owner_without_reachable_s3(runner).await;
    let (first, second) = (id(), id());
    for run in [&first, &second] {
        add_local_run(&owner, &local_run(run)).await;
    }

    let s = owner.service.clone();
    let record = owner.run(&first).await;
    let pending = tokio::spawn(async move { publication::capture(&s, &record).await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let unrelated = tokio::time::timeout(
        Duration::from_millis(500),
        moves::latest(&owner.service, &second),
    )
    .await;
    let same = tokio::time::timeout(
        Duration::from_millis(50),
        moves::latest(&owner.service, &first),
    )
    .await;
    release.notify_one();
    assert!(pending.await.unwrap().is_err());
    server.abort();

    assert!(
        unrelated
            .expect("another conversation must not wait for this transfer")
            .unwrap()
            .is_none()
    );
    assert!(same.is_err(), "the same disk must remain fenced");
}

#[tokio::test]
async fn synchronization_scheduler_starts_two_disks_and_leaves_the_third_queued() {
    let (entered, mut requests) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let wait = release.clone();
    let runner = Router::new().fallback(move |request: Request<Body>| {
        let (entered, wait) = (entered.clone(), wait.clone());
        async move {
            entered.send(request.uri().path().to_owned()).unwrap();
            wait.acquire().await.unwrap().forget();
            StatusCode::SERVICE_UNAVAILABLE
        }
    });
    let (owner, server) = owner_without_reachable_s3(runner).await;
    for _ in 0..3 {
        let mut record = local_run(&id());
        record["isolated"] = true.into();
        record["sessionId"] = "fixture".into();
        record["storage"] = json!({ "mode": "on-demand", "dirtyBytes": 4096 });
        add_local_run(&owner, &record).await;
    }

    let scheduler = tokio::spawn(publication::maintain(owner.service.clone()));
    let first = tokio::time::timeout(Duration::from_secs(8), requests.recv())
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first, second);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), requests.recv())
            .await
            .is_err()
    );

    release.add_permits(1);
    let third = tokio::time::timeout(Duration::from_secs(8), requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(third, first);
    assert_ne!(third, second);

    owner.service.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(2), scheduler)
        .await
        .unwrap()
        .unwrap();
    release.add_permits(3);
    server.abort();
}

#[tokio::test]
async fn slot_shortage_reports_free_slots_and_check_reserves_nothing() {
    let owner = Owner::new().await;
    let (node, agent) = (id(), id());
    let mut record = schedulable_node(&node, &limits(7, 16384, 131072));
    record["name"] = "Server".into();
    record["slots"] = 1.into();
    owner.put("nodes", record).await;
    owner.grant_nodes(&agent, json!([node])).await;
    let attempt = id();
    placement::reserve(&owner.service, &agent_run(&id(), &agent), &attempt)
        .await
        .unwrap();
    let another = agent_run(&id(), &agent);
    let error = placement::check(&owner.service, &another)
        .await
        .unwrap_err();
    assert!(placement::is_no_capacity(&error));
    assert!(
        error.message.contains("Server has 0 free slots"),
        "{}",
        error.message
    );
    assert_eq!(
        owner
            .service
            .store
            .list("node-attempts")
            .await
            .unwrap()
            .len(),
        1
    );
    placement::release(&owner.service, &attempt).await.unwrap();
    placement::check(&owner.service, &another).await.unwrap();
    assert_eq!(
        owner
            .service
            .store
            .list("node-attempts")
            .await
            .unwrap()
            .len(),
        1
    );
    let (_, inventory) = owner.get("/api/nodes").await;
    let node = find_by_id(&inventory, &node);
    assert_eq!(node["availableSlots"], 1);
    assert!(node.get("reserved").is_none());
}
