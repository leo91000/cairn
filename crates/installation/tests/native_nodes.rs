use common::attempt_fixture::*;
use common::node_fixture::*;
#[allow(unused_imports)]
use common::{PluginServiceFixture as _, PluginStoreFixture as _};

use crate::common::run_fixture::DbRunFixture;
use crate::common::run_fixture::RunFixture;
use common::AgentFixture;
use common::MissionFixture;
use common::StoreAgentFixture;
use common::StoreMissionFixture;

mod common;
#[path = "native_nodes/lineage_cycles.rs"]
mod lineage_cycles;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use cairn_installation::{
    auth,
    config::{id, now},
    native::policy,
    nodes::placement,
    service::Service,
    validation::text,
};
use common::{RelayContext, read_json};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};

const TRUSTED: &str = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
const OTHER: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

async fn administrator_admission(backend: &str) {
    let mut f = Fixture::new().await;
    f.agent = f
        .service
        .agent_fixture(
            json!({ "access": { "backend": backend } }),
            f.agent["id"].as_str(),
        )
        .await
        .unwrap();
    let node_id = text(&f.node, "id");
    let path = format!("/api/nodes/{node_id}/native-mode");
    let mut unsupported = f.node.clone();
    unsupported["capabilities"]["nativeModeProtocol"] = 0.into();
    f.service.store.put_node_fixture(unsupported).await.unwrap();
    let (status, _) = f
        .owner_call("POST", &path, json!({ "mode": "administrator" }))
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "Unsupported supervisors must not receive privileged work"
    );
    let mut node = f.node.clone();
    node["capabilities"]["nativeModeProtocol"] = 1.into();
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let (status, _) = f
        .call(
            &f.trusted,
            "POST",
            &path,
            json!({ "mode": "administrator" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = f.owner_call("POST", &path, json!({ "mode": "root" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, preview) = f
        .owner_call("POST", &path, json!({ "mode": "administrator" }))
        .await;
    assert!(
        preview["preview"]["warning"]
            .as_str()
            .unwrap()
            .contains("real host")
    );
    assert!(
        preview["preview"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == f.agent["id"])
    );
    assert!(f.service.node_boundary(node_id).await.unwrap()["nativeModeRequest"].is_null());
    let (status, pending) = f
        .owner_call(
            "POST",
            &path,
            json!({
                "mode": "administrator", "previewId": preview["previewId"], "confirm": true,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{pending}");
    assert_eq!(pending["node"]["nativeModeStatus"], "pending");
    assert_eq!(pending["node"]["executionReady"], false);
    assert_eq!(
        pending["node"]["nativeModeRequest"]["mode"],
        "administrator"
    );
    let pending = pending["node"].clone();
    let run = f.run();
    let denied = {
        let run = run.clone();
        let pending = pending.clone();
        f.service
            .store
            .read(move |db| policy::allows(db, &run, &pending))
            .await
            .unwrap()
    };
    assert!(!denied, "Pending transitions cannot admit work");
    let (status, _) = f
        .owner_call(
            "POST",
            &path,
            json!({
                "mode": "administrator", "previewId": preview["previewId"], "confirm": true,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "Confirmation is single-use");
    let mut applied = pending;
    applied["nativeModeObserved"] = json!({
        "mode": "administrator", "generation": applied["nativeModeRequest"]["generation"], "status": "applied",
    });
    let approved = {
        let run = run.clone();
        let applied = applied.clone();
        f.service
            .store
            .read(move |db| policy::allows(db, &run, &applied))
            .await
            .unwrap()
    };
    assert!(approved);
    for status in ["pending", "failed"] {
        let run = run.clone();
        let mut failed = applied.clone();
        failed["nativeModeObserved"]["status"] = status.into();
        assert!(
            !f.service
                .store
                .read(move |db| policy::allows(db, &run, &failed))
                .await
                .unwrap()
        );
    }
    let mut stale = applied.clone();
    stale["nativeModeObserved"]["generation"] = id().into();
    let checked_run = run.clone();
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &checked_run, &stale))
            .await
            .unwrap()
    );
    // A future wildcard agent did not appear in the owner's confirmation.
    let future = f
        .service
        .agent_fixture(
            json!({ "name": "Future native", "access": { "backend": backend, "nodes": null } }),
            None,
        )
        .await
        .unwrap();
    let mut future_run = run;
    future_run["snapshot"]["agent"] = future;
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &future_run, &applied))
            .await
            .unwrap()
    );
}

/// These members may use every agent: native authority is checked on its own, after
/// account access to agents (ADR-0075), with the memberships of the native evidence.
async fn members_use_every_agent(service: &Service) {
    members_use_agents(service, &[(TRUSTED, "one"), (OTHER, "two")]).await;
}

async fn members_use_agents(service: &Service, members: &[(&str, &'static str)]) {
    for agent in service.store.agents().await.unwrap() {
        for &(account, access) in members {
            common::relay_fixture::add_member(service, account, access).await;
            let (agent, account) = (agent.id.to_string(), account.to_owned());
            service
                .store
                .transaction(move |db| {
                    db.0.execute(
                        "INSERT OR IGNORE INTO agent_account_access VALUES(?1,?2,?3,0)",
                        [agent, account, access.to_owned()],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    service: Arc<Service>,
    router: Router,
    trusted: RelayContext,
    other: RelayContext,
    agent: Value,
    node: Value,
}

async fn public_mcp_call(
    f: &Fixture,
    account: &str,
    scopes: &[&str],
    operation: &str,
    args: Value,
) -> Value {
    f.service
        .code_mode
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let mut identity = auth::InstallationIdentity::trusted(auth::InstallationRole::Owner, account);
    identity.mcp_scopes = Some(scopes.iter().map(|scope| (*scope).to_owned()).collect());
    let mut request = common::request("POST", "/api/mcp")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": common::mcp_arguments(operation, &args),
            })
            .to_string(),
        ))
        .unwrap();
    request.extensions_mut().insert(identity);
    let response = common::send(&f.router, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    read_json(response).await
}

impl Fixture {
    async fn vm_node(&self) -> Value {
        let mut node = self.node.clone();
        node["id"] = id().into();
        node["name"] = "VM runner".into();
        node["capabilities"]["backend"] = "firecracker".into();
        node["capabilities"]["kvm"] = true.into();
        node["nativeEnabled"] = false.into();
        node["nativeAccounts"] = json!([]);
        self.service
            .store
            .put_node_fixture(node.clone())
            .await
            .unwrap();
        node
    }

    async fn new() -> Self {
        Self::with_workspace(false).await
    }

    async fn with_workspace(workspace: bool) -> Self {
        Self::with_workspace_and_codex(workspace, false).await
    }

    async fn with_codex_fixture() -> Self {
        Self::with_workspace_and_codex(false, true).await
    }

    async fn with_workspace_and_codex(workspace: bool, fixture_codex: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let mut config = common::config(root.path());
        if fixture_codex {
            config.codex_bin = common::fixture("codex.mjs");
        }
        if workspace {
            config.runner_url = "http://unused-runner:4311".into();
        }
        let service = Service::new(config).await.unwrap();
        let router = common::relay_fixture::router(service.clone())
            .await
            .unwrap();
        let trusted =
            RelayContext::new(&common::relay_fixture::member_context(&service, TRUSTED).await);
        let other =
            RelayContext::new(&common::relay_fixture::member_context(&service, OTHER).await);
        service
            .store
            .set(
                "native-authority",
                json!({ "observedAt": now(), "accounts": {
                    TRUSTED: { "email": "trusted@example.test", "accessId": "one" },
                    OTHER: { "email": "other@example.test", "accessId": "two" }
                }}),
                None,
            )
            .await
            .unwrap();
        let node = json!({
            "id": id(),
            "name": "Shared desktop",
            "local": false,
            "accepting": true,
            "revoked": false,
            "nativeEnabled": true,
            "nativeAccounts": [TRUSTED],
            "lastSeen": now(),
            "executionReady": true,
            "runtimeId": "development",
            "nativeModeObserved": { "mode": "protected", "generation": null, "status": "applied" },
            "capabilities": {
                "nativeModeProtocol": 1,
                "backend": "native",
                "ublk": true,
                "os": "linux",
                "arch": "x86_64",
                "cpu": 4,
                "memoryMiB": 8192,
                "diskMiB": 100000,
                "diskTotalMiB": 100000,
                "kvm": false,
            },
            "limits": { "cpu": 2, "memoryMiB": 4096, "diskMiB": 32768 },
            "slots": 1,
        });
        service.store.put_node_fixture(node.clone()).await.unwrap();
        let agent = service.agent_fixture(json!({ "name": "Trusted native", "access": { "backend": "native", "nodes": [node["id"]] } }), None).await.unwrap();
        Self {
            _root: root,
            service,
            router,
            trusted,
            other,
            agent,
            node,
        }
    }

    async fn call(
        &self,
        session: &RelayContext,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        self.call_as(session, method, path, body, false).await
    }

    async fn owner_call(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
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
        self.call_as(&self.other, method, path, body, true).await
    }

    async fn call_as(
        &self,
        session: &RelayContext,
        method: &str,
        path: &str,
        body: Value,
        owner: bool,
    ) -> (StatusCode, Value) {
        members_use_every_agent(&self.service).await;
        let mut request = session
            .authorize(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", common::HOST),
            )
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        if owner {
            request
                .extensions_mut()
                .insert(auth::InstallationIdentity::trusted(
                    auth::InstallationRole::Owner,
                    OTHER,
                ));
        }
        let response = common::send(&self.router, request).await;
        let status = response.status();
        (status, read_json(response).await)
    }

    async fn conversation(&self) -> (Value, Value) {
        let (status, chat) = self
            .call(
                &self.trusted,
                "POST",
                "/api/chats",
                json!({ "agentId": self.agent["id"] }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{chat}");
        let chat_id = chat["id"].as_str().unwrap();
        let (status, message) = self
            .call(
                &self.trusted,
                "POST",
                &format!("/api/chats/{chat_id}/messages"),
                json!({ "id": id(), "text": "Build the project" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{message}");
        self.service.chat_tick(&HashSet::new()).await.unwrap();
        let chat = self.service.conversation_boundary(chat_id).await.unwrap();
        let run = self
            .service
            .store
            .run(chat["runId"].as_str().unwrap())
            .await
            .unwrap();
        assert_eq!(run["nativePrincipals"], json!([TRUSTED]));
        (chat, run)
    }

    fn run(&self) -> Value {
        json!({
            "id": id(),
            "taskId": id(),
            "projectId": "",
            "status": "queued",
            "createdAt": now(),
            "nativePrincipals": [TRUSTED],
            "nativeAuthorityNodes": [self.node["id"]],
            "nativeProvenanceUnknown": false,
            "snapshot": { "agent": self.agent },
        })
    }
}

async fn delegate(f: &Fixture, provenance: bool) -> (Value, Value) {
    let parent_agent = f
        .service
        .agent_fixture(
            json!({ "name": "Trusted VM coordinator", "access": { "installation": true } }),
            None,
        )
        .await
        .unwrap();
    let mut parent = f.run();
    parent["snapshot"]["agent"] = parent_agent;
    parent["status"] = "running".into();
    if !provenance {
        parent["nativeProvenanceUnknown"] = true.into();
    }
    common::add_run(&f.service.store, &parent).await;
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &parent)
        .await
        .unwrap();
    let response = delegated_call(
        f,
        &configuration,
        "create_chat",
        json!({ "agentId": f.agent["id"] }),
    )
    .await;
    (parent, response)
}

async fn delegated_call(f: &Fixture, configuration: &Value, name: &str, args: Value) -> Value {
    let router = cairn_installation::http::router(f.service.clone())
        .await
        .unwrap();
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let request = common::request("POST", "/mcp-installation")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": common::mcp_arguments(name, &args),
            })
            .to_string(),
        ))
        .unwrap();
    let response = common::send(&router, request).await;
    let status = response.status();
    let value = read_json(response).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(value["result"].is_object(), "{value}");
    value
}

// This installation has one owner (TRUSTED) and two current members. Native
// directing principals and authenticated mission roles are supplied independently.
async fn mission_authority(f: &Fixture) -> tokio::task::JoinHandle<()> {
    let policy = serde_json::to_value(cairn_protocol::TaskAuthorPolicy {
        owner: cairn_protocol::TaskAuthorGrant {
            account_id: TRUSTED.into(),
            access_id: "one".into(),
        },
        members: [(OTHER, "two"), ("fixture-owner", "fixture-owner")]
            .into_iter()
            .map(|(account, access)| cairn_protocol::TaskAuthorGrant {
                account_id: account.into(),
                access_id: access.into(),
            })
            .collect(),
    })
    .unwrap();
    let app = Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(move || {
            let policy = policy.clone();
            async move { axum::Json(policy) }
        }),
    );
    let (origin, server) = common::serve_locally(app).await;
    let path = f
        .service
        .config
        .data_dir
        .join("installation-relay/identity.json");
    let mut identity: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    identity["origin"] = origin.into();
    std::fs::write(path, serde_json::to_vec(&identity).unwrap()).unwrap();
    server
}

async fn trust_owner(f: &Fixture) {
    let mut node = f.node.clone();
    node["nativeAccounts"] = json!([TRUSTED, "fixture-owner"]);
    f.service.store.put_node_fixture(node).await.unwrap();
    let mut authority = f
        .service
        .store
        .kv("native-authority")
        .await
        .unwrap()
        .unwrap();
    authority["accounts"]["fixture-owner"] =
        json!({ "email": "owner@example.test", "accessId": "fixture-owner" });
    f.service
        .store
        .set("native-authority", authority, None)
        .await
        .unwrap();
}

async fn unplaced_administrator_preview(backend: &str) {
    let mut f = Fixture::new().await;
    f.agent = f
        .service
        .agent_fixture(
            json!({ "access": { "backend": backend } }),
            f.agent["id"].as_str(),
        )
        .await
        .unwrap();
    let node_id = text(&f.node, "id");
    let mut node = f.node.clone();
    node["capabilities"]["nativeModeProtocol"] = 1.into();
    f.service.store.put_node_fixture(node).await.unwrap();
    let first = f.run();
    common::add_run(&f.service.store, &first).await;
    let mut vm = f.run();
    vm["snapshot"]["agent"]["access"]["backend"] = "firecracker".into();
    common::add_run(&f.service.store, &vm).await;
    let path = format!("/api/nodes/{node_id}/native-mode");
    let (status, preview) = f
        .owner_call("POST", &path, json!({ "mode": "administrator" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert!(
        !preview["preview"]["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["id"] == vm["id"])
    );
    assert!(
        preview["preview"]["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["id"] == first["id"])
    );

    let second = f.run();
    common::add_run(&f.service.store, &second).await;
    let (status, _) = f
        .owner_call(
            "POST",
            &path,
            json!({ "mode": "administrator", "previewId": preview["previewId"], "confirm": true }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "An undisclosed automatic arrival requires a fresh preview"
    );
    assert!(f.service.node_boundary(node_id).await.unwrap()["nativeModeRequest"].is_null());
    let (_, fresh) = f
        .owner_call("POST", &path, json!({ "mode": "administrator" }))
        .await;
    let listed = fresh["preview"]["conversations"].as_array().unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|run| run["id"] == second["id"]));
    let (status, pending) = f
        .owner_call(
            "POST",
            &path,
            json!({ "mode": "administrator", "previewId": fresh["previewId"], "confirm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{pending}");
    assert_eq!(pending["node"]["nativeModeStatus"], "pending");
}

async fn saved_native_mission(f: &Fixture) -> Value {
    let mut run = f.run();
    run["status"] = "failed".into();
    run["resumeAvailable"] = true.into();
    run["sessionId"] = "retained-session".into();
    common::add_run(&f.service.store, &run).await;
    common::set_checkpoint(
        &f.service.store,
        text(&run, "id"),
        json!({
            "launched": true, "prepared": { "cwd": "/saved/native/workspace" },
        }),
    )
    .await;
    f.service.store.run(text(&run, "id")).await.unwrap()
}

async fn resume_coordinator(f: &Fixture, traced: bool) -> (Value, Value) {
    let agent = f
        .service
        .agent_fixture(
            json!({
                "name": "Resume coordinator", "access": { "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let mut caller = f.run();
    caller["snapshot"]["agent"] = agent;
    caller["status"] = "running".into();
    caller["nativeProvenanceUnknown"] = (!traced).into();
    common::add_run(&f.service.store, &caller).await;
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &caller)
        .await
        .unwrap();
    (caller, configuration)
}

async fn delegated_move_result(
    fixture: &Fixture,
    configuration: &Value,
    input: Value,
) -> cairn_contracts::NodeMoveOperation {
    let response = delegated_call(fixture, configuration, "move_to_node", input).await;
    assert_ne!(response["result"]["isError"], true, "{response}");
    let started: cairn_contracts::NodeMoveStarted =
        serde_json::from_value(response["result"]["structuredContent"]["result"].clone()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let response = delegated_call(
                fixture,
                configuration,
                "get_node_move_operation",
                json!({"operationId":started.operation}),
            )
            .await;
            assert_ne!(response["result"]["isError"], true, "{response}");
            let operation: cairn_contracts::NodeMoveOperation =
                serde_json::from_value(response["result"]["structuredContent"]["result"].clone())
                    .unwrap();
            if operation.state != cairn_contracts::NodeMoveOperationState::Running {
                return operation;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Native directing operation did not finish")
}

#[path = "native_nodes/admission.rs"]
mod admission;
#[path = "native_nodes/controls_and_capacity.rs"]
mod controls_and_capacity;
#[path = "native_nodes/delegation.rs"]
mod delegation;
#[path = "native_nodes/transitions_and_resume.rs"]
mod transitions_and_resume;
