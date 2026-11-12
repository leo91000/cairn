use common::McpFixture;
use common::StoreAgentFixture;
use common::attempt_fixture::*;
use common::node_fixture::*;
use common::run_fixture::DbRunFixture;
use common::run_fixture::*;

#[path = "nodes/backend_migration.rs"]
mod backend_migration;
mod common;

use cairn_installation::plugins::run_credentials::RunCredentials;
use common::relay_fixture::router;

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, request::Builder},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use cairn_installation::{
    auth,
    config::{Config, MAIN_AGENT_ID, id, now},
    microvm::wire,
    nodes::{LOCAL_NODE_ID, alerts, executor, files, moves, placement, workspace},
    recovery,
    run_status::RunStatus,
    service::Service,
    storage::policy::Policy,
    validation::text,
};
use common::RelayContext;
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
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
            concurrency: 4,
            runner_url,
            ..common::config(root.path())
        })
        .await
        .unwrap();
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
        mut body: Value,
        token: Option<&str>,
    ) -> (StatusCode, Value) {
        if path == HEARTBEAT {
            let fields = body.as_object_mut().unwrap();
            fields.entry("nodeProtocol").or_insert(json!(2));
            fields.entry("runtimeId").or_insert(json!("development"));
        }
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
        let body = if method == "PUT" {
            common::node_fixture::configuration(&self.service, path, body)
                .await
                .unwrap()
        } else {
            body
        };
        let body = if matches!(method, "POST" | "PUT")
            && (path == "/api/agents" || path.starts_with("/api/agents/"))
            && body.get("settings").is_none()
        {
            let existing = (method == "PUT").then(|| path.trim_start_matches("/api/agents/"));
            common::agent_fixture::agent_command(&self.service, body, existing)
                .await
                .unwrap()
        } else {
            body
        };
        self.call(method, path, body, None).await
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let (status, value) = self.send("GET", path, Value::Null).await;
        (
            status,
            if path == "/api/agents" {
                value["agents"].clone()
            } else {
                value
            },
        )
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

    async fn put_attempt_fixture(&self, value: Value) {
        self.service.store.put_attempt_fixture(value).await.unwrap();
    }

    async fn put_node_fixture(&self, value: Value) {
        self.service.store.put_node_fixture(value).await.unwrap();
    }

    async fn save_conversation_boundary(&self, document: Value) {
        self.service
            .store
            .save_conversation_boundary(document)
            .await
            .unwrap();
    }

    async fn put(&self, kind: &str, document: Value) {
        if kind == "node-attempts" {
            self.service
                .store
                .put_attempt_fixture(document)
                .await
                .unwrap();
            return;
        }
        if kind == "agents" {
            self.service
                .store
                .put_agent_fixture(document)
                .await
                .unwrap();
        } else {
            self.service.store.put(kind, document).await.unwrap();
        }
    }

    async fn stored(&self, kind: &str, id: &str) -> Option<Value> {
        if kind == "node-attempts" {
            return self.service.store.attempt_fixture(id).await.unwrap();
        }
        if kind == "agents" {
            self.service.store.agent_snapshot(id).await.unwrap()
        } else if kind == "nodes" {
            self.service.store.node_boundary(id).await.unwrap()
        } else if kind == "chats" {
            self.service.store.conversation_boundary(id).await.unwrap()
        } else {
            self.service.store.get(kind, id).await.unwrap()
        }
    }

    async fn run(&self, id: &str) -> Value {
        self.service.store.run(id).await.unwrap()
    }

    async fn add_run_fixture(&self, record: &Value) {
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
        self.put_node_fixture(json!({ "id": node, "revoked": false }))
            .await;
        self.authorize_node(node, token).await;
    }

    async fn grant_nodes(&self, agent: &str, nodes: Value) {
        self.put(
            "agents",
            json!({ "id": agent, "provider": "codex", "access": { "nodes": nodes } }),
        )
        .await;
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
        "runtimeId": "development",
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
        "runtimeId": "development",
        "sharedResources": true,
        "lastSeen": now(),
        "capabilities": { "kvm": true },
        "limits": limits,
    })
}

/// A run owned by `agent`, as seen by placement.
fn agent_run(run: &str, agent: &str) -> Value {
    json!({ "id": run, "snapshot": { "agent": { "id": agent, "provider": "codex" } } })
}

/// A persisted run record in `status`.
fn run_record(run: &str, status: RunStatus) -> Value {
    json!({
        "id": run,
        "taskId": "fixture",
        "createdAt": 0,
        "status": status,
        "nativeProvenanceUnknown": true,
        "nativePrincipals": [],
        "nativeAuthorityNodes": [],
    })
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

async fn wait_for_path(path: &Path) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
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
    owner.put_node_fixture(record).await;
    owner.authorize_node(&node, token).await;
    let mut heartbeat = json!({
        "runtimeId": "development",
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
    let retained_run = id();
    heartbeat["pool"] = json!({ "quarantined": true, "retained_runs": [retained_run] });
    owner
        .call("POST", HEARTBEAT, heartbeat.clone(), Some(token))
        .await;
    assert_eq!(
        owner.stored("nodes", &node).await.unwrap()["executionReady"],
        false,
        "A quarantined pool cannot admit work even if executionReady was stale"
    );
    assert_eq!(
        owner.stored("nodes", &node).await.unwrap()["retainedRuns"],
        json!([retained_run])
    );
    heartbeat["pool"]["quarantined"] = false.into();
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
    assert_eq!(identity["nodeId"], invitation["nodeId"]);
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
            json!({ "runtimeId": "development" }),
            Some(token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let node = identity["nodeId"].as_str().unwrap();
    let (status, _) = owner
        .send(
            "POST",
            &format!("/api/nodes/{node}/revoke"),
            json!({ "discardWorkingFiles": true }),
        )
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
async fn component_version_mismatches_are_explicitly_rejected() {
    let owner = Owner::new().await;
    let invitation = owner.invite("Versioned").await;
    let mut request = enrollment(
        &invitation,
        "Versioned",
        &capabilities(true, 8, 16384, 65536),
    );
    request["runtimeId"] = "other-version".into();
    let (status, error) = owner.call("POST", ENROLL, request.clone(), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    request["runtimeId"] = "development".into();
    let (status, identity) = owner.call("POST", ENROLL, request, None).await;
    assert_eq!(status, StatusCode::OK, "{identity}");
    let node_id = identity["nodeId"].as_str().unwrap();
    let mut node = owner.stored("nodes", node_id).await.unwrap();
    node["slots"] = 2.into();
    node["accepting"] = true.into();
    let valid = json!({
        "runtimeId": "development", "nodeProtocol": 2, "executionReady": true,
        "sharedResources": true, "dataRoot": owner.service.config.data_dir,
        "budget": { "slots": 2, "limits": node["limits"] },
    });
    owner.put_node_fixture(node).await;
    owner.grant_nodes(MAIN_AGENT_ID, json!([node_id])).await;
    for request in [
        json!({ "runtimeId": "other-version", "nodeProtocol": 2 }),
        json!({ "runtimeId": "development", "nodeProtocol": 3 }),
        json!({ "runtimeId": "development", "nodeProtocol": null, "executionReady": false }),
        json!({ "runtimeId": "other-version", "nodeProtocol": 2, "capabilities": { "backend": "firecracker" } }),
    ] {
        assert_eq!(
            owner
                .call("POST", HEARTBEAT, valid.clone(), identity["token"].as_str())
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            owner.stored("nodes", node_id).await.unwrap()["executionReady"],
            true
        );
        let (run_id, reservation, attempt) = (id(), id(), id());
        let mut run = run_record(&run_id, RunStatus::Queued);
        run["taskId"] = run_id.clone().into();
        run["snapshot"] = json!({ "agent": { "id": MAIN_AGENT_ID, "provider": "codex" } });
        run["isolated"] = true.into();
        run["resources"] = limits(1, 512, 1024);
        run["placementTransition"] = true.into();
        owner.add_run_fixture(&run).await;
        placement::reserve(&owner.service, &run, &reservation)
            .await
            .unwrap();
        run["placementTransition"] = false.into();
        run["moveReservation"] = reservation.clone().into();
        owner
            .service
            .store
            .update_run_boundary_fixture(
                &run_id,
                json!({
                    "placementTransition": false, "moveReservation": reservation,
                }),
            )
            .await
            .unwrap();
        let (status, error) = owner
            .call("POST", HEARTBEAT, request, identity["token"].as_str())
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(
            owner.stored("nodes", node_id).await.unwrap()["executionReady"],
            false,
            "A rejected or unavailable controller must not retain its previous readiness"
        );
        let error = placement::reserve(&owner.service, &run, &attempt)
            .await
            .unwrap_err();
        assert!(error.message.contains("not ready"), "{error:?}");
        assert_eq!(
            owner.stored("node-attempts", &reservation).await.unwrap()["released"],
            false
        );
        assert!(owner.stored("node-attempts", &attempt).await.is_none());
        assert_eq!(owner.run(&run_id).await["moveReservation"], reservation);
        assert_eq!(
            owner
                .call("POST", HEARTBEAT, valid.clone(), identity["token"].as_str())
                .await
                .0,
            StatusCode::OK
        );
        placement::reserve(&owner.service, &run, &attempt)
            .await
            .unwrap();
        placement::release(&owner.service, &attempt).await.unwrap();
    }
}

#[tokio::test]
async fn enrollment_identity_tracks_the_exact_invitation() {
    let owner = Owner::new().await;
    let first = owner.invite("Workstation").await;
    let second = owner.invite("Workstation").await;
    assert_ne!(first["nodeId"], second["nodeId"]);
    for invitation in [second, first] {
        let (status, identity) = owner
            .call(
                "POST",
                ENROLL,
                enrollment(
                    &invitation,
                    "Workstation",
                    &capabilities(true, 8, 16384, 65536),
                ),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{identity}");
        assert_eq!(identity["nodeId"], invitation["nodeId"]);
    }
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
                .json(&json!({ "runtimeId": "development", "nodeProtocol": 2 }));
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
                .send(
                    "POST",
                    &format!("/api/nodes/{node}/revoke"),
                    json!({ "discardWorkingFiles": true })
                )
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
    let (status, task) = owner
        .send("POST", "/api/tasks", json!({ "settings": task }))
        .await;
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
    for wildcard in [false, true] {
        revocation_clears_agent_preferences(wildcard).await;
    }
}

async fn revocation_clears_agent_preferences(wildcard: bool) {
    let owner = Owner::new().await;
    let identity = owner
        .enroll("Revocation", &capabilities(true, 2, 4096, 32768))
        .await;
    let node = identity["nodeId"].as_str().unwrap();
    let main = format!("/api/agents/{MAIN_AGENT_ID}");
    let selection = if wildcard {
        Value::Null
    } else {
        json!([LOCAL_NODE_ID, node])
    };
    let grant = json!({ "name": "Main", "access": { "nodes": selection }, "nodePriorities": [node, LOCAL_NODE_ID] });
    assert_eq!(owner.send("PUT", &main, grant).await.0, StatusCode::OK);
    let before = owner.service.agent_snapshot(MAIN_AGENT_ID).await.unwrap()["revision"].clone();
    let (status, _) = owner
        .send(
            "POST",
            &format!("/api/nodes/{node}/revoke"),
            json!({ "discardWorkingFiles": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, agents) = owner.get("/api/agents").await;
    let mut main_agent = find_by_id(&agents, MAIN_AGENT_ID);
    assert_eq!(
        main_agent["access"]["nodes"],
        if wildcard {
            Value::Null
        } else {
            json!([LOCAL_NODE_ID])
        }
    );
    assert_eq!(main_agent["nodePriorities"], json!([LOCAL_NODE_ID]));
    assert_ne!(main_agent["revision"], before);
    main_agent["name"] = "Renamed after revocation".into();
    assert_eq!(owner.send("PUT", &main, main_agent).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_node_keeps_the_execution_backend_it_enrolled_with() {
    let owner = Owner::new().await;
    // Native heartbeats refresh the trusted people first; keep that local to the test.
    owner
        .service
        .store
        .set(
            "native-authority",
            json!({ "observedAt": now(), "accounts": {} }),
            None,
        )
        .await
        .unwrap();
    let vm = capabilities(true, 8, 16384, 65536);
    let mut native = capabilities(false, 8, 16384, 65536);
    native["backend"] = "native".into();
    native["ublk"] = true.into();

    let identity = owner.enroll("Workstation", &vm).await;
    let token = identity["token"].as_str().unwrap();
    let node = identity["nodeId"].as_str().unwrap();
    let (status, body) = owner
        .call(
            "POST",
            HEARTBEAT,
            json!({ "capabilities": native }),
            Some(token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let stored = owner.stored("nodes", node).await.unwrap();
    assert_eq!(stored["capabilities"]["backend"], "firecracker");
    let (status, body) = owner
        .call(
            "POST",
            HEARTBEAT,
            json!({ "capabilities": vm }),
            Some(token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, invitation) = owner
        .send(
            "POST",
            "/api/nodes/enrollments",
            json!({ "name": "Desktop", "backend": "native" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{invitation}");
    let (status, identity) = owner
        .call(
            "POST",
            ENROLL,
            enrollment(&invitation, "Desktop", &native),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{identity}");
    let token = identity["token"].as_str().unwrap();
    let node = identity["nodeId"].as_str().unwrap();
    let (status, body) = owner
        .call(
            "POST",
            HEARTBEAT,
            json!({ "capabilities": vm }),
            Some(token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let stored = owner.stored("nodes", node).await.unwrap();
    assert_eq!(stored["capabilities"]["backend"], "native");
    assert_eq!(stored["slots"], 1);
}

#[tokio::test]
async fn revoking_a_replaced_node_can_release_its_conversation_placements() {
    let owner = Owner::new().await;
    let machine = capabilities(true, 8, 16384, 65536);
    let replaced = owner.enroll("Replaced", &machine).await["nodeId"]
        .as_str()
        .unwrap()
        .to_owned();
    let plain = owner.enroll("Plain", &machine).await["nodeId"]
        .as_str()
        .unwrap()
        .to_owned();
    let other = id();

    let (pinned, preferred, elsewhere, kept) = (id(), id(), id(), id());
    for (run, pin, prefer) in [
        (&pinned, Some(replaced.as_str()), Some(other.as_str())),
        (&preferred, None, Some(replaced.as_str())),
        (&elsewhere, Some(other.as_str()), Some(other.as_str())),
        (&kept, Some(plain.as_str()), None),
    ] {
        let mut record = run_record(run, RunStatus::Succeeded);
        record["pinnedNodeId"] = pin.into();
        record["preferredNodeId"] = prefer.into();
        owner.add_run_fixture(&record).await;
    }

    // A plain revocation keeps explicit placements: the owner chose to wait for that machine.
    let (status, value) = owner
        .send(
            "POST",
            &format!("/api/nodes/{plain}/revoke"),
            json!({ "discardWorkingFiles": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["releasedPlacements"], 0);
    assert_eq!(owner.run(&kept).await["pinnedNodeId"], plain.as_str());

    let path = format!("/api/nodes/{replaced}/revoke");
    let (status, _) = owner.send("POST", &path, json!({ "release": true })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        owner.stored("nodes", &replaced).await.unwrap()["revoked"],
        false
    );

    let (status, value) = owner
        .send(
            "POST",
            &path,
            json!({ "releasePlacements": true, "discardWorkingFiles": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["node"]["revoked"], true);
    assert_eq!(value["releasedPlacements"], 2);
    let run = owner.run(&pinned).await;
    assert!(run["pinnedNodeId"].is_null());
    assert_eq!(run["preferredNodeId"], other.as_str());
    assert!(owner.run(&preferred).await["preferredNodeId"].is_null());
    let run = owner.run(&elsewhere).await;
    assert_eq!(run["pinnedNodeId"], other.as_str());
    assert_eq!(run["preferredNodeId"], other.as_str());
    assert_eq!(owner.run(&kept).await["pinnedNodeId"], plain.as_str());
}

#[tokio::test]
async fn workspace_seed_omits_projects_only_for_an_explicit_vm_resume() {
    let owner = Owner::new().await;
    let identity = owner
        .enroll("Worker", &capabilities(true, 4, 8192, 65536))
        .await;
    let node = text(&identity, "nodeId");
    owner.grant_nodes(MAIN_AGENT_ID, json!([node])).await;

    for (backend, resume, selection, expect_project) in [
        (
            "firecracker",
            true,
            json!({"kind":"initial", "workspace":false}),
            false,
        ),
        (
            "firecracker",
            false,
            json!({"kind":"initial", "workspace":false}),
            true,
        ),
        (
            "native",
            true,
            json!({"kind":"initial", "workspace":false}),
            true,
        ),
        ("firecracker", true, json!({"kind":"initial"}), true),
    ] {
        let (run, attempt) = (id(), id());
        let mut record = run_record(&run, RunStatus::Running);
        record["taskId"] = run.clone().into();
        record["snapshot"] = json!({"agent":{"id":MAIN_AGENT_ID}});
        owner.add_run_fixture(&record).await;
        owner
            .put_attempt_fixture(json!({
                "id": attempt,
                "runId": run,
                "nodeId": node,
                "released": false,
            }))
            .await;
        owner
            .set_checkpoint(&run, json!({"nodeId":node,"runnerId":attempt}))
            .await;
        let data = &owner.service.config.data_dir;
        let root = data.join("runs").join(&run);
        for directory in ["workspace/project", "home/.config/cairn", "chat-input"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        std::fs::write(
            root.join("workspace/project/unpublished"),
            "project contents",
        )
        .unwrap();
        std::fs::write(
            root.join("home/.config/cairn/credentials"),
            "fixture credentials",
        )
        .unwrap();
        std::fs::write(root.join("chat-input/messages.json"), "[]").unwrap();
        std::fs::write(root.join("credential-files.json"), "[]").unwrap();
        std::fs::create_dir_all(data.join("runner-plans")).unwrap();
        std::fs::write(
            data.join("runner-plans").join(format!("{attempt}.json")),
            json!({
                "id": attempt,
                "runId": run,
                "backend": backend,
                "resumeWorkspace": resume,
                "projectRoot": root.join("workspace"),
            })
            .to_string(),
        )
        .unwrap();

        let request = owner
            .request("POST", &format!("/internal/node-workspace/{attempt}/seed"))
            .header("authorization", bearer(text(&identity, "token")))
            .header("content-type", "application/json")
            .body(Body::from(selection.to_string()))
            .unwrap();
        let response = common::send(&owner.app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        let destination = TempDir::new().unwrap();
        files::receive(&mut bytes.as_ref(), destination.path(), 1_000_000)
            .await
            .unwrap();
        assert_eq!(
            destination
                .path()
                .join("workspace/project/unpublished")
                .exists(),
            expect_project,
            "{backend}/resume={resume}/selection={selection}"
        );
        assert_eq!(
            std::fs::read(destination.path().join("home/.config/cairn/credentials")).unwrap(),
            b"fixture credentials"
        );
        assert!(destination.path().join("chat-input/messages.json").exists());
        assert!(destination.path().join("credential-files.json").exists());
        assert!(
            root.join("workspace/project/unpublished").exists(),
            "source must remain untouched"
        );
    }
}

#[tokio::test]
async fn native_project_archive_handshake_skips_the_tree_and_rechecks_cache_grants() {
    assert_native_project_archive_handshake("workspace").await;
}

#[tokio::test]
async fn native_project_archive_handshake_uses_the_current_workspace_generation() {
    assert_native_project_archive_handshake("workspace-fdeab5b6").await;
}

async fn assert_native_project_archive_handshake(workspace_directory: &str) {
    let owner = Owner::new().await;
    let (node, token, person, project, run, attempt) =
        (id(), auth::token(), id(), id(), id(), id());
    owner.register_node(&node, &token).await;
    owner
        .put_node_fixture(json!({
            "id": node, "revoked": false, "accepting": true,
            "capabilities": { "backend": "native", "nativeModeProtocol": 1 }, "nativeEnabled": true,
            "nativeModeObserved": { "mode": "protected", "generation": null, "status": "applied" },
            "nativeAccounts": [person],
        }))
        .await;
    owner
        .service
        .store
        .set(
            "native-authority",
            json!({ "observedAt": now(), "accounts": { person.clone(): {} } }),
            None,
        )
        .await
        .unwrap();
    let root = owner.service.config.data_dir.join("runs").join(&run);
    let project_root = root.join(workspace_directory);
    let project_path = project_root.join(&project);
    std::fs::create_dir_all(&project_path).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::write(project_path.join("file"), "project seed").unwrap();
    std::fs::write(root.join("home/config"), "fresh configuration").unwrap();
    let fixture_project = cairn_contracts::Project {
        id: cairn_contracts::ProjectId(uuid::Uuid::parse_str(&project).unwrap()),
        settings: serde_json::from_value(json!({ "name": "Fixture", "path": project_path }))
            .unwrap(),
        origin: "".into(),
        source: None,
        revision: id(),
        created_at: now(),
    };
    let project_record = serde_json::to_value(&fixture_project).unwrap();
    owner
        .service
        .store
        .transaction(move |db| db.save_project(&fixture_project, None))
        .await
        .unwrap();
    owner.put("agents", json!({
        "id": MAIN_AGENT_ID, "access": { "backend": "native", "nodes": [node], "projects": [project] },
    })).await;

    let mut record = run_record(&run, RunStatus::Running);
    record["snapshot"] = json!({
        "agent": { "id": MAIN_AGENT_ID, "provider": "codex", "access": { "backend": "native" } },
        "projects": [project_record],
    });
    record["workspaces"] =
        json!([{ "projectId": project, "revision": "a".repeat(40), "path": project_path }]);
    record["nativePrincipals"] = json!([person]);
    record["nativeAuthorityNodes"] = json!([node]);
    record["nativeProvenanceUnknown"] = false.into();
    owner.add_run_fixture(&record).await;
    owner
        .put_attempt_fixture(
            json!({ "id": attempt, "runId": run, "nodeId": node, "released": false }),
        )
        .await;
    owner
        .set_checkpoint(&run, json!({ "nodeId": node, "runnerId": attempt }))
        .await;
    let plans = owner.service.config.data_dir.join("runner-plans");
    std::fs::create_dir_all(&plans).unwrap();
    std::fs::write(
        plans.join(format!("{attempt}.json")),
        json!({
            "id": attempt, "runId": run, "backend": "native", "projectRoot": project_root,
        })
        .to_string(),
    )
    .unwrap();
    let (status, plan) = owner
        .call(
            "POST",
            &format!("/internal/node-workspace/{attempt}/plan"),
            json!({}),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    let archive = &plan["projectArchives"][0];
    assert!(
        archive.is_object(),
        "the current seed must be available as an archive: {plan}"
    );
    assert_eq!(archive["projectId"], project);
    for (kind, path) in [
        ("seed", format!("/internal/node-workspace/{attempt}/seed")),
        (
            "project",
            format!("/internal/node-workspace/{attempt}/seed"),
        ),
        (
            "run-archive",
            format!("/internal/node-workspace/{attempt}/archive"),
        ),
        (
            "archive",
            format!("/internal/node-workspace/cache/{project}"),
        ),
    ] {
        let body = if kind == "seed" {
            json!({ "kind": "initial", "workspace": false, "projectArchives": true })
        } else if kind == "project" {
            json!({ "kind": "project", "projectId": project })
        } else if kind == "run-archive" {
            json!({ "projectId": project, "hash": archive["hash"] })
        } else {
            json!({ "hash": archive["hash"] })
        };
        let request = owner
            .request("POST", &path)
            .header("authorization", bearer(&token))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&owner.app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        if matches!(kind, "seed" | "project") {
            let destination = TempDir::new().unwrap();
            files::receive(&mut bytes.as_ref(), destination.path(), 1_000_000)
                .await
                .unwrap();
            if kind == "seed" {
                assert!(!destination.path().join(workspace_directory).exists());
                assert!(destination.path().join("home/config").exists());
            } else {
                assert_eq!(
                    std::fs::read(destination.path().join("file")).unwrap(),
                    b"project seed"
                );
            }
        } else {
            assert_eq!(
                blake3::hash(&bytes).to_hex().as_str(),
                archive["hash"].as_str().unwrap()
            );
        }
    }
    owner.grant_nodes(MAIN_AGENT_ID, json!([])).await;
    let (status, _) = owner
        .call(
            "POST",
            &format!("/internal/node-workspace/cache/{project}"),
            json!({ "hash": archive["hash"] }),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn node_preparations_isolate_conversations_and_coalesce_duplicate_attempts() {
    let owner = Owner::new().await;
    let identity = owner
        .enroll("Worker", &capabilities(true, 4, 8192, 65536))
        .await;
    let node = text(&identity, "nodeId");
    owner.grant_nodes(MAIN_AGENT_ID, json!([node])).await;
    let data = &owner.service.config.data_dir;
    let mut attempts = Vec::new();
    for _ in 0..2 {
        let (run, attempt) = (id(), id());
        let mut record = run_record(&run, RunStatus::Running);
        record["taskId"] = run.clone().into();
        record["snapshot"] = json!({"agent":{"id":MAIN_AGENT_ID}});
        owner.add_run_fixture(&record).await;
        owner
            .put_attempt_fixture(json!({
                "id": attempt,
                "runId": run,
                "nodeId": node,
                "released": false,
            }))
            .await;
        owner
            .set_checkpoint(&run, json!({"nodeId":node,"runnerId":attempt}))
            .await;
        let root = data.join("runs").join(&run);
        std::fs::create_dir_all(root.join("workspace/project")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("chat-input")).unwrap();
        std::fs::write(root.join("chat-input/messages.json"), b"[]").unwrap();
        std::fs::write(
            root.join("workspace/project/source"),
            "unneeded bootstrap source",
        )
        .unwrap();
        std::fs::write(root.join("home/fixture"), "fresh credentials").unwrap();
        std::fs::create_dir_all(data.join("runner-plans")).unwrap();
        std::fs::write(
            data.join("runner-plans").join(format!("{attempt}.json")),
            json!({
                "id": attempt,
                "runId": run,
                "backend": "firecracker",
                "resumeWorkspace": true,
                "projectRoot": root.join("workspace"),
                "chat": { "provider": "codex" },
                "imports": [
                    { "source": root.join("workspace"), "target": root.join("workspace") },
                    { "source": root.join("workspace/project"), "target": root.join("workspace/project") },
                ],
            }).to_string(),
        ).unwrap();
        attempts.push((run, attempt));
    }
    let entered = Arc::new(tokio::sync::Notify::new());
    let inbox_seeded = Arc::new(tokio::sync::Notify::new());
    let release = CancellationToken::new();
    let seeds = Arc::new(AtomicUsize::new(0));
    let slow_seed = format!("/internal/node-workspace/{}/seed", attempts[0].1);
    let app = owner.app.clone().layer(axum::middleware::from_fn({
        let entered = entered.clone();
        let inbox_seeded = inbox_seeded.clone();
        let release = release.clone();
        let seeds = seeds.clone();
        move |mut request: Request<Body>, next: axum::middleware::Next| {
            let blocked = request.uri().path() == slow_seed;
            let entered = entered.clone();
            let inbox_seeded = inbox_seeded.clone();
            let release = release.clone();
            let seeds = seeds.clone();
            async move {
                if blocked {
                    let (parts, body) = request.into_parts();
                    let bytes = to_bytes(body, 16384).await.unwrap();
                    let selection: Value = serde_json::from_slice(&bytes).unwrap();
                    request = Request::from_parts(parts, Body::from(bytes));
                    if selection["kind"] == "inbox" {
                        inbox_seeded.notify_one();
                    } else {
                        assert_eq!(selection["kind"], "initial");
                        seeds.fetch_add(1, Ordering::SeqCst);
                        entered.notify_one();
                        release.cancelled().await;
                    }
                }
                next.run(request).await
            }
        }
    }));
    let (manager_url, manager) = common::serve_locally(app).await;
    let controller = Router::new()
        .route("/health", axum::routing::get(async || Json(json!({}))))
        .route(
            "/runs/{id}/lease",
            axum::routing::post(async || Json(json!({}))),
        );
    let (runner, runner_server) = common::serve_locally(controller).await;
    let node_directory = owner.root().join("node");
    std::fs::create_dir(&node_directory).unwrap();
    std::fs::write(
        node_directory.join("identity.json"),
        json!({
            "master":manager_url,"nodeId":node,"token":identity["token"],
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
    let prepare = |attempt: String| {
        let transport = owner.service.node_transport.clone();
        let node = node.to_owned();
        tokio::spawn(async move {
            transport
                .request(&node, "POST", &format!("/prepare/{attempt}"), vec![])
                .await
                .unwrap()
        })
    };
    let slow = prepare(attempts[0].1.clone());
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let duplicate = prepare(attempts[0].1.clone());
    let fast = prepare(attempts[1].1.clone());
    let response = tokio::time::timeout(Duration::from_secs(5), fast)
        .await
        .expect("another conversation must not wait for the held seed")
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        !duplicate.is_finished(),
        "the duplicate must wait for its own seed"
    );
    let fast_root = data.join("runs").join(&attempts[1].0);
    assert!(fast_root.join("workspace/project").is_dir());
    assert!(!fast_root.join("workspace/project/source").exists());
    assert_eq!(
        std::fs::read(fast_root.join("home/fixture")).unwrap(),
        b"fresh credentials"
    );
    release.cancel();
    assert_eq!(slow.await.unwrap().status(), StatusCode::OK);
    assert_eq!(duplicate.await.unwrap().status(), StatusCode::OK);
    // Inbox refresh shares this endpoint but is independent of preparation
    // coalescing. Include it deterministically without counting it as a bootstrap.
    tokio::time::timeout(Duration::from_secs(5), inbox_seeded.notified())
        .await
        .unwrap();
    assert_eq!(seeds.load(Ordering::SeqCst), 1);
    connector.kill().await.unwrap();
    manager.abort();
    runner_server.abort();
}

#[tokio::test]
async fn prepared_node_disks_preserve_the_local_owner() {
    let owner = Owner::new().await;
    let identity = owner
        .enroll("VPN node", &capabilities(true, 4, 8192, 65536))
        .await;
    let node_id = identity["nodeId"].as_str().unwrap();
    let (run, attempt) = (id(), id());
    owner.grant_nodes(MAIN_AGENT_ID, json!([node_id])).await;
    owner
        .put_attempt_fixture(json!({
            "id": attempt,
            "runId": run,
            "nodeId": node_id,
            "released": false,
        }))
        .await;
    let mut record = run_record(&run, RunStatus::Running);
    record["snapshot"] = json!({ "agent": { "id": MAIN_AGENT_ID } });
    owner.add_run_fixture(&record).await;
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
                "owner": run,
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
    assert_eq!(storage["owner"], run);
    assert!(storage.get("master").is_none());
    assert!(storage.get("grant").is_none());

    connector.kill().await.unwrap();
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
async fn native_project_proxy_preserves_long_imports_and_classifies_head_expiration() {
    let owner = Owner::new().await;
    let native = id();
    let firecracker = id();
    for (node, backend) in [(&native, "native"), (&firecracker, "firecracker")] {
        owner.register_node(node, &auth::token()).await;
        owner
            .put_node_fixture(json!({
                "id": node,
                "revoked": false,
                "capabilities": { "backend": backend },
            }))
            .await;
    }
    let credential =
        cairn_installation::execution::secret(&owner.service.config.data_dir, "runner-secret")
            .await
            .unwrap();
    let project = format!("/runs/{}/projects/{}", id(), id());
    for (node, method, path, seconds, reply, expected) in [
        (&native, "POST", project.as_str(), 260, true, StatusCode::OK),
        (
            &native,
            "POST",
            project.as_str(),
            286,
            false,
            StatusCode::GATEWAY_TIMEOUT,
        ),
        (
            &native,
            "GET",
            "/health",
            31,
            false,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            &firecracker,
            "POST",
            project.as_str(),
            31,
            false,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ] {
        let request = owner
            .request("POST", &format!("/internal/execution/{node}{path}"))
            .method(method)
            .header("authorization", bearer(&credential))
            .body(Body::empty())
            .unwrap();
        let app = owner.app.clone();
        let waiting = tokio::spawn(async move { common::send(&app, request).await });
        let command = owner.service.node_transport.poll(node).await.unwrap();
        assert_eq!(command["path"], path);
        assert_eq!(command["method"], method);

        // Start virtual time only after the real proxy completed its store reads.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(seconds)).await;
        tokio::task::yield_now().await;
        if reply {
            assert!(
                !waiting.is_finished(),
                "Native import expired before its route budget"
            );
            owner
                .service
                .node_transport
                .reply(
                    node,
                    json!({
                        "id": command["id"],
                        "sequence": 0,
                        "status": 200,
                        "length": 0,
                        "done": true,
                    }),
                )
                .await
                .unwrap();
        }
        tokio::time::resume();
        let response = waiting.await.unwrap();
        assert_eq!(response.status(), expected, "{method} {path}");
        if !reply {
            let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            if expected == StatusCode::GATEWAY_TIMEOUT {
                assert_eq!(body["error"], "Project import node response timed out.");
            }
            assert_eq!(
                owner
                    .service
                    .node_transport
                    .reply(
                        node,
                        json!({ "id": command["id"], "status": 200, "done": true })
                    )
                    .await
                    .unwrap_err()
                    .status,
                409,
                "An expired request must release its outstanding command"
            );
        }
    }
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
async fn interrupted_workspace_seed_preserves_the_existing_tree_until_a_complete_transfer() {
    use tokio::io::AsyncReadExt;

    for (finish_http, finish_transfer, expected_status) in [
        (false, false, 502),
        (false, true, 502),
        (true, false, 400),
        (true, true, 200),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                assert_ne!(read, 0);
                request.extend_from_slice(&buffer[..read]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&request[..end]).unwrap();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            let mut body = b"{\"kind\":\"file\",\"path\":\"file\",\"size\":3,\"mode\":384}\n{\"data\":\"bmV3\"}\n".to_vec();
            if finish_transfer {
                body.extend_from_slice(b"{\"complete\":true}\n");
            }
            stream
                .write_all(format!("{:x}\r\n", body.len()).as_bytes())
                .await
                .unwrap();
            stream.write_all(&body).await.unwrap();
            stream.write_all(b"\r\n").await.unwrap();
            if finish_http {
                stream.write_all(b"0\r\n\r\n").await.unwrap();
            }
            stream.shutdown().await.unwrap();
        });
        let root = TempDir::new().unwrap();
        let target = root.path().join("retained");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("file"), b"old private work").unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            workspace::seed(
                &reqwest::Client::new(),
                &format!("http://{address}/").parse().unwrap(),
                "synthetic-secret-token",
                &id(),
                json!({ "kind": "initial" }),
                &target,
            ),
        )
        .await
        .unwrap();
        server.await.unwrap();
        if expected_status == 200 {
            result.unwrap();
            assert_eq!(std::fs::read(target.join("file")).unwrap(), b"new");
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.status, expected_status, "{error}");
            if expected_status == 502 {
                assert_eq!(
                    error.message,
                    "Workspace transfer was interrupted; source files are preserved."
                );
            }
            assert_eq!(
                std::fs::read(target.join("file")).unwrap(),
                b"old private work"
            );
        }
        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            1,
            "failed staging must be removed"
        );
    }
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
    std::fs::write(source.join("empty"), []).unwrap();
    std::fs::set_permissions(source.join("empty"), std::fs::Permissions::from_mode(0o640)).unwrap();
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
    assert!(std::fs::read(target.join("empty")).unwrap().is_empty());
    assert_eq!(mode(&target.join("empty")), 0o640);
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
async fn concurrent_admission_reserves_capacity_once_and_preserves_agent_grants() {
    let owner = Owner::new().await;
    let (node, agent) = (id(), id());
    let mut record = schedulable_node(&node, &limits(2, 4096, 65536));
    record["local"] = false.into();
    record["slots"] = 1.into();
    owner.put_node_fixture(record).await;
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
async fn retained_s3_disks_charge_local_cache_and_keep_their_node() {
    let owner = Owner::new().await;
    let (node, agent, run, attempt) = (id(), id(), id(), id());
    owner
        .put_node_fixture(schedulable_node(&node, &limits(4, 8192, 32768)))
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
}

#[tokio::test]
async fn explicit_resource_requests_are_rejected_before_moving() {
    let owner = Owner::new().await;
    let run = json!({ "id": id(), "resources": limits(2, 4096, 32768) });
    let mut request = limits(2, 4096, 128);
    request["discardWorkingFiles"] = true.into();
    let error = moves::request(&owner.service, &run, &request)
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
async fn runtime_settings_remove_publication_controls_and_preserve_the_last_good_configuration() {
    let owner = Owner::new().await;
    let (status, initial) = owner.get(NODE_SETTINGS).await;
    assert_eq!(status, StatusCode::OK);
    assert!(initial.get("destination").is_none());
    assert!(initial.get("retention").is_none());
    assert_eq!(
        initial,
        json!({ "disconnectTimeoutSeconds": 60, "shutdownTimeoutSeconds": 300 })
    );
    let settings = json!({ "disconnectTimeoutSeconds": 30, "shutdownTimeoutSeconds": 90 });
    assert_eq!(
        owner.send("PUT", NODE_SETTINGS, settings.clone()).await.0,
        StatusCode::OK
    );
    for (field, value) in [
        ("disconnectTimeoutSeconds", 9),
        ("shutdownTimeoutSeconds", 301),
        ("intervalSeconds", 60),
        ("budgetMiB", 128),
        ("maxCapacityWaitSeconds", 30),
    ] {
        let mut bad = settings.clone();
        bad[field] = value.into();
        assert_eq!(
            owner.send("PUT", NODE_SETTINGS, bad).await.0,
            StatusCode::BAD_REQUEST,
            "{field}"
        );
        assert_eq!(owner.get(NODE_SETTINGS).await.1, settings);
    }
}

#[tokio::test]
async fn tags_filter_authorized_nodes_without_granting_access() {
    let owner = Owner::new().await;
    let (fast, other, agent) = (id(), id(), id());
    for (node, tags) in [(&fast, json!(["fast"])), (&other, json!(["slow"]))] {
        let mut record = schedulable_node(node, &limits(4, 8192, 65536));
        record["tags"] = tags;
        owner.put_node_fixture(record).await;
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
                "provider": "codex",
                "access": { "nodes": ["e2000000-0000-4000-8000-000000000002"] },
            },
            "projects": [],
            "skills": [],
            "task": {},
        },
    });
    owner.put("agents", run["snapshot"]["agent"].clone()).await;
    let prepared = cairn_installation::execution::prepare(
        &run,
        &owner.service.config,
        &RunCredentials::default(),
        None,
        None,
    )
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
            .put_node_fixture(json!({
                "id": node,
                "name": "Placement fixture",
                "local": local,
                "lastSeen": last_seen,
                "revoked": is_revoked,
            }))
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
    owner.add_run_fixture(&record).await;
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
async fn native_auth_relays_refresh_all_providers_and_stop_after_grant_revocation() {
    for (provider, provider_home) in [
        ("codex", ".codex"),
        ("claude", ".claude"),
        ("opencode", ".opencode"),
    ] {
        let (listener, address) = common::bind().await;
        let owner = Owner::at(address.to_string()).await;
        let (node, attempt, run, token) = (id(), id(), id(), auth::token());
        owner.register_node(&node, &token).await;
        owner.grant_nodes(MAIN_AGENT_ID, json!([node])).await;
        owner
            .put_attempt_fixture(
                json!({ "id": attempt, "runId": run, "nodeId": node, "released": false }),
            )
            .await;
        let mut record = run_record(&run, RunStatus::Running);
        record["snapshot"] = json!({ "agent": { "id": MAIN_AGENT_ID } });
        owner.add_run_fixture(&record).await;
        owner
            .set_checkpoint(&run, json!({ "nodeId": node, "runnerId": attempt }))
            .await;
        let plans = owner.service.config.data_dir.join("runner-plans");
        std::fs::create_dir_all(&plans).unwrap();
        let plan = json!({
            "runId": run,
            "chat": {
                "provider": provider,
                "claudeManagedAuth": provider == "claude",
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
            .join(provider_home);
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
            let value = cairn_installation::accounts::broker::request(
                &socket,
                &json!({ "method": "refresh" }),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
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
async fn remote_inbox_preserves_empty_invalid_and_oversized_change_detection() {
    let owner = Owner::new().await;
    let (node, attempt, run, token) = (id(), id(), id(), auth::token());
    owner.register_node(&node, &token).await;
    owner.grant_nodes(MAIN_AGENT_ID, json!([node])).await;
    owner
        .put_attempt_fixture(json!({ "id": attempt, "nodeId": node, "released": false }))
        .await;
    let mut record = run_record(&run, RunStatus::Running);
    record["snapshot"] = json!({ "agent": { "id": MAIN_AGENT_ID } });
    owner.add_run_fixture(&record).await;
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
    let inbox = owner
        .service
        .config
        .data_dir
        .join("runs")
        .join(&run)
        .join("chat-input");
    std::fs::create_dir_all(&inbox).unwrap();
    let path = format!("/internal/node-workspace/{attempt}/inbox");

    let (status, body) = owner.call("POST", &path, json!({}), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "messages": [] }));

    for length in [2_000_001, 16_000_000] {
        let bytes = format!("[]{}", " ".repeat(length - 2));
        std::fs::write(inbox.join("messages.json"), bytes).unwrap();
        let (status, body) = owner.call("POST", &path, json!({}), Some(&token)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "messages": [] }));
    }

    std::fs::write(inbox.join("messages.json"), b"invalid json").unwrap();
    let (status, _) = owner.call("POST", &path, json!({}), Some(&token)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    std::fs::write(inbox.join("messages.json"), vec![b' '; 16_000_001]).unwrap();
    let (status, body) = owner.call("POST", &path, json!({}), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "oversized": true }));
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
        .put_attempt_fixture(
            json!({ "id": attempt, "runId": run, "nodeId": node, "released": false }),
        )
        .await;
    let snapshot = json!({ "agent": { "id": MAIN_AGENT_ID, "provider": "codex", "access": { "nodes": [node] } } });
    let mut record = run_record(&run, RunStatus::Running);
    record["snapshot"] = snapshot.clone();
    owner.add_run_fixture(&record).await;
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
    let connection = owner.service.mcp_fixture(connection, None).await.unwrap();
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
    let mut claude_record = record.clone();
    claude_record["snapshot"]["agent"]["provider"] = "claude".into();
    let claude_configuration = owner
        .service
        .mcps
        .run_configuration(&owner.service, &claude_record)
        .await
        .unwrap();
    // Both native configurations receive the VM-local origin.
    for (name, path) in [
        ("cairn_workspace", "/mcp-workspace".to_owned()),
        (gateway.as_str(), format!("/mcp-gateway/{connection}")),
    ] {
        let url = format!("http://127.0.0.1:5202{path}");
        assert_eq!(
            claude_configuration["claudeMcps"]["mcpServers"][name]["url"],
            url
        );
        assert_eq!(configuration["codexConfig"][name]["url"], url);
    }
    // Another run in progress on the same installation.
    let mut other_run = run_record(&id(), RunStatus::Running);
    other_run["taskId"] = "other".into();
    other_run["snapshot"] = snapshot;
    owner.add_run_fixture(&other_run).await;
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
    for path in [
        "/api/mcp".to_owned(),
        "/internal/nodes/heartbeat".to_owned(),
        "/api/agents".to_owned(),
        format!("/mcp-gateway/{connection}/extra"),
        "/mcp-gateway/".to_owned(),
    ] {
        let (status, _) = mcp_through(&socket, &path, run_token, "tools/list", json!({})).await;
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
async fn retained_completed_disk_monitor_discovers_late_writes() {
    use cairn_installation::nodes::storage;

    let runner = Router::new().fallback(|| async {
        Json(json!({
            "mode": "on-demand",
            "dirtyBytes": 114688,
            "dirtySince": 1,
            "localBytes": 114688,
            "activeLocalBytes": 114688,
        }))
    });
    let (runner_url, server) = common::serve_locally(runner).await;
    let owner = Owner::with_runner(common::HOST.into(), runner_url).await;
    let run = id();
    let mut record = run_record(&run, RunStatus::Succeeded);
    record["nodeId"] = LOCAL_NODE_ID.into();
    record["storage"] = json!({ "mode": "on-demand", "dirtyBytes": 0 });
    record["backup"] =
        json!({ "status": "ready", "requestedRevision": 1, "acknowledgedRevision": 1 });
    owner.add_run_fixture(&record).await;
    owner
        .put_node_fixture(json!({ "id": LOCAL_NODE_ID, "retainedRuns": [run] }))
        .await;
    let monitor = tokio::spawn(storage::monitor(owner.service.clone()));
    common::eventually(
        Duration::from_secs(8),
        Duration::from_millis(50),
        async || (owner.run(&run).await["storage"]["dirtyBytes"] == 114688).then_some(()),
    )
    .await;
    assert_eq!(owner.run(&run).await["status"], RunStatus::Succeeded);
    monitor.abort();
    server.abort();
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
    owner.add_run_fixture(&record).await;
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
        .update_run_boundary_fixture(
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
            .put_node_fixture(json!({ "id": node, "revoked": false }))
            .await;
        owner.grant_nodes(MAIN_AGENT_ID, json!([node])).await;
        owner.authorize_node(&node, &token).await;
        owner
            .put_attempt_fixture(json!({
                "id": attempt,
                "nodeId": node,
                "runId": run,
                "released": false,
                "leaseRequired": true,
            }))
            .await;
        let mut record = run_record(&run, RunStatus::Running);
        record["isolated"] = true.into();
        record["snapshot"] = json!({ "agent": { "id": MAIN_AGENT_ID } });
        owner.add_run_fixture(&record).await;
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
                json!({ "runtimeId": "development" }),
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
        .put_node_fixture(schedulable_node(&node, &limits(4, 8192, 65536)))
        .await;
    owner.grant_nodes(&agent, json!([node])).await;
    owner
        .add_run_fixture(&run_record(&run, RunStatus::Running))
        .await;
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
        .put_node_fixture(schedulable_node(&node, &limits(1, 1024, 8192)))
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

#[tokio::test]
async fn new_disks_use_the_default_size_within_shared_headroom_and_retained_disks_keep_their_size()
{
    let owner = Owner::new().await;
    let (node, agent) = (id(), id());
    owner.grant_nodes(&agent, json!([node])).await;
    for (budget, expected) in [(400_844, 32_768), (8_192, 8_192)] {
        owner
            .put_node_fixture(schedulable_node(&node, &limits(7, 36_352, budget)))
            .await;
        let result = placement::reserve(&owner.service, &agent_run(&id(), &agent), &id())
            .await
            .unwrap();
        assert_eq!(result["resources"]["diskMiB"], expected);
        assert_eq!(result["resources"]["cpu"], 7);
        assert_eq!(result["resources"]["memoryMiB"], 36_352);
    }

    owner
        .put_node_fixture(schedulable_node(&node, &limits(7, 36_352, 400_844)))
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
            .put_node_fixture(schedulable_node(node, &limits(cpu, memory, 262_144)))
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
        .put_node_fixture(json!({
            "id": node,
            "name": "Desktop",
            "local": false,
            "revoked": false,
            "accepting": true,
            "tags": [],
            "lastSeen": now(),
            "capabilities": { "kvm": true },
            "limits": limits(4, 8192, 65536),
        }))
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
                "name": "Agent", "access": { "nodes": null }
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
    owner.add_run_fixture(&record).await;
    owner
        .save_conversation_boundary(json!({ "id": chat, "runId": run, "title": "Fixture" }))
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
        .read(move |db| db.events_fixture(&run, 0, 100))
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
            if request.uri().path() == "/health" {
                return Json(json!({ "diskRetirementProtocol":2 }));
            }
            recorded
                .lock()
                .unwrap()
                .push(request.uri().path().to_owned());
            let bytes = axum::body::to_bytes(request.into_body(), 4096)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            Json(json!({ "deleted": true, "retirementId": body["retirementId"] }))
        }
    });
    let (url, _runner) = common::serve_locally(runner).await;
    let owner = Owner::with_runner(common::HOST.into(), url).await;
    let other = id();
    owner
        .put_node_fixture(json!({
            "id": LOCAL_NODE_ID,
            "name": "Current runner",
            "local": true,
            "revoked": false,
            "accepting": true,
            "tags": [],
            "capabilities": { "kvm": true },
            "limits": limits(4, 8192, 65536),
        }))
        .await;
    let (moved, current) = (id(), id());
    for (run, node, total) in [
        (&moved, other.as_str(), 1024),
        (&current, LOCAL_NODE_ID, 3072),
    ] {
        owner
            .add_run_fixture(&run_record(run, RunStatus::Succeeded))
            .await;
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
#[tokio::test]
async fn slot_shortage_reports_free_slots_and_check_reserves_nothing() {
    let owner = Owner::new().await;
    let (node, agent) = (id(), id());
    let mut record = schedulable_node(&node, &limits(7, 16384, 131072));
    record["name"] = "Server".into();
    record["slots"] = 1.into();
    owner.put_node_fixture(record).await;
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
        owner.service.store.attempts_fixture().await.unwrap().len(),
        1
    );
    placement::release(&owner.service, &attempt).await.unwrap();
    placement::check(&owner.service, &another).await.unwrap();
    assert_eq!(
        owner.service.store.attempts_fixture().await.unwrap().len(),
        1
    );
    let (_, inventory) = owner.get("/api/nodes").await;
    let node = find_by_id(&inventory, &node);
    assert_eq!(node["availableSlots"], 1);
    assert!(node.get("reserved").is_none());
}

#[tokio::test]
async fn node_configuration_conflicts_are_atomic_and_heartbeats_keep_the_configuration_revision() {
    let owner = Owner::new().await;
    let identity = owner
        .enroll("Build node", &capabilities(true, 8, 16384, 131072))
        .await;
    let node_id = text(&identity, "nodeId");
    let path = format!("/api/nodes/{node_id}");
    let node = owner.service.get_node(node_id).await.unwrap();
    let request = json!({
        "revision": node.revision, "name": "First editor", "tags": ["build"],
        "limits": node.limits, "slots": 4, "accepting": true,
    });
    let mut missing = request.clone();
    missing.as_object_mut().unwrap().remove("revision");
    assert_eq!(
        owner.call("PUT", &path, missing, None).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut concurrent = request.clone();
    concurrent["name"] = "Second editor".into();
    let (first, second) = tokio::join!(
        owner.call("PUT", &path, request, None),
        owner.call("PUT", &path, concurrent, None)
    );
    assert!(matches!(
        (first.0, second.0),
        (StatusCode::OK, StatusCode::CONFLICT) | (StatusCode::CONFLICT, StatusCode::OK)
    ));
    let saved = owner.service.get_node(node_id).await.unwrap();
    assert_ne!(saved.revision, node.revision);
    assert_eq!(saved.tags, ["build"]);
    assert_eq!(
        saved.name,
        if first.0 == StatusCode::OK {
            "First editor"
        } else {
            "Second editor"
        }
    );
    let observation = json!({
        "nodeProtocol":2, "runtimeId":"development", "capabilities":saved.capabilities,
        "pool":{"capacity":4,"retained_runs":[]}, "usage":{"memoryMiB":128,"diskMiB":128},
    });
    let (status, response) = owner
        .call(
            "POST",
            HEARTBEAT,
            observation,
            Some(text(&identity, "token")),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let observed = owner.service.get_node(node_id).await.unwrap();
    assert_eq!(observed.revision, saved.revision);
    assert_eq!(observed.name, saved.name);
    assert_eq!(observed.tags, saved.tags);
    assert!(
        owner.service.store.list("nodes").await.unwrap().is_empty(),
        "No records mirror remains"
    );
}

#[tokio::test]
async fn incompatible_local_health_keeps_the_configuration_committed_while_its_writer_waits() {
    let observed = Arc::new(tokio::sync::Notify::new());
    let health_seen = observed.clone();
    let controller = Router::new().route(
        "/health",
        axum::routing::get(move || {
            let health_seen = health_seen.clone();
            async move {
                health_seen.notify_one();
                Json(json!({ "runtimeId": "incompatible-controller", "nodeProtocol": 2 }))
            }
        }),
    );
    let (url, server) = common::serve_locally(controller).await;
    let owner = Owner::with_runner(common::HOST.into(), url).await;
    owner
        .put_node_fixture(json!({
            "id": LOCAL_NODE_ID,
            "name": "Before",
            "revision": "r1",
            "local": true,
            "executionReady": true,
            "capabilities": capabilities(true, 8, 16384, 131072),
        }))
        .await;
    let service = owner.service.clone();
    let (entered, holding) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let configuration = tokio::spawn(async move {
        service
            .store
            .transaction(move |db| {
                let mut node = db.node(LOCAL_NODE_ID)?.unwrap();
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(10)).unwrap();
                node.name = "Concurrent configuration".into();
                node.tags = vec!["configured".into()];
                node.limits.memory_mi_b = 2048;
                node.revision = "r2".into();
                db.save_node(&node)
            })
            .await
            .unwrap()
    });
    holding.await.unwrap();
    let service = owner.service.clone();
    let refresh =
        tokio::spawn(async move { cairn_installation::nodes::refresh_local(&service).await });
    observed.notified().await;
    // Allow the rejected observation to wait behind the held configuration writer.
    tokio::time::sleep(Duration::from_millis(500)).await;
    release.send(()).unwrap();
    let configured = configuration.await.unwrap();
    assert_eq!(
        refresh.await.unwrap().unwrap_err().status,
        StatusCode::CONFLICT
    );
    let fenced = owner.service.get_node(LOCAL_NODE_ID).await.unwrap();
    assert_eq!(fenced.name, configured.name);
    assert_eq!(fenced.tags, configured.tags);
    assert_eq!(fenced.limits, configured.limits);
    assert_eq!(fenced.revision, configured.revision);
    assert!(!fenced.execution_ready);
    server.abort();
}

#[tokio::test]
async fn saved_storage_policy_is_retried_after_the_controller_fails_to_confirm_it() {
    let failed = Arc::new(AtomicBool::new(true));
    let applied = Arc::new(Mutex::new(Value::Null));
    let observed = applied.clone();
    let configured = applied.clone();
    let failure = failed.clone();
    let capacity = capabilities(true, 8, 16384, 131072);
    let health_capacity = capacity.clone();
    let controller = Router::new()
        .route(
            "/health",
            axum::routing::get(move || {
                let applied = observed.clone();
                let capabilities = health_capacity.clone();
                async move {
                    Json(json!({
                        "status": "ok",
                        "runtimeId": "development",
                        "nodeProtocol": 2,
                        "capabilities": capabilities,
                        "sharedResources": true,
                        "budget": { "slots": 4, "limits": limits(4, 8192, 32768) },
                        "pool": { "capacity": 4 },
                        "storagePolicy": applied.lock().unwrap().clone(),
                    }))
                }
            }),
        )
        .route(
            "/storage-policy/probe",
            axum::routing::post(|| async { StatusCode::OK }),
        )
        .route(
            "/storage-policy",
            axum::routing::post(move |Json(configuration): Json<Value>| {
                let applied = configured.clone();
                let failure = failure.clone();
                async move {
                    if failure.load(Ordering::SeqCst) {
                        return StatusCode::SERVICE_UNAVAILABLE;
                    }
                    *applied.lock().unwrap() = configuration;
                    StatusCode::OK
                }
            }),
        );
    let (url, server) = common::serve_locally(controller).await;
    let owner = Owner::with_runner(common::HOST.into(), url).await;
    std::fs::write(
        owner.service.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "synthetic-storage", "awsBinary": "/bin/true" }).to_string(),
    )
    .unwrap();
    owner
        .put_node_fixture(json!({
            "id": LOCAL_NODE_ID,
            "name": "Current runner",
            "local": true,
            "capabilities": capacity,
            "limits": limits(4, 8192, 32768),
            "slots": 4,
        }))
        .await;
    let before = owner.service.get_node(LOCAL_NODE_ID).await.unwrap();
    let policy = Policy {
        memory_cache_mi_b: 512,
        ..Policy::default()
    };
    let (status, error) = owner
        .call(
            "PUT",
            &format!("/api/nodes/{LOCAL_NODE_ID}/storage"),
            json!({ "nodeId": LOCAL_NODE_ID, "revision": before.revision, "storage": policy }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{error}");
    assert!(text(&error, "error").contains("were saved"), "{error}");
    let saved = owner.service.get_node(LOCAL_NODE_ID).await.unwrap();
    assert_eq!(saved.storage, policy);
    assert_ne!(saved.revision, before.revision);
    assert!(applied.lock().unwrap().is_null());

    failed.store(false, Ordering::SeqCst);
    cairn_installation::nodes::refresh_local(&owner.service)
        .await
        .unwrap();
    assert_eq!(
        applied.lock().unwrap()["policy"],
        serde_json::to_value(policy).unwrap()
    );
    assert_eq!(applied.lock().unwrap()["generation"], 1);
    server.abort();
}

#[tokio::test]
async fn private_move_inherits_tags_for_omitted_or_null_and_clears_for_an_empty_list() {
    for tags in [None, Some(Value::Null), Some(json!([]))] {
        let owner = Owner::new().await;
        let node = id();
        let mut destination = schedulable_node(&node, &limits(2, 4096, 65536));
        destination["tags"] = json!(["retained"]);
        owner.put_node_fixture(destination).await;
        owner.grant_nodes(MAIN_AGENT_ID, json!([node])).await;
        let run_id = id();
        let mut run = run_record(&run_id, RunStatus::Running);
        run["snapshot"] = agent_run(&run_id, MAIN_AGENT_ID)["snapshot"].clone();
        run["requiredTags"] = json!(["retained"]);
        owner.add_run_fixture(&run).await;
        let mut input = json!({"nodeId": node, "discardWorkingFiles": true});
        if let Some(tags) = tags {
            input["requiredTags"] = tags;
        }
        let expected = if input["requiredTags"].is_array() {
            json!([])
        } else {
            json!(["retained"])
        };
        let moved = moves::request_by_agent(&owner.service, &run, &input)
            .await
            .unwrap();
        assert_eq!(moved["status"], "moved");
        let changed = owner.run(&run_id).await;
        assert_eq!(changed["requiredTags"], expected);
        assert_eq!(changed["environmentExpirationCause"], "node-change");
        assert!(changed["moveRequest"].is_null());
    }
}
