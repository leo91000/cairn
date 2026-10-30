mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::{MAIN_AGENT_ID, id, now},
    service::Service,
};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};

async fn pending_question(s: &Arc<Service>) -> (String, String) {
    std::fs::create_dir_all(&s.config.home).unwrap();
    let chat = s
        .chat_create(json!({ "agentId": MAIN_AGENT_ID }))
        .await
        .unwrap();
    let run = id();
    common::add_run(
        &s.store,
        &json!({
            "id": run, "taskId": chat["id"], "status": "running", "createdAt": now(),
        }),
    )
    .await;
    let mut chat = s
        .conversation_boundary(chat["id"].as_str().unwrap())
        .await
        .unwrap();
    chat["runId"] = run.clone().into();
    s.store
        .save_conversation_boundary(chat.clone())
        .await
        .unwrap();
    let question = "a".repeat(64);
    s.question_receive(
        &run,
        json!({
            "id": question, "blocking": true, "fields": [{ "id": "answer", "title": "Which?" }],
        }),
    )
    .await
    .unwrap();
    (chat["id"].as_str().unwrap().to_owned(), question)
}

async fn recipients(
    s: &Arc<Service>,
    identity: InstallationIdentity,
    chat: &str,
    question: &str,
) -> (StatusCode, Value) {
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let request = common::request(
        "GET",
        &format!("/api/notification-recipients/{chat}/{question}"),
    )
    .extension(identity)
    .body(Body::empty())
    .unwrap();
    let response = common::send(&app, request).await;
    (response.status(), common::read_json(response).await)
}

#[tokio::test]
async fn question_recipients_require_maintenance_authority_and_current_membership_bound_grants() {
    let root = tempfile::tempdir().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    common::relay_fixture::allow_member(&s, "member", MAIN_AGENT_ID).await;
    let (chat, question) = pending_question(&s).await;
    for identity in [
        InstallationIdentity::trusted(InstallationRole::Owner, "fixture-owner"),
        InstallationIdentity::trusted(InstallationRole::Member, "member"),
    ] {
        assert_eq!(
            recipients(&s, identity, &chat, &question).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let mut tool = InstallationIdentity::trusted(InstallationRole::Owner, "");
    tool.mcp_scopes = Some(vec!["manage".into()]);
    assert_eq!(
        recipients(&s, tool, &chat, &question).await.0,
        StatusCode::FORBIDDEN
    );
    let maintenance = InstallationIdentity::trusted(InstallationRole::Owner, "");
    let (status, current) = recipients(&s, maintenance.clone(), &chat, &question).await;
    assert_eq!(status, StatusCode::OK, "{current}");
    assert_eq!(
        current,
        json!({ "members": [{ "account_id": "member", "access_id": "membership-member" }] })
    );

    // An event already in the outbox must not preserve the owner's revoked grant.
    s.store
        .transaction(|db| {
            db.0.execute(
                "DELETE FROM agent_account_access WHERE account_id='member'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (_, revoked) = recipients(&s, maintenance, &chat, &question).await;
    assert_eq!(revoked, json!({ "members": [] }));
}

#[tokio::test]
async fn old_outbox_snapshots_cannot_authorize_legacy_member_pushes() {
    let root = tempfile::tempdir().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    let (chat, question) = pending_question(&s).await;
    let key = format!("push-outbox:{question}");
    let mut old = s.store.kv(&key).await.unwrap().unwrap();
    old["members"] = json!(["member"]);
    s.store.set(&key, old, None).await.unwrap();
    let pending = cairn_installation::notifications::pending(&s, HashSet::new(), String::new(), 4)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].members, Some(Vec::new()));
    assert_eq!(pending[0].chat_id, chat);
}
