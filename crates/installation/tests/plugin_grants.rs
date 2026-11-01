//! Grant contracts through the authenticated API and actual plugin dispatcher.
mod common;

use axum::{Router, body::Body, http::StatusCode, routing::any};
use cairn_installation::{
    config::MAIN_AGENT_ID,
    plugins::{agents, grant_authorization},
    validation::text,
};
use common::{AgentFixture, McpFixture};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[path = "fixtures/plugin_grants.rs"]
mod fixture;

use fixture::{Fixture, contribution, running_conversation, support};

#[tokio::test]
async fn tools_commands_and_hooks_resolve_every_designation_and_reject_contradictions() {
    let f = Fixture::new().await;
    f.set(json!([
        contribution("tool", "grant_probe_post", support()),
        contribution("command", "post", support()),
        contribution("hook", "message", support())
    ]))
    .await;
    let tools = agents::tools(&f.s, &f.bearer).await.unwrap();
    assert_eq!(
        tools.iter().map(|t| text(t, "name")).collect::<Vec<_>>(),
        vec!["grant_probe_post"]
    );
    let run = cairn_installation::project_workspaces::authorize(&f.s, &f.bearer)
        .await
        .unwrap();
    assert_eq!(agents::instructions(&f.s, &run).await.unwrap(), "");
    for input in [
        json!({
            "channel": "support",
            "author": "anyone",
        }),
        json!({"*":"support"}),
        json!({"thread":"support-thread"}),
        json!({
            "channel": "support",
            "thread": "support-thread",
            "server": "guild",
        }),
        json!({"items":[{"channel":"support"}]}),
    ] {
        agents::call_tool(&f.s, &f.bearer, "grant_probe_post", &input)
            .await
            .unwrap();
        grant_authorization::authorize_hook_delivery(
            &f.s,
            MAIN_AGENT_ID,
            &f.instance,
            "message",
            &input,
        )
        .await
        .unwrap();
    }
    let effects = f.posts.load(Ordering::SeqCst);
    for input in [
        json!({}),
        json!({"channel":"general"}),
        json!({"*":"general"}),
        json!({
            "*": "support",
            "items": [{"channel":"general"}],
        }),
        json!({"thread":"general-thread"}),
        json!({
            "channel": "support",
            "thread": "general-thread",
        }),
        json!({
            "channel": "support",
            "server": "different",
        }),
        json!({"channel":"unknown"}),
        json!({"items":[{"channel":"support"},{"channel":"general"}]}),
        json!({"channel":""}),
        json!({"author":"support"}),
    ] {
        assert!(
            agents::call_tool(&f.s, &f.bearer, "grant_probe_post", &input)
                .await
                .is_err(),
            "{input}"
        );
        assert!(
            grant_authorization::authorize_hook_delivery(
                &f.s,
                MAIN_AGENT_ID,
                &f.instance,
                "message",
                &input
            )
            .await
            .is_err(),
            "{input}"
        );
    }
    assert_eq!(
        effects,
        f.posts.load(Ordering::SeqCst),
        "denied input reached a host effect"
    );
    agents::command(
        &f.s,
        &f.bearer,
        &json!({
            "plugin": "grant-probe",
            "command": "post",
            "args": ["support"],
        }),
    )
    .await
    .unwrap();
    assert!(
        agents::command(
            &f.s,
            &f.bearer,
            &json!({
                "plugin": "grant-probe",
                "command": "post",
                "args": ["general"],
            })
        )
        .await
        .is_err()
    );
    assert!(
        agents::call_tool(&f.s, &f.bearer, "grant_probe_other", &json!({}))
            .await
            .is_err()
    );
    assert!(
        cairn_installation::plugins::conditions::schemas(&f.s, MAIN_AGENT_ID)
            .await
            .unwrap()
            .is_empty()
    );
    f.set(json!([
        contribution("instructions", "", json!([])),
        contribution("condition", "ready", json!([]))
    ]))
    .await;
    assert_eq!(
        agents::instructions(&f.s, &run).await.unwrap(),
        "probe instructions"
    );
    assert_eq!(
        cairn_installation::plugins::conditions::schemas(&f.s, MAIN_AGENT_ID)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(agents::tools(&f.s, &f.bearer).await.unwrap().is_empty());
}

#[tokio::test]
async fn grant_replacement_is_revision_guarded_public_manage_and_installation_reduce_only() {
    let f = Fixture::new().await;
    let before = f
        .set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    let body = json!({
        "agentId": MAIN_AGENT_ID,
        "revision": before["revision"],
        "grants": [{ "instanceId": f.instance, "access": { "type": "full" } }],
        "runCredentialInstances": [],
    });
    let denied = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            body.clone(),
        )
        .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let read_only = common::relay_fixture::grant(&f.s, &["read"]).await;
    let denied = f
        .mcp(
            "/mcp",
            &read_only,
            "replace_agent_plugin_grants",
            body.clone(),
        )
        .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let public = common::relay_fixture::grant(&f.s, &["manage", "read"]).await;
    let changed = f
        .mcp("/mcp", &public, "replace_agent_plugin_grants", body.clone())
        .await;
    assert_ne!(changed["result"]["isError"], true, "{changed}");
    assert_eq!(f.api("PUT", body).await.unwrap_err().status, 409);
    let read = f
        .mcp(
            "/mcp",
            &public,
            "get_agent_plugin_grants",
            json!({"agentId":MAIN_AGENT_ID}),
        )
        .await;
    assert_ne!(read["result"]["isError"], true, "{read}");
    let current = f.api("GET", json!({})).await.unwrap();
    let reduced = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            json!({
                "agentId": MAIN_AGENT_ID,
                "revision": current["revision"],
                "grants": [],
                "runCredentialInstances": [],
            }),
        )
        .await;
    assert_ne!(reduced["result"]["isError"], true, "{reduced}");
    assert!(
        f.api("GET", json!({})).await.unwrap()["grants"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn instances_are_filtered_when_another_grants_a_different_contribution() {
    let f = Fixture::new().await;
    let second = common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({
            "name": "Second",
            "enabled": true,
        }),
        None,
    )
    .await
    .unwrap();
    let current = f.api("GET", json!({})).await.unwrap();
    f.api(
        "PUT",
        json!({
            "agentId": MAIN_AGENT_ID,
            "revision": current["revision"],
            "grants": [
                {
                    "instanceId": f.instance,
                    "access": {
                        "type": "selected",
                        "contributions": [contribution(
                            "tool",
                            "grant_probe_post",
                            support(),
                        )],
                    },
                },
                {
                    "instanceId": second["id"],
                    "access": {
                        "type": "selected",
                        "contributions": [contribution(
                            "instructions",
                            "",
                            json!([]),
                        )],
                    },
                },
            ],
            "runCredentialInstances": [],
        }),
    )
    .await
    .unwrap();
    let effects = f.posts.load(Ordering::SeqCst);
    assert!(
        agents::call_tool(
            &f.s,
            &f.bearer,
            "grant_probe_post",
            &json!({
                "channel": "support",
                "instance": second["id"],
            })
        )
        .await
        .is_err()
    );
    assert_eq!(f.posts.load(Ordering::SeqCst), effects);
    agents::call_tool(
        &f.s,
        &f.bearer,
        "grant_probe_post",
        &json!({
            "channel": "support",
            "instance": f.instance,
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn revocation_during_io_withholds_the_result_and_prevents_the_next_effect() {
    let f = Fixture::new().await;
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    f.pause.store(true, Ordering::SeqCst);
    let (s, bearer) = (f.s.clone(), f.bearer.clone());
    let called = tokio::spawn(async move {
        agents::call_tool(
            &s,
            &bearer,
            "grant_probe_post",
            &json!({
                "channel": "support",
                "two": true,
            }),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), f.entered.notified())
        .await
        .unwrap();
    f.set(json!([contribution("instructions", "", json!([]))]))
        .await;
    f.release.notify_one();
    assert!(called.await.unwrap().is_err());
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn delegated_plugin_instructions_recheck_the_requesters_contribution_grants() {
    let f = Fixture::new().await;
    let requester =
        f.s.agent_fixture(
            json!({
                "access": {
                    "delegation": true,
                    "installation": true,
                },
            }),
            Some(MAIN_AGENT_ID),
        )
        .await
        .unwrap();
    let requester_id = text(&requester, "id");
    let child =
        f.s.agent_fixture(
            json!({
                "name": "Child",
                "access": { "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let child_id = text(&child, "id");
    let instruction = json!([contribution("instructions", "", json!([]))]);
    f.set_for(requester_id, instruction.clone()).await;
    f.set_for(child_id, instruction).await;

    let parent =
        f.s.chat_create(json!({ "agentId": requester_id }))
            .await
            .unwrap();
    let parent_id = text(&parent, "id");
    f.s.chat_send(
        parent_id,
        json!({
            "id": cairn_installation::config::id(),
            "text": "Delegate",
        }),
    )
    .await
    .unwrap();
    let parent_token = running_conversation(&f.s, parent_id).await;
    let task = cairn_installation::delegation::delegate(
        &f.s,
        &parent_token,
        &json!({
            "agentId": child_id,
            "task": "Use the plugin",
        }),
    )
    .await
    .unwrap();
    let child_chat = text(&task, "chatId");
    let child_token = running_conversation(&f.s, child_chat).await;
    let run = cairn_installation::project_workspaces::authorize(&f.s, &child_token)
        .await
        .unwrap();

    assert_eq!(
        agents::with_instructions(&f.s, &run).await["pluginInstructions"],
        "probe instructions"
    );

    f.remove_for(requester_id).await;
    assert_eq!(
        agents::with_instructions(&f.s, &run).await["pluginInstructions"],
        "",
        "the target's instruction grant must not outlive its requester's contribution ceiling"
    );
}

#[tokio::test]
async fn delegated_descendant_grants_are_resolved_and_rechecked_after_requester_reduction() {
    let f = Fixture::new().await;
    f.s.agent_fixture(json!({"access":{"delegation":true}}), Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    let child =
        f.s.agent_fixture(json!({"name":"Child"}), None)
            .await
            .unwrap();
    let child_id = text(&child, "id");
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    f.set_for(
        child_id,
        json!([contribution(
            "tool",
            "grant_probe_post",
            json!([{"resourceType":"thread","id":"support-thread"}])
        )]),
    )
    .await;
    let parent =
        f.s.chat_create(json!({"agentId":MAIN_AGENT_ID}))
            .await
            .unwrap();
    let parent_id = text(&parent, "id");
    f.s.chat_send(
        parent_id,
        json!({
            "id": cairn_installation::config::id(),
            "text": "Delegate",
        }),
    )
    .await
    .unwrap();
    let parent_token = running_conversation(&f.s, parent_id).await;
    let task = cairn_installation::delegation::delegate(
        &f.s,
        &parent_token,
        &json!({
            "agentId": child_id,
            "task": "Use support",
        }),
    )
    .await
    .unwrap();
    let child_token = running_conversation(&f.s, text(&task, "chatId")).await;
    agents::call_tool(
        &f.s,
        &child_token,
        "grant_probe_post",
        &json!({"thread":"support-thread"}),
    )
    .await
    .unwrap();
    let effects = f.posts.load(Ordering::SeqCst);
    f.set(json!([contribution(
        "tool",
        "grant_probe_post",
        json!([{"resourceType":"channel","id":"general"}])
    )]))
    .await;
    assert!(
        agents::call_tool(
            &f.s,
            &child_token,
            "grant_probe_post",
            &json!({"thread":"support-thread"})
        )
        .await
        .is_err()
    );
    assert_eq!(effects, f.posts.load(Ordering::SeqCst));
}

#[tokio::test]
async fn credential_supplier_is_explicit_and_reductions_never_select_another() {
    let f = Fixture::new().await;
    let second = common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({
            "name": "Second",
            "enabled": true,
            "settings": { "marker": "second" },
        }),
        None,
    )
    .await
    .unwrap();
    let current = f.api("GET", json!({})).await.unwrap();
    let full = |id: &Value| {
        json!({
            "instanceId": id,
            "access": { "type": "full" },
        })
    };
    let first = json!(f.instance);
    let saved = f
        .api(
            "PUT",
            json!({
                "agentId": MAIN_AGENT_ID,
                "revision": current["revision"],
                "grants": [full(&first), full(&second["id"])],
                "runCredentialInstances": [first],
            }),
        )
        .await
        .unwrap();
    let read = cairn_installation::plugins::run_credentials::for_run(&f.s, MAIN_AGENT_ID, &[])
        .await
        .unwrap();
    assert_eq!(
        read.files,
        vec![(".config/probe/value".into(), "first".into())]
    );
    let reduced = f
        .api(
            "PUT",
            json!({
                "agentId": MAIN_AGENT_ID,
                "revision": saved["revision"],
                "grants": [full(&second["id"])],
                "runCredentialInstances": [],
            }),
        )
        .await
        .unwrap();
    assert!(
        cairn_installation::plugins::run_credentials::for_run(&f.s, MAIN_AGENT_ID, &[])
            .await
            .unwrap()
            .files
            .is_empty()
    );
    let choice = json!({
        "agentId": MAIN_AGENT_ID,
        "revision": reduced["revision"],
        "grants": [full(&second["id"])],
        "runCredentialInstances": [second["id"]],
    });
    let denied = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            choice.clone(),
        )
        .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    f.api("PUT", choice).await.unwrap();
    let read = cairn_installation::plugins::run_credentials::for_run(&f.s, MAIN_AGENT_ID, &[])
        .await
        .unwrap();
    assert_eq!(
        read.files,
        vec![(".config/probe/value".into(), "second".into())]
    );
}

#[tokio::test]
async fn legacy_full_grant_sync_clears_supplier_across_package_without_run_credentials() {
    let f = Fixture::new().await;
    f.s.agent_fixture(json!({ "plugins": [f.instance] }), Some(MAIN_AGENT_ID))
        .await
        .unwrap();

    let selected = cairn_installation::plugins::run_credentials::for_run(&f.s, MAIN_AGENT_ID, &[])
        .await
        .unwrap();
    assert_eq!(
        selected.files,
        vec![(".config/probe/value".into(), "first".into())]
    );

    let without_credentials = common::package_install(
        &f.s,
        &json!({
            "manifest": {
                "id": "grant-probe",
                "name": "Grant probe",
                "version": "2",
                "apiVersion": 2,
                "capabilities": [],
            },
            "source": "return {}",
        }),
    )
    .await
    .unwrap();
    common::package_approve(
        &f.s,
        "grant-probe",
        "2",
        text(&without_credentials, "digest"),
    )
    .await
    .unwrap();

    f.s.agent_fixture(json!({ "plugins": [f.instance] }), Some(MAIN_AGENT_ID))
        .await
        .unwrap();

    let with_credentials = common::package_install(
        &f.s,
        &json!({
            "manifest": {
                "id": "grant-probe",
                "name": "Grant probe",
                "version": "3",
                "apiVersion": 2,
                "capabilities": [],
                "agentAccess": {},
                "runCredentials": {
                    "files": [{
                        "path": ".config/probe/value",
                        "content": "reintroduced",
                    }],
                },
            },
            "source": "return {}",
        }),
    )
    .await
    .unwrap();
    common::package_approve(&f.s, "grant-probe", "3", text(&with_credentials, "digest"))
        .await
        .unwrap();

    f.s.agent_fixture(json!({ "plugins": [f.instance] }), Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    assert!(
        cairn_installation::plugins::run_credentials::for_run(&f.s, MAIN_AGENT_ID, &[])
            .await
            .unwrap()
            .files
            .is_empty(),
        "restoring runCredentials must not silently reuse the prior supplier"
    );
}

#[tokio::test]
async fn legacy_full_grant_sync_preserves_supplier_while_package_is_unavailable() {
    let f = Fixture::new().await;
    let profile = json!({ "plugins": [f.instance] });
    f.s.agent_fixture(profile.clone(), Some(MAIN_AGENT_ID))
        .await
        .unwrap();

    let path = f.s.config.data_dir.join("plugins/grant-probe/1/main.luau");
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, "return {} -- temporarily unavailable").unwrap();
    f.s.agent_fixture(profile, Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    std::fs::write(&path, original).unwrap();

    assert_eq!(
        cairn_installation::plugins::run_credentials::for_run(&f.s, MAIN_AGENT_ID, &[])
            .await
            .unwrap()
            .files,
        vec![(".config/probe/value".into(), "first".into())],
        "an unavailable package must not erase its explicitly retained supplier"
    );
}

#[tokio::test]
async fn installation_can_narrow_a_resource_to_its_descendant_but_cannot_reverse_it() {
    let f = Fixture::new().await;
    let before = f
        .set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    let replace = |revision: &Value, resources: Value| {
        json!({
            "agentId": MAIN_AGENT_ID,
            "revision": revision,
            "grants": [{
                "instanceId": f.instance,
                "access": {
                    "type": "selected",
                    "contributions": [contribution(
                        "tool",
                        "grant_probe_post",
                        resources,
                    )],
                },
            }],
            "runCredentialInstances": [],
        })
    };
    let narrowed = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            replace(
                &before["revision"],
                json!([{"resourceType":"thread","id":"support-thread"}]),
            ),
        )
        .await;
    assert_ne!(narrowed["result"]["isError"], true, "{narrowed}");
    let current = f.api("GET", json!({})).await.unwrap();
    let widened = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            replace(&current["revision"], support()),
        )
        .await;
    assert_eq!(widened["result"]["isError"], true, "{widened}");
}

#[tokio::test]
async fn transfer_reads_reauthorize_the_producing_contribution_and_resources() {
    let f = Fixture::new().await;
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    let response = agents::call_tool(
        &f.s,
        &f.bearer,
        "grant_probe_post",
        &json!({
            "channel": "support",
            "download": true,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let transfer = json!({
        "plugin": "grant-probe",
        "file": {
            "method": "read",
            "instance": f.instance,
            "id": response["file"]["id"],
        },
    });
    let read = agents::command(&f.s, &f.bearer, &transfer).await.unwrap();
    assert_eq!(read["value"]["data"], "b2s");
    f.set(json!([contribution(
        "tool",
        "grant_probe_post",
        json!([{ "resourceType": "channel", "id": "general" }])
    )]))
    .await;
    assert!(agents::command(&f.s, &f.bearer, &transfer).await.is_err());
    f.set(json!([contribution("instructions", "", json!([]))]))
        .await;
    assert!(agents::command(&f.s, &f.bearer, &transfer).await.is_err());
}

#[tokio::test]
async fn unavailable_existing_grants_and_their_supplier_survive_other_reductions() {
    let f = Fixture::new().await;
    let second = common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({
            "name": "Second",
            "enabled": true,
        }),
        None,
    )
    .await
    .unwrap();
    let full = |id: &Value| {
        json!({
            "instanceId": id,
            "access": { "type": "full" },
        })
    };
    let current = f.api("GET", json!({})).await.unwrap();
    let saved = f
        .api(
            "PUT",
            json!({
                "agentId": MAIN_AGENT_ID,
                "revision": current["revision"],
                "grants": [full(&json!(f.instance)), full(&second["id"])],
                "runCredentialInstances": [f.instance],
            }),
        )
        .await
        .unwrap();
    let path = f.s.config.data_dir.join("plugins/grant-probe/1/main.luau");
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, "return {} -- awaiting review").unwrap();
    let reduced = f
        .api(
            "PUT",
            json!({
                "agentId": MAIN_AGENT_ID,
                "revision": saved["revision"],
                "grants": [full(&json!(f.instance))],
                "runCredentialInstances": [f.instance],
            }),
        )
        .await
        .unwrap();
    assert_eq!(reduced["runCredentialInstances"], json!([f.instance]));
    assert_eq!(reduced["grants"].as_array().unwrap().len(), 1);
    std::fs::write(&path, original).unwrap();
    assert_eq!(
        cairn_installation::plugins::run_credentials::for_run(&f.s, MAIN_AGENT_ID, &[])
            .await
            .unwrap()
            .files
            .len(),
        1
    );
}

#[tokio::test]
async fn legacy_full_grants_without_run_credentials_round_trip_without_a_supplier() {
    let f = Fixture::new().await;
    common::save_plugin(
        &f.s,
        "onepassword",
        &json!({
            "name": "Fixture",
            "enabled": true,
            "secrets": { "token": "ops_fixture" },
            "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await
    .unwrap();
    let current = f.api("GET", json!({})).await.unwrap();
    assert_eq!(current["runCredentialInstances"], json!([]));
    let saved = f.api("PUT", current).await.unwrap();
    assert_eq!(saved["runCredentialInstances"], json!([]));
}

#[tokio::test]
async fn merges_accept_semantically_covered_descendant_grants() {
    let f = Fixture::new().await;
    let source =
        f.s.agent_fixture(json!({ "name": "Source" }), None)
            .await
            .unwrap();
    let source_id = text(&source, "id");
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    f.set_for(
        source_id,
        json!([contribution(
            "tool",
            "grant_probe_post",
            json!([{ "resourceType": "thread", "id": "support-thread" }])
        )]),
    )
    .await;
    let merged =
        f.s.merge_agents(
            serde_json::from_value(json!({
                "sourceId": source_id,
                "targetId": MAIN_AGENT_ID,
                "confirm": true,
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(merged.deleted);
    assert!(f.s.store.agent(source_id).await.unwrap().is_none());
    assert_eq!(
        f.api("GET", json!({})).await.unwrap()["grants"][0]["access"]["contributions"][0]["resources"],
        support()
    );
}

#[tokio::test]
async fn deferred_resource_effects_retain_the_delegation_resource_ceiling() {
    let f = Fixture::new().await;
    f.s.agent_fixture(
        json!({ "access": { "delegation": true } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let child =
        f.s.agent_fixture(
            json!({
                "name": "Child",
                "access": { "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let child_id = text(&child, "id");
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    f.set_for(
        child_id,
        json!([contribution(
            "tool",
            "grant_probe_post",
            json!([{ "resourceType": "thread", "id": "support-thread" }])
        )]),
    )
    .await;
    let parent =
        f.s.chat_create(json!({ "agentId": MAIN_AGENT_ID }))
            .await
            .unwrap();
    f.s.chat_send(
        text(&parent, "id"),
        json!({
            "id": cairn_installation::config::id(),
            "text": "Delegate",
        }),
    )
    .await
    .unwrap();
    let parent_token = running_conversation(&f.s, text(&parent, "id")).await;
    let task = cairn_installation::delegation::delegate(
        &f.s,
        &parent_token,
        &json!({
            "agentId": child_id,
            "task": "Read resources",
        }),
    )
    .await
    .unwrap();
    let child_token = running_conversation(&f.s, text(&task, "chatId")).await;
    f.pause.store(true, Ordering::SeqCst);
    let started = f
        .mcp(
            "/mcp-installation",
            &child_token,
            "list_plugin_resources",
            json!({
                "pluginId": "grant-probe",
                "instanceId": f.instance,
                "resourceType": "channel",
            }),
        )
        .await;
    assert_ne!(started["result"]["isError"], true, "{started}");
    let operation = text(
        &started["result"]["structuredContent"]["result"],
        "operation",
    );
    assert!(!operation.is_empty(), "{started}");
    tokio::time::timeout(std::time::Duration::from_secs(5), f.entered.notified())
        .await
        .unwrap();
    f.set(json!([contribution(
        "tool",
        "grant_probe_post",
        json!([{ "resourceType": "channel", "id": "general" }])
    )]))
    .await;
    f.release.notify_one();
    let state = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let state = f.s.plugins.operations.get(operation).unwrap();
            if matches!(text(&state, "state"), "completed" | "failed" | "cancelled") {
                break state;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state["state"], "failed", "{state}");
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cross_agent_descendant_reductions_keep_the_callers_instance_boundary() {
    let f = Fixture::new().await;
    let child =
        f.s.agent_fixture(json!({ "name": "Child" }), None)
            .await
            .unwrap();
    let child_id = text(&child, "id");
    let before = f
        .set_for(
            child_id,
            json!([contribution("tool", "grant_probe_post", support())]),
        )
        .await;
    let request = json!({
        "agentId": child_id,
        "revision": before["revision"],
        "grants": [{
            "instanceId": f.instance,
            "access": {
                "type": "selected",
                "contributions": [contribution("tool", "grant_probe_post", json!([
                    { "resourceType": "thread", "id": "support-thread" },
                ]))],
            },
        }],
        "runCredentialInstances": [],
    });
    let denied = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            request.clone(),
        )
        .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);

    f.set(json!([contribution("instructions", "", json!([]))]))
        .await;
    let allowed = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            request,
        )
        .await;
    assert_ne!(allowed["result"]["isError"], true, "{allowed}");
}

#[tokio::test]
async fn nested_coverage_effects_stop_when_the_calling_token_is_revoked() {
    let f = Fixture::new().await;
    let child =
        f.s.agent_fixture(
            json!({
                "name": "Child",
                "access": { "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let child_id = text(&child, "id");
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    f.set_for(
        child_id,
        json!([contribution(
            "tool",
            "grant_probe_post",
            json!([
                { "resourceType": "thread", "id": "support-thread" },
            ])
        )]),
    )
    .await;
    let (chat, token) = f.delegated(MAIN_AGENT_ID, child_id).await;
    let run = f.s.chat_detail(&chat).await.unwrap()["run"].clone();
    f.marker("resolver-effects").await;
    f.pause.store(true, Ordering::SeqCst);
    let action = cairn_installation::project_workspaces::authorize(&f.s, &token);
    let revoke = async {
        tokio::time::timeout(std::time::Duration::from_secs(10), f.entered.notified())
            .await
            .unwrap();
        f.s.mcps.revoke_run(&f.s, text(&run, "id")).await.unwrap();
        f.release.notify_one();
    };
    let (result, ()) = tokio::join!(action, revoke);
    assert!(result.is_err());
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn appending_a_protected_transfer_retains_its_source_authority() {
    let f = Fixture::new().await;
    let grants = json!([
        contribution("tool", "grant_probe_post", support()),
        contribution("tool", "grant_probe_other", json!([])),
    ]);
    f.set(grants).await;
    let original = agents::call_tool(
        &f.s,
        &f.bearer,
        "grant_probe_post",
        &json!({
            "channel": "support",
            "download": true,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let copied = agents::call_tool(
        &f.s,
        &f.bearer,
        "grant_probe_other",
        &json!({ "copy": original["file"]["id"] }),
    )
    .await
    .unwrap()
    .unwrap();
    let request = json!({
        "plugin": "grant-probe",
        "file": {
            "method": "read",
            "instance": f.instance,
            "id": copied["id"],
        },
    });
    assert_eq!(
        agents::command(&f.s, &f.bearer, &request).await.unwrap()["value"]["data"],
        "b2s"
    );
    f.set(json!([contribution(
        "tool",
        "grant_probe_other",
        json!([])
    )]))
    .await;
    assert!(agents::command(&f.s, &f.bearer, &request).await.is_err());
}

#[tokio::test]
async fn gateway_calls_recheck_the_requesters_resource_coverage() {
    let f = Fixture::new().await;
    let remote =
        f.s.mcp_fixture(
            json!({
                "name": "Remote", "url": "https://service.example.test/mcp", "auth": "none",
            }),
            None,
        )
        .await
        .unwrap();
    let child =
        f.s.agent_fixture(
            json!({
                "name": "Child",
                "access": { "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let child_id = text(&child, "id");
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    f.set_for(
        child_id,
        json!([contribution(
            "tool",
            "grant_probe_post",
            json!([
                { "resourceType": "thread", "id": "support-thread" },
            ])
        )]),
    )
    .await;
    let (_, token) = f.delegated(MAIN_AGENT_ID, child_id).await;
    assert!(
        f.s.mcps
            .grant(&f.s, text(&remote, "id"), &token)
            .await
            .is_ok()
    );
    f.set(json!([contribution(
        "tool",
        "grant_probe_post",
        json!([
            { "resourceType": "channel", "id": "general" },
        ])
    )]))
    .await;
    let app = common::relay_fixture::router(f.s.clone()).await.unwrap();
    let request = common::request("POST", &format!("/mcp-gateway/{}", text(&remote, "id")))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {},
            })
            .to_string(),
        ))
        .unwrap();
    let response = common::send(&app, request).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancelled_mission_observations_stop_before_the_next_host_effect() {
    let f = Fixture::new().await;
    f.set(json!([contribution("condition", "ready", json!([]))]))
        .await;
    f.marker("condition-pending").await;
    let scheduled = cairn_installation::waits::schedule(
        &f.s,
        &f.bearer,
        &json!({
            "conditions": [{ "type": "grant-probe_ready", "pluginId": "grant-probe" }],
            "reason": "Wait for probe", "timeoutSeconds": 600,
        }),
    )
    .await
    .unwrap();
    assert_eq!(scheduled["deferred"], true, "{scheduled}");
    f.s.store
        .change_run(
            &f.run,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Waiting),
                recovery_pending: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    f.marker("condition-effects").await;
    f.pause.store(true, Ordering::SeqCst);
    let observe = cairn_installation::plugins::conditions::poll_once(&f.s);
    let cancel = async {
        tokio::time::timeout(std::time::Duration::from_secs(10), f.entered.notified())
            .await
            .unwrap();
        f.s.worker.cancel(&f.s, &f.run).await.unwrap();
        f.release.notify_one();
    };
    let (result, ()) = tokio::join!(observe, cancel);
    result.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn condition_watchers_keep_distinct_requester_authority() {
    let f = Fixture::new().await;
    let other =
        f.s.agent_fixture(json!({ "name": "Other requester" }), None)
            .await
            .unwrap();
    let child =
        f.s.agent_fixture(
            json!({
                "name": "Child",
                "access": { "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let other_id = text(&other, "id");
    let child_id = text(&child, "id");
    let parent_grants = json!([
        contribution("tool", "grant_probe_post", support()),
        contribution("condition", "ready", json!([])),
    ]);
    f.set(parent_grants.clone()).await;
    f.set_for(other_id, parent_grants).await;
    f.set_for(
        child_id,
        json!([
            contribution(
                "tool",
                "grant_probe_post",
                json!([{ "resourceType": "thread", "id": "support-thread" }])
            ),
            contribution("condition", "ready", json!([])),
        ]),
    )
    .await;
    let (first_chat, first_token) = f.delegated(MAIN_AGENT_ID, child_id).await;
    let (second_chat, second_token) = f.delegated(other_id, child_id).await;
    f.marker("condition-pending").await;
    for (chat, token) in [(&first_chat, &first_token), (&second_chat, &second_token)] {
        let result = cairn_installation::waits::schedule(
            &f.s,
            token,
            &json!({
                "conditions": [{ "type": "grant-probe_ready", "pluginId": "grant-probe" }],
                "reason": "Wait for probe", "timeoutSeconds": 600,
            }),
        )
        .await
        .unwrap();
        assert_eq!(result["deferred"], true, "{result}");
        let run = f.s.chat_detail(chat).await.unwrap()["run"].clone();
        f.s.store
            .change_run(
                text(&run, "id"),
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Succeeded),
                    recovery_pending: Some(false),
                    session_id: Some(Some("saved-session".into())),
                    workspace: Some(Some("/saved/workspace".into())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    f.set(json!([
        contribution(
            "tool",
            "grant_probe_post",
            json!([{ "resourceType": "channel", "id": "general" }])
        ),
        contribution("condition", "ready", json!([])),
    ]))
    .await;
    f.marker("ready").await;
    cairn_installation::plugins::conditions::poll_once(&f.s)
        .await
        .unwrap();
    let first = f.s.chat_detail(&first_chat).await.unwrap();
    let second = f.s.chat_detail(&second_chat).await.unwrap();
    assert_eq!(
        first["wakeup"]["observation"]["state"], "unavailable",
        "{first}"
    );
    assert_eq!(
        second["wakeup"]["observation"]["state"], "completed",
        "{second}"
    );
}

#[tokio::test]
async fn instruction_only_packages_cannot_shadow_a_granted_tool() {
    let f = Fixture::new().await;
    let mut grants = Vec::new();
    for plugin in ["foo", "foo-bar"] {
        let installed = common::package_install(&f.s, &json!({
            "manifest": {
                "id": plugin, "name": plugin, "version": "1", "apiVersion": 2,
                "capabilities": [], "agentAccess": {}, "instructions": "Fixture",
                "tools": [{ "name": "foo_bar_read", "description": "Read", "inputSchema": { "type": "object" } }],
            },
            "source": format!("return {{ tool = function(input) return {{ source = '{plugin}' }} end }}"),
        })).await.unwrap();
        common::package_approve(&f.s, plugin, "1", text(&installed, "digest"))
            .await
            .unwrap();
        let instance = common::save_plugin(
            &f.s,
            plugin,
            &json!({
                "name": plugin,
                "enabled": true,
            }),
            None,
        )
        .await
        .unwrap();
        let contributions = if plugin == "foo" {
            vec![contribution("instructions", "", json!([]))]
        } else {
            vec![contribution("tool", "foo_bar_read", json!([]))]
        };
        grants.push(json!({
            "instanceId": instance["id"],
            "access": {
                "type": "selected",
                "contributions": contributions,
            },
        }));
    }
    let current = f.api("GET", json!({})).await.unwrap();
    f.api(
        "PUT",
        json!({
            "agentId": MAIN_AGENT_ID,
            "revision": current["revision"],
            "grants": grants,
            "runCredentialInstances": [],
        }),
    )
    .await
    .unwrap();
    let tools = agents::tools(&f.s, &f.bearer).await.unwrap();
    assert_eq!(
        tools
            .iter()
            .filter(|tool| tool["name"] == "foo_bar_read")
            .count(),
        1
    );
    let result = agents::call_tool(&f.s, &f.bearer, "foo_bar_read", &json!({}))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["source"], "foo-bar");
}

#[tokio::test]
async fn repeated_appends_preserve_the_supported_chunked_transfer_size() {
    let f = Fixture::new().await;
    f.set(json!([contribution(
        "tool",
        "grant_probe_other",
        json!([])
    )]))
    .await;
    let result = agents::call_tool(
        &f.s,
        &f.bearer,
        "grant_probe_other",
        &json!({ "chunks": 90 }),
    )
    .await
    .unwrap()
    .unwrap();
    let target = result["id"].clone();
    for size in [900, 1350] {
        let result = agents::call_tool(
            &f.s,
            &f.bearer,
            "grant_probe_other",
            &json!({
                "chunks": 90,
                "target": target,
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["size"], size);
    }
}

#[tokio::test]
async fn requester_deletion_stops_the_turn_without_blocking_a_fresh_authorized_turn() {
    let f = Fixture::new().await;
    let child =
        f.s.agent_fixture(json!({ "name": "Child" }), None)
            .await
            .unwrap();
    let child_id = text(&child, "id");
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    f.set_for(
        child_id,
        json!([contribution("tool", "grant_probe_post", support())]),
    )
    .await;
    let (child_chat, token) = f.delegated(MAIN_AGENT_ID, child_id).await;
    let child_detail = f.s.chat_detail(&child_chat).await.unwrap();
    let parent = text(&child_detail["requester"], "chatId");
    agents::call_tool(
        &f.s,
        &token,
        "grant_probe_post",
        &json!({ "channel": "support" }),
    )
    .await
    .unwrap();
    f.s.chat_trash(parent, true).await.unwrap();
    let stopped =
        f.s.store
            .run(text(&child_detail["run"], "id"))
            .await
            .unwrap();
    assert_eq!(stopped["status"], "cancelled", "{stopped}");
    let effects = f.posts.load(Ordering::SeqCst);
    assert!(
        agents::call_tool(
            &f.s,
            &token,
            "grant_probe_post",
            &json!({ "channel": "support" })
        )
        .await
        .is_err()
    );
    assert_eq!(f.posts.load(Ordering::SeqCst), effects);

    f.s.chat_pause(&child_chat, false).await.unwrap();
    f.s.chat_send(
        &child_chat,
        json!({
            "id": cairn_installation::config::id(),
            "text": "Continue with this agent",
        }),
    )
    .await
    .unwrap();
    let fresh = running_conversation(&f.s, &child_chat).await;
    agents::call_tool(
        &f.s,
        &fresh,
        "grant_probe_post",
        &json!({ "channel": "support" }),
    )
    .await
    .unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), effects + 1);
}

#[tokio::test]
async fn deleting_an_idle_requester_pauses_its_unstarted_delegated_work() {
    let f = Fixture::new().await;
    f.s.agent_fixture(
        json!({ "access": { "delegation": true } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let child =
        f.s.agent_fixture(json!({ "name": "Child" }), None)
            .await
            .unwrap();
    let parent =
        f.s.chat_create(json!({ "agentId": MAIN_AGENT_ID }))
            .await
            .unwrap();
    let parent_id = text(&parent, "id");
    f.s.chat_send(
        parent_id,
        json!({
            "id": cairn_installation::config::id(),
            "text": "Delegate",
        }),
    )
    .await
    .unwrap();
    let token = running_conversation(&f.s, parent_id).await;
    let task = cairn_installation::delegation::delegate(
        &f.s,
        &token,
        &json!({
            "agentId": child["id"],
            "task": "Pending task",
        }),
    )
    .await
    .unwrap();
    let child_id = text(&task, "chatId");
    assert!(f.s.chat_detail(child_id).await.unwrap()["run"].is_null());
    let parent_run = f.s.chat_detail(parent_id).await.unwrap()["run"].clone();
    f.s.store
        .change_run(
            text(&parent_run, "id"),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Succeeded),
                recovery_pending: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.s.chat_trash(parent_id, false).await.unwrap_err().status,
        409
    );
    assert_eq!(f.s.chat_detail(child_id).await.unwrap()["paused"], false);
    f.s.chat_trash(parent_id, true).await.unwrap();
    assert_eq!(f.s.chat_detail(child_id).await.unwrap()["paused"], true);
    f.s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    assert!(f.s.chat_detail(child_id).await.unwrap()["run"].is_null());
    assert_eq!(f.s.store.run(&f.run).await.unwrap()["status"], "running");
}

#[tokio::test]
async fn merge_rechecks_previously_resolved_instances_before_publication() {
    let f = Fixture::new().await;
    let second = common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({
            "name": "Second",
            "enabled": true,
        }),
        None,
    )
    .await
    .unwrap();
    let mut ids = [f.instance.clone(), text(&second, "id").to_owned()];
    ids.sort();
    let [first, last] = ids;
    let mut held = f.s.store.plugin_instance(&last).await.unwrap().unwrap();
    held.settings["marker"] = "resolver-effects".into();
    held.revision = cairn_installation::config::id();
    f.s.store
        .transaction(move |db| db.save_plugin_instance(&held))
        .await
        .unwrap();
    let source =
        f.s.agent_fixture(json!({ "name": "Source" }), None)
            .await
            .unwrap();
    let source_id = text(&source, "id").to_owned();
    for (agent, roots) in [
        (MAIN_AGENT_ID, support()),
        (
            source_id.as_str(),
            json!([{ "resourceType": "thread", "id": "support-thread" }]),
        ),
    ] {
        let before = f.api("GET", json!({ "agentId": agent })).await.unwrap();
        let grants = [&first, &last]
            .into_iter()
            .map(|instance| {
                json!({
                    "instanceId": instance,
                    "access": {
                        "type": "selected",
                        "contributions": [contribution(
                            "tool",
                            "grant_probe_post",
                            roots.clone(),
                        )],
                    },
                })
            })
            .collect::<Vec<_>>();
        f.api(
            "PUT",
            json!({
                "agentId": agent,
                "revision": before["revision"],
                "grants": grants,
                "runCredentialInstances": [],
            }),
        )
        .await
        .unwrap();
    }
    f.pause.store(true, Ordering::SeqCst);
    let s = f.s.clone();
    let merging = source_id.clone();
    let merge = tokio::spawn(async move {
        s.merge_agents(
            serde_json::from_value(json!({
                "sourceId": merging, "targetId": MAIN_AGENT_ID, "confirm": true,
            }))
            .unwrap(),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), f.entered.notified())
        .await
        .unwrap();
    // A's coverage completed. B's resolver is held open. Change A without
    // changing either profile revision, then let the coverage operation finish.
    let mut changed = f.s.store.plugin_instance(&first).await.unwrap().unwrap();
    changed.settings["marker"] = "different-parent".into();
    changed.revision = cairn_installation::config::id();
    f.s.store
        .transaction(move |db| db.save_plugin_instance(&changed))
        .await
        .unwrap();
    f.release.notify_one();
    assert!(
        merge.await.unwrap().is_err(),
        "merge accepted stale resource ancestry"
    );
    assert!(
        f.s.store.agent(&source_id).await.unwrap().is_some(),
        "a refused merge must preserve the source"
    );
}

#[tokio::test]
async fn gateway_reresolves_external_ancestry_after_discovery_without_a_local_revision_change() {
    use axum::Json;
    let f = Fixture::new().await;
    f.marker("live-parent").await;
    let moved = f.moved_parent.clone();
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let app = Router::new().fallback(any(move |Json(request): Json<Value>| {
        let (moved, count) = (moved.clone(), count.clone());
        async move {
            let result = if request["method"] == "server/discover" {
                moved.store(true, Ordering::SeqCst);
                json!({
                    "supportedVersions": [cairn_installation::mcp_client::MODERN],
                    "capabilities": { "tools": {} },
                })
            } else {
                if request["method"] == "tools/call" {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                json!({ "content": [] })
            };
            Json(json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": result,
            }))
        }
    }));
    let (origin, server) = common::serve_locally(app).await;
    let remote =
        f.s.mcp_fixture(
            json!({
                "name": "Remote",
                "url": origin,
                "auth": "none",
                "allowPrivateNetwork": true,
            }),
            None,
        )
        .await
        .unwrap();
    let child =
        f.s.agent_fixture(
            json!({
                "name": "Child",
                "access": { "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    f.set_for(
        text(&child, "id"),
        json!([contribution(
            "tool",
            "grant_probe_post",
            json!([{ "resourceType": "thread", "id": "support-thread" }])
        )]),
    )
    .await;
    let (_, token) = f.delegated(MAIN_AGENT_ID, text(&child, "id")).await;
    let before =
        f.s.store
            .plugin_instance(&f.instance)
            .await
            .unwrap()
            .unwrap()
            .revision;
    let app = common::relay_fixture::router(f.s.clone()).await.unwrap();
    let request = common::request("POST", &format!("/mcp-gateway/{}", text(&remote, "id")))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "mutate", "arguments": {} },
            })
            .to_string(),
        ))
        .unwrap();
    let response = common::send(&app, request).await;
    assert!(
        f.moved_parent.load(Ordering::SeqCst),
        "discovery must reach the remote service"
    );
    assert_eq!(
        before,
        f.s.store
            .plugin_instance(&f.instance)
            .await
            .unwrap()
            .unwrap()
            .revision
    );
    let body = axum::body::to_bytes(response.into_body(), 1_048_576)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], -32603, "{body}");
    assert!(
        text(&body["error"], "message").contains("resources"),
        "{body}"
    );
    assert_eq!(
        effects.load(Ordering::SeqCst),
        0,
        "stale ancestry authorized a gateway mutation"
    );
    server.abort();
}

#[tokio::test]
async fn consuming_a_transfer_rechecks_its_source_after_an_async_effect() {
    let f = Fixture::new().await;
    f.marker("live-parent").await;
    f.set(json!([
        contribution("tool", "grant_probe_post", support()),
        contribution("tool", "grant_probe_other", json!([])),
    ]))
    .await;
    let original = agents::call_tool(
        &f.s,
        &f.bearer,
        "grant_probe_post",
        &json!({
            "thread": "support-thread",
            "download": true,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let input = json!({ "consume": original["file"]["id"] });
    let action = agents::call_tool(&f.s, &f.bearer, "grant_probe_other", &input);
    let move_source = async {
        tokio::time::timeout(std::time::Duration::from_secs(10), f.entered.notified())
            .await
            .unwrap();
        f.moved_parent.store(true, Ordering::SeqCst);
        f.release.notify_one();
    };
    let (result, ()) = tokio::join!(action, move_source);
    assert_eq!(
        f.uploads.load(Ordering::SeqCst),
        0,
        "protected bytes escaped after source ancestry changed"
    );
    assert!(result.is_err(), "{result:?}");
}

#[tokio::test]
async fn explicit_setup_grant_can_choose_after_removal_without_profile_or_settings_fallback() {
    let f = Fixture::new().await;
    let first = common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({
            "name": "First", "enabled": true, "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        f.api("GET", json!({})).await.unwrap()["runCredentialInstances"],
        json!([first["id"]])
    );
    let retained = common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({
            "name": "Existing full grant", "enabled": true,
        }),
        None,
    )
    .await
    .unwrap();
    let current = f.api("GET", json!({})).await.unwrap();
    f.api(
        "PUT",
        json!({ "agentId": MAIN_AGENT_ID, "revision": current["revision"],
            "grants": [{ "instanceId": retained["id"], "access": { "type": "full" } }],
            "runCredentialInstances": [],
        }),
    )
    .await
    .unwrap();
    f.s.agent_fixture(
        json!({ "name": "Renamed after removal" }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({
            "name": "Only settings changed", "enabled": true,
        }),
        Some(text(&retained, "id")),
    )
    .await
    .unwrap();
    assert_eq!(
        f.api("GET", json!({})).await.unwrap()["runCredentialInstances"],
        json!([])
    );
    let second = common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({
            "name": "Second", "enabled": true, "agentIds": [MAIN_AGENT_ID],
            "settings": { "marker": "second" },
        }),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        f.api("GET", json!({})).await.unwrap()["runCredentialInstances"],
        json!([second["id"]])
    );
    assert_eq!(
        cairn_installation::plugins::run_credentials::for_run(&f.s, MAIN_AGENT_ID, &[])
            .await
            .unwrap()
            .files,
        vec![(".config/probe/value".into(), "second".into())]
    );
    let third = common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({
            "name": "Third", "enabled": true, "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await
    .unwrap();
    let current = f.api("GET", json!({})).await.unwrap();
    assert_eq!(current["runCredentialInstances"], json!([second["id"]]));
    f.api("PUT", json!({ "agentId": MAIN_AGENT_ID, "revision": current["revision"],
        "grants": [{ "instanceId": third["id"], "access": { "type": "full" } }], "runCredentialInstances": [],
    })).await.unwrap();
    common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({
            "name": "Only settings changed", "enabled": true,
        }),
        Some(text(&third, "id")),
    )
    .await
    .unwrap();
    assert_eq!(
        f.api("GET", json!({})).await.unwrap()["runCredentialInstances"],
        json!([])
    );
    assert!(
        cairn_installation::plugins::run_credentials::for_run(&f.s, MAIN_AGENT_ID, &[])
            .await
            .unwrap()
            .files
            .is_empty()
    );
}
