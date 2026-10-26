use common::AgentFixture;

mod common;

use axum::{body::Body, http::StatusCode};
use cairn_contracts::{CreateProjectInput, ProjectId, ReplaceProjectInput};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::MAIN_AGENT_ID,
    service::Service,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;

async fn http(
    s: &Arc<Service>,
    role: InstallationRole,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let request = common::request(method, path)
        .extension(InstallationIdentity::trusted(role, "owner"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = common::send(&app, request).await;
    (response.status(), common::read_json(response).await)
}

#[tokio::test]
async fn project_http_replacements_are_complete_revision_checked_and_owner_only() {
    let root = TempDir::new().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    let settings = json!({ "name": "  Project  ", "path": root.path() });
    let input = json!({ "creationId": cairn_installation::config::id(), "settings": settings });
    let (status, first) = http(
        &s,
        InstallationRole::Owner,
        "POST",
        "/api/projects",
        input.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["name"], "Project");
    assert_eq!(first["sourceMode"], "remote");
    assert_eq!(first["baseBranch"], "main");
    assert!(first["revision"].is_string());
    let id = first["id"].as_str().unwrap();
    let path = format!("/api/projects/{id}");
    let replacement = json!({ "projectId": id, "revision": first["revision"], "settings": { "name": "Updated", "path": root.path(), "sourceMode": "local" } });
    let (left, right) = tokio::join!(
        http(
            &s,
            InstallationRole::Owner,
            "PUT",
            &path,
            replacement.clone()
        ),
        http(
            &s,
            InstallationRole::Owner,
            "PUT",
            &path,
            replacement.clone()
        ),
    );
    assert!(matches!(
        (left.0, right.0),
        (StatusCode::OK, StatusCode::CONFLICT) | (StatusCode::CONFLICT, StatusCode::OK)
    ));
    let saved = if left.0 == StatusCode::OK {
        left.1
    } else {
        right.1
    };
    assert_ne!(saved["revision"], first["revision"]);
    assert_eq!(saved["createdAt"], first["createdAt"]);
    assert_eq!(
        http(&s, InstallationRole::Owner, "POST", "/api/projects", input)
            .await
            .1,
        saved
    );
    assert_eq!(
        http(&s, InstallationRole::Member, "GET", &path, Value::Null)
            .await
            .1,
        saved
    );
    for method in ["POST", "PUT", "DELETE"] {
        let route = if method == "POST" {
            "/api/projects"
        } else {
            &path
        };
        assert_eq!(
            http(
                &s,
                InstallationRole::Member,
                method,
                route,
                replacement.clone()
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let mut wrong = replacement.clone();
    wrong["projectId"] = cairn_installation::config::id().into();
    assert_eq!(
        http(&s, InstallationRole::Owner, "PUT", &path, wrong)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        http(
            &s,
            InstallationRole::Owner,
            "PUT",
            &path,
            json!({ "settings": settings })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        http(
            &s,
            InstallationRole::Owner,
            "GET",
            "/api/projects/not-an-id",
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let reopened = Service::new(common::config(root.path())).await.unwrap();
    assert_eq!(
        http(
            &reopened,
            InstallationRole::Owner,
            "GET",
            &path,
            Value::Null
        )
        .await
        .1,
        saved
    );
    let count: i64 = reopened
        .store
        .read(|db| {
            Ok(db.0.query_row(
                "SELECT count(*) FROM records WHERE kind='projects'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        http(&s, InstallationRole::Owner, "DELETE", &path, Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        http(&s, InstallationRole::Owner, "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn project_deletion_keeps_current_reference_rules_and_historical_snapshots() {
    let root = TempDir::new().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    let input: CreateProjectInput =
        serde_json::from_value(json!({ "settings": { "name": "Project", "path": root.path() } }))
            .unwrap();
    let project = s.create_project(input, None, |_| Ok(())).await.unwrap();
    let id = project.id.to_string();
    let agent = s
        .agent_fixture(
            json!({ "name": "Assigned", "access": { "projects": [id] } }),
            None,
        )
        .await
        .unwrap();
    assert!(s.delete_project(project.id, |_| Ok(())).await.is_err());
    assert_eq!(
        http(
            &s,
            InstallationRole::Owner,
            "DELETE",
            &format!("/api/agents/{}", agent["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    let task = s.task(json!({ "name": "Uses project", "prompt": "Work", "agentId": MAIN_AGENT_ID, "projectId": id }), None).await.unwrap();
    assert!(s.delete_project(project.id, |_| Ok(())).await.is_err());
    s.remove("tasks", task["id"].as_str().unwrap())
        .await
        .unwrap();
    let run = json!({ "id": cairn_installation::config::id(), "taskId": "snapshot", "status": "running", "createdAt": 0, "snapshot": { "projects": [project] } });
    common::add_run(&s.store, &run).await;
    assert!(s.delete_project(project.id, |_| Ok(())).await.is_err());
    s.store
        .patch_run(
            run["id"].as_str().unwrap(),
            json!({ "status": "succeeded" }),
        )
        .await
        .unwrap();
    s.delete_project(project.id, |_| Ok(())).await.unwrap();
    assert_eq!(
        s.store.run(run["id"].as_str().unwrap()).await.unwrap()["snapshot"]["projects"][0]["id"],
        id
    );
}

#[tokio::test]
async fn preparation_does_not_recreate_deleted_projects_or_publish_after_denial() {
    let root = TempDir::new().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    let settings =
        serde_json::from_value(json!({ "name": "Project", "path": root.path() })).unwrap();
    let input = CreateProjectInput {
        creation_id: None,
        settings,
    };
    let denied = s
        .create_project(input.clone(), None, |_| {
            Err(cairn_installation::error::Error::forbidden("Revoked"))
        })
        .await;
    assert_eq!(denied.unwrap_err().status, 403);
    assert!(s.store.projects().await.unwrap().is_empty());
    let project = s
        .create_project(input.clone(), None, |_| Ok(()))
        .await
        .unwrap();
    let project_id = project.id;
    let replacement = ReplaceProjectInput {
        project_id,
        revision: project.revision,
        settings: input.settings,
    };
    s.delete_project(project_id, |_| Ok(())).await.unwrap();
    assert_eq!(
        s.replace_project(replacement, |_| Ok(()))
            .await
            .unwrap_err()
            .status,
        404
    );
    assert!(s.store.projects().await.unwrap().is_empty());
    let missing = ProjectId(uuid::Uuid::new_v4());
    assert_eq!(
        s.delete_project(missing, |_| Ok(()))
            .await
            .unwrap_err()
            .status,
        404
    );
}
