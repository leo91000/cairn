//! Account access to agents at the API boundary (ADR-0075, « Accès des comptes aux agents »).
mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::MAIN_AGENT_ID,
    service::Service,
};
use common::{AgentFixture, relay_fixture, run_fixture::DbRunFixture};
use serde_json::{Value, json};
use std::sync::Arc;

const MEMBER: &str = "member-account";

const SUSPENDED: &str =
    "Its author no longer has access to this agent. It resumes once the owner gives access again.";

async fn call(
    s: &Arc<Service>,
    account: Option<&str>,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let app = relay_fixture::router(s.clone()).await.unwrap();
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
    let (status, usable) = member(s, "GET", "/api/usable-agents", Value::Null).await;
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
        json!({ "id": cairn_installation::config::id(), "text": "Review it" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");

    let (status, revoked) = owner(&s, "DELETE", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    assert!(accounts(&revoked).is_empty());

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
        json!({ "id": cairn_installation::config::id(), "text": "Again" }),
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

    let (_, chat) = owner(&s, "POST", "/api/chats", json!({ "agentId": reviewer })).await;
    let chat = chat["id"].as_str().unwrap().to_owned();
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
    s.store.save_conversation_boundary(record).await.unwrap();
    let settings = json!({ "name": "Review", "prompt": "Review the changes", "agentId": reviewer });
    let (_, task) = owner(&s, "POST", "/api/tasks", json!({ "settings": settings })).await;
    let run = s
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    let run = run["id"].as_str().unwrap().to_owned();
    let unknown = cairn_installation::config::id();
    let routes = [
        (
            "PUT",
            format!("/api/chats/{chat}/messages/{unknown}"),
            json!({ "text": "Steer" }),
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
        ("POST", format!("/api/runs/{run}/resume"), json!({})),
        ("POST", format!("/api/runs/{run}/chat"), json!({})),
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

    // Stopping, pausing and archiving stay open to everyone.
    let task_id = task["id"].as_str().unwrap();
    let reductions = [
        ("POST", format!("/api/runs/{run}/cancel"), json!({})),
        (
            "POST",
            format!("/api/chats/{chat}/pause"),
            json!({ "paused": true }),
        ),
        (
            "POST",
            format!("/api/tasks/{task_id}/state"),
            json!({ "taskId": task_id, "revision": task["revision"], "enabled": false, "archived": true }),
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
    assert_eq!(owner(&s, "PUT", &path, json!({})).await.0, StatusCode::OK);
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
async fn members_manage_tasks_only_on_agents_they_can_use_and_lost_access_suspends_them() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let reviewer = agent(&s, "Reviewer").await;
    relay_fixture::add_member(&s, MEMBER, "membership-1").await;
    let grant = format!("/api/agents/{reviewer}/accounts/{MEMBER}");

    let settings = json!({
        "name": "Nightly review",
        "prompt": "Review the changes",
        "agentId": reviewer,
        "cron": "0 3 * * *",
    });
    let (status, _) = member(&s, "POST", "/api/tasks", json!({ "settings": settings })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let mut unnamed = settings.clone();
    unnamed.as_object_mut().unwrap().remove("agentId");
    let (status, _) = member(&s, "POST", "/api/tasks", json!({ "settings": unnamed })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(owner(&s, "PUT", &grant, json!({})).await.0, StatusCode::OK);
    let (status, task) = member(&s, "POST", "/api/tasks", json!({ "settings": settings })).await;
    assert_eq!(status, StatusCode::OK, "{task}");
    assert!(task["scheduleWaitReason"].is_null());
    let id = task["id"].as_str().unwrap().to_owned();

    // A member moves a task only between agents they can use.
    let mut moved = settings.clone();
    moved["agentId"] = MAIN_AGENT_ID.into();
    let replace = json!({ "taskId": id, "revision": task["revision"], "settings": moved });
    let (status, _) = member(&s, "PUT", &format!("/api/tasks/{id}"), replace).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    assert_eq!(
        owner(&s, "DELETE", &grant, Value::Null).await.0,
        StatusCode::OK
    );
    let task = s.store.mission(&id).await.unwrap().unwrap();
    assert_eq!(task.schedule_wait_reason.as_deref(), Some(SUSPENDED));

    // The occurrence passes without a run or a failure while the author lacks access.
    let due = id.clone();
    s.store
        .transaction(move |db| {
            db.0.execute("UPDATE missions SET next_run=1 WHERE id=?", [&due])?;
            Ok(())
        })
        .await
        .unwrap();
    s.schedule().await.unwrap();
    let task = s.store.mission(&id).await.unwrap().unwrap();
    assert!(task.next_run.unwrap() > 1);
    assert!(task.schedule_failure.is_none());
    assert_eq!(task.schedule_wait_reason.as_deref(), Some(SUSPENDED));
    let (_, runs) = owner(&s, "GET", &format!("/api/runs?taskId={id}"), Value::Null).await;
    assert_eq!(runs, json!([]));

    // Pausing stays open to everyone; resuming, launching and editing need access.
    let state = format!("/api/tasks/{id}/state");
    let paused =
        json!({ "taskId": id, "revision": task.revision, "enabled": false, "archived": false });
    let (status, paused) = member(&s, "POST", &state, paused).await;
    assert_eq!(status, StatusCode::OK, "{paused}");
    assert_eq!(paused["scheduleWaitReason"], SUSPENDED);
    let resumed =
        json!({ "taskId": id, "revision": paused["revision"], "enabled": true, "archived": false });
    assert_eq!(
        member(&s, "POST", &state, resumed.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    let run = format!("/api/tasks/{id}/run");
    assert_eq!(
        member(&s, "POST", &run, json!({})).await.0,
        StatusCode::FORBIDDEN
    );

    // The returned access lifts the suspension by itself, and the next occurrence runs.
    assert_eq!(owner(&s, "PUT", &grant, json!({})).await.0, StatusCode::OK);
    let (status, resumed) = member(&s, "POST", &state, resumed).await;
    assert_eq!(status, StatusCode::OK, "{resumed}");
    assert!(resumed["scheduleWaitReason"].is_null());
    let due = id.clone();
    s.store
        .transaction(move |db| {
            db.0.execute("UPDATE missions SET next_run=1 WHERE id=?", [&due])?;
            Ok(())
        })
        .await
        .unwrap();
    s.schedule().await.unwrap();
    let (_, runs) = owner(&s, "GET", &format!("/api/runs?taskId={id}"), Value::Null).await;
    assert_eq!(runs.as_array().unwrap().len(), 1, "{runs}");
}

#[tokio::test]
async fn retries_follow_the_tasks_current_agent_and_authors_can_move_a_suspended_task() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let reviewer = agent(&s, "Reviewer").await;
    let writer = agent(&s, "Writer").await;
    relay_fixture::allow_member(&s, MEMBER, &reviewer).await;

    let settings = json!({
        "name": "Nightly review",
        "prompt": "Review the changes",
        "agentId": reviewer,
        "cron": "0 3 * * *",
    });
    let (_, task) = member(&s, "POST", "/api/tasks", json!({ "settings": settings })).await;
    let id = task["id"].as_str().unwrap().to_owned();
    let run = s.enqueue(&id, "manual", None).await.unwrap();
    let retry = format!("/api/runs/{}/retry", run["id"].as_str().unwrap());

    // The owner moves the task: a retry would now start it on Main.
    let mut moved = settings.clone();
    moved["agentId"] = MAIN_AGENT_ID.into();
    let path = format!("/api/tasks/{id}");
    let replace = json!({ "taskId": id, "revision": task["revision"], "settings": moved });
    let (status, owned) = owner(&s, "PUT", &path, replace).await;
    assert_eq!(status, StatusCode::OK, "{owned}");
    assert_eq!(owned["scheduleWaitReason"], SUSPENDED);
    assert_eq!(
        member(&s, "POST", &retry, json!({})).await.0,
        StatusCode::FORBIDDEN
    );

    // Pausing it with the full settings stays open, like the state command.
    let mut paused = moved.clone();
    paused["enabled"] = false.into();
    let replace = json!({ "taskId": id, "revision": owned["revision"], "settings": paused });
    let (status, owned) = member(&s, "PUT", &path, replace).await;
    assert_eq!(status, StatusCode::OK, "{owned}");
    let mut edited = moved.clone();
    edited["prompt"] = "Something else".into();
    let replace = json!({ "taskId": id, "revision": owned["revision"], "settings": edited });
    assert_eq!(
        member(&s, "PUT", &path, replace).await.0,
        StatusCode::FORBIDDEN
    );

    // Like a rule, its author moves it to an agent they may use.
    relay_fixture::allow_member(&s, MEMBER, &writer).await;
    let mut usable = settings.clone();
    usable["agentId"] = writer.clone().into();
    let replace = json!({ "taskId": id, "revision": owned["revision"], "settings": usable });
    let (status, moved) = member(&s, "PUT", &path, replace).await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert!(moved["scheduleWaitReason"].is_null());
}

#[tokio::test]
async fn an_entry_from_an_ended_membership_is_dropped_before_it_allows_work() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let reviewer = agent(&s, "Reviewer").await;
    relay_fixture::allow_member(&s, MEMBER, &reviewer).await;

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

    let settings = json!({
        "name": "Nightly review",
        "prompt": "Review the changes",
        "agentId": source,
        "cron": "0 3 * * *",
    });
    let (status, task) = member(&s, "POST", "/api/tasks", json!({ "settings": settings })).await;
    assert_eq!(status, StatusCode::OK, "{task}");

    s.merge_agents_fixture(json!({ "sourceId": source, "targetId": destination, "confirm": true }))
        .await
        .unwrap();
    assert_eq!(usable(&s).await, json!([]));
    let moved = s
        .store
        .mission(task["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved.settings.agent_id.to_string(), destination);
    assert_eq!(moved.schedule_wait_reason.as_deref(), Some(SUSPENDED));
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
    let mut record = s
        .conversation_boundary(delegated["id"].as_str().unwrap())
        .await
        .unwrap();
    record["requestedBy"] =
        json!({ "runId": cairn_installation::config::id(), "chatId": requesting["id"] });
    s.store.save_conversation_boundary(record).await.unwrap();

    let messages = format!("/api/chats/{}/messages", delegated["id"].as_str().unwrap());
    let (status, sent) = member(
        &s,
        "POST",
        &messages,
        json!({ "id": cairn_installation::config::id(), "text": "Continue" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");

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
        json!({ "id": cairn_installation::config::id(), "text": "Again" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn existing_members_keep_existing_agents_at_rollout_only() {
    let root = tempfile::tempdir().unwrap();
    let existing = {
        let s = service(root.path()).await;
        agent(&s, "Existing").await
    };

    // Reconstruct a version-18 database: the same tables without account access.
    let db = rusqlite::Connection::open(root.path().join("data/manager.db")).unwrap();
    db.execute_batch("DROP TABLE agent_account_access; PRAGMA user_version=18;")
        .unwrap();
    drop(db);

    let s = Service::new(common::config(root.path())).await.unwrap();
    relay_fixture::add_member(&s, MEMBER, "membership-1").await;
    relay_fixture::claimed(&s).await.unwrap();
    s.synchronize_task_authors().await.unwrap();
    let mut kept = usable(&s).await.as_array().unwrap().clone();
    kept.sort_by_key(|id| id.as_str().unwrap().to_owned());
    let mut expected = vec![json!(existing), json!(MAIN_AGENT_ID)];
    expected.sort_by_key(|id| id.as_str().unwrap().to_owned());
    assert_eq!(kept, expected);

    // Later agents and later members start without access.
    let later = agent(&s, "Later").await;
    relay_fixture::add_member(&s, "later-member", "membership-2").await;
    s.synchronize_task_authors().await.unwrap();
    assert!(!usable(&s).await.as_array().unwrap().contains(&json!(later)));
    let (_, later_usable) = call(
        &s,
        Some("later-member"),
        "GET",
        "/api/usable-agents",
        Value::Null,
    )
    .await;
    assert_eq!(later_usable["agentIds"], json!([]));
}

#[tokio::test]
async fn a_members_scheduled_task_keeps_running_after_the_rollout() {
    let root = tempfile::tempdir().unwrap();
    let task = {
        let s = service(root.path()).await;
        let existing = agent(&s, "Existing").await;
        relay_fixture::allow_member(&s, MEMBER, &existing).await;
        let settings = json!({
            "name": "Nightly review",
            "prompt": "Review the changes",
            "agentId": existing,
            "cron": "0 3 * * *",
        });
        let (status, task) =
            member(&s, "POST", "/api/tasks", json!({ "settings": settings })).await;
        assert_eq!(status, StatusCode::OK, "{task}");
        task["id"].as_str().unwrap().to_owned()
    };

    // The same membership, on a version-18 database without account access.
    let db = rusqlite::Connection::open(root.path().join("data/manager.db")).unwrap();
    db.execute_batch("DROP TABLE agent_account_access; PRAGMA user_version=18;")
        .unwrap();
    drop(db);

    let s = Service::new(common::config(root.path())).await.unwrap();
    relay_fixture::claimed(&s).await.unwrap();
    s.synchronize_task_authors().await.unwrap();
    let mission = s.store.mission(&task).await.unwrap().unwrap();
    assert!(mission.schedule_wait_reason.is_none());
    let due = task.clone();
    s.store
        .transaction(move |db| {
            db.0.execute("UPDATE missions SET next_run=1 WHERE id=?", [&due])?;
            Ok(())
        })
        .await
        .unwrap();
    s.schedule().await.unwrap();
    let (_, runs) = owner(&s, "GET", &format!("/api/runs?taskId={task}"), Value::Null).await;
    assert_eq!(runs.as_array().unwrap().len(), 1, "{runs}");
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
    use common::MissionFixture;
    let manager = s
        .agent_fixture(
            json!({ "name": "Manager", "access": { "installation": true } }),
            None,
        )
        .await
        .unwrap();
    let task = s
        .task_fixture(
            json!({ "name": "Caller", "prompt": "Manage", "agentId": manager["id"] }),
            None,
        )
        .await
        .unwrap();
    let run = s
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
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
            let input = json!({ "agentId": reviewer, "accountId": "another-member" });
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
