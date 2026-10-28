use crate::common::AgentFixture;

use super::*;

pub(super) async fn running_turn(s: &Service, chat: &str) -> (Value, String) {
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let detail = s.chat_detail(chat).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&detail["run"], "id"),
            json!({ "status": "running", "isolated": true }),
        )
        .await
        .unwrap();
    s.chat_acknowledge(text(&run, "id"), text(&run["chatExecution"], "messageId"))
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(s, &run).await.unwrap();
    (
        run,
        text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN").to_owned(),
    )
}

#[tokio::test]
async fn delegated_tasks_keep_the_original_external_person_and_channel_scope() {
    Box::pin(delegated_source(SourceCheck::Scope)).await;
}

#[tokio::test]
async fn revoked_external_requesters_lose_delegated_task_controls() {
    Box::pin(delegated_source(SourceCheck::Revoked)).await;
}

#[tokio::test]
async fn delegated_reports_recheck_the_source_after_reading() {
    Box::pin(delegated_source(SourceCheck::DuringRead)).await;
}

#[tokio::test]
async fn delegated_cancellation_rechecks_a_source_revoked_during_authorization() {
    Box::pin(delegated_source(SourceCheck::DuringCancel)).await;
}

#[derive(Clone, Copy)]
enum SourceCheck {
    Scope,
    Revoked,
    DuringRead,
    DuringCancel,
}

async fn workspace_tool(
    app: &axum::Router,
    bearer: &str,
    name: &str,
    args: Value,
) -> (StatusCode, Value) {
    let response = common::send(
        app,
        common::request("POST", "/mcp-workspace")
            .header("authorization", format!("Bearer {bearer}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": name, "arguments": args},
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    (response.status(), common::read_json(response).await)
}

async fn delegated_source(check: SourceCheck) {
    let check_controls = !matches!(check, SourceCheck::Scope);
    let disk_deletions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let deletions = disk_deletions.clone();
    let controller = axum::Router::new().route(
        "/disks/{run}/delete",
        axum::routing::post(move || {
            deletions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { axum::Json(json!({ "deleted": true })) }
        }),
    );
    let (origin, controller_server) = common::serve_locally(controller).await;
    let (_root, s, instance, resource, app) = configured_fixture_with_runner(None, &origin).await;
    let agent = s
        .agent_fixture(
            json!({
                "name": "Routed delegator", "access": { "projects": [], "delegation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let reviewer = s
        .agent_fixture(
            json!({
                "name": "Other agent", "access": { "projects": [] },
            }),
            None,
        )
        .await
        .unwrap();
    let route = create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Alice's channel", "enabled": true, "match": { "channel": "C1" },
            "agentId": agent["id"], "accessMode": "requests", "userIds": ["alice"],
        }),
    )
    .await;
    let grants =
        format!("/api/plugins/router/connections/{instance}/resources/workspace/{resource}/grants");
    for granted in [&agent, &reviewer] {
        assert_eq!(
            common::send(
                &app,
                owner_request(
                    "POST",
                    &grants,
                    json!({
                        "agentId": granted["id"], "operations": ["channel-read"],
                    })
                )
            )
            .await
            .status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        deliver(
            &app,
            &instance,
            json!({
                "id": "delegation-source", "resource": resource, "author": { "id": "alice" },
                "context": { "channel": "C1", "thread": "delegation-thread" },
                "body": { "text": "Delegate a review of this channel" },
            })
        )
        .await
        .1["status"],
        "accepted"
    );
    let package = registry::active(&s, "router").await.unwrap();
    let connection = connections::get(&s, "router", &instance).await.unwrap();
    let mut session = s
        .plugins
        .host
        .start(
            &PathBuf::from(env!("CARGO_BIN_EXE_cairn")),
            &package.manifest,
            &package.source,
        )
        .await
        .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    session.stop().await;
    let chat = s.chat_list().await.unwrap().pop().unwrap();
    let (_, parent) = running_turn(&s, text(&chat, "id")).await;
    let permitted = json!({ "resource": resource, "channel": "C1" });
    let outside = json!({ "resource": resource, "channel": "C2" });
    assert!(
        agents::call_tool(&s, &parent, "router_scope", &permitted)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        agents::call_tool(&s, &parent, "router_scope", &outside)
            .await
            .is_err()
    );
    let child = cairn_installation::delegation::delegate(
        &s,
        &parent,
        &json!({
            "task": "Review this channel", "agentId": reviewer["id"],
            "provider": "claude", "model": "claude-sonnet-5",
        }),
    )
    .await
    .unwrap();
    let (_, child_token) = running_turn(&s, text(&child, "chatId")).await;
    assert!(
        agents::call_tool(&s, &child_token, "router_scope", &permitted)
            .await
            .unwrap()
            .is_some()
    );
    let escaped = agents::call_tool(&s, &child_token, "router_scope", &outside).await;
    assert!(
        escaped.is_err(),
        "A delegated task escaped Alice's channel: {escaped:?}"
    );
    if check_controls {
        for args in [json!({}), json!({"chatId": child["chatId"]})] {
            let (status, response) = workspace_tool(&app, &parent, "task_status", args).await;
            assert_eq!(status, StatusCode::OK);
            assert_ne!(response["result"]["isError"], true, "{response}");
        }
        let (status, sibling) = workspace_tool(
            &app,
            &parent,
            "delegate_task",
            json!({"task": "An authorized cancellation witness"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_ne!(sibling["result"]["isError"], true, "{sibling}");
        let (status, stopped) = workspace_tool(
            &app,
            &parent,
            "task_cancel",
            json!({"chatId": sibling["result"]["structuredContent"]["chatId"]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(stopped["result"]["structuredContent"]["paused"], true);
    }
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut authority_server = None;
    let mut pending = None;
    if matches!(check, SourceCheck::DuringRead | SourceCheck::DuringCancel) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (entering, releasing) = (entered.clone(), release.clone());
        let calls = Arc::new(AtomicUsize::new(0));
        // A read checks again after its snapshot; cancellation checks before
        // waiting for the worker's atomic authorization/effect boundary.
        let hold_at = usize::from(matches!(check, SourceCheck::DuringRead));
        let authority = axum::Router::new().route(
            "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
            axum::routing::get(move || {
                let (entered, release, calls) =
                    (entering.clone(), releasing.clone(), calls.clone());
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) == hold_at {
                        entered.notify_one();
                        release.notified().await;
                    }
                    axum::Json(json!({
                        "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
                        "members": [],
                    }))
                }
            }),
        );
        let (origin, server) = common::serve_locally(authority).await;
        authority_server = Some(server);
        let path = s.config.data_dir.join("installation-relay/identity.json");
        let mut identity: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        identity["origin"] = json!(origin);
        std::fs::write(path, serde_json::to_vec(&identity).unwrap()).unwrap();
        let (app, parent, child) = (app.clone(), parent.clone(), child["chatId"].clone());
        let tool = if matches!(check, SourceCheck::DuringRead) {
            "task_status"
        } else {
            "task_cancel"
        };
        let mut operation = tokio::spawn(async move {
            workspace_tool(&app, &parent, tool, json!({"chatId": child})).await
        });
        tokio::select! {
            () = entered.notified() => {},
            result = &mut operation => panic!("The delegated control returned before its source check: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("The delegated source check was not reached"),
        }
        pending = Some(operation);
    }
    let people = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}/people/alice",
        text(&route, "id")
    );
    assert_eq!(
        common::send(
            &app,
            owner_request(
                "POST",
                &people,
                json!({
                    "action": "remove", "revision": route["revision"],
                })
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert!(
        agents::call_tool(&s, &child_token, "router_scope", &permitted)
            .await
            .is_err(),
        "The delegated task outlived Alice's source grant"
    );
    let mut leaked_controls = Vec::new();
    if let Some(pending) = pending {
        release.notify_one();
        let (status, response) = tokio::time::timeout(std::time::Duration::from_secs(10), pending)
            .await
            .unwrap()
            .unwrap();
        let denied = status == StatusCode::FORBIDDEN
            || (status == StatusCode::OK && response["result"]["isError"] == true);
        if !denied {
            leaked_controls
                .push(json!({"effect": "A late source revocation allowed a delegated control"}));
        }
    }
    if check_controls {
        for (name, args) in [
            ("task_status", json!({})),
            ("task_status", json!({"chatId": child["chatId"]})),
            ("task_cancel", json!({"chatId": child["chatId"]})),
        ] {
            let (status, response) = workspace_tool(&app, &parent, name, args.clone()).await;
            let denied = status == StatusCode::FORBIDDEN
                || (status == StatusCode::OK && response["result"]["isError"] == true);
            if !denied {
                leaked_controls.push(json!({
                    "tool": name, "args": args, "status": status.as_u16(),
                    "isError": response["result"]["isError"],
                }));
            }
        }
        let current = s
            .store
            .get("chats", text(&child, "chatId"))
            .await
            .unwrap()
            .unwrap();
        if current["paused"] == true {
            leaked_controls.push(json!({"effect": "The revoked parent paused its delegated task"}));
        }
    }
    let child = text(&child, "chatId").to_owned();
    let requester_prefix = format!("plugin-message-requester:{child}:");
    let control_prefix = format!("plugin-requester-suspended:{child}:");
    for prefix in [&requester_prefix, &control_prefix] {
        let prefix = prefix.clone();
        assert!(
            !s.store
                .read(move |db| db.keys(&prefix))
                .await
                .unwrap()
                .is_empty()
        );
    }
    let deleted = common::send(
        &app,
        owner_request(
            "DELETE",
            &format!("/api/chats/{child}"),
            json!({ "confirm": true }),
        ),
    )
    .await;
    assert_eq!(
        deleted.status(),
        StatusCode::OK,
        "{}",
        common::read_text(deleted).await
    );
    let chat = s.store.get("chats", &child).await.unwrap().unwrap();
    // No provider was launched by this lifecycle fixture. Purge after the
    // deletion intent, as the manager does after process settlement.
    s.store
        .patch_run(text(&chat, "runId"), json!({ "status": "cancelled" }))
        .await
        .unwrap();
    cairn_installation::conversation_deletion::purge(&s, chat)
        .await
        .unwrap();
    assert_eq!(s.chat_detail(&child).await.unwrap_err().status, 404);
    for prefix in [requester_prefix, control_prefix] {
        assert!(
            s.store
                .read(move |db| db.keys(&prefix))
                .await
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(disk_deletions.load(std::sync::atomic::Ordering::SeqCst), 1);
    controller_server.abort();
    if let Some(server) = authority_server {
        server.abort();
    }
    assert!(
        leaked_controls.is_empty(),
        "Revoked source retained delegated-task controls: {leaked_controls:?}"
    );
}

#[tokio::test]
async fn delegation_cannot_borrow_another_agents_granular_resource_grants() {
    let (_root, s, instance, resource, app) = fixture().await;
    let parent = s
        .agent_fixture(
            json!({
                "name": "Requester without resources", "access": { "delegation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let target = s
        .agent_fixture(json!({ "name": "Granted reviewer" }), None)
        .await
        .unwrap();
    let grants =
        format!("/api/plugins/router/connections/{instance}/resources/workspace/{resource}/grants");
    assert_eq!(common::send(&app, owner_request("POST", &grants, json!({
        "agentId": target["id"], "operations": ["channel-read"], "scopes": { "channel": ["C1"] },
    }))).await.status(), StatusCode::OK);
    let chat = s
        .chat_create(json!({ "agentId": parent["id"] }))
        .await
        .unwrap();
    s.chat_send(
        text(&chat, "id"),
        json!({
            "id": cairn_installation::config::id(), "text": "Ask the other model to review",
        }),
    )
    .await
    .unwrap();
    let (_, token) = running_turn(&s, text(&chat, "id")).await;
    assert!(
        agents::call_tool(
            &s,
            &token,
            "router_scope",
            &json!({
                "resource": resource, "channel": "C1",
            })
        )
        .await
        .unwrap()
        .is_none()
    );
    let result = cairn_installation::delegation::delegate(
        &s,
        &token,
        &json!({
            "task": "Review with your channel access", "agentId": target["id"],
        }),
    )
    .await;
    assert!(
        result.is_err(),
        "Delegation widened resource access: {result:?}"
    );
    assert_eq!(result.unwrap_err().status, 403);

    // A grant on a different channel still does not cover the target.
    assert_eq!(
        common::send(
            &app,
            owner_request(
                "POST",
                &grants,
                json!({
                    "agentId": parent["id"], "profile": "read", "scopes": { "channel": ["C2"] },
                })
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        cairn_installation::delegation::delegate(
            &s,
            &token,
            &json!({
                "task": "Try the other channel", "agentId": target["id"],
            })
        )
        .await
        .unwrap_err()
        .status,
        403
    );

    // The profile covers the target's explicit operation on the same channel.
    assert_eq!(
        common::send(
            &app,
            owner_request(
                "POST",
                &grants,
                json!({
                    "agentId": parent["id"], "profile": "read", "scopes": { "channel": ["C1"] },
                })
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    let allowed = cairn_installation::delegation::delegate(
        &s,
        &token,
        &json!({
            "task": "Review within shared permissions", "agentId": target["id"],
        }),
    )
    .await
    .unwrap();
    let (_, child) = running_turn(&s, text(&allowed, "chatId")).await;
    assert_eq!(
        agents::call_tool(
            &s,
            &child,
            "router_scope",
            &json!({
                "resource": resource, "channel": "C1",
            })
        )
        .await
        .unwrap()
        .unwrap()["channel"],
        "C1"
    );
    assert!(
        agents::call_tool(
            &s,
            &child,
            "router_scope",
            &json!({
                "resource": resource, "channel": "C2",
            })
        )
        .await
        .is_err()
    );
    assert_eq!(
        common::send(
            &app,
            owner_request(
                "POST",
                &grants,
                json!({
                    "agentId": target["id"], "operations": ["channel-read"],
                    "scopes": { "channel": ["C1", "C2"] },
                })
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    let combined = cairn_installation::delegation::delegate(
        &s,
        &token,
        &json!({
            "task": "Review both channels already covered by my rules", "agentId": target["id"],
        }),
    )
    .await;
    assert!(
        combined.is_ok(),
        "Complementary requester rules were not combined: {combined:?}"
    );
}

#[tokio::test]
async fn altered_route_approvals_do_not_gain_implicit_delegation_defaults() {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent_fixture(json!({ "name": "Existing routed agent" }), None)
        .await
        .unwrap();
    let route = create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Existing route", "enabled": true, "match": { "channel": "C1" },
            "agentId": agent["id"], "accessMode": "allowlist", "userIds": ["alice"],
        }),
    )
    .await;
    // A current approval is complete. Missing a stored permission must not
    // silently reconstruct authority or rewrite the saved proof.
    let mut saved = s
        .store
        .get("plugin-routes", text(&route, "id"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved["grantedAccess"]["delegation"], false);
    saved["grantedAccess"]
        .as_object_mut()
        .unwrap()
        .remove("delegation");
    s.store.put("plugin-routes", saved.clone()).await.unwrap();
    let event = json!({
        "id": "incomplete-route-approval", "resource": resource, "author": { "id": "alice" },
        "context": { "channel": "C1", "thread": "incomplete-route-thread" },
        "body": { "text": "Refuse an incomplete stored approval" },
    });
    let admitted = deliver(&app, &instance, event.clone()).await;
    assert_ne!(admitted.1["status"], "accepted", "{}", admitted.1);
    assert_eq!(
        s.store
            .get("plugin-routes", text(&route, "id"))
            .await
            .unwrap(),
        Some(saved)
    );

    // Adding the real power cannot repair the incomplete approval implicitly.
    let mut changed = s.agent_snapshot(text(&agent, "id")).await.unwrap();
    changed["access"]["delegation"] = json!(true);
    s.agent_fixture(changed, Some(text(&agent, "id")))
        .await
        .unwrap();
    let mut event = event;
    event["id"] = json!("new-delegation-power");
    let denied = deliver(&app, &instance, event).await;
    assert_ne!(denied.1["status"], "accepted", "{}", denied.1);
}

#[tokio::test]
async fn delegated_scope_unions_cover_combinations_without_filling_permission_holes() {
    let (_root, s, instance, resource, app) = fixture().await;
    let package = registry::active(&s, "router").await.unwrap();
    let mut manifest = serde_json::to_value(&package.manifest).unwrap();
    manifest["version"] = json!("2");
    manifest["operations"][1]["scopes"] = json!(["channel", "topic"]);
    manifest["tools"][1]["inputSchema"]["required"] = json!(["resource", "channel", "topic"]);
    manifest["tools"][1]["inputSchema"]["properties"]["topic"] = json!({ "type": "string" });
    let installed = registry::install(
        &s,
        &json!({
            "manifest": manifest, "source": package.source.main, "modules": package.source.modules,
        }),
    )
    .await
    .unwrap();
    registry::approve(&s, "router", "2", text(&installed, "digest"))
        .await
        .unwrap();
    let parent = s
        .agent_fixture(
            json!({
                "name": "Two scoped rules", "access": { "delegation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let target = s
        .agent_fixture(json!({ "name": "Both dimensions" }), None)
        .await
        .unwrap();
    let grants =
        format!("/api/plugins/router/connections/{instance}/resources/workspace/{resource}/grants");
    let rules = [
        json!({ "agentId": parent["id"], "scopes": { "channel": ["C1"], "topic": ["T1"] } }),
        json!({ "agentId": parent["id"], "scopes": { "channel": ["C2"], "topic": ["T2"] } }),
        json!({ "agentId": target["id"], "scopes": { "channel": ["C1", "C2"], "topic": ["T1", "T2"] } }),
    ];
    for mut rule in rules {
        rule["operations"] = json!(["channel-read"]);
        assert_eq!(
            common::send(&app, owner_request("POST", &grants, rule))
                .await
                .status(),
            StatusCode::OK
        );
    }
    let chat = s
        .chat_create(json!({ "agentId": parent["id"] }))
        .await
        .unwrap();
    s.chat_send(
        text(&chat, "id"),
        json!({
            "id": cairn_installation::config::id(), "text": "Review all permitted combinations",
        }),
    )
    .await
    .unwrap();
    let (_, token) = running_turn(&s, text(&chat, "id")).await;
    let task = json!({ "task": "Review channels and topics", "agentId": target["id"] });
    assert_eq!(
        cairn_installation::delegation::delegate(&s, &token, &task)
            .await
            .unwrap_err()
            .status,
        403
    );
    for (channel, topic) in [("C1", "T2"), ("C2", "T1")] {
        assert_eq!(
            common::send(
                &app,
                owner_request(
                    "POST",
                    &grants,
                    json!({
                        "agentId": parent["id"], "operations": ["channel-read"],
                        "scopes": { "channel": [channel], "topic": [topic] },
                    })
                )
            )
            .await
            .status(),
            StatusCode::OK
        );
    }
    let child = cairn_installation::delegation::delegate(&s, &token, &task)
        .await
        .unwrap();
    let (_, token) = running_turn(&s, text(&child, "chatId")).await;
    for (channel, topic) in [("C1", "T1"), ("C1", "T2"), ("C2", "T1"), ("C2", "T2")] {
        let result = agents::call_tool(
            &s,
            &token,
            "router_scope",
            &json!({
                "resource": resource, "channel": channel, "topic": topic,
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["channel"], channel);
        assert_eq!(result["topic"], topic);
    }
    assert!(
        agents::call_tool(
            &s,
            &token,
            "router_scope",
            &json!({
                "resource": resource, "channel": "C1", "topic": "T3",
            })
        )
        .await
        .is_err()
    );
}
