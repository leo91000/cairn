mod common;

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
};
use common::{RelayContext, read_json};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};

const TRUSTED: &str = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
const OTHER: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

struct Fixture {
    _root: tempfile::TempDir,
    service: Arc<Service>,
    router: Router,
    trusted: RelayContext,
    other: RelayContext,
    agent: Value,
    node: Value,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let service = Service::new(common::config(root.path())).await.unwrap();
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
            "runtimes": ["development"],
            "capabilities": {
                "backend": "native",
                "os": "linux",
                "arch": "x86_64",
                "cpu": 4,
                "memoryMiB": 8192,
                "diskMiB": 100000,
                "diskTotalMiB": 100000,
                "kvm": false,
                "fuse": false,
            },
            "limits": { "cpu": 2, "memoryMiB": 4096, "diskMiB": 32768 },
            "slots": 1,
        });
        service.store.put("nodes", node.clone()).await.unwrap();
        let agent = service.agent(json!({ "name": "Trusted native", "access": { "backend": "native", "nodes": [node["id"]] } }), None).await.unwrap();
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
        let request = session
            .authorize(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", common::HOST),
            )
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&self.router, request).await;
        let status = response.status();
        (status, read_json(response).await)
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

#[tokio::test]
async fn selected_native_agent_requires_a_trusted_human_on_every_message() {
    let f = Fixture::new().await;
    let (status, _) = f
        .call(
            &f.other,
            "POST",
            "/api/chats",
            json!({ "agentId": f.agent["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, chat) = f
        .call(
            &f.trusted,
            "POST",
            "/api/chats",
            json!({ "agentId": f.agent["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let path = format!("/api/chats/{}/messages", chat["id"].as_str().unwrap());
    let (status, _) = f
        .call(
            &f.other,
            "POST",
            &path,
            json!({ "text": "Run on the host", "nativePrincipals": [TRUSTED] }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = f
        .call(
            &f.trusted,
            "POST",
            &path,
            json!({ "id": id(), "text": "Build the project" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // Internal callers and plugins do not inherit a human's authority by choosing their agent.
    let rejected = f
        .service
        .chat_send(
            chat["id"].as_str().unwrap(),
            json!({ "id": id(), "text": "Unscoped native work" }),
        )
        .await
        .unwrap_err();
    assert_eq!(rejected.status, 403);
}

#[tokio::test]
async fn native_placement_works_without_kvm_or_fuse_and_never_falls_back_to_a_vm() {
    let f = Fixture::new().await;
    let run = f.run();
    common::add_run(&f.service.store, &run).await;
    let attempt = id();
    let placement = placement::reserve(&f.service, &run, &attempt)
        .await
        .unwrap();
    assert_eq!(placement["nodeId"], f.node["id"]);
    placement::release(&f.service, &attempt).await.unwrap();
    let mut node = f.node.clone();
    node["nativeEnabled"] = false.into();
    f.service.store.put("nodes", node).await.unwrap();
    assert!(placement::check(&f.service, &run).await.is_err());
}

#[tokio::test]
async fn retained_conversations_exhaust_the_shared_native_admission_budget() {
    let f = Fixture::new().await;
    let mut node = f.node.clone();
    node["usage"] = json!({ "diskMiB": 20 * 1024 });
    f.service.store.put("nodes", node.clone()).await.unwrap();
    let first = f.run();
    common::add_run(&f.service.store, &first).await;
    let attempt = id();
    placement::reserve(&f.service, &first, &attempt)
        .await
        .unwrap();
    placement::release(&f.service, &attempt).await.unwrap();
    // Heartbeats include all retained conversations, not just a running slot.
    node["usage"] = json!({ "diskMiB": 40 * 1024 });
    f.service.store.put("nodes", node).await.unwrap();
    let next = f.run();
    common::add_run(&f.service.store, &next).await;
    assert!(placement::reserve(&f.service, &next, &id()).await.is_err());
}

#[tokio::test]
async fn wildcard_access_and_untrusted_contributors_do_not_authorize_native_execution() {
    let f = Fixture::new().await;
    assert!(
        f.service
            .agent(
                json!({ "name": "Wildcard", "access": { "backend": "native", "nodes": null } }),
                None
            )
            .await
            .is_err()
    );
    let mut run = f.run();
    run["nativePrincipals"] = json!([TRUSTED, OTHER]);
    let node = f.node.clone();
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &run, &node))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn revoking_a_native_person_stops_lease_renewals_and_workspace_access() {
    let f = Fixture::new().await;
    let mut run = f.run();
    run["status"] = "running".into();
    common::add_run(&f.service.store, &run).await;
    let attempt = id();
    common::set_checkpoint(
        &f.service.store,
        run["id"].as_str().unwrap(),
        json!({ "runnerId": attempt, "nodeId": f.node["id"] }),
    )
    .await;
    f.service
        .store
        .put(
            "node-attempts",
            json!({
                "id": attempt,
                "runId": run["id"],
                "nodeId": f.node["id"],
                "released": false,
                "leaseExpiresAt": now() + 60000,
            }),
        )
        .await
        .unwrap();
    let token = auth::token();
    f.service
        .store
        .set(
            &format!("node-token:{}", auth::digest(&token)),
            f.node["id"].clone(),
            None,
        )
        .await
        .unwrap();
    let heartbeat = json!({
        "runtimeId": "development",
        "capabilities": f.node["capabilities"],
        "dataRoot": "/private/native-node/data",
        "backend": "native",
        "nodeProtocol": 2,
        "executionReady": true,
        "budget": { "slots": 1, "limits": f.node["limits"] },
        "pool": { "capacity": 1 },
    });
    let send = |body: Value| {
        let request = Request::builder()
            .method("POST")
            .uri("/internal/nodes/heartbeat")
            .header("host", common::HOST)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        common::send(&f.router, request)
    };
    let first = read_json(send(heartbeat.clone()).await).await;
    assert_eq!(first["leases"].as_array().unwrap().len(), 1);
    let mut node = f
        .service
        .get("nodes", f.node["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(node["executionReady"], true);
    node["nativeAccounts"] = json!([]);
    f.service.store.put("nodes", node).await.unwrap();
    let renewed = read_json(send(heartbeat).await).await;
    assert!(renewed["leases"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn expired_authority_and_removed_members_fail_closed() {
    let f = Fixture::new().await;
    f.service
        .store
        .set(
            "native-authority",
            json!({ "observedAt": now() - 60001, "accounts": { TRUSTED: {} } }),
            None,
        )
        .await
        .unwrap();
    let (run, node) = (f.run(), f.node.clone());
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &run, &node))
            .await
            .unwrap()
    );
    f.service
        .store
        .set(
            "native-authority",
            json!({ "observedAt": now(), "accounts": {} }),
            None,
        )
        .await
        .unwrap();
    let (run, node) = (f.run(), f.node.clone());
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &run, &node))
            .await
            .unwrap()
    );
}

async fn delegate(f: &Fixture, provenance: bool) -> (Value, Value) {
    let parent_agent = f
        .service
        .agent(
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

#[tokio::test]
async fn a_vm_coordinator_can_delegate_only_its_traced_native_authority() {
    let f = Fixture::new().await;
    let (parent, response) = delegate(&f, true).await;
    assert_ne!(response["result"]["isError"], true, "{response}");
    let child = &response["result"]["structuredContent"]["result"];
    assert_eq!(child["nativeParentRunId"], parent["id"], "{response}");
    assert_eq!(child["nativeAuthorityNodes"], json!([f.node["id"]]));
    let mut run = f.run();
    run["nativeParentRunId"] = parent["id"].clone();
    let node = f.node.clone();
    let valid = run.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &valid, &node))
            .await
            .unwrap()
    );
    f.service
        .store
        .patch_run(
            parent["id"].as_str().unwrap(),
            json!({ "cancelRequestedAt": now() }),
        )
        .await
        .unwrap();
    let node = f.node.clone();
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &run, &node))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn unattributed_vm_work_cannot_acquire_native_access_through_delegation() {
    let f = Fixture::new().await;
    let (_, response) = delegate(&f, false).await;
    assert_eq!(response["result"]["isError"], true, "{response}");
}

#[tokio::test]
async fn a_vm_coordinator_directed_by_unrelated_parents_loses_native_delegation() {
    let f = Fixture::new().await;
    let (parent_a, _) = delegate(&f, true).await;
    let parent_b_agent = f
        .service
        .agent(
            json!({
                "name": "Other VM director", "access": { "installation": true }
            }),
            None,
        )
        .await
        .unwrap();
    let mut parent_b = parent_a.clone();
    parent_b["id"] = id().into();
    parent_b["taskId"] = id().into();
    parent_b["snapshot"]["agent"] = parent_b_agent.clone();
    common::add_run(&f.service.store, &parent_b).await;
    let coordinator = f
        .service
        .agent(
            json!({
                "name": "Shared VM coordinator", "access": { "installation": true }
            }),
            None,
        )
        .await
        .unwrap();
    let a = f
        .service
        .mcps
        .run_configuration(&f.service, &parent_a)
        .await
        .unwrap();
    let b = f
        .service
        .mcps
        .run_configuration(&f.service, &parent_b)
        .await
        .unwrap();
    let created = delegated_call(
        &f,
        &a,
        "create_chat",
        json!({ "agentId": coordinator["id"] }),
    )
    .await;
    assert_ne!(created["result"]["isError"], true, "{created}");
    let chat_id = created["result"]["structuredContent"]["result"]["id"].clone();
    let sent = delegated_call(
        &f,
        &a,
        "send_chat_message",
        json!({
            "chatId": chat_id, "message": { "id": id(), "text": "Coordinate trusted work" }
        }),
    )
    .await;
    assert_ne!(sent["result"]["isError"], true, "{sent}");
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let chat = f
        .service
        .get("chats", chat_id.as_str().unwrap())
        .await
        .unwrap();
    let coordinator_run = f
        .service
        .store
        .run(chat["runId"].as_str().unwrap())
        .await
        .unwrap();
    let coordinator_run = f
        .service
        .store
        .patch_run(
            coordinator_run["id"].as_str().unwrap(),
            json!({ "status": "running" }),
        )
        .await
        .unwrap();
    let c = f
        .service
        .mcps
        .run_configuration(&f.service, &coordinator_run)
        .await
        .unwrap();
    let child = delegated_call(&f, &c, "create_chat", json!({ "agentId": f.agent["id"] })).await;
    assert_ne!(child["result"]["isError"], true, "{child}");
    let child_id = child["result"]["structuredContent"]["result"]["id"].clone();
    let sent = delegated_call(
        &f,
        &c,
        "send_chat_message",
        json!({
            "chatId": child_id, "message": { "id": id(), "text": "Use the native node" }
        }),
    )
    .await;
    assert_ne!(sent["result"]["isError"], true, "{sent}");
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let child_chat = f
        .service
        .get("chats", child_id.as_str().unwrap())
        .await
        .unwrap();
    let existing_native = f
        .service
        .store
        .run(child_chat["runId"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(existing_native["nativeParentRunId"], coordinator_run["id"]);
    let valid = existing_native.clone();
    let node = f.node.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &valid, &node))
            .await
            .unwrap()
    );

    let directed = delegated_call(
        &f,
        &b,
        "send_chat_message",
        json!({
            "chatId": chat_id, "message": { "id": id(), "text": "Also act on my instructions" }
        }),
    )
    .await;
    assert_ne!(directed["result"]["isError"], true, "{directed}");
    let updated = f
        .service
        .store
        .run(chat["runId"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(updated["nativeProvenanceUnknown"], true);

    for reduction in [
        json!({ "cancelRequestedAt": now() }),
        json!({ "cancelRequestedAt": null, "nativeAuthorityNodes": [] }),
    ] {
        f.service
            .store
            .patch_run(parent_b["id"].as_str().unwrap(), reduction)
            .await
            .unwrap();
        let run = existing_native.clone();
        let node = f.node.clone();
        assert!(
            !f.service
                .store
                .read(move |db| policy::allows(db, &run, &node))
                .await
                .unwrap()
        );
        let denied =
            delegated_call(&f, &c, "create_chat", json!({ "agentId": f.agent["id"] })).await;
        assert_eq!(denied["result"]["isError"], true, "{denied}");
    }
    f.service
        .agent(
            json!({
                "name": "Other VM director", "access": { "installation": false }
            }),
            parent_b_agent["id"].as_str(),
        )
        .await
        .unwrap();
    let node = f.node.clone();
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &existing_native, &node))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn changing_the_parent_permission_or_node_ceiling_revokes_descendants() {
    let f = Fixture::new().await;
    let (parent, _) = delegate(&f, true).await;
    let mut child = f.run();
    child["nativeParentRunId"] = parent["id"].clone();
    f.service
        .store
        .patch_run(
            parent["id"].as_str().unwrap(),
            json!({ "nativeAuthorityNodes": [] }),
        )
        .await
        .unwrap();
    let (run, node) = (child.clone(), f.node.clone());
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &run, &node))
            .await
            .unwrap()
    );
    f.service
        .store
        .patch_run(
            parent["id"].as_str().unwrap(),
            json!({ "nativeAuthorityNodes": [f.node["id"]] }),
        )
        .await
        .unwrap();
    let mut agent = parent["snapshot"]["agent"].clone();
    agent["access"]["installation"] = false.into();
    f.service.store.put("agents", agent).await.unwrap();
    let node = f.node.clone();
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &child, &node))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn editing_a_vm_task_cannot_replace_its_authors_with_a_native_trusted_editor() {
    let f = Fixture::new().await;
    let vm = f
        .service
        .agent(json!({ "name": "VM task agent" }), None)
        .await
        .unwrap();
    let task = policy::CALLER.scope(Some(OTHER.into()), f.service.task(
        json!({ "name": "Retained commitment", "prompt": "Original brief", "agentId": vm["id"] }), None
    )).await.unwrap();
    assert_eq!(task["authorId"], "fixture-owner");
    let mut edited = task.clone();
    edited["agentId"] = f.agent["id"].clone();
    let error = policy::CALLER
        .scope(
            Some(TRUSTED.into()),
            f.service.task(edited, task["id"].as_str()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.status, 403);
    assert_eq!(
        f.service
            .get("tasks", task["id"].as_str().unwrap())
            .await
            .unwrap()["agentId"],
        vm["id"]
    );
}

async fn trust_owner(f: &Fixture) {
    let mut node = f.node.clone();
    node["nativeAccounts"] = json!([TRUSTED, "fixture-owner"]);
    f.service.store.put("nodes", node).await.unwrap();
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

#[tokio::test]
async fn native_tasks_retain_previous_directing_editors_and_current_author_rights() {
    let f = Fixture::new().await;
    trust_owner(&f).await;
    let task = policy::CALLER.scope(Some("fixture-owner".into()), f.service.task(
        json!({ "name": "Native commitment", "prompt": "Original brief", "agentId": f.agent["id"] }), None
    )).await.unwrap();
    let mut edited = task.clone();
    edited["prompt"] = "Updated brief".into();
    let saved = policy::CALLER
        .scope(
            Some(TRUSTED.into()),
            f.service.task(edited, task["id"].as_str()),
        )
        .await
        .unwrap();
    assert!(
        saved["nativePrincipals"]
            .as_array()
            .unwrap()
            .contains(&json!(TRUSTED))
    );
    assert!(
        saved["nativePrincipals"]
            .as_array()
            .unwrap()
            .contains(&json!("fixture-owner"))
    );
    f.service.store.put("nodes", f.node.clone()).await.unwrap();
    let error = policy::CALLER
        .scope(
            Some(TRUSTED.into()),
            f.service.task(saved, task["id"].as_str()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.status, 403);
}

#[tokio::test]
async fn revoked_native_permissions_still_allow_disabling_but_never_reenabling_a_task() {
    let f = Fixture::new().await;
    trust_owner(&f).await;
    let owner = RelayContext::new(&common::relay_fixture::context(&f.service).await);
    let (status, task) = f
        .call(
            &owner,
            "POST",
            "/api/tasks",
            json!({
                "name": "Native schedule", "prompt": "Build", "agentId": f.agent["id"],
                "cron": "0 * * * *", "timezone": "UTC"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{task}");
    f.service.store.put("nodes", f.node.clone()).await.unwrap();
    // Even an unavailable authority must not block an execution-reducing edit.
    f.service
        .store
        .set(
            "native-authority",
            json!({ "observedAt": 0, "accounts": {} }),
            None,
        )
        .await
        .unwrap();
    let mut disabled = task.clone();
    disabled["enabled"] = false.into();
    let path = format!("/api/tasks/{}", task["id"].as_str().unwrap());
    let (status, saved) = f.call(&owner, "PUT", &path, disabled).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["enabled"], false);
    assert_eq!(saved["nativePrincipals"], task["nativePrincipals"]);
    assert_eq!(saved["nativeAuthorityNodes"], task["nativeAuthorityNodes"]);
    f.service
        .store
        .set(
            "native-authority",
            json!({
                "observedAt": now(),
                "accounts": { TRUSTED: {} },
            }),
            None,
        )
        .await
        .unwrap();
    let mut enabled = saved;
    enabled["enabled"] = true.into();
    let (status, _) = f.call(&owner, "PUT", &path, enabled).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn scheduled_native_placement_rechecks_the_retained_author_even_on_older_provenance() {
    let f = Fixture::new().await;
    let mut run = f.run();
    run["trigger"] = "schedule".into();
    run["snapshot"]["task"] = json!({ "authorId": OTHER });
    common::add_run(&f.service.store, &run).await;
    assert!(placement::check(&f.service, &run).await.is_err());
}
