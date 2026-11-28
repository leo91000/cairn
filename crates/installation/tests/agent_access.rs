//! Account access to agents at the API boundary (ADR-0075, « Accès des comptes aux agents »).
mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::MAIN_AGENT_ID,
    service::Service,
};
use common::{AgentFixture, ChatRunFixture, relay_fixture, run_fixture::DbRunFixture};
use serde_json::{Value, json};
use std::sync::Arc;

const MEMBER: &str = "member-account";

async fn call(
    s: &Arc<Service>,
    account: Option<&str>,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    // The trusted identity reaches the real HTTP boundary without the fixture relay's
    // membership refresh. Direct connections must authorize even if refreshes were lost.
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let identity = match account {
        Some(account) => InstallationIdentity::trusted(InstallationRole::Member, account),
        None => InstallationIdentity::trusted(InstallationRole::Owner, "fixture-owner"),
    };
    let request = common::request(method, path)
        .extension(identity)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = common::send(&app, request).await;
    (response.status(), common::read_json(response).await)
}

async fn owner(s: &Arc<Service>, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    call(s, None, method, path, body).await
}

async fn member(s: &Arc<Service>, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    call(s, Some(MEMBER), method, path, body).await
}

async fn service(root: &std::path::Path) -> Arc<Service> {
    let s = Service::new(common::config(root)).await.unwrap();
    std::fs::create_dir_all(&s.config.home).unwrap();
    relay_fixture::claimed(&s).await.unwrap();
    s
}

async fn agent(s: &Arc<Service>, name: &str) -> String {
    let agent = s
        .agent_fixture(json!({ "name": name }), None)
        .await
        .unwrap();
    agent["id"].as_str().unwrap().to_owned()
}

async fn usable(s: &Arc<Service>) -> Value {
    usable_for(s, MEMBER).await
}

async fn usable_for(s: &Arc<Service>, account: &str) -> Value {
    let (status, usable) = call(s, Some(account), "GET", "/api/usable-agents", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{usable}");
    usable["agentIds"].clone()
}

fn accounts(output: &Value) -> Vec<&str> {
    output["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|account| account["accountId"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn only_the_owner_gives_access_and_members_use_only_their_agents() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let reviewer = agent(&s, "Reviewer").await;
    relay_fixture::add_member(&s, MEMBER, "membership-1").await;
    let path = format!("/api/agents/{reviewer}/accounts/{MEMBER}");

    // A new agent is the owner's alone.
    assert_eq!(usable(&s).await, json!([]));
    let (status, _) = member(&s, "POST", "/api/chats", json!({ "agentId": reviewer })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, owned) = owner(&s, "GET", "/api/usable-agents", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(owned["agentIds"].as_array().unwrap().len(), 2);

    // Members can neither read nor give access; the owner cannot list themself or strangers.
    assert_eq!(
        member(&s, "PUT", &path, json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    let listing = format!("/api/agents/{reviewer}/accounts");
    assert_eq!(
        member(&s, "GET", &listing, Value::Null).await.0,
        StatusCode::FORBIDDEN
    );
    let owner_path = format!("/api/agents/{reviewer}/accounts/fixture-owner");
    assert_eq!(
        owner(&s, "PUT", &owner_path, json!({})).await.0,
        StatusCode::BAD_REQUEST
    );
    let stranger = format!("/api/agents/{reviewer}/accounts/stranger");
    assert_eq!(
        owner(&s, "PUT", &stranger, json!({})).await.0,
        StatusCode::NOT_FOUND
    );

    let (status, granted) = owner(&s, "PUT", &path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{granted}");
    assert_eq!(accounts(&granted), [MEMBER]);
    assert_eq!(owner(&s, "PUT", &path, json!({})).await.1, granted);
    assert_eq!(owner(&s, "GET", &listing, Value::Null).await.1, granted);
    assert_eq!(usable(&s).await, json!([reviewer]));

    let (status, chat) = member(&s, "POST", "/api/chats", json!({ "agentId": reviewer })).await;
    assert_eq!(status, StatusCode::OK, "{chat}");
    let chat = chat["id"].as_str().unwrap().to_owned();
    let (status, _) = member(
        &s,
        "POST",
        "/api/chats",
        json!({ "agentId": MAIN_AGENT_ID }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let messages = format!("/api/chats/{chat}/messages");
    let (status, sent) = member(
        &s,
        "POST",
        &messages,
        json!({
            "id": cairn_installation::config::id(),
            "text": "Review it",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");

    let (status, revoked) = owner(&s, "DELETE", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    assert!(accounts(&revoked).is_empty());

    let message_id = sent["id"].as_str().unwrap();
    let edit_message = format!("{messages}/{message_id}");
    let (status, refused) = member(
        &s,
        "PUT",
        &edit_message,
        json!({ "text": "Edit after access was revoked" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");

    // Without access, a member still sees and stops the work, but cannot direct or delete it.
    let detail = format!("/api/chats/{chat}");
    assert_eq!(
        member(&s, "GET", &detail, Value::Null).await.0,
        StatusCode::OK
    );
    let (status, _) = member(
        &s,
        "POST",
        &messages,
        json!({
            "id": cairn_installation::config::id(),
            "text": "Again",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let pause = format!("/api/chats/{chat}/pause");
    let (status, paused) = member(&s, "POST", &pause, json!({ "paused": true })).await;
    assert_eq!(status, StatusCode::OK, "{paused}");
    let (status, _) = member(&s, "POST", &pause, json!({ "paused": false })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let stop = format!("/api/chats/{chat}/stop");
    assert_ne!(
        member(&s, "POST", &stop, json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, _) = member(&s, "DELETE", &detail, json!({ "confirm": true })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, deleted) = owner(&s, "DELETE", &detail, json!({ "confirm": true })).await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
}

#[tokio::test]
async fn every_route_that_directs_an_agent_needs_access_to_it() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let reviewer = agent(&s, "Reviewer").await;
    relay_fixture::add_member(&s, MEMBER, "membership-1").await;
    let grant = format!("/api/agents/{reviewer}/accounts/{MEMBER}");

    let (_, created_chat) = owner(&s, "POST", "/api/chats", json!({ "agentId": reviewer })).await;
    let chat = created_chat["id"].as_str().unwrap().to_owned();
    // A conversation's finished runs, which a retry would start again: its current one
    // and an earlier one, which still follows the conversation.
    let chat_run = cairn_installation::config::id();
    let earlier_run = cairn_installation::config::id();
    for run in [&chat_run, &earlier_run] {
        let record = json!({
            "id": run,
            "taskId": chat,
            "createdAt": 0,
            "status": "failed",
            "nativeProvenanceUnknown": true,
            "nativePrincipals": [],
            "nativeAuthorityNodes": [],
        });
        s.store
            .transaction(move |db| db.add_run_fixture(&record, None))
            .await
            .unwrap();
    }
    let mut record = s.conversation_boundary(&chat).await.unwrap();
    record["runId"] = chat_run.clone().into();
    let updated_chat = s.store.save_conversation_boundary(record).await.unwrap();
    let chat_revision = updated_chat["revision"].as_str().unwrap().to_owned();
    let (_run_chat, run) = s
        .chat_run_fixture(&reviewer, None, "Review the changes")
        .await
        .unwrap();
    let run = run["id"].as_str().unwrap().to_owned();
    let unknown = cairn_installation::config::id();
    let routes = [
        (
            "POST",
            format!("/api/chats/{chat}/side-questions"),
            json!({}),
        ),
        (
            "PUT",
            format!("/api/chats/{chat}/messages/{unknown}"),
            json!({
                "text": "Steer",
            }),
        ),
        (
            "POST",
            format!("/api/chats/{chat}/questions/{unknown}/answer"),
            json!({}),
        ),
        (
            "POST",
            format!("/api/chats/{chat}/questions/{unknown}/cancel"),
            json!({}),
        ),
        (
            "POST",
            format!("/api/chats/{chat}/new-session"),
            json!({ "confirm": true }),
        ),
        ("POST", format!("/api/chats/{chat}/compact"), json!({})),
        (
            "POST",
            format!("/api/chats/{chat}/coding-agent"),
            json!({ "provider": "claude" }),
        ),
        ("POST", format!("/api/chats/{chat}/resume"), json!({})),
        ("POST", format!("/api/runs/{run}/resume"), json!({})),
        ("POST", format!("/api/runs/{run}/cleanup"), json!({})),
        ("POST", format!("/api/runs/{chat_run}/retry"), json!({})),
        ("POST", format!("/api/runs/{earlier_run}/retry"), json!({})),
        ("POST", format!("/api/chats/{chat}/restore"), json!({})),
        (
            "PUT",
            format!("/api/chats/{chat}/attachments/{unknown}"),
            json!({}),
        ),
    ];

    // Each route reaches its own handler, whatever it then decides, only with access.
    for (method, path, body) in &routes {
        let (status, refused) = member(&s, method, path, body.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {refused}");
    }

    // Renaming conversation metadata does not direct the agent, so every member can do it.
    let rename_path = format!("/api/chats/{chat}");
    let (status, renamed) = member(
        &s,
        "PUT",
        &rename_path,
        json!({
            "revision": chat_revision,
            "title": "Renamed",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["title"], "Renamed");

    // Stopping and pausing stay open to everyone.
    let reductions = [
        ("POST", format!("/api/runs/{run}/cancel"), json!({})),
        (
            "POST",
            format!("/api/chats/{chat}/pause"),
            json!({ "paused": true }),
        ),
    ];
    for (method, path, body) in &reductions {
        let (status, reduced) = member(&s, method, path, body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{method} {path}: {reduced}");
    }
    assert_eq!(owner(&s, "PUT", &grant, json!({})).await.0, StatusCode::OK);
    for (method, path, body) in &routes {
        let (status, _) = member(&s, method, path, body.clone()).await;
        assert_ne!(status, StatusCode::FORBIDDEN, "{method} {path}");
    }
}

#[tokio::test]
async fn access_belongs_to_one_membership_and_a_new_invitation_does_not_restore_it() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let reviewer = agent(&s, "Reviewer").await;
    relay_fixture::add_member(&s, MEMBER, "membership-1").await;
    let path = format!("/api/agents/{reviewer}/accounts/{MEMBER}");
    let (status, granted) = owner(&s, "PUT", &path, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(granted["accounts"][0]["accessId"], "membership-1");
    assert_eq!(usable(&s).await, json!([reviewer]));

    relay_fixture::remove_member(&s, MEMBER).await;
    relay_fixture::add_member(&s, MEMBER, "membership-2").await;
    s.synchronize_task_authors().await.unwrap();
    assert_eq!(usable(&s).await, json!([]));
    let listing = format!("/api/agents/{reviewer}/accounts");
    assert!(accounts(&owner(&s, "GET", &listing, Value::Null).await.1).is_empty());

    // Deleting an agent removes its entries; they never pass to another agent.
    assert_eq!(owner(&s, "PUT", &path, json!({})).await.0, StatusCode::OK);
    let (status, _) = owner(
        &s,
        "DELETE",
        &format!("/api/agents/{reviewer}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(usable(&s).await, json!([]));
}

#[tokio::test]
async fn an_entry_from_an_ended_membership_is_dropped_before_it_allows_work() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let reviewer = agent(&s, "Reviewer").await;
    relay_fixture::allow_member(&s, MEMBER, &reviewer).await;

    // Warm the membership cache with an admitted request from the original membership.
    let (status, chat) = member(&s, "POST", "/api/chats", json!({ "agentId": reviewer })).await;
    assert_eq!(status, StatusCode::OK, "{chat}");

    // Removed and invited again while the installation missed both reconciliations.
    relay_fixture::remove_member(&s, MEMBER).await;
    relay_fixture::add_member(&s, MEMBER, "membership-2").await;
    let (status, _) = member(&s, "POST", "/api/chats", json!({ "agentId": reviewer })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(usable(&s).await, json!([]));
}

#[tokio::test]
async fn merging_an_agent_never_passes_its_account_access_to_the_receiving_agent() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let source = agent(&s, "Source").await;
    let destination = agent(&s, "Destination").await;
    relay_fixture::allow_member(&s, MEMBER, &source).await;

    let (status, chat) = member(&s, "POST", "/api/chats", json!({ "agentId": source })).await;
    assert_eq!(status, StatusCode::OK, "{chat}");
    let chat_id = chat["id"].as_str().unwrap();

    s.merge_agents_fixture(json!({
        "sourceId": source,
        "targetId": destination,
        "confirm": true,
    }))
    .await
    .unwrap();
    assert_eq!(usable(&s).await, json!([]));
    let moved = s.conversation_boundary(chat_id).await.unwrap();
    assert_eq!(moved["agentId"], destination);
    let (status, _) = member(
        &s,
        "POST",
        &format!("/api/chats/{chat_id}/messages"),
        json!({
            "id": cairn_installation::config::id(),
            "text": "Still forbidden",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_delegated_task_follows_its_requesting_conversation_until_it_is_deleted() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let reviewer = agent(&s, "Reviewer").await;
    relay_fixture::allow_member(&s, MEMBER, &reviewer).await;

    let (_, requesting) = owner(&s, "POST", "/api/chats", json!({ "agentId": reviewer })).await;
    let (_, delegated) = owner(
        &s,
        "POST",
        "/api/chats",
        json!({ "agentId": MAIN_AGENT_ID }),
    )
    .await;
    let delegated_run = cairn_installation::config::id();
    let run = json!({
        "id": delegated_run,
        "taskId": delegated["id"],
        "createdAt": 0,
        "status": "failed",
        "snapshot": { "agent": { "id": MAIN_AGENT_ID } },
    });
    s.store
        .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
        .await
        .unwrap();
    let mut record = s
        .conversation_boundary(delegated["id"].as_str().unwrap())
        .await
        .unwrap();
    record["runId"] = delegated_run.clone().into();
    record["requestedBy"] = json!({
        "runId": cairn_installation::config::id(),
        "chatId": requesting["id"],
    });
    s.store.save_conversation_boundary(record).await.unwrap();

    let messages = format!("/api/chats/{}/messages", delegated["id"].as_str().unwrap());
    let (status, sent) = member(
        &s,
        "POST",
        &messages,
        json!({
            "id": cairn_installation::config::id(),
            "text": "Continue",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");

    let cleanup = format!("/api/runs/{delegated_run}/cleanup");
    let (status, _) = member(&s, "POST", &cleanup, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Deleting it, or restoring it once deleted, still needs its own agent.
    let detail = format!("/api/chats/{}", delegated["id"].as_str().unwrap());
    let (status, _) = member(&s, "DELETE", &detail, json!({ "confirm": true })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, deleted) = owner(&s, "DELETE", &detail, json!({ "confirm": true })).await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    let restore = format!("{detail}/restore");
    let (status, _) = member(&s, "POST", &restore, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, restored) = owner(&s, "POST", &restore, json!({})).await;
    assert_ne!(status, StatusCode::FORBIDDEN, "{restored}");

    let requester = format!("/api/chats/{}", requesting["id"].as_str().unwrap());
    let (status, _) = owner(&s, "DELETE", &requester, json!({ "confirm": true })).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = member(
        &s,
        "POST",
        &messages,
        json!({
            "id": cairn_installation::config::id(),
            "text": "Again",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_first_authoritative_snapshot_includes_current_and_reinvited_members() {
    let root = tempfile::tempdir().unwrap();
    let existing = {
        let s = service(root.path()).await;
        relay_fixture::add_member(&s, MEMBER, "membership-1").await;
        relay_fixture::add_member(&s, "reinvited-member", "membership-old").await;
        agent(&s, "Existing").await
    };

    // Simulate a pending rollout state before the first authoritative membership snapshot.
    let db = rusqlite::Connection::open(root.path().join("data/manager.db")).unwrap();
    db.execute("DELETE FROM agent_account_access", []).unwrap();
    db.execute(
        "INSERT OR REPLACE INTO kv(key,data) VALUES('agent-access-rollout',?)",
        [json!({ "agentIds": [existing, MAIN_AGENT_ID] }).to_string()],
    )
    .unwrap();
    drop(db);

    let s = Service::new(common::config(root.path())).await.unwrap();
    let rollout = s.store.kv("agent-access-rollout").await.unwrap().unwrap();
    assert!(
        rollout["agentIds"]
            .as_array()
            .unwrap()
            .contains(&json!(existing))
    );
    relay_fixture::authority_unavailable(&s, true).await;
    relay_fixture::remove_member(&s, "reinvited-member").await;
    relay_fixture::add_member(&s, "reinvited-member", "membership-again").await;
    relay_fixture::add_member(&s, "new-member", "membership-new").await;
    relay_fixture::add_member(&s, "boundary-member", "membership-boundary").await;

    // Do not observe until a complete current membership snapshot is available.
    tokio::fs::remove_file(s.config.data_dir.join("installation-relay/identity.json"))
        .await
        .unwrap();
    relay_fixture::claimed(&s).await.unwrap();
    assert!(s.synchronize_task_authors().await.is_err());
    assert_eq!(
        s.store.kv("agent-access-rollout").await.unwrap(),
        Some(rollout)
    );

    relay_fixture::authority_unavailable(&s, false).await;
    s.synchronize_task_authors().await.unwrap();
    let mut expected = vec![json!(existing), json!(MAIN_AGENT_ID)];
    expected.sort_by_key(|id| id.as_str().unwrap().to_owned());
    for account in [MEMBER, "reinvited-member", "new-member", "boundary-member"] {
        let mut actual = usable_for(&s, account).await.as_array().unwrap().clone();
        actual.sort_by_key(|id| id.as_str().unwrap().to_owned());
        assert_eq!(actual, expected, "{account}");
    }
    assert!(s.store.kv("agent-access-rollout").await.unwrap().is_none());

    // A later member and agent are outside the one-time snapshot.
    let later = agent(&s, "Later").await;
    relay_fixture::add_member(&s, "later-member", "membership-later").await;
    s.synchronize_task_authors().await.unwrap();
    assert_eq!(usable_for(&s, "later-member").await, json!([]));
    assert!(!usable(&s).await.as_array().unwrap().contains(&json!(later)));
}

#[tokio::test]
async fn direct_write_completes_delayed_rollout_without_overriding_owner_revocation() {
    let root = tempfile::tempdir().unwrap();
    let existing = {
        let s = service(root.path()).await;
        relay_fixture::add_member(&s, MEMBER, "membership-1").await;
        relay_fixture::add_member(&s, "reinvited-member", "membership-old").await;
        agent(&s, "Existing").await
    };
    let db = rusqlite::Connection::open(root.path().join("data/manager.db")).unwrap();
    db.execute("DELETE FROM agent_account_access", []).unwrap();
    db.execute(
        "INSERT OR REPLACE INTO kv(key,data) VALUES('agent-access-rollout',?)",
        [json!({ "agentIds": [existing, MAIN_AGENT_ID] }).to_string()],
    )
    .unwrap();
    drop(db);

    let s = Service::new(common::config(root.path())).await.unwrap();
    relay_fixture::authority_unavailable(&s, true).await;
    tokio::fs::remove_file(s.config.data_dir.join("installation-relay/identity.json"))
        .await
        .unwrap();
    relay_fixture::claimed(&s).await.unwrap();

    let rollout = s.store.kv("agent-access-rollout").await.unwrap().unwrap();
    let start = json!({ "agentId": existing });
    let (status, _) = member(&s, "POST", "/api/chats", start.clone()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        s.store.kv("agent-access-rollout").await.unwrap(),
        Some(rollout)
    );

    // Revoking before the snapshot remains effective even when no access row exists.
    let excluded = format!("/api/agents/{MAIN_AGENT_ID}/accounts/{MEMBER}");
    assert_eq!(
        owner(&s, "DELETE", &excluded, Value::Null).await.0,
        StatusCode::OK
    );

    relay_fixture::authority_unavailable(&s, false).await;
    relay_fixture::remove_member(&s, "reinvited-member").await;
    relay_fixture::add_member(&s, "reinvited-member", "membership-again").await;
    relay_fixture::add_member(&s, "new-member", "membership-new").await;
    let (status, chat) = member(&s, "POST", "/api/chats", start).await;
    assert_eq!(status, StatusCode::OK, "{chat}");
    assert!(s.store.kv("agent-access-rollout").await.unwrap().is_none());

    assert_eq!(usable(&s).await, json!([existing]));
    for account in ["reinvited-member", "new-member"] {
        let mut actual = usable_for(&s, account).await.as_array().unwrap().clone();
        actual.sort_by_key(|id| id.as_str().unwrap().to_owned());
        let mut expected = vec![json!(existing), json!(MAIN_AGENT_ID)];
        expected.sort_by_key(|id| id.as_str().unwrap().to_owned());
        assert_eq!(actual, expected, "{account}");
    }
}

#[tokio::test]
async fn a_pending_rollout_ignores_legacy_timestamp_cutoffs() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    relay_fixture::add_member(&s, MEMBER, "membership-1").await;
    s.store
        .set(
            "agent-access-rollout",
            json!({
                "agentIds": [MAIN_AGENT_ID],
                "upgradedAt": 0,
            }),
            None,
        )
        .await
        .unwrap();

    s.synchronize_task_authors().await.unwrap();
    assert_eq!(usable(&s).await, json!([MAIN_AGENT_ID]));
    assert!(s.store.kv("agent-access-rollout").await.unwrap().is_none());
}

async fn tool(app: &axum::Router, endpoint: &str, bearer: &str, name: &str, input: Value) -> Value {
    let request = common::request("POST", endpoint)
        .header("authorization", format!("Bearer {bearer}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": common::mcp_arguments(name, &input),
            })
            .to_string(),
        ))
        .unwrap();
    common::read_json(common::send(app, request).await).await
}

async fn installation_token(s: &Arc<Service>) -> String {
    let manager = s
        .agent_fixture(
            json!({ "name": "Manager", "access": { "installation": true } }),
            None,
        )
        .await
        .unwrap();
    let (_chat, run) = s
        .chat_run_fixture(manager["id"].as_str().unwrap(), None, "Manage")
        .await
        .unwrap();
    s.store
        .change_run(
            run["id"].as_str().unwrap(),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let run = s.store.run(run["id"].as_str().unwrap()).await.unwrap();
    let config = s.mcps.run_configuration(s, &run).await.unwrap();
    config["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn the_public_mcp_and_agent_tools_read_but_never_give_account_access() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let reviewer = agent(&s, "Reviewer").await;
    relay_fixture::allow_member(&s, MEMBER, &reviewer).await;
    let app = relay_fixture::router(s.clone()).await.unwrap();
    let token = installation_token(&s).await;
    let public = relay_fixture::grant(&s, &["read", "manage"]).await;

    for (endpoint, token) in [("/mcp", &public), ("/mcp-installation", &token)] {
        let listed = tool(
            &app,
            endpoint,
            token,
            "list_agent_accounts",
            json!({ "agentId": reviewer }),
        )
        .await;
        let output = &listed["result"]["structuredContent"]["result"];
        assert_eq!(accounts(output), [MEMBER], "{listed}");

        for name in ["grant_agent_account", "revoke_agent_account"] {
            let input = json!({
                "agentId": reviewer,
                "accountId": "another-member",
            });
            let refused = tool(&app, endpoint, token, name, input).await;
            assert!(
                refused["result"]["isError"] == true || !refused["error"].is_null(),
                "{refused}"
            );
        }
    }
    let listing = format!("/api/agents/{reviewer}/accounts");
    assert_eq!(
        accounts(&owner(&s, "GET", &listing, Value::Null).await.1),
        [MEMBER]
    );
}
