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

#[tokio::test]
async fn an_owner_approved_bot_can_submit_to_its_native_agent() {
    use cairn_installation::plugins::{capabilities, registry, work};

    let f = Fixture::new().await;
    f.service
        .plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let mut node = f.node.clone();
    node["nativeAccounts"] = json!([TRUSTED, "fixture-owner"]);
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    f.service.store.set("native-authority", json!({
        "observedAt": now(),
        "accounts": { "fixture-owner": { "email": "owner@example.test", "accessId": "fixture-owner" } }
    }), None).await.unwrap();
    let owner = RelayContext::new(&common::relay_fixture::context(&f.service).await);
    let input = json!({
        "name": "Native bot", "enabled": true, "agentId": f.agent["id"],
        "accessMode": "everyone", "settings": { "guildId": "100", "channelId": "200" },
        "token": "synthetic-discord-token"
    });
    let mut forged = input.clone();
    forged["nativeAuthority"] =
        json!({ "nativePrincipals": ["fixture-owner"], "nativeProvenanceUnknown": false });
    assert_eq!(
        common::save_plugin(&f.service, "discord", &forged, None)
            .await
            .unwrap_err()
            .status,
        400,
        "The typed command must not accept authority from JSON"
    );
    let denied = common::save_plugin(&f.service, "discord", &input, None)
        .await
        .unwrap_err();
    assert_eq!(
        denied.status, 403,
        "Unscoped management cannot mint a native bot grant"
    );
    let (status, saved) = f
        .call_as(
            &owner,
            "POST",
            "/api/plugins/discord/connections",
            input,
            false,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let connection = f
        .service
        .store
        .plugin_instance(saved["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let event = json!({
        "name": "message", "revision": connection.revision, "authorId": "external-person",
        "payload": { "id": "external-message", "author": { "id": "external-person", "name": "Person" } }
    });
    let context = capabilities::Context {
        management: None,
        connection: connection.clone(),
        manifest: registry::builtin("discord").unwrap().manifest,
        event,
        link: None,
        draft: None,
    };
    let receipt = work::submit(
        &f.service,
        &context,
        &json!({
            "text": "Which projects can you access?", "thread": "500", "shared": true
        }),
    )
    .await
    .expect("The selected native agent must honor the owner's bot delegation");
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let chat = f
        .service
        .get("chats", text(&receipt, "chatId"))
        .await
        .unwrap();
    let run = f.service.store.run(text(&chat, "runId")).await.unwrap();
    assert_eq!(run["nativePrincipals"], json!(["fixture-owner"]));
    assert_eq!(run["nativeProvenanceUnknown"], false);
    let check = run.clone();
    let target = node.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &check, &target))
            .await
            .unwrap()
    );

    // The current mission view must retain the plugin's revocation lineage through
    // SQL reload, replacement and the frozen launch snapshot, independently of its author.
    let mission_owner =
        auth::InstallationIdentity::trusted(auth::InstallationRole::Owner, "fixture-owner");
    let mission = f.service.task_as_fixture(json!({
        "name": "Retained bot commitment", "prompt": "Follow the approved delegation", "agentId": f.agent["id"],
        "cron": "0 9 * * *", "timezone": "UTC"
    }), None, Some(&mission_owner)).await.unwrap();
    let mut stored = f
        .service
        .store
        .mission(text(&mission, "id"))
        .await
        .unwrap()
        .unwrap();
    stored.native_plugin_grants =
        serde_json::from_value(run["nativePluginGrants"].clone()).unwrap();
    f.service
        .store
        .transaction(move |db| db.save_mission(&stored))
        .await
        .unwrap();
    let replaced = f
        .service
        .task_as_fixture(
            json!({ "prompt": "Retain the approved delegation after editing" }),
            Some(text(&mission, "id")),
            Some(&mission_owner),
        )
        .await
        .unwrap();
    assert_eq!(replaced["nativePluginGrants"], run["nativePluginGrants"]);
    let restarted = Service::new(f.service.config.clone()).await.unwrap();
    assert_eq!(
        serde_json::to_value(
            restarted
                .store
                .mission(text(&mission, "id"))
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        replaced
    );
    let queued = f
        .service
        .enqueue(text(&mission, "id"), "schedule", None)
        .await
        .unwrap();
    assert_eq!(queued["nativePluginGrants"], run["nativePluginGrants"]);
    assert_eq!(queued["snapshot"]["task"], replaced);

    let original = serde_json::to_value(&connection).unwrap();
    for (label, field, value) in [
        ("disabled", "enabled", json!(false)),
        ("replaced", "revision", json!(id())),
        ("person removed", "accessMode", json!("allowlist")),
        ("owner changed", "ownerAccessId", json!("different-owner")),
        (
            "agent policy revoked",
            "grantedAccess",
            json!({ "backend": "firecracker" }),
        ),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        f.service.store.put_instance_fixture(changed).await.unwrap();
        let (check, target) = (run.clone(), node.clone());
        assert!(
            !f.service
                .store
                .read(move |db| policy::allows(db, &check, &target))
                .await
                .unwrap(),
            "{label}"
        );
        let (check, target) = (queued.clone(), node.clone());
        assert!(
            !f.service
                .store
                .read(move |db| policy::allows(db, &check, &target))
                .await
                .unwrap(),
            "Mission lineage: {label}"
        );
        f.service
            .store
            .put_instance_fixture(original.clone())
            .await
            .unwrap();
    }

    f.service.store.set("native-authority", json!({
        "observedAt": now(), "accounts": {
            "fixture-owner": { "email": "owner@example.test", "accessId": "replacement-access" }
        }
    }), None).await.unwrap();
    let (check, target) = (run.clone(), node.clone());
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &check, &target))
            .await
            .unwrap(),
        "Fresh membership must match the owner access that approved the delegation"
    );

    common::enable_plugin(
        &f.service,
        "discord",
        &connection.id,
        &json!({ "enabled": false }),
    )
    .await
    .unwrap();
    let paused = f
        .service
        .task_as_fixture(
            json!({ "enabled": false }),
            Some(text(&mission, "id")),
            Some(&mission_owner),
        )
        .await
        .unwrap();
    assert_eq!(paused["nativePluginGrants"], replaced["nativePluginGrants"]);
    assert_eq!(
        f.service.store.run(text(&queued, "id")).await.unwrap()["snapshot"]["task"],
        replaced
    );
    let denied = f
        .service
        .task_as_fixture(
            json!({ "enabled": true }),
            Some(text(&mission, "id")),
            Some(&mission_owner),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.status, 403);
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &run, &node))
            .await
            .unwrap(),
        "Disabling the bot must revoke its native work, including already queued work"
    );
}

#[tokio::test]
async fn native_administrator_requires_owner_review_verified_generation_and_explicit_admission() {
    for backend in ["native", "auto"] {
        administrator_admission(backend).await;
    }
}

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

#[tokio::test]
async fn native_mode_preview_detects_concurrent_access_changes_and_agent_requests_need_owner() {
    let f = Fixture::new().await;
    let node_id = text(&f.node, "id");
    let path = format!("/api/nodes/{node_id}/native-mode");
    let mut node = f.node.clone();
    node["capabilities"]["nativeModeProtocol"] = 1.into();
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let (_, preview) = f
        .owner_call("POST", &path, json!({ "mode": "administrator" }))
        .await;
    node["nativeAccounts"] = json!([TRUSTED, OTHER]);
    f.service.store.put_node_fixture(node).await.unwrap();
    let (status, _) = f
        .owner_call(
            "POST",
            &path,
            json!({
                "mode": "administrator", "previewId": preview["previewId"], "confirm": true,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let preview = cairn_installation::nodes::native_mode::change(
        &f.service,
        node_id,
        json!({ "mode": "administrator" }),
        |_| Ok(()),
    )
    .await
    .unwrap();
    let proposal = cairn_installation::nodes::native_mode::change(
        &f.service,
        node_id,
        json!({
            "mode": "administrator", "previewId": preview["previewId"], "confirm": true,
        }),
        |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(proposal["status"], "awaiting-owner");
    assert!(f.service.node_boundary(node_id).await.unwrap()["nativeModeRequest"].is_null());
    // The owner reviews again through the same API and can then apply the request.
    let (_, preview) = f
        .owner_call("POST", &path, json!({ "mode": "administrator" }))
        .await;
    let (status, changed) = f
        .owner_call(
            "POST",
            &path,
            json!({
                "mode": "administrator", "previewId": preview["previewId"], "confirm": true,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(changed["node"]["nativeModeProposal"].is_null());
}

#[tokio::test]
async fn converting_a_mission_retains_native_authority_and_rejects_unauthorized_callers() {
    let f = Fixture::new().await;
    let mut run = f.run();
    run["status"] = "succeeded".into();
    run["sessionId"] = "saved-session".into();
    run["projectId"] = Value::Null;
    run["snapshot"]["task"] =
        json!({ "name": "Native mission", "prompt": "Inspect the workspace" });
    let run_id = run["id"].as_str().unwrap().to_owned();
    let saved = run.clone();
    f.service
        .store
        .transaction(move |db| {
            db.add_run_fixture(&saved, None)?;
            db.save_checkpoint_boundary(saved["id"].as_str().unwrap(), &json!({ "prepared": {} }))
        })
        .await
        .unwrap();
    let run = f.service.store.run(&run_id).await.unwrap();
    let path = format!("/api/runs/{run_id}/chat");
    let (status, _) = f.call(&f.other, "POST", &path, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(f.service.chat_list().await.unwrap().is_empty());
    assert_eq!(f.service.store.run(&run_id).await.unwrap(), run);
    let (status, chat) = f.call(&f.trusted, "POST", &path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{chat}");
    assert_eq!(chat["nativePrincipals"], json!([TRUSTED]));
    assert_eq!(chat["nativeAuthorityNodes"], json!([f.node["id"]]));
    assert_eq!(chat["nativeProvenanceUnknown"], false);
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

#[tokio::test]
async fn public_mcp_preserves_the_trusted_native_caller_and_automatic_priority() {
    for backend in ["native", "auto"] {
        let mut f = Fixture::new().await;
        let vm = f.vm_node().await;
        let nodes = if backend == "auto" {
            json!([f.node["id"], vm["id"]])
        } else {
            json!([f.node["id"]])
        };
        f.agent = f
            .service
            .agent_fixture(
                json!({
                    "access": { "backend": backend, "nodes": nodes },
                    "nodePriorities": nodes,
                }),
                f.agent["id"].as_str(),
            )
            .await
            .unwrap();
        let response = public_mcp_call(
            &f,
            TRUSTED,
            &["manage"],
            "create_chat",
            json!({ "agentId": f.agent["id"], "projectId": null }),
        )
        .await;
        assert_ne!(response["result"]["isError"], true, "{backend}: {response}");
        let chat = &response["result"]["structuredContent"]["result"];
        assert_eq!(
            chat["nativePrincipals"],
            json!([TRUSTED]),
            "{backend}: {chat}"
        );
        assert_eq!(chat["nativeAuthorityNodes"], json!([f.node["id"]]));
        assert_eq!(chat["nativeProvenanceUnknown"], false);

        let response = public_mcp_call(
            &f, TRUSTED, &["run"], "send_chat_message",
            json!({ "chatId": chat["id"], "message": { "id": id(), "text": "Continue the authorized work" } }),
        ).await;
        assert_ne!(response["result"]["isError"], true, "{backend}: {response}");
        f.service.chat_tick(&HashSet::new()).await.unwrap();
        let chat = f.service.get("chats", text(chat, "id")).await.unwrap();
        let run = f.service.store.run(text(&chat, "runId")).await.unwrap();
        assert_eq!(run["nativePrincipals"], json!([TRUSTED]));
        assert_eq!(run["nativeProvenanceUnknown"], false);
        let placed = placement::reserve(&f.service, &run, &id()).await.unwrap();
        assert_eq!(placed["nodeId"], f.node["id"], "{backend}: {placed}");
    }
}

#[tokio::test]
async fn public_mcp_native_execution_keeps_caller_and_oauth_scope_restrictions() {
    let f = Fixture::new().await;
    for (account, scopes) in [(OTHER, vec!["manage"]), (TRUSTED, vec!["read"])] {
        let response = public_mcp_call(
            &f,
            account,
            &scopes,
            "create_chat",
            json!({ "agentId": f.agent["id"], "projectId": null }),
        )
        .await;
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert!(f.service.chat_list().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn public_mcp_rechecks_the_native_caller_when_directing_existing_work() {
    let f = Fixture::new().await;
    let (status, chat) = f
        .call(
            &f.trusted,
            "POST",
            "/api/chats",
            json!({ "agentId": f.agent["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{chat}");
    let args = json!({
        "chatId": chat["id"],
        "message": { "id": id(), "text": "Continue the authorized work" },
    });
    for (account, scopes) in [(OTHER, vec!["run"]), (TRUSTED, vec!["manage"])] {
        let response =
            public_mcp_call(&f, account, &scopes, "send_chat_message", args.clone()).await;
        assert_eq!(response["result"]["isError"], true, "{response}");
    }
    let mut node = f.node.clone();
    node["nativeAccounts"] = json!([]);
    f.service.store.put_node_fixture(node).await.unwrap();
    let response = public_mcp_call(&f, TRUSTED, &["run"], "send_chat_message", args).await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    let saved = f.service.get("chats", text(&chat, "id")).await.unwrap();
    assert_eq!(
        saved, chat,
        "Rejected calls must not change the conversation"
    );
}

#[tokio::test]
async fn automatic_placement_uses_agent_priorities_and_falls_back_without_converting_existing_work()
{
    use cairn_installation::nodes::LOCAL_NODE_ID;

    for scenario in [
        "preferred",
        "full",
        "untrusted",
        "offline",
        "existing-vm",
        "preferred-vm",
        "no-controller",
    ] {
        let mut f = Fixture::new().await;
        let vm = f.vm_node().await;
        let vm_id = if scenario == "no-controller" {
            LOCAL_NODE_ID
        } else {
            text(&vm, "id")
        };
        f.agent = f
            .service
            .agent_fixture(
                json!({
                    "access": { "backend": "auto", "nodes": [f.node["id"], vm_id] },
                    "nodePriorities": if scenario == "no-controller" { json!([vm_id, f.node["id"]]) } else { json!([f.node["id"], vm_id]) },
                }),
                f.agent["id"].as_str(),
            )
            .await
            .unwrap();
        let persisted = f
            .service
            .agent_snapshot(text(&f.agent, "id"))
            .await
            .unwrap();
        assert_eq!(persisted["nodePriorities"], f.agent["nodePriorities"]);

        let mut run = f.run();
        match scenario {
            "full" => {
                f.service
                    .store
                    .put(
                        "node-attempts",
                        json!({
                            "id": id(), "runId": id(), "nodeId": f.node["id"], "role": "execution",
                            "released": false, "leaseExpiresAt": now() + 60_000,
                            "resources": { "cpu": 2, "memoryMiB": 4096, "diskMiB": 32768 },
                        }),
                    )
                    .await
                    .unwrap();
            }
            "offline" => {
                f.node["lastSeen"] = 0.into();
            }
            "untrusted" => {
                run["nativeProvenanceUnknown"] = true.into();
            }
            "existing-vm" => {
                run["snapshot"]["agent"]["access"]["backend"] = "firecracker".into();
            }
            "preferred-vm" => {
                run["preferredNodeId"] = vm_id.into();
            }
            _ => {}
        }
        f.service
            .store
            .put_node_fixture(f.node.clone())
            .await
            .unwrap();
        common::add_run(&f.service.store, &run).await;
        let attempt = id();
        let placement = placement::reserve(&f.service, &run, &attempt)
            .await
            .unwrap();
        let expected = if matches!(scenario, "preferred" | "no-controller") {
            text(&f.node, "id")
        } else {
            vm_id
        };
        assert_eq!(placement["nodeId"], expected, "{scenario}");
        let saved = f.service.store.run(text(&run, "id")).await.unwrap();
        let backend = if matches!(scenario, "preferred" | "no-controller") {
            "native"
        } else {
            "firecracker"
        };
        assert_eq!(
            saved["snapshot"]["agent"]["access"]["backend"], backend,
            "{scenario}"
        );
        if scenario == "existing-vm" {
            f.service
                .store
                .update_run_boundary(text(&saved, "id"), json!({ "nodeId": vm_id }))
                .await
                .unwrap();
            let grant = cairn_installation::nodes::disk_grants::new_disk(&f.service, &saved, vm_id)
                .await
                .unwrap();
            assert!(
                cairn_installation::nodes::disk_grants::authorize(&f.service, &grant)
                    .await
                    .is_ok()
            );
        }
        placement::release(&f.service, &attempt).await.unwrap();

        if scenario == "preferred" {
            f.node["accepting"] = false.into();
            f.service
                .store
                .put_node_fixture(f.node.clone())
                .await
                .unwrap();
            assert!(placement::is_no_capacity(
                &placement::reserve(&f.service, &run, &id())
                    .await
                    .unwrap_err()
            ));
        }
    }
}

#[tokio::test]
async fn priorities_never_authorize_an_ungranted_node() {
    let f = Fixture::new().await;
    let error = f
        .service
        .agent_fixture(
            json!({
                "access": { "backend": "auto", "nodes": [f.node["id"]] },
                "nodePriorities": [cairn_installation::nodes::LOCAL_NODE_ID],
            }),
            f.agent["id"].as_str(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.status, 400);
    assert!(error.message.contains("Only authorized nodes"));
    let saved = f
        .service
        .agent_snapshot(text(&f.agent, "id"))
        .await
        .unwrap();
    assert_eq!(saved["access"]["backend"], "native");
}

#[tokio::test]
async fn automatic_profiles_offer_optional_conversion_to_the_other_environment() {
    let mut f = Fixture::new().await;
    let vm = f.vm_node().await;
    f.node["nativeAccounts"] = json!([TRUSTED, OTHER]);
    f.service
        .store
        .put_node_fixture(f.node.clone())
        .await
        .unwrap();
    f.agent = f
        .service
        .agent_fixture(
            json!({ "access": { "backend": "auto", "nodes": [f.node["id"], vm["id"]] } }),
            f.agent["id"].as_str(),
        )
        .await
        .unwrap();
    let (_, run) = f.conversation().await;
    for from in ["firecracker", "native"] {
        let mut snapshot = run["snapshot"].clone();
        snapshot["agent"]["access"]["backend"] = from.into();
        f.service
            .store
            .change_run(
                text(&run, "id"),
                cairn_installation::runs::RunChanges {
                    snapshot: Some(serde_json::from_value(snapshot).unwrap()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let (status, preview) = f
            .owner_call(
                "GET",
                &format!(
                    "/api/nodes/placement/{}/backend-migration",
                    text(&run, "id")
                ),
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert_eq!(preview["fromBackend"], from);
        assert_eq!(preview["required"], false);
        assert_eq!(preview["optional"], true);
        let (to, node) = if from == "native" {
            ("firecracker", &vm["id"])
        } else {
            ("native", &f.node["id"])
        };
        assert_eq!(preview["toBackend"], to);
        assert!(preview["reason"].is_null(), "{preview}");
        assert!(
            preview["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|candidate| &candidate["id"] == node),
            "{preview}"
        );
    }
}

#[tokio::test]
async fn expired_conversations_explicitly_change_backend_without_recovering_deleted_disks() {
    for destination_backend in ["native", "firecracker"] {
        let f = Fixture::new().await;
        let vm = f.vm_node().await;
        let (chat, run) = f.conversation().await;
        let chat_id = text(&chat, "id").to_owned();
        let run_id = text(&run, "id").to_owned();
        let from = if destination_backend == "native" {
            "firecracker"
        } else {
            "native"
        };
        let mut snapshot = run["snapshot"].clone();
        snapshot["agent"]["access"]["backend"] = from.into();
        let (cid, rid) = (chat_id.clone(), run_id.clone());
        f.service
            .store
            .transaction(move |db| {
                for mut message in db.messages(&cid)? {
                    message["status"] = "delivered".into();
                    db.put_message(&message)?;
                }
                db.change_run(
                    &rid,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_installation::run_status::RunStatus::Succeeded),
                        snapshot: Some(serde_json::from_value(snapshot).unwrap()),
                        recovery_pending: Some(false),
                        boundary: Some(json!({
                            "environmentState": "expired",
                            "environmentExpiredAt": now(),
                            "isolated": false,
                            "nodeId": null,
                        })),
                        ..Default::default()
                    },
                )?;
                db.save_checkpoint_boundary(&rid, &json!({ "completed": true }))?;
                let mut chat = db.get("chats", &cid)?.unwrap();
                chat["environmentState"] = "expired".into();
                chat["environmentExpiredAt"] = now().into();
                chat["paused"] = true.into();
                db.put("chats", &chat)?;
                Ok(())
            })
            .await
            .unwrap();
        let destination = if destination_backend == "native" {
            &f.node
        } else {
            &vm
        };
        f.service.agent_fixture(json!({ "access": { "backend": destination_backend, "nodes": [destination["id"]] } }), f.agent["id"].as_str()).await.unwrap();
        let path = format!("/api/nodes/placement/{run_id}/backend-migration");
        let input =
            json!({ "nodeId": destination["id"], "backend": destination_backend, "confirm": true });
        let (status, preview) = f.owner_call("GET", &path, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert_eq!(preview["environmentExpired"], true);
        assert_eq!(preview["required"], true);
        // Only the trusted caller has native authority; management cannot invent it.
        if destination_backend == "native" {
            assert!(preview["nodes"].as_array().unwrap().is_empty());
            assert_eq!(
                f.owner_call("POST", &path, input.clone()).await.0,
                StatusCode::FORBIDDEN
            );
            let mut node = f.node.clone();
            node["nativeAccounts"] = json!([TRUSTED, OTHER]);
            f.service.store.put_node_fixture(node).await.unwrap();
        }
        let mut unconfirmed = input.clone();
        unconfirmed["confirm"] = false.into();
        assert_eq!(
            f.owner_call("POST", &path, unconfirmed).await.0,
            StatusCode::BAD_REQUEST
        );
        // Expiration does not bypass the existing restriction on other reduced access.
        let before = f.service.store.run(&run_id).await.unwrap();
        let mut broader = before["snapshot"].clone();
        broader["agent"]["access"]["installation"] = true.into();
        f.service
            .store
            .change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    snapshot: Some(serde_json::from_value(broader).unwrap()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let (status, denied) = f.owner_call("POST", &path, input.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{denied}");
        assert!(text(&denied, "error").contains("Other agent access was reduced"));
        f.service
            .store
            .change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    snapshot: Some(serde_json::from_value(before["snapshot"].clone()).unwrap()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let (status, result) = f.owner_call("POST", &path, input).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        let migrated = f.service.store.run(&run_id).await.unwrap();
        assert_eq!(
            migrated["snapshot"]["agent"]["access"]["backend"],
            destination_backend
        );
        assert_eq!(migrated["nodeId"], destination["id"]);
        assert!(migrated["backup"].is_null());
        assert!(migrated["backendMigration"].is_null());
        assert!(migrated["sessionId"].is_null());
        let checkpoint = f
            .service
            .store
            .checkpoint_boundary(&run_id)
            .await
            .unwrap()
            .unwrap();
        assert!(checkpoint["prepared"].is_null());
        assert_eq!(
            f.service.get("chats", &chat_id).await.unwrap()["paused"],
            true
        );
        assert_eq!(
            f.owner_call(
                "POST",
                &format!("/api/chats/{chat_id}/pause"),
                json!({ "paused": false })
            )
            .await
            .0,
            StatusCode::OK
        );
        let (status, message) = f
            .owner_call(
                "POST",
                &format!("/api/chats/{chat_id}/messages"),
                json!({ "id": id(), "text": "Continue in a fresh environment" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{message}");
        f.service.chat_tick(&HashSet::new()).await.unwrap();
        let continued = f.service.store.run(&run_id).await.unwrap();
        assert_eq!(continued["status"], "queued");
        assert!(continued["environmentState"].is_null());
        assert_eq!(
            continued["snapshot"]["agent"]["access"]["backend"],
            destination_backend
        );
        assert_eq!(
            f.service.get("chats", &chat_id).await.unwrap()["runId"],
            run_id
        );
    }
}

#[tokio::test]
async fn native_authority_outage_preserves_automatic_vm_admission_and_vm_continuations() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    for mixed in [false, true] {
        let mut f = Fixture::new().await;
        let vm = f.vm_node().await;
        let nodes = if mixed {
            json!([vm["id"], f.node["id"]])
        } else {
            json!([vm["id"]])
        };
        f.agent = f
            .service
            .agent_fixture(
                json!({ "access": { "backend": "auto", "nodes": nodes } }),
                f.agent["id"].as_str(),
            )
            .await
            .unwrap();
        let (listener, address) = common::bind().await;
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().fallback(move || {
                    observed.fetch_add(1, Ordering::SeqCst);
                    async { StatusCode::SERVICE_UNAVAILABLE }
                }),
            )
            .await
            .unwrap();
        });
        let identity_dir = f.service.config.data_dir.join("installation-relay");
        std::fs::create_dir_all(&identity_dir).unwrap();
        let identity = identity_dir.join("identity.json");
        std::fs::write(&identity, json!({ "origin": format!("http://{address}"), "installationId": id(), "token": "synthetic-native-authority-test" }).to_string()).unwrap();
        std::fs::set_permissions(identity, std::fs::Permissions::from_mode(0o600)).unwrap();
        f.service.store.set("native-authority", json!({ "observedAt": now() - 30_000, "accounts": { TRUSTED: { "email": "trusted@example.test", "accessId": "one" } } }), None).await.unwrap();

        let (chat, run) = f.conversation().await;
        assert_eq!(requests.load(Ordering::SeqCst) > 0, mixed);
        let placement = placement::reserve(&f.service, &run, &id()).await.unwrap();
        assert_eq!(placement["nodeId"], vm["id"]);
        let before = requests.load(Ordering::SeqCst);
        policy::refresh_chat(&f.service, text(&chat, "id"))
            .await
            .unwrap();
        assert_eq!(
            requests.load(Ordering::SeqCst),
            before,
            "A concrete VM never requires native authority"
        );
        let (status, body) = f
            .call(
                &f.trusted,
                "POST",
                &format!("/api/chats/{}/messages", text(&chat, "id")),
                json!({ "id": id(), "text": "Continue in this VM" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(requests.load(Ordering::SeqCst), before);

        let mut saved = f.service.store.run(text(&run, "id")).await.unwrap();
        saved["snapshot"]["agent"]["access"]["backend"] = "native".into();
        f.service
            .store
            .change_run(
                text(&run, "id"),
                cairn_installation::runs::RunChanges {
                    snapshot: Some(serde_json::from_value(saved["snapshot"].clone()).unwrap()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            policy::refresh_chat(&f.service, text(&chat, "id"))
                .await
                .is_err(),
            "A concrete native environment still requires fresh authority"
        );
        assert!(requests.load(Ordering::SeqCst) > before);
        server.abort();
    }
}

#[tokio::test]
async fn automatic_vm_only_bot_delegation_needs_no_native_authority() {
    let mut f = Fixture::new().await;
    f.service
        .plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    f.agent = f.service.agent_fixture(json!({ "access": { "backend": "auto", "nodes": [cairn_installation::nodes::LOCAL_NODE_ID] } }), f.agent["id"].as_str()).await.unwrap();
    f.service
        .store
        .set(
            "native-authority",
            json!({ "observedAt": 0, "accounts": {} }),
            None,
        )
        .await
        .unwrap();
    let owner = RelayContext::new(&common::relay_fixture::context(&f.service).await);
    let (status, saved) = f
        .call_as(
            &owner,
            "POST",
            "/api/plugins/discord/connections",
            json!({
                "name": "Automatic VM bot", "enabled": true, "agentId": f.agent["id"],
                "accessMode": "everyone", "settings": { "guildId": "100", "channelId": "200" },
                "token": "synthetic-discord-token"
            }),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let connection = f
        .service
        .store
        .plugin_instance(text(&saved, "id"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connection.granted_access.unwrap().backend, "auto");
    assert!(connection.native_authority.is_none());
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
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let mut config = common::config(root.path());
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
        let chat = self.service.get("chats", chat_id).await.unwrap();
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

#[tokio::test]
async fn delegated_native_administration_requires_the_requesters_current_review() {
    let mut f = Fixture::with_workspace(true).await;
    f.agent["access"]["delegation"] = true.into();
    f.agent["access"]["nodes"] = Value::Null;
    f.service
        .store
        .put_agent_fixture(f.agent.clone())
        .await
        .unwrap();

    let mut administrator = f.node.clone();
    administrator["id"] = id().into();
    administrator["name"] = "Administrative host".into();
    f.service
        .store
        .put_node_fixture(administrator.clone())
        .await
        .unwrap();
    let target = f
        .service
        .agent_fixture(
            json!({
                "name": "Reviewed target",
                "access": { "backend": "native", "nodes": [administrator["id"]] },
            }),
            None,
        )
        .await
        .unwrap();
    let (_, parent) = f.conversation().await;
    let parent = f
        .service
        .store
        .change_run(
            text(&parent, "id"),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Running),
                boundary: Some(serde_json::json!({"nodeId": f.node["id"]})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let parent = f.service.store.run(&parent.id).await.unwrap();
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &parent)
        .await
        .unwrap();
    let token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");

    let generation = id();
    administrator["capabilities"]["nativeModeProtocol"] = 1.into();
    administrator["nativeModeRequest"] =
        json!({ "mode": "administrator", "generation": generation });
    administrator["nativeModeObserved"] = json!({
        "mode": "administrator", "generation": generation, "status": "applied",
    });
    administrator["nativeReviewedAgents"] = json!([target["id"]]);
    f.service
        .store
        .put_node_fixture(administrator.clone())
        .await
        .unwrap();
    let input = json!({ "task": "Review on the administrative host", "agentId": target["id"] });
    let error = cairn_installation::delegation::delegate(&f.service, token, &input)
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::FORBIDDEN);

    administrator["nativeReviewedAgents"] = json!([target["id"], f.agent["id"]]);
    f.service
        .store
        .put_node_fixture(administrator.clone())
        .await
        .unwrap();
    let task = cairn_installation::delegation::delegate(&f.service, token, &input)
        .await
        .unwrap();
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let child = f.service.chat_detail(text(&task, "chatId")).await.unwrap()["run"].clone();
    let check = |node: Value| {
        let child = child.clone();
        f.service
            .store
            .read(move |db| policy::allows(db, &child, &node))
    };
    assert!(check(administrator.clone()).await.unwrap());

    administrator["nativeReviewedAgents"] = json!([target["id"]]);
    f.service
        .store
        .put_node_fixture(administrator.clone())
        .await
        .unwrap();
    assert!(
        !check(administrator).await.unwrap(),
        "Removing the requester's review revokes its child's administrator admission"
    );
}

#[tokio::test]
async fn task_delegation_has_its_own_revocable_native_authority() {
    let mut f = Fixture::with_workspace(true).await;
    f.agent["access"]["delegation"] = true.into();
    f.service
        .store
        .put_agent_fixture(f.agent.clone())
        .await
        .unwrap();
    let (_, parent) = f.conversation().await;
    let parent = f
        .service
        .store
        .change_run(
            text(&parent, "id"),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let parent = f.service.store.run(&parent.id).await.unwrap();
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &parent)
        .await
        .unwrap();
    let task = cairn_installation::delegation::delegate(
        &f.service,
        text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN"),
        &json!({ "task": "Review on the authorized native node" }),
    )
    .await
    .unwrap();
    let child_id = text(&task, "chatId");
    let child = f.service.get("chats", child_id).await.unwrap();
    assert_eq!(child["nativeParentRunId"], parent["id"]);
    assert_eq!(child["nativeParentAccess"], "delegation");
    assert_eq!(child["nativeAuthorityNodes"], json!([f.node["id"]]));
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let child = f.service.chat_detail(child_id).await.unwrap()["run"].clone();
    assert_eq!(child["nativeParentAccess"], "delegation");

    let check = |run: Value| {
        let node = f.node.clone();
        f.service
            .store
            .read(move |db| policy::allows(db, &run, &node))
    };
    assert!(check(child.clone()).await.unwrap());
    let mut management_child = child.clone();
    management_child["nativeParentAccess"] = Value::Null;
    assert!(
        !check(management_child).await.unwrap(),
        "Management-created and historical edges still require installation access"
    );

    // A human follow-up preserves the capability that authorized the parent edge.
    policy::CALLER
        .scope(
            Some(TRUSTED.into()),
            f.service.chat_send(
                child_id,
                json!({ "id": id(), "text": "Also inspect the tests" }),
            ),
        )
        .await
        .unwrap();
    let continued = f.service.get("chats", child_id).await.unwrap();
    assert_eq!(continued["nativeParentAccess"], "delegation");

    let mut revoked = f.agent.clone();
    revoked["access"]["delegation"] = false.into();
    revoked["access"]["installation"] = true.into();
    f.service.store.put_agent_fixture(revoked).await.unwrap();
    assert!(
        !check(child.clone()).await.unwrap(),
        "Installation access cannot replace revoked task delegation"
    );
    f.service
        .store
        .put_agent_fixture(f.agent.clone())
        .await
        .unwrap();
    assert!(check(child.clone()).await.unwrap());
    f.service
        .store
        .update_run_boundary(text(&parent, "id"), json!({ "nativeAuthorityNodes": [] }))
        .await
        .unwrap();
    assert!(
        !check(child).await.unwrap(),
        "The parent's node ceiling remains live"
    );
}

#[tokio::test]
async fn rejected_native_mutations_do_not_become_historical_directors() {
    let f = Fixture::new().await;
    let (chat, run) = f.conversation().await;
    let (chat_id, run_id) = (chat["id"].as_str().unwrap(), run["id"].as_str().unwrap());
    let mut node = f.node.clone();
    node["nativeAccounts"] = json!([TRUSTED, OTHER]);
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let mutations = [
        (
            "POST",
            format!("/api/chats/{chat_id}/coding-agent"),
            json!({ "requestId": id(), "runId": run_id, "turnId": "stale-turn", "provider": "claude" }),
            StatusCode::CONFLICT,
        ),
        (
            "POST",
            format!("/api/chats/{chat_id}/messages"),
            json!({ "id": "invalid", "text": "Rejected" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            format!("/api/chats/{chat_id}/messages"),
            json!({ "id": id(), "text": "x".repeat(50_001) }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            format!("/api/chats/{chat_id}/unsupported"),
            json!({}),
            StatusCode::NOT_FOUND,
        ),
        (
            "POST",
            format!("/api/runs/{run_id}/resume"),
            json!({}),
            StatusCode::CONFLICT,
        ),
        (
            "POST",
            format!("/api/chats/{chat_id}/new-session"),
            json!({ "confirm": false }),
            StatusCode::CONFLICT,
        ),
        (
            "POST",
            format!("/api/chats/{chat_id}/restore"),
            json!({}),
            StatusCode::CONFLICT,
        ),
        (
            "POST",
            format!("/api/chats/{chat_id}/questions/{}/cancel", id()),
            json!({}),
            StatusCode::NOT_FOUND,
        ),
        (
            "PUT",
            format!("/api/nodes/placement/{run_id}"),
            json!({ "unsupported": true }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            format!("/api/nodes/placement/{run_id}/move"),
            json!({ "nodeId": node["id"], "cpu": 1 }),
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (method, path, body, expected) in mutations {
        let (status, response) = f.owner_call(method, &path, body).await;
        assert_eq!(status, expected, "{method} {path}: {response}");
        let current_chat = f.service.get("chats", chat_id).await.unwrap();
        let current_run = f.service.store.run(run_id).await.unwrap();
        for key in [
            "nativePrincipals",
            "nativeAuthorityNodes",
            "nativeParentRunId",
            "nativeProvenanceUnknown",
        ] {
            assert_eq!(current_chat[key], chat[key], "{path}: chat {key}");
            assert_eq!(current_run[key], run[key], "{path}: run {key}");
        }
    }

    // The MCP control path must also keep rejected resumes out of provenance.
    let director = f
        .service
        .agent_fixture(
            json!({ "name": "Other director", "access": { "installation": true } }),
            None,
        )
        .await
        .unwrap();
    let mut parent = f.run();
    parent["snapshot"]["agent"] = director;
    parent["status"] = "running".into();
    parent["nativePrincipals"] = json!([OTHER]);
    common::add_run(&f.service.store, &parent).await;
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &parent)
        .await
        .unwrap();
    let response = delegated_call(
        &f,
        &configuration,
        "resume_chat",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    let current_chat = f.service.get("chats", chat_id).await.unwrap();
    let current_run = f.service.store.run(run_id).await.unwrap();
    for key in [
        "nativePrincipals",
        "nativeAuthorityNodes",
        "nativeParentRunId",
        "nativeProvenanceUnknown",
    ] {
        assert_eq!(current_chat[key], chat[key], "MCP chat {key}");
        assert_eq!(current_run[key], run[key], "MCP run {key}");
    }

    // Revoking a rejected caller must leave the original person's work authorized.
    node["nativeAccounts"] = json!([TRUSTED]);
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let record = current_run;
    let target = node.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &record, &target))
            .await
            .unwrap()
    );

    // A successful unpause records the accepted director in the same effect.
    node["nativeAccounts"] = json!([TRUSTED, OTHER]);
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    f.service.chat_pause(chat_id, true).await.unwrap();
    let (status, response) = f
        .owner_call(
            "POST",
            &format!("/api/chats/{chat_id}/pause"),
            json!({ "paused": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let directed = f.service.store.run(run_id).await.unwrap();
    assert_eq!(directed["nativePrincipals"], json!([TRUSTED, OTHER]));
    assert_eq!(response["nativePrincipals"], directed["nativePrincipals"]);
    node["nativeAccounts"] = json!([TRUSTED]);
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &directed, &node))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn native_stop_controls_remain_available_without_host_admission() {
    for action in ["pause", "cancel", "stop"] {
        let f = Fixture::new().await;
        let (mut chat, run) = f.conversation().await;
        let chat_id = chat["id"].as_str().unwrap().to_owned();
        let run_id = run["id"].as_str().unwrap().to_owned();
        if action == "stop" {
            f.service
                .store
                .change_run(
                    &run_id,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_contracts::RunStatus::Succeeded),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            chat["wakeup"] = json!({ "at": now() + 60_000, "reason": "Saved continuation" });
            f.service.store.put("chats", chat).await.unwrap();
        }
        let mut node = f.node.clone();
        node["nativeEnabled"] = false.into();
        node["nativeAccounts"] = json!([]);
        f.service.store.put_node_fixture(node).await.unwrap();
        // The synthetic Beacon has no native-authority endpoint. A refresh
        // would fail; execution-reducing controls must not depend on it.
        f.service
            .store
            .set(
                "native-authority",
                json!({ "observedAt": 0, "accounts": {} }),
                None,
            )
            .await
            .unwrap();
        let path = if action == "cancel" {
            format!("/api/runs/{run_id}/cancel")
        } else {
            format!("/api/chats/{chat_id}/{action}")
        };
        let (status, response) = f
            .call(&f.other, "POST", &path, json!({ "paused": true }))
            .await;
        assert_eq!(status, StatusCode::OK, "{action}: {response}");
        let chat = f.service.get("chats", &chat_id).await.unwrap();
        let run = f.service.store.run(&run_id).await.unwrap();
        assert_eq!(chat["nativePrincipals"], json!([TRUSTED]));
        assert_eq!(run["nativePrincipals"], json!([TRUSTED]));
        if action != "cancel" {
            assert_eq!(chat["paused"], true);
        }
        if action == "stop" {
            assert!(chat["wakeup"].is_null());
        }
        if action == "cancel" {
            assert_eq!(run["status"], "cancelled");
        }
        if action == "pause" {
            let (status, response) = f
                .call(&f.other, "POST", &path, json!({ "paused": false }))
                .await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "unpause must still fail closed: {response}"
            );
            assert_eq!(
                f.service.get("chats", &chat_id).await.unwrap()["paused"],
                true
            );
        }
    }
}

#[tokio::test]
async fn accepted_native_run_controls_retain_the_director_on_the_chat_and_run() {
    for action in ["resume", "placement"] {
        let f = Fixture::new().await;
        let (chat, run) = f.conversation().await;
        let (chat_id, run_id) = (chat["id"].as_str().unwrap(), run["id"].as_str().unwrap());
        assert_eq!(chat["runId"], run["id"]);
        let mut node = f.node.clone();
        node["nativeAccounts"] = json!([TRUSTED, OTHER]);
        f.service
            .store
            .put_node_fixture(node.clone())
            .await
            .unwrap();
        let (method, path, body) = if action == "resume" {
            f.service
                .store
                .change_run(
                    run_id,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_contracts::RunStatus::Cancelled),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            ("POST", format!("/api/runs/{run_id}/resume"), json!({}))
        } else {
            (
                "PUT",
                format!("/api/nodes/placement/{run_id}"),
                json!({ "pinnedNodeId": node["id"] }),
            )
        };
        let (status, response) = f.owner_call(method, &path, body).await;
        assert_eq!(status, StatusCode::OK, "{action}: {response}");
        let chat = f.service.get("chats", chat_id).await.unwrap();
        let run = f.service.store.run(run_id).await.unwrap();
        assert_eq!(
            chat["nativePrincipals"],
            json!([TRUSTED, OTHER]),
            "{action}"
        );
        assert_eq!(
            run["nativePrincipals"], chat["nativePrincipals"],
            "{action}"
        );
        node["nativeAccounts"] = json!([TRUSTED]);
        f.service
            .store
            .put_node_fixture(node.clone())
            .await
            .unwrap();
        assert!(
            !f.service
                .store
                .read(move |db| policy::allows(db, &run, &node))
                .await
                .unwrap(),
            "{action}"
        );
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
    f.service.store.put_node_fixture(node).await.unwrap();
    assert!(placement::check(&f.service, &run).await.is_err());
}

#[tokio::test]
async fn moved_conversations_claim_reservations_for_their_execution_backend() {
    for (backend, changed_backend) in [("native", false), ("firecracker", false), ("native", true)]
    {
        let f = Fixture::new().await;
        let native = backend == "native";
        let mut node = f.node.clone();
        node["capabilities"]["backend"] = backend.into();
        node["capabilities"]["kvm"] = (!native).into();
        f.service
            .store
            .put_node_fixture(node.clone())
            .await
            .unwrap();
        let mut agent = f.agent.clone();
        agent["access"]["backend"] = backend.into();
        f.service
            .store
            .put_agent_fixture(agent.clone())
            .await
            .unwrap();

        let mut run = f.run();
        run["snapshot"]["agent"] = agent;
        run["placementTransition"] = true.into();
        run["targetNodeId"] = node["id"].clone();
        common::add_run(&f.service.store, &run).await;
        let run_id = run["id"].as_str().unwrap();
        let reservation = id();
        placement::reserve(&f.service, &run, &reservation)
            .await
            .unwrap();
        let held = f
            .service
            .store
            .get("node-attempts", &reservation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(held["role"], "destination");

        // An active/automatic move persists this state after restoring files.
        let resumed = f
            .service
            .store
            .change_run(
                run_id,
                cairn_installation::runs::RunChanges {
                    recovery_pending: Some(true),
                    status: Some(cairn_contracts::RunStatus::Queued),
                    boundary: Some(serde_json::json!({
                        "placementTransition": false,
                        "targetNodeId": null,
                        "moveReservation": reservation,
                        "nodeId": node["id"],
                        "nodeState": "resuming",
                    })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        common::set_checkpoint(
            &f.service.store,
            run_id,
            json!({ "nodeId": node["id"], "runtimeId": "development" }),
        )
        .await;

        if changed_backend {
            node["capabilities"]["backend"] = "firecracker".into();
            node["capabilities"]["kvm"] = true.into();
        }
        f.service.store.put_node_fixture(node).await.unwrap();
        let attempt = id();
        let resumed = f.service.store.run(&resumed.id).await.unwrap();
        let claimed = placement::reserve(&f.service, &resumed, &attempt).await;
        let succeeds = !changed_backend;
        if succeeds {
            let claimed = claimed.unwrap();
            assert_eq!(claimed["nodeId"], f.node["id"]);
            assert_eq!(claimed["resources"], held["resources"]);
            assert_eq!(claimed["runtimeId"], held["runtimeId"]);
            let execution = f
                .service
                .store
                .get("node-attempts", &attempt)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(execution["role"], "execution");
            assert!(execution["leaseExpiresAt"].as_i64().unwrap() > now());
        } else {
            let error = claimed.unwrap_err();
            assert_eq!(error.status, 409);
            assert_eq!(error.message, "Destination execution backend changed.");
            assert!(
                f.service
                    .store
                    .get("node-attempts", &attempt)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        let previous = f
            .service
            .store
            .get("node-attempts", &reservation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(previous["released"], succeeds);
        let persisted = f.service.store.run(run_id).await.unwrap();
        if succeeds {
            assert!(persisted["moveReservation"].is_null());
        } else {
            assert_eq!(persisted["moveReservation"], reservation);
        }
    }
}

#[tokio::test]
async fn retained_conversations_exhaust_the_shared_native_admission_budget() {
    let f = Fixture::new().await;
    let mut node = f.node.clone();
    node["usage"] = json!({ "diskMiB": 20 * 1024 });
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let first = f.run();
    common::add_run(&f.service.store, &first).await;
    let attempt = id();
    placement::reserve(&f.service, &first, &attempt)
        .await
        .unwrap();
    placement::release(&f.service, &attempt).await.unwrap();
    // Heartbeats include all retained conversations, not just a running slot.
    node["usage"] = json!({ "diskMiB": 40 * 1024 });
    f.service.store.put_node_fixture(node).await.unwrap();
    let next = f.run();
    common::add_run(&f.service.store, &next).await;
    assert!(placement::reserve(&f.service, &next, &id()).await.is_err());
}

#[tokio::test]
async fn native_same_node_resume_reuses_retained_capacity_on_a_small_pool() {
    let f = Fixture::new().await;
    let mut node = f.node.clone();
    node["limits"]["diskMiB"] = 32768.into();
    node["diskGrowthProtocol"] = 1.into();
    node["diskCapacity"] = json!({ "protocol": 1, "capturedAt": now(), "usedBytes": 0,
        "freeBytes": 900 * 1024 * 1024 * 1024u64, "ownedBytes": {}, "retainedBytes": {}, "requests": [] });
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let mut run = f.run();
    common::add_run(&f.service.store, &run).await;
    let first = id();
    let selected = placement::reserve(&f.service, &run, &first).await.unwrap();
    assert_eq!(selected["resources"]["diskMiB"], 31_000);
    common::set_checkpoint(
        &f.service.store,
        text(&run, "id"),
        json!({
            "nodeId": node["id"], "runnerId": first, "resources": selected["resources"],
        }),
    )
    .await;
    placement::materialize(&f.service, &first).await.unwrap();
    placement::release(&f.service, &first).await.unwrap();
    let volume_id = format!("{}:{}", text(&run, "id"), text(&node, "id"));
    let mut volume = f
        .service
        .store
        .get("node-volumes", &volume_id)
        .await
        .unwrap()
        .unwrap();
    volume["materialized"] = true.into();
    volume["storageMode"] = "on-demand".into();
    volume["diskMiB"] = 256.into();
    volume["activeDiskMiB"] = 256.into();
    f.service.store.put("node-volumes", volume).await.unwrap();
    node["diskCapacity"]["usedBytes"] = (256 * 1024 * 1024u64).into();
    node["diskCapacity"]["retainedBytes"] = json!({ text(&run, "id"): 256 * 1024 * 1024u64 });
    f.service.store.put_node_fixture(node).await.unwrap();
    run["status"] = "queued".into();
    run["finishedAt"] = Value::Null;
    run["recoveryPending"] = true.into();
    f.service
        .store
        .change_run_fixture(text(&run, "id"), run.clone())
        .await
        .unwrap();
    placement::check(&f.service, &run).await.unwrap();
    let selected = placement::reserve(&f.service, &run, &id()).await.unwrap();
    assert_eq!(selected["resources"]["diskMiB"], 31_000);
}

#[tokio::test]
async fn native_disk_growth_is_idempotent_and_competes_atomically_with_admission() {
    let f = Fixture::new().await;
    let gib = 1024 * 1024 * 1024u64;
    let mut node = f.node.clone();
    node["slots"] = 4.into();
    node["limits"]["diskMiB"] = (80 * 1024).into();
    node["capabilities"]["diskTotalMiB"] = (1024 * 1024).into();
    node["capabilities"]["diskMiB"] = (900 * 1024).into();
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let token = auth::token();
    f.service
        .store
        .set(
            &format!("node-token:{}", auth::digest(&token)),
            node["id"].clone(),
            None,
        )
        .await
        .unwrap();
    let mut attempts = Vec::new();
    let mut runs = Vec::new();
    for _ in 0..2 {
        let mut run = f.run();
        run["status"] = "running".into();
        run["resources"] = json!({ "cpu": 2, "memoryMiB": 4096, "diskMiB": 32768 });
        common::add_run(&f.service.store, &run).await;
        let attempt = id();
        placement::reserve(&f.service, &run, &attempt)
            .await
            .unwrap();
        common::set_checkpoint(
            &f.service.store,
            text(&run, "id"),
            json!({ "nodeId": node["id"], "runnerId": attempt }),
        )
        .await;
        attempts.push(attempt);
        runs.push(run);
    }
    let body = json!({
        "backend": "native", "nodeProtocol": 2, "executionReady": true, "dataRoot": "/fixture",
        "runtimeId": "development",
        "capabilities": node["capabilities"], "pool": { "capacity": 4 },
        "sharedResources": true, "budget": { "slots": 4, "limits": node["limits"] },
        "nativeMode": { "mode": "protected", "generation": null, "status": "applied" },
        "usage": { "memoryMiB": 0, "diskMiB": 16384, "diskBytes": 16 * gib },
        "diskGrowth": { "protocol": 1, "capturedAt": now(), "usedBytes": 16 * gib, "freeBytes": 900 * gib,
            "ownedBytes": { &attempts[0]: 8 * gib, &attempts[1]: 8 * gib },
            "requests": [
                { "attemptId": attempts[0], "currentMiB": 32768 },
                { "attemptId": attempts[1], "currentMiB": 32768 },
            ] },
    });
    for _ in 0..2 {
        let request = Request::builder()
            .method("POST")
            .uri("/internal/nodes/heartbeat")
            .header("host", common::HOST)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&f.router, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let reply = read_json(response).await;
        assert_eq!(
            reply["diskGrowth"]["grants"][0]["allowanceMiB"], 40960,
            "{reply}"
        );
        assert_eq!(
            reply["diskGrowth"]["grants"][1]["allowanceMiB"], 32768,
            "{reply}"
        );
    }
    assert_eq!(
        f.service
            .get_node(text(&node, "id"))
            .await
            .unwrap()
            .usage
            .unwrap()
            .disk_bytes,
        Some(16 * gib),
        "Retain the exact disk bytes emitted by native Controller::usage"
    );
    assert_eq!(
        f.service.store.run(text(&runs[0], "id")).await.unwrap()["resources"]["diskMiB"],
        40960
    );
    let incoming = f.run();
    common::add_run(&f.service.store, &incoming).await;
    assert!(
        placement::reserve(&f.service, &incoming, &id())
            .await
            .is_err()
    );
    placement::release(&f.service, &attempts[0]).await.unwrap();
    placement::release(&f.service, &attempts[0]).await.unwrap();
    assert!(
        placement::reserve(&f.service, &incoming, &id())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn storage_pressure_reserves_waiting_admission_before_another_growth_increment() {
    let f = Fixture::new().await;
    let gib = 1024 * 1024 * 1024u64;
    let mut node = f.node.clone();
    node["slots"] = 4.into();
    node["limits"]["diskMiB"] = (80 * 1024).into();
    node["capabilities"]["diskTotalMiB"] = (1024 * 1024).into();
    node["capabilities"]["diskMiB"] = (900 * 1024).into();
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let token = auth::token();
    f.service
        .store
        .set(
            &format!("node-token:{}", auth::digest(&token)),
            node["id"].clone(),
            None,
        )
        .await
        .unwrap();

    let mut running = f.run();
    running["status"] = "running".into();
    running["isolated"] = true.into();
    common::add_run(&f.service.store, &running).await;
    let attempt = id();
    placement::reserve(&f.service, &running, &attempt)
        .await
        .unwrap();
    common::set_checkpoint(
        &f.service.store,
        text(&running, "id"),
        json!({
            "nodeId": node["id"], "runnerId": attempt
        }),
    )
    .await;
    let mut waiting = f.run();
    waiting["isolated"] = true.into();
    common::add_run(&f.service.store, &waiting).await;

    for (current, expected, retrying) in [
        (32768, 40960, false),
        (40960, 40960, false),
        (40960, 49152, true),
    ] {
        if retrying {
            common::set_checkpoint(
                &f.service.store,
                text(&waiting, "id"),
                json!({
                    "nodeId": node["id"], "capacityRetryAt": now() + 30_000
                }),
            )
            .await;
        }
        let body = json!({
            "backend": "native", "nodeProtocol": 2, "executionReady": true,
            "dataRoot": "/fixture", "runtimeId": "development",
            "capabilities": node["capabilities"], "pool": { "capacity": 4 },
            "sharedResources": true, "budget": { "slots": 4, "limits": node["limits"] },
            "nativeMode": { "mode": "protected", "generation": null, "status": "applied" },
            "usage": { "diskMiB": 8192 },
            "diskGrowth": {
                "protocol": 1, "capturedAt": now(), "usedBytes": 8 * gib,
                "freeBytes": 900 * gib, "ownedBytes": { &attempt: 8 * gib },
                "requests": [{ "attemptId": attempt, "currentMiB": current }]
            }
        });
        let request = Request::builder()
            .method("POST")
            .uri("/internal/nodes/heartbeat")
            .header("host", common::HOST)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&f.router, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let reply = read_json(response).await;
        assert_eq!(
            reply["diskGrowth"]["grants"][0]["allowanceMiB"], expected,
            "Growth must leave room for the queued conversation: {reply}"
        );
        if current == 40960 && !retrying {
            assert!(placement::check(&f.service, &waiting).await.is_ok());
        }
    }
}

#[tokio::test]
async fn wildcard_node_access_still_requires_trusted_contributors() {
    let f = Fixture::new().await;
    let mut agent = f.agent.clone();
    agent["access"]["nodes"] = Value::Null;
    let agent = f
        .service
        .agent_fixture(agent, f.agent["id"].as_str())
        .await
        .unwrap();
    assert!(agent["access"]["nodes"].is_null());
    let mut run = f.run();
    let trusted = run.clone();
    let node = f.node.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &trusted, &node))
            .await
            .unwrap()
    );
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
async fn native_nodes_admit_six_attempts_only_after_the_configured_budget_is_applied() {
    let f = Fixture::new().await;
    let (status, configured) = f
        .owner_call(
            "PUT",
            &format!("/api/nodes/{}", text(&f.node, "id")),
            json!({
                "name": f.node["name"], "tags": [], "accepting": true,
                "limits": f.node["limits"], "slots": 6,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{configured}");
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
    let send = |slots| {
        let body = json!({
            "runtimeId": "development",
            "capabilities": f.node["capabilities"], "dataRoot": "/private/native-node/data",
            "backend": "native", "nodeProtocol": 2, "executionReady": true,
            "nativeMode": f.node["nativeModeObserved"],
            "budget": { "slots": slots, "limits": f.node["limits"] },
            "pool": { "capacity": slots },
        });
        common::send(
            &f.router,
            Request::builder()
                .method("POST")
                .uri("/internal/nodes/heartbeat")
                .header("host", common::HOST)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
    };
    assert_eq!(send(1).await.status(), StatusCode::OK);
    let run = f.run();
    common::add_run(&f.service.store, &run).await;
    assert!(placement::reserve(&f.service, &run, &id()).await.is_err());
    assert_eq!(send(6).await.status(), StatusCode::OK);
    let mut attempts = Vec::new();
    for _ in 0..6 {
        let run = f.run();
        common::add_run(&f.service.store, &run).await;
        let attempt = id();
        placement::reserve(&f.service, &run, &attempt)
            .await
            .unwrap();
        attempts.push(attempt);
    }
    let overflow = f.run();
    common::add_run(&f.service.store, &overflow).await;
    assert!(
        placement::reserve(&f.service, &overflow, &id())
            .await
            .is_err()
    );
    placement::release(&f.service, &attempts[0]).await.unwrap();
    placement::reserve(&f.service, &overflow, &id())
        .await
        .unwrap();
}

#[tokio::test]
async fn native_mode_default_protected_failure_is_observable_without_a_saved_request() {
    let f = Fixture::new().await;
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
    let mut capabilities = f.node["capabilities"].clone();
    capabilities["nativeModeProtocol"] = 1.into();
    for state in ["failed", "pending", "applied"] {
        let body = json!({
            "runtimeId": "development", "capabilities": capabilities,
            "dataRoot": "/private/native-node/data", "backend": "native", "nodeProtocol": 2,
            "executionReady": true, "budget": { "slots": 1, "limits": f.node["limits"] },
            "pool": { "capacity": 1 },
            "nativeMode": { "mode": "protected", "generation": null, "status": state },
        });
        let request = Request::builder()
            .method("POST")
            .uri("/internal/nodes/heartbeat")
            .header("host", common::HOST)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&f.router, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let node = f.service.node_boundary(text(&f.node, "id")).await.unwrap();
        assert!(
            node["nativeModeRequest"].is_null(),
            "No default migration creates an elevation request"
        );
        assert_eq!(node["nativeModeStatus"], state);
        assert_eq!(node["executionReady"], state == "applied");
        let (status, listed) = f.owner_call("GET", "/api/nodes", Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        let public = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == f.node["id"])
            .unwrap();
        assert_eq!(public["nativeModeStatus"], state);
    }
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
        "nativeMode": f.node["nativeModeObserved"],
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
        .node_boundary(f.node["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(node["executionReady"], true);
    node["nativeAccounts"] = json!([]);
    f.service.store.put_node_fixture(node).await.unwrap();
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
        .change_run(
            parent["id"].as_str().unwrap(),
            cairn_installation::runs::RunChanges {
                cancel_requested_at: Some(Some(now())),
                ..Default::default()
            },
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
async fn parent_node_grant_reductions_revoke_existing_native_delegation() {
    let f = Fixture::new().await;
    let vm_nodes = [id(), id()];
    for node_id in &vm_nodes {
        let mut node = f.node.clone();
        node["id"] = node_id.clone().into();
        node["capabilities"]["backend"] = "firecracker".into();
        node["capabilities"]["kvm"] = true.into();
        f.service.store.put_node_fixture(node).await.unwrap();
    }
    let agent = f
        .service
        .agent_fixture(
            json!({
                "name": "Bounded coordinator", "access": { "installation": true, "nodes": vm_nodes }
            }),
            None,
        )
        .await
        .unwrap();
    let mut parent = f.run();
    parent["snapshot"]["agent"] = agent.clone();
    parent["status"] = "running".into();
    common::add_run(&f.service.store, &parent).await;
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &parent)
        .await
        .unwrap();
    let response = delegated_call(
        &f,
        &configuration,
        "create_chat",
        json!({ "agentId": f.agent["id"] }),
    )
    .await;
    assert_ne!(response["result"]["isError"], true, "{response}");
    let chat_id = response["result"]["structuredContent"]["result"]["id"]
        .as_str()
        .unwrap();
    let response = delegated_call(
        &f,
        &configuration,
        "send_chat_message",
        json!({
            "chatId": chat_id, "message": { "id": id(), "text": "Run native work" }
        }),
    )
    .await;
    assert_ne!(response["result"]["isError"], true, "{response}");
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let chat = f.service.get("chats", chat_id).await.unwrap();
    let child = f
        .service
        .store
        .run(chat["runId"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(child["nativeParentRunId"], parent["id"]);

    for (nodes, retains_grant) in [
        (json!(vm_nodes), true),
        (json!([vm_nodes[0], vm_nodes[1], f.node["id"]]), true),
        (json!([vm_nodes[0]]), false),
        (json!([vm_nodes[0], f.node["id"]]), false),
        (json!([]), false),
    ] {
        let mut current = agent.clone();
        current["access"]["nodes"] = nodes.clone();
        f.service.store.put_agent_fixture(current).await.unwrap();
        let (run, node) = (child.clone(), f.node.clone());
        let existing_allowed = f
            .service
            .store
            .read(move |db| policy::allows(db, &run, &node))
            .await
            .unwrap();
        assert_eq!(existing_allowed, retains_grant, "parent nodes {nodes}");
    }
}

#[tokio::test]
async fn a_vm_coordinator_directed_by_unrelated_parents_loses_native_delegation() {
    let f = Fixture::new().await;
    let (parent_a, _) = delegate(&f, true).await;
    let parent_b_agent = f
        .service
        .agent_fixture(
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
        .agent_fixture(
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
        .change_run(
            coordinator_run["id"].as_str().unwrap(),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let coordinator_run = f.service.store.run(&coordinator_run.id).await.unwrap();
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
            .change_run_fixture(parent_b["id"].as_str().unwrap(), reduction)
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
        .agent_fixture(
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
        .update_run_boundary(
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
        .update_run_boundary(
            parent["id"].as_str().unwrap(),
            json!({ "nativeAuthorityNodes": [f.node["id"]] }),
        )
        .await
        .unwrap();
    let mut agent = parent["snapshot"]["agent"].clone();
    agent["access"]["installation"] = false.into();
    f.service.store.put_agent_fixture(agent).await.unwrap();
    let node = f.node.clone();
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &child, &node))
            .await
            .unwrap()
    );
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

#[tokio::test]
async fn editing_a_vm_task_cannot_replace_its_authors_with_a_native_trusted_editor() {
    let f = Fixture::new().await;
    let _authority = mission_authority(&f).await;
    let author = auth::InstallationIdentity::trusted(auth::InstallationRole::Member, OTHER);
    let editor = auth::InstallationIdentity::trusted(auth::InstallationRole::Owner, TRUSTED);
    let vm = f
        .service
        .agent_fixture(json!({ "name": "VM task agent" }), None)
        .await
        .unwrap();
    let task = f
        .service
        .task_as_fixture(
            json!({
                "name": "Retained commitment",
                "prompt": "Original brief",
                "agentId": vm["id"],
            }),
            None,
            Some(&author),
        )
        .await
        .unwrap();
    assert_eq!(task["authorId"], OTHER);
    let mut edited = task.clone();
    edited["agentId"] = f.agent["id"].clone();
    let error = f
        .service
        .task_as_fixture(edited, task["id"].as_str(), Some(&editor))
        .await
        .unwrap_err();
    assert_eq!(error.status, 403);
    assert_eq!(
        f.service
            .mission_snapshot_fixture(task["id"].as_str().unwrap())
            .await
            .unwrap()["agentId"],
        vm["id"]
    );
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

#[tokio::test]
async fn native_tasks_retain_previous_directing_editors_and_current_author_rights() {
    let f = Fixture::new().await;
    trust_owner(&f).await;
    let _authority = mission_authority(&f).await;
    let author =
        auth::InstallationIdentity::trusted(auth::InstallationRole::Member, "fixture-owner");
    let editor = auth::InstallationIdentity::trusted(auth::InstallationRole::Owner, TRUSTED);
    let task = f
        .service
        .task_as_fixture(
            json!({
                "name": "Native commitment",
                "prompt": "Original brief",
                "agentId": f.agent["id"],
            }),
            None,
            Some(&author),
        )
        .await
        .unwrap();
    let mut edited = task.clone();
    edited["prompt"] = "Updated brief".into();
    let saved = f
        .service
        .task_as_fixture(edited, task["id"].as_str(), Some(&editor))
        .await
        .unwrap();
    assert_eq!(saved["authorId"], "fixture-owner");
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
    f.service
        .store
        .put_node_fixture(f.node.clone())
        .await
        .unwrap();
    let error = f
        .service
        .task_as_fixture(saved, task["id"].as_str(), Some(&editor))
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
            json!({ "settings": {
                "name": "Native schedule", "prompt": "Build", "agentId": f.agent["id"],
                "cron": "0 * * * *", "timezone": "UTC"
            } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{task}");
    f.service
        .store
        .put_node_fixture(f.node.clone())
        .await
        .unwrap();
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
    let path = format!("/api/tasks/{}/state", task["id"].as_str().unwrap());
    let disabled = json!({ "taskId": task["id"], "revision": task["revision"], "enabled": false, "archived": false });
    let (status, saved) = f.call(&owner, "POST", &path, disabled).await;
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
    let enabled = json!({ "taskId": saved["id"], "revision": saved["revision"], "enabled": true, "archived": false });
    let (status, _) = f.call(&owner, "POST", &path, enabled).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_native_task_without_recorded_provenance_says_how_to_schedule_it_again() {
    let f = Fixture::new().await;
    trust_owner(&f).await;
    // A complete current mission whose agent subsequently moves to native.
    let vm = f
        .service
        .agent_fixture(json!({ "name": "VM before native" }), None)
        .await
        .unwrap();
    let mut task = f
        .service
        .task_fixture(
            json!({
                "name": "Existing commitment", "prompt": "Original brief", "agentId": vm["id"],
                "cron": "0 9 * * *", "timezone": "Europe/Paris"
            }),
            None,
        )
        .await
        .unwrap();
    task["agentId"] = f.agent["id"].clone();
    let author =
        auth::InstallationIdentity::trusted(auth::InstallationRole::Owner, "fixture-owner");
    f.service
        .store
        .put_mission_fixture(task.clone())
        .await
        .unwrap();

    let error = f
        .service
        .task_as_fixture(task.clone(), task["id"].as_str(), Some(&author))
        .await
        .unwrap_err();
    assert_eq!(error.status, 403, "{}", error.message);
    assert!(
        error.message.contains("no recorded native authority"),
        "{}",
        error.message
    );

    let mut duplicate = task.clone();
    duplicate.as_object_mut().unwrap().remove("id");
    let saved = f
        .service
        .task_as_fixture(duplicate, None, Some(&author))
        .await
        .unwrap();
    assert_eq!(saved["nativeProvenanceUnknown"], false);
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

#[tokio::test]
async fn converting_wildcard_access_never_reviews_host_root_without_explicit_owner_confirmation() {
    let f = Fixture::new().await;
    let node_id = text(&f.node, "id");
    let mut node = f.node.clone();
    node["nativeModeRequest"] = json!({ "mode": "administrator", "generation": id() });
    node["nativeReviewedAgents"] = json!([]);
    f.service.store.put_node_fixture(node).await.unwrap();
    let agent = f
        .service
        .agent_fixture(
            json!({ "name": "Future wildcard", "access": { "backend": "native", "nodes": null } }),
            None,
        )
        .await
        .unwrap();
    let path = format!("/api/agents/{}", text(&agent, "id"));
    let edit =
        json!({ "name": "Future wildcard", "access": { "backend": "native", "nodes": [node_id] } });
    let (status, _) = f.owner_call("PUT", &path, edit.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        f.service.node_boundary(node_id).await.unwrap()["nativeReviewedAgents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        f.service.agent_snapshot(text(&agent, "id")).await.unwrap()["access"]["nodes"].is_null()
    );
    let mut confirmed = edit;
    confirmed["nativeNodeReviews"] = json!([node_id]);
    let (status, result) = f.owner_call("PUT", &path, confirmed).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(
        f.service.node_boundary(node_id).await.unwrap()["nativeReviewedAgents"]
            .as_array()
            .unwrap()
            .contains(&agent["id"])
    );
    assert!(
        result["nativeNodeReviews"].is_null(),
        "The acknowledgement is not a new profile permission"
    );
}

#[tokio::test]
async fn native_mode_preview_includes_unplaced_automatic_work_and_rejects_concurrent_arrivals() {
    for backend in ["native", "auto"] {
        unplaced_administrator_preview(backend).await;
    }
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

#[tokio::test]
async fn native_mode_confirmation_covers_incoming_moves_and_reservations() {
    let f = Fixture::new().await;
    let node_id = text(&f.node, "id");
    let mut node = f.node.clone();
    node["capabilities"]["nativeModeProtocol"] = 1.into();
    f.service.store.put_node_fixture(node).await.unwrap();
    let source = id();
    let mut incoming = Vec::new();
    for selection in ["move", "reservation", "later"] {
        let mut run = f.run();
        run["nodeId"] = source.clone().into();
        if selection == "move" {
            run["moveRequest"] = json!({ "nodeId": node_id, "reservation": id() });
        }
        common::add_run(&f.service.store, &run).await;
        if selection == "reservation" {
            f.service
                .store
                .put(
                    "node-attempts",
                    json!({
                        "id": id(), "runId": run["id"], "nodeId": node_id, "released": false,
                    }),
                )
                .await
                .unwrap();
        }
        incoming.push(run);
    }
    let path = format!("/api/nodes/{node_id}/native-mode");
    let (status, preview) = f
        .owner_call("POST", &path, json!({ "mode": "administrator" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let listed = preview["preview"]["conversations"].as_array().unwrap();
    assert_eq!(listed.len(), 2);
    for run in &incoming[..2] {
        assert!(listed.iter().any(|listed| listed["id"] == run["id"]));
    }
    f.service
        .store
        .update_run_boundary(
            text(&incoming[2], "id"),
            json!({
                "moveRequest": { "nodeId": node_id, "reservation": id() },
            }),
        )
        .await
        .unwrap();
    let (status, _) = f
        .owner_call(
            "POST",
            &path,
            json!({
                "mode": "administrator", "previewId": preview["previewId"], "confirm": true,
            }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "A newly incoming conversation requires review"
    );
    let (_, preview) = f
        .owner_call("POST", &path, json!({ "mode": "administrator" }))
        .await;
    assert_eq!(
        preview["preview"]["conversations"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    // A new attempt for an already displayed conversation also invalidates the
    // interruption snapshot, even though the conversation count stays the same.
    f.service
        .store
        .put(
            "node-attempts",
            json!({
                "id": id(), "runId": incoming[0]["id"], "nodeId": node_id, "released": false,
            }),
        )
        .await
        .unwrap();
    let (status, _) = f
        .owner_call(
            "POST",
            &path,
            json!({
                "mode": "administrator", "previewId": preview["previewId"], "confirm": true,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(f.service.node_boundary(node_id).await.unwrap()["nativeModeRequest"].is_null());
}

#[tokio::test]
async fn native_mode_transition_keeps_automatic_recovery_and_capacity_moves_on_the_source() {
    use cairn_installation::nodes::{moves, snapshots};

    let f = Fixture::new().await;
    let source = text(&f.node, "id");
    let destination = id();
    let mut other = f.node.clone();
    other["id"] = destination.clone().into();
    f.service.store.put_node_fixture(other).await.unwrap();
    let mut agent = f.agent.clone();
    agent["access"]["nodes"] = json!([source, destination]);
    f.service
        .store
        .put_agent_fixture(agent.clone())
        .await
        .unwrap();
    let mut run = f.run();
    let run_id = text(&run, "id").to_owned();
    let point = id();
    run["nodeId"] = source.into();
    run["snapshot"]["agent"] = agent;
    run["nativeAuthorityNodes"] = json!([source, destination]);
    run["resources"] = json!({ "cpu": 1, "memoryMiB": 128, "diskMiB": 128 });
    run["backup"] = json!({ "id": point });
    common::add_run(&f.service.store, &run).await;
    common::set_checkpoint(
        &f.service.store,
        &run_id,
        json!({
            "nodeId": source, "runtimeId": "development", "completed": true,
        }),
    )
    .await;
    let manifest = json!({
        "version": 1, "size": snapshots::BLOCK, "blockSize": snapshots::BLOCK,
        "blocks": [{ "offset": 0, "size": snapshots::BLOCK, "hash": null }],
        "runtime": { "runtimeId": "development", "backend": "native" },
    });
    f.service
        .store
        .put(
            "node-backups",
            json!({
                "id": point, "runId": run_id, "destination": "s3", "blockFormat": "shared-v1", "sessionId": "saved-session",
                "manifest": f.service.vault.encrypt(&format!("backup:{point}"), &manifest).unwrap(),
            }),
        )
        .await
        .unwrap();
    assert!(moves::latest(&f.service, &run_id).await.unwrap().is_some());
    let mut node = f.node.clone();
    let generation = id();
    node["capabilities"]["nativeModeProtocol"] = 1.into();
    node["nativeModeRequest"] = json!({ "mode": "administrator", "generation": generation });
    node["nativeReviewedAgents"] = json!([f.agent["id"]]);
    node["executionReady"] = false.into();
    for state in ["pending", "failed"] {
        node["nativeModeObserved"] = json!({
            "mode": "protected", "generation": null, "status": state,
        });
        f.service
            .store
            .put_node_fixture(node.clone())
            .await
            .unwrap();
        assert!(!moves::advance(&f.service, &run).await.unwrap());
        assert!(!moves::queue_capacity_move(&f.service, &run).await.unwrap());
        assert!(f.service.store.run(&run_id).await.unwrap()["moveRequest"].is_null());
        assert!(
            f.service
                .store
                .list("node-attempts")
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            f.service
                .store
                .checkpoint_boundary(&run_id)
                .await
                .unwrap()
                .unwrap()["nodeId"],
            source
        );
    }
    node["nativeModeObserved"] = json!({
        "mode": "administrator", "generation": generation, "status": "applied",
    });
    node["executionReady"] = true.into();
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    assert!(moves::advance(&f.service, &run).await.unwrap());
    assert!(!moves::queue_capacity_move(&f.service, &run).await.unwrap());
    // Genuine unavailability after confirmation retains ordinary relocation.
    node["executionReady"] = false.into();
    f.service.store.put_node_fixture(node).await.unwrap();
    assert!(moves::queue_capacity_move(&f.service, &run).await.unwrap());
    assert_eq!(
        f.service.store.run(&run_id).await.unwrap()["moveRequest"]["nodeId"],
        destination
    );
}

#[tokio::test]
async fn deleting_an_agent_revokes_admin_review_before_reusing_its_creation_id() {
    let f = Fixture::new().await;
    let node_id = text(&f.node, "id");
    let mut node = f.node.clone();
    let generation = id();
    node["nativeModeRequest"] = json!({ "mode": "administrator", "generation": generation });
    node["nativeModeObserved"] =
        json!({ "mode": "administrator", "generation": generation, "status": "applied" });
    node["capabilities"]["nativeModeProtocol"] = 1.into();
    node["nativeReviewedAgents"] = json!([f.agent["id"]]);
    let original = f.run();
    let before = node.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &original, &before))
            .await
            .unwrap()
    );
    f.service.store.put_node_fixture(node).await.unwrap();
    let path = format!("/api/agents/{}", text(&f.agent, "id"));
    let (status, result) = f.owner_call("DELETE", &path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(
        f.service.node_boundary(node_id).await.unwrap()["nativeReviewedAgents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (status, recreated) = f.owner_call("POST", "/api/agents", json!({
        "creationId": f.agent["id"], "name": "Replayed creation", "access": { "backend": "native", "nodes": null }
    })).await;
    assert_eq!(status, StatusCode::OK, "{recreated}");
    assert_eq!(recreated["id"], f.agent["id"]);
    let node = f.service.node_boundary(node_id).await.unwrap();
    assert!(!cairn_installation::nodes::native_mode::reviewed(
        &node, &recreated
    ));
    let (status, _) = f.owner_call("PUT", &path, json!({ "name": "Explicit replay", "access": { "backend": "native", "nodes": [node_id] } })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
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

#[tokio::test]
async fn public_mcp_resumes_native_missions_with_verified_owner_authority() {
    let f = Fixture::new().await;
    let run = saved_native_mission(&f).await;
    let run_id = text(&run, "id");
    let args = json!({ "runId": run_id });
    for (account, scopes) in [(OTHER, vec!["run"]), (TRUSTED, vec!["read"])] {
        let response = public_mcp_call(&f, account, &scopes, "resume_run", args.clone()).await;
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(f.service.store.run(run_id).await.unwrap(), run);
    }
    let response = public_mcp_call(&f, TRUSTED, &["run"], "resume_run", args).await;
    assert_ne!(response["result"]["isError"], true, "{response}");
    let resumed = f.service.store.run(run_id).await.unwrap();
    assert_eq!(resumed["nativePrincipals"], json!([TRUSTED]));
    assert_eq!(resumed["nativeProvenanceUnknown"], false);
    assert_eq!(resumed["sessionId"], "retained-session");
    f.service.worker.cancel(&f.service, run_id).await.unwrap();
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

#[tokio::test]
async fn resume_run_preserves_native_human_and_delegated_provenance() {
    let f = Fixture::new().await;
    let run = saved_native_mission(&f).await;
    let run_id = text(&run, "id");
    let path = format!("/api/runs/{run_id}/resume");
    let (status, _) = f.call(&f.other, "POST", &path, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(f.service.store.run(run_id).await.unwrap(), run);
    let public = common::relay_fixture::grant(&f.service, &["run"]).await;
    let request = common::request("POST", "/mcp")
        .header("authorization", format!("Bearer {public}"))
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": common::mcp_arguments("resume_run", &json!({ "runId": run_id })),
            })
            .to_string(),
        ))
        .unwrap();
    let denied = read_json(common::send(&f.router, request).await).await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let (_, unknown) = resume_coordinator(&f, false).await;
    let denied = delegated_call(&f, &unknown, "resume_run", json!({ "runId": run_id })).await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert_eq!(f.service.store.run(run_id).await.unwrap(), run);
    let (caller, traced) = resume_coordinator(&f, true).await;
    let resumed = delegated_call(&f, &traced, "resume_run", json!({ "runId": run_id })).await;
    assert_ne!(resumed["result"]["isError"], true, "{resumed}");
    let after = f.service.store.run(run_id).await.unwrap();
    assert_eq!(after["nativeParentRunId"], caller["id"]);
    assert_eq!(after["nativePrincipals"], json!([TRUSTED]));
    assert_eq!(after["sessionId"], "retained-session");
    f.service.worker.cancel(&f.service, run_id).await.unwrap();
    f.service
        .store
        .change_run(
            run_id,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Cancelled),
                recovery_pending: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (status, _) = f.call(&f.trusted, "POST", &path, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let after = f.service.store.run(run_id).await.unwrap();
    assert_eq!(after["nativePrincipals"], json!([TRUSTED]));
    assert_eq!(after["nativeParentRunId"], caller["id"]);
}

#[tokio::test]
async fn resume_run_rechecks_grant_and_cancellation_after_native_preparation() {
    for revoke in [true, false] {
        let f = Fixture::new().await;
        let target = saved_native_mission(&f).await;
        let (caller, configuration) = resume_coordinator(&f, true).await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let server = Router::new().route("/api/relay/00000000-0000-4000-8000-000000000055/native-authority", axum::routing::get({
            let entered = entered.clone();
            let release = release.clone();
            move || {
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    entered.notify_one();
                    release.notified().await;
                    axum::Json(json!({ TRUSTED: { "email": "trusted@example.test", "accessId": "one" } }))
                }
            }
        }));
        let (origin, authority) = common::serve_locally(server).await;
        let identity_path = f
            .service
            .config
            .data_dir
            .join("installation-relay/identity.json");
        let mut identity: Value =
            serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
        identity["origin"] = origin.into();
        std::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
        f.service.store.delete("native-authority").await.unwrap();
        let attempt = delegated_call(
            &f,
            &configuration,
            "resume_run",
            json!({ "runId": target["id"] }),
        );
        tokio::pin!(attempt);
        tokio::select! {
            () = entered.notified() => {},

            response = &mut attempt => panic!("Resume did not enter preparation: {response}"),

            () = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("Native preparation did not start"),
        }
        if revoke {
            f.service
                .mcps
                .revoke_run(&f.service, text(&caller, "id"))
                .await
                .unwrap();
        } else {
            f.service
                .store
                .change_run(
                    text(&caller, "id"),
                    cairn_installation::runs::RunChanges {
                        cancel_requested_at: Some(Some(now())),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        release.notify_one();
        let response = attempt.await;
        authority.abort();
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(
            f.service.store.run(text(&target, "id")).await.unwrap(),
            target
        );
    }
}
