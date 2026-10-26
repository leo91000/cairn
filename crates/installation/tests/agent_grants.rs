//! Permissions are edited on typed profiles. Resource lists are projections of
//! the same relations used by enforcement; no resource JSON is authoritative.
use common::MissionFixture;
use common::StoreAgentFixture;

mod common;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use cairn_installation::{
    config::{Config, MAIN_AGENT_ID},
    nodes::LOCAL_NODE_ID,
    plugins::connections,
    service::{Service, covers, policy},
    validation::text,
};
use common::{RelayContext, relay_fixture::router};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tempfile::TempDir;

const NODE: &str = "6f7e1c2a-3b4d-4e5f-8a9b-0c1d2e3f4a5b";
const GHOST: &str = "1f2e3d4c-5b6a-4978-8695-a4b3c2d1e0f9";

struct Owner {
    service: Arc<Service>,
    app: Router,
    session: RelayContext,
    _root: TempDir,
}

async fn start(root: &Path) -> Arc<Service> {
    Service::new(Config {
        ..common::config(root)
    })
    .await
    .unwrap()
}

impl Owner {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let service = start(root.path()).await;
        Self::serve(service, root).await
    }

    async fn serve(service: Arc<Service>, root: TempDir) -> Self {
        let session = RelayContext::new(&common::relay_fixture::context(&service).await);
        Self {
            app: router(service.clone()).await.unwrap(),
            service,
            session,
            _root: root,
        }
    }

    async fn send(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let body = if path == "/api/agents" && method == "POST" {
            json!({ "settings": body, "review": {} })
        } else if path.starts_with("/api/agents/")
            && (method == "PUT" || path.ends_with("/preview"))
        {
            let id = path
                .trim_start_matches("/api/agents/")
                .split('/')
                .next()
                .unwrap();
            let mut settings = body.clone();
            let revision = settings
                .as_object_mut()
                .unwrap()
                .remove("revision")
                .unwrap_or_else(|| json!(""));
            let review = json!({
                "nativeNodeReviews": settings.as_object_mut().unwrap().remove("nativeNodeReviews").unwrap_or_else(|| json!([])),
                "reapproveDelegations": settings.as_object_mut().unwrap().remove("reapproveDelegations").unwrap_or_else(|| json!([])),
            });
            for key in ["id", "createdAt", "avatar"] {
                settings.as_object_mut().unwrap().remove(key);
            }
            json!({ "agentId": id, "revision": revision, "settings": settings, "review": review })
        } else {
            body
        };
        let request = self
            .session
            .authorize(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", common::HOST)
                    .header("content-type", "application/json"),
            )
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&self.app, request).await;
        let status = response.status();
        (status, common::read_json(response).await)
    }

    async fn stored(&self, kind: &str, id: &str) -> Value {
        if kind == "agents" {
            self.service
                .store
                .agent_snapshot(id)
                .await
                .unwrap()
                .unwrap()
        } else {
            self.service.store.get(kind, id).await.unwrap().unwrap()
        }
    }

    async fn add_node(&self) {
        self.service
            .store
            .put(
                "nodes",
                json!({
                    "id": NODE,
                    "name": "gpu-box",
                    "revoked": false,
                    "accepting": true,
                    "tags": [],
                    "limits": { "cpu": 2, "memoryMiB": 4096, "diskMiB": 8192 },
                    "capabilities": { "cpu": 4, "memoryMiB": 8192, "diskMiB": 16384 },
                }),
            )
            .await
            .unwrap();
    }

    async fn add_service_account(&self, agents: Value) -> String {
        let saved = connections::save(
            &self.service,
            "onepassword",
            &json!({
                "name": "Production",
                "enabled": true,
                "secrets": { "token": "ops_fixture" },
                "agentIds": agents,
            }),
            None,
        )
        .await
        .unwrap();
        text(&saved, "id").to_owned()
    }

    async fn create_agent(&self, name: &str) -> Value {
        let (status, agent) = self
            .send(
                "POST",
                "/api/agents",
                json!({ "name": name, "exclusiveNodes": [], "plugins": [], "access": cairn_contracts::AccessPolicy::default() }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{agent}");
        agent
    }
}

fn path(agent: &Value) -> String {
    format!("/api/agents/{}", text(agent, "id"))
}

#[tokio::test]
async fn the_agent_page_grants_exclusive_nodes_and_service_accounts() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let account = owner.add_service_account(json!([])).await;
    let mut agent = owner.create_agent("Release engineer").await;

    // Nothing new is granted by default, and the grants sit beside `access`.
    assert_eq!(agent["exclusiveNodes"], json!([]));
    assert_eq!(agent["plugins"], json!([]));
    assert!(agent["access"].get("exclusiveNodes").is_none());

    agent["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    agent["exclusiveNodes"] = json!([NODE]);
    agent["plugins"] = json!([account]);
    let (status, saved) = owner.send("PUT", &path(&agent), agent.clone()).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let id = text(&saved, "id").to_owned();

    // Enforcement and older releases read the resource copies.
    assert_eq!(
        owner.stored("nodes", NODE).await["exclusiveAgents"],
        json!([id])
    );
    let instance = owner.stored(connections::KIND, &account).await;
    assert_eq!(instance["agentIds"], json!([id]));
    let instance: connections::Connection = serde_json::from_value(instance).unwrap();
    assert!(instance.grants(&id));

    // A replacement cannot silently reset owner grants by omitting them.
    let mut incomplete = saved.clone();
    incomplete.as_object_mut().unwrap().remove("exclusiveNodes");
    incomplete.as_object_mut().unwrap().remove("plugins");
    assert_eq!(
        owner.send("PUT", &path(&saved), incomplete).await.0,
        StatusCode::BAD_REQUEST
    );
    let kept = owner.stored("agents", &id).await;
    assert_eq!(kept["exclusiveNodes"], json!([NODE]));
    assert_eq!(kept["plugins"], json!([account]));

    // Removing both grants removes the copies.
    let mut revoked = kept.clone();
    revoked["exclusiveNodes"] = json!([]);
    revoked["plugins"] = json!([]);
    let (status, _) = owner.send("PUT", &path(&kept), revoked).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        owner.stored("nodes", NODE).await["exclusiveAgents"],
        json!([])
    );
    assert_eq!(
        owner.stored(connections::KIND, &account).await["agentIds"],
        json!([])
    );
}

#[tokio::test]
async fn the_main_agent_can_hold_live_grants_without_wider_resource_access() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let account = owner.add_service_account(json!([])).await;
    let mut main = owner.stored("agents", MAIN_AGENT_ID).await;
    main["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    main["exclusiveNodes"] = json!([NODE]);
    main["plugins"] = json!([account]);
    let (status, saved) = owner.send("PUT", &path(&main), main).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["plugins"], json!([account]));

    // Live grants are not captured by runs: they never block queued work.
    assert!(policy(&saved).get("exclusiveNodes").is_none());
    assert!(policy(&saved).get("plugins").is_none());
    let mut fewer = saved.clone();
    fewer["plugins"] = json!([]);
    assert!(covers(&fewer, &saved));
}

#[tokio::test]
async fn invalid_live_grants_are_refused() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let mut agent = owner.create_agent("Careful").await;
    // Allowed to run everywhere, so exclusive reservations are kept for validation.
    agent["access"]["nodes"] = Value::Null;
    for (key, value) in [
        ("exclusiveNodes", json!([GHOST])),
        ("plugins", json!([GHOST])),
    ] {
        let mut draft = agent.clone();
        draft[key] = value;
        let (status, _) = owner.send("PUT", &path(&agent), draft).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{key}");
    }

    owner
        .send("POST", &format!("/api/nodes/{NODE}/revoke"), json!({}))
        .await;
    agent["exclusiveNodes"] = json!([NODE]);
    let (status, _) = owner.send("PUT", &path(&agent), agent).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_stale_editor_cannot_overwrite_newer_access() {
    let owner = Owner::new().await;
    let agent = owner.create_agent("Shared").await;
    let mut first = agent.clone();
    first["description"] = "First".into();
    assert_eq!(
        owner.send("PUT", &path(&agent), first).await.0,
        StatusCode::OK
    );

    let mut stale = agent.clone();
    stale["description"] = "Stale".into();
    let (status, error) = owner.send("PUT", &path(&agent), stale).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");

    // An empty revision never bypasses concurrency control.
    let mut empty = agent.clone();
    empty["revision"] = "".into();
    assert_eq!(
        owner.send("PUT", &path(&agent), empty).await.0,
        StatusCode::CONFLICT
    );

    // Omitted revisions cannot bypass concurrency control either.
    let mut unversioned = agent.clone();
    unversioned.as_object_mut().unwrap().remove("revision");
    assert_eq!(
        owner.send("PUT", &path(&agent), unversioned).await.0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn resource_configuration_cannot_write_exclusivity_and_keeps_projected_grants() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let agent = owner.create_agent("Android QA").await;
    let id = text(&agent, "id").to_owned();

    // An agent that may not run on the node cannot reserve it.
    let refused = json!({
        "name": "gpu-box",
        "tags": [],
        "limits": { "cpu": 2, "memoryMiB": 4096, "diskMiB": 8192 },
        "accepting": true,
        "exclusiveAgents": [id],
    });
    owner
        .send("PUT", &format!("/api/nodes/{NODE}"), refused)
        .await;
    assert_eq!(
        owner.stored("agents", &id).await["exclusiveNodes"],
        json!([])
    );
    assert_eq!(
        owner.stored("nodes", NODE).await["exclusiveAgents"],
        json!([])
    );
    let mut runs_here = owner.stored("agents", &id).await;
    runs_here["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    let (status, _) = owner
        .send("PUT", &path(&runs_here), runs_here.clone())
        .await;
    assert_eq!(status, StatusCode::OK);

    let configuration = json!({
        "name": "gpu-box",
        "tags": [],
        "limits": { "cpu": 2, "memoryMiB": 4096, "diskMiB": 8192 },
        "accepting": true,
        "exclusiveAgents": [id],
    });
    let (status, node) = owner
        .send("PUT", &format!("/api/nodes/{NODE}"), configuration.clone())
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{node}");
    let mut runs_here = owner.stored("agents", &id).await;
    runs_here["exclusiveNodes"] = json!([NODE]);
    let (status, _) = owner
        .send("PUT", &path(&runs_here), runs_here.clone())
        .await;
    assert_eq!(status, StatusCode::OK);

    // A configuration without the list keeps the grants.
    let mut without = configuration.clone();
    without.as_object_mut().unwrap().remove("exclusiveAgents");
    owner
        .send("PUT", &format!("/api/nodes/{NODE}"), without)
        .await;
    assert_eq!(
        owner.stored("nodes", NODE).await["exclusiveAgents"],
        json!([id])
    );

    let account = owner.add_service_account(json!([id])).await;
    assert_eq!(
        owner.stored("agents", &id).await["plugins"],
        json!([account])
    );

    // Editing the instance without the list keeps its agents.
    connections::save(
        &owner.service,
        "onepassword",
        &json!({ "name": "Production renamed", "enabled": true }),
        Some(&account),
    )
    .await
    .unwrap();
    assert_eq!(
        owner.stored(connections::KIND, &account).await["agentIds"],
        json!([id])
    );

    // Deleting the instance removes it from the agent.
    connections::remove(&owner.service, "onepassword", &account)
        .await
        .unwrap();
    assert_eq!(owner.stored("agents", &id).await["plugins"], json!([]));
}

#[tokio::test]
async fn deleting_an_agent_or_revoking_a_node_removes_its_live_grants() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let mut agent = owner.create_agent("Temporary").await;
    agent["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    agent["exclusiveNodes"] = json!([NODE]);
    let (_, agent) = owner.send("PUT", &path(&agent), agent).await;
    let (status, _) = owner.send("DELETE", &path(&agent), Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        owner.stored("nodes", NODE).await["exclusiveAgents"],
        json!([])
    );

    // Saving the node no longer trips over the deleted agent.
    let node = owner.stored("nodes", NODE).await;
    let configuration = json!({
        "name": node["name"],
        "tags": [],
        "limits": node["limits"],
        "accepting": true,
    });
    let (status, error) = owner
        .send("PUT", &format!("/api/nodes/{NODE}"), configuration)
        .await;
    assert_eq!(status, StatusCode::OK, "{error}");

    let mut kept = owner.create_agent("Kept").await;
    kept["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    kept["exclusiveNodes"] = json!([NODE]);
    let (_, kept) = owner.send("PUT", &path(&kept), kept).await;
    let (status, revoked) = owner
        .send("POST", &format!("/api/nodes/{NODE}/revoke"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    assert_eq!(revoked["exclusiveAgents"], json!([]));
    assert_eq!(
        owner.stored("agents", text(&kept, "id")).await["exclusiveNodes"],
        json!([])
    );
}

#[tokio::test]
async fn preview_counts_work_that_reduced_access_would_stop() {
    let owner = Owner::new().await;
    let agent = owner.create_agent("Busy").await;
    let task = owner
        .service
        .task_fixture(
            json!({ "name": "Nightly", "prompt": "Build", "agentId": agent["id"] }),
            None,
        )
        .await
        .unwrap();
    owner
        .service
        .enqueue(text(&task, "id"), "manual", None)
        .await
        .unwrap();

    let mut wider = agent.clone();
    wider["description"] = "Only the description".into();
    let (status, preview) = owner
        .send("POST", &format!("{}/preview", path(&agent)), wider)
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["runs"], json!({ "queued": 0, "running": 0 }));

    let mut narrower = agent.clone();
    narrower["access"]["sandbox"] = "read-only".into();
    let (_, preview) = owner
        .send("POST", &format!("{}/preview", path(&agent)), narrower)
        .await;
    assert_eq!(preview["runs"], json!({ "queued": 1, "running": 0 }));
    assert_eq!(preview["delegations"], json!([]));

    // Previewing saves nothing.
    assert_eq!(
        owner.stored("agents", text(&agent, "id")).await["access"]["sandbox"],
        "yolo"
    );
}

#[tokio::test]
async fn deleting_an_mcp_connection_keeps_live_grants_and_marks_changed_agents() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let account = owner.add_service_account(json!([])).await;
    let mcp = owner
        .service
        .mcps
        .save(
            &owner.service,
            json!({ "name": "Docs", "transport": "stdio", "command": "node", "args": ["mcp.mjs"] }),
            None,
        )
        .await
        .unwrap();
    let mcp = text(&mcp, "id").to_owned();

    let mut agent = owner.create_agent("Connected").await;
    agent["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    agent["exclusiveNodes"] = json!([NODE]);
    agent["plugins"] = json!([account]);
    agent["access"]["mcps"] = json!([mcp]);
    let (status, saved) = owner.send("PUT", &path(&agent), agent).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let unrelated = owner.create_agent("Unrelated").await;

    owner
        .service
        .mcps
        .disconnect(&owner.service, &mcp, true)
        .await
        .unwrap();

    let after = owner.stored("agents", text(&saved, "id")).await;
    assert_eq!(after["access"]["mcps"], json!([]));
    assert_eq!(after["exclusiveNodes"], json!([NODE]));
    assert_eq!(after["plugins"], json!([account]));
    assert_ne!(after["revision"], saved["revision"]);
    // Agents without that connection are left untouched.
    let untouched = owner.stored("agents", text(&unrelated, "id")).await;
    assert_eq!(untouched["revision"], unrelated["revision"]);

    // A later unrelated save keeps the grants.
    let mut renamed = after.clone();
    renamed["description"] = "Renamed".into();
    let (status, _) = owner.send("PUT", &path(&after), renamed).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        owner.stored(connections::KIND, &account).await["agentIds"],
        json!([text(&saved, "id")])
    );
}

#[tokio::test]
async fn live_grants_do_not_expand_a_delegation_policy() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let account = owner.add_service_account(json!([])).await;
    let mut agent = owner.create_agent("Bot").await;
    agent["access"]["projects"] = json!([]);
    agent["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    let (status, agent) = owner.send("PUT", &path(&agent), agent).await;
    assert_eq!(status, StatusCode::OK, "{agent}");
    // A delegation approved for this agent, as the instance editor stores it.
    owner
        .service
        .store
        .put(
            connections::KIND,
            json!({
                "id": GHOST, "pluginId": "discord", "name": "#support", "enabled": true,
                "revision": "r1", "settings": {}, "agentId": agent["id"], "projectId": null,
                "grantedAccess": policy(&agent), "accessMode": "allowlist",
                "userIds": ["1"], "people": [], "maxMessagesPerHour": 20, "updatedAt": 0,
            }),
        )
        .await
        .unwrap();
    let id = GHOST.to_owned();

    // Grant 1Password and an exclusive reservation from the agent page.
    let mut granted = agent.clone();
    granted["exclusiveNodes"] = json!([NODE]);
    granted["plugins"] = json!([account]);
    let (status, granted) = owner.send("PUT", &path(&agent), granted).await;
    assert_eq!(status, StatusCode::OK, "{granted}");

    let approved = owner.stored(connections::KIND, &id).await["grantedAccess"].clone();
    // Both releases compare the same policy, before and after a save by the previous
    // release, which drops the fields it does not know.
    assert_eq!(policy(&granted), approved);
    let mut saved_by_previous = granted.clone();
    for key in ["exclusiveNodes", "plugins", "revision"] {
        saved_by_previous.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(policy(&saved_by_previous), approved);
    assert_eq!(policy(&granted), approved);
}

#[tokio::test]
async fn a_held_grant_on_an_instance_awaiting_review_does_not_block_other_edits() {
    let owner = Owner::new().await;
    let agent = owner.create_agent("Reviewer").await;
    let id = text(&agent, "id").to_owned();
    // An instance of a package that is not active (for example awaiting review).
    owner
        .service
        .store
        .put(
            connections::KIND,
            json!({ "id": GHOST, "pluginId": "third-party", "name": "Pending" }),
        )
        .await
        .unwrap();
    let mut held = owner.stored("agents", &id).await;
    held["plugins"] = json!([GHOST]);
    owner
        .service
        .store
        .put_agent_fixture(held.clone())
        .await
        .unwrap();

    let mut renamed = held.clone();
    renamed["description"] = "Still editable".into();
    let (status, saved) = owner.send("PUT", &path(&held), renamed).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["plugins"], json!([GHOST]));

    // Granting it to another agent still requires an active package.
    let mut other = owner.create_agent("Other").await;
    other["plugins"] = json!([GHOST]);
    let (status, _) = owner.send("PUT", &path(&other), other).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn losing_permission_to_run_on_a_node_removes_its_exclusive_reservation() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let mut agent = owner.create_agent("Heavy").await;
    agent["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    agent["exclusiveNodes"] = json!([NODE, NODE]);
    let (status, agent) = owner.send("PUT", &path(&agent), agent).await;
    assert_eq!(status, StatusCode::OK, "{agent}");
    assert_eq!(agent["exclusiveNodes"], json!([NODE]));

    // From the agent page.
    let mut narrowed = agent.clone();
    narrowed["access"]["nodes"] = json!([LOCAL_NODE_ID]);
    let (_, narrowed) = owner.send("PUT", &path(&agent), narrowed).await;
    assert_eq!(narrowed["exclusiveNodes"], json!([]));
    assert_eq!(
        owner.stored("nodes", NODE).await["exclusiveAgents"],
        json!([])
    );

    // From an older client editing the node's agents.
    let mut again = narrowed.clone();
    again["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    again["exclusiveNodes"] = json!([NODE]);
    let (_, again) = owner.send("PUT", &path(&narrowed), again).await;
    let (status, _) = owner
        .send(
            "PUT",
            &format!("/api/nodes/{NODE}/agents"),
            json!({ "agentIds": [] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let stored = owner.stored("agents", text(&again, "id")).await;
    assert_eq!(stored["exclusiveNodes"], json!([]));
    assert_eq!(
        owner.stored("nodes", NODE).await["exclusiveAgents"],
        json!([])
    );
}

#[tokio::test]
async fn adding_agents_to_a_new_node_never_revokes_existing_access() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let mut granted = owner.create_agent("Granted meanwhile").await;
    granted["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    let (_, granted) = owner.send("PUT", &path(&granted), granted).await;
    let chosen = owner.create_agent("Chosen").await;

    let (status, _) = owner
        .send(
            "PUT",
            &format!("/api/nodes/{NODE}/agents"),
            json!({ "agentIds": [chosen["id"]], "add": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    for agent in [&granted, &chosen] {
        let stored = owner.stored("agents", text(agent, "id")).await;
        assert!(
            stored["access"]["nodes"]
                .as_array()
                .unwrap()
                .contains(&json!(NODE)),
            "{stored}"
        );
    }
}

#[tokio::test]
async fn grants_follow_the_agent_execution_backend() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let native = "7a8b9c0d-1e2f-4a3b-8c4d-5e6f7a8b9c0d";
    owner
        .service
        .store
        .put(
            "nodes",
            json!({
                "id": native, "name": "desk", "revoked": false, "accepting": true, "tags": [],
                "capabilities": { "backend": "native", "cpu": 4, "memoryMiB": 8192, "diskMiB": 16384 },
                "limits": { "cpu": 2, "memoryMiB": 4096, "diskMiB": 8192 },
            }),
        )
        .await
        .unwrap();
    let agent = owner.create_agent("VM agent").await;

    // A Firecracker agent cannot run on, nor reserve, a native node.
    let mut cross = agent.clone();
    cross["access"]["nodes"] = json!([LOCAL_NODE_ID, native]);
    let (status, _) = owner.send("PUT", &path(&agent), cross).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut reserve = agent.clone();
    reserve["access"]["nodes"] = Value::Null;
    reserve["exclusiveNodes"] = json!([native]);
    let (status, _) = owner.send("PUT", &path(&agent), reserve).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        owner.stored("nodes", native).await["exclusiveAgents"],
        json!([])
    );

    // A node held before (for example from before the backend check) still saves.
    let mut held = owner.stored("agents", text(&agent, "id")).await;
    held["access"]["nodes"] = json!([LOCAL_NODE_ID, native]);
    owner
        .service
        .store
        .put_agent_fixture(held.clone())
        .await
        .unwrap();
    let mut renamed = owner.stored("agents", text(&agent, "id")).await;
    renamed["description"] = "Unrelated edit".into();
    let (status, error) = owner.send("PUT", &path(&renamed), renamed).await;
    assert_eq!(status, StatusCode::OK, "{error}");
    let agent = owner.stored("agents", text(&agent, "id")).await;

    // A Firecracker node it may run on stays reservable.
    let mut vm = agent.clone();
    vm["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    vm["exclusiveNodes"] = json!([NODE]);
    let (status, saved) = owner.send("PUT", &path(&agent), vm).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
}

#[tokio::test]
async fn an_incomplete_replacement_cannot_restore_a_concurrent_revocation() {
    let owner = Owner::new().await;
    let account = owner.add_service_account(json!([])).await;
    let mut agent = owner.create_agent("Older client").await;
    agent["plugins"] = json!([account]);
    let (_, agent) = owner.send("PUT", &path(&agent), agent).await;

    // The owner revokes the account while another editor holds the previous revision.
    let mut revoked = agent.clone();
    revoked["plugins"] = json!([]);
    let (status, _) = owner.send("PUT", &path(&agent), revoked).await;
    assert_eq!(status, StatusCode::OK);

    let mut older = agent.clone();
    for key in ["plugins", "exclusiveNodes", "revision"] {
        older.as_object_mut().unwrap().remove(key);
    }
    older["description"] = "Edited by an older client".into();
    let (status, saved) = owner.send("PUT", &path(&agent), older).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{saved}");
    assert_eq!(
        owner.stored("agents", text(&agent, "id")).await["plugins"],
        json!([])
    );
    assert_eq!(
        owner.stored(connections::KIND, &account).await["agentIds"],
        json!([])
    );
}

#[tokio::test]
async fn native_agents_cannot_newly_get_the_current_runner_and_drop_reservations() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let mut agent = owner.create_agent("Switching").await;
    agent["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    agent["exclusiveNodes"] = json!([NODE]);
    let (_, agent) = owner.send("PUT", &path(&agent), agent).await;
    assert_eq!(agent["exclusiveNodes"], json!([NODE]));

    // A native switch (here with a native node) drops the reservation it cannot keep.
    let desk = "7a8b9c0d-1e2f-4a3b-8c4d-5e6f7a8b9c0d";
    owner
        .service
        .store
        .put(
            "nodes",
            json!({
                "id": desk, "name": "desk", "revoked": false, "accepting": true, "tags": [],
                "capabilities": { "backend": "native", "cpu": 4, "memoryMiB": 8192, "diskMiB": 16384 },
                "limits": { "cpu": 2, "memoryMiB": 4096, "diskMiB": 8192 },
            }),
        )
        .await
        .unwrap();
    let mut native = agent.clone();
    native["access"]["backend"] = "native".into();
    native["access"]["nodes"] = json!([desk]);
    native["exclusiveNodes"] = json!([]);
    let (status, saved) = owner.send("PUT", &path(&agent), native).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["exclusiveNodes"], json!([]));
    assert_eq!(
        owner.stored("nodes", NODE).await["exclusiveAgents"],
        json!([])
    );

    // Switching backend never keeps nodes of the previous backend.
    let mut kept = owner.create_agent("Kept").await;
    kept["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    let (_, kept) = owner.send("PUT", &path(&kept), kept).await;
    let mut switched = kept.clone();
    switched["access"]["backend"] = "native".into();
    let (status, _) = owner.send("PUT", &path(&kept), switched).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A native agent cannot newly be given the current runner.
    let mut fresh = owner.create_agent("Fresh").await;
    fresh["access"]["nodes"] = json!([NODE]);
    let (_, fresh) = owner.send("PUT", &path(&fresh), fresh).await;
    let mut local = fresh.clone();
    local["access"]["backend"] = "native".into();
    local["access"]["nodes"] = json!([LOCAL_NODE_ID]);
    let (status, _) = owner.send("PUT", &path(&fresh), local).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn selected_permissions_and_empty_tool_selections_survive_reopening() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let account = owner.add_service_account(json!([])).await;
    let connection = owner
        .service
        .mcps
        .save(
            &owner.service,
            json!({
                "name": "Tools", "transport": "http", "url": "https://example.test/mcp",
            }),
            None,
        )
        .await
        .unwrap();
    let project_path = owner._root.path().join("project");
    std::fs::create_dir(&project_path).unwrap();
    let (status, project) = owner
        .send(
            "POST",
            "/api/projects",
            json!({
                "settings": { "name": "App", "path": project_path, "sourceMode": "local" },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{project}");
    let mut agent = owner.create_agent("Selections").await;
    agent["access"]["projects"] = json!([project["id"]]);
    agent["access"]["skills"] = json!(["global/review", "global/review"]);
    agent["access"]["mcps"] = json!([connection["id"]]);
    agent["access"]["mcpTools"] = json!({ text(&connection,"id"): [] });
    agent["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE, NODE]);
    agent["exclusiveNodes"] = json!([NODE]);
    agent["plugins"] = json!([account]);
    let (status, saved) = owner.send("PUT", &path(&agent), agent).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["access"]["skills"], json!(["global/review"]));
    assert_eq!(saved["access"]["nodes"], json!([LOCAL_NODE_ID, NODE]));
    let mut other = owner.create_agent("Selected tools").await;
    other["access"]["mcpTools"] = json!({ text(&connection,"id"): ["read", "read", "search"] });
    let (status, other) = owner.send("PUT", &path(&other), other).await;
    assert_eq!(status, StatusCode::OK, "{other}");
    assert_eq!(
        other["access"]["mcpTools"][text(&connection, "id")],
        json!(["read", "search"])
    );
    let reopened = start(owner._root.path()).await;
    assert_eq!(
        reopened.agent_snapshot(text(&saved, "id")).await.unwrap(),
        saved
    );

    let listed = reopened.store.agents().await.unwrap();
    assert_eq!(listed.len(), 3);
    for profile in listed {
        assert_eq!(
            Some(profile.clone()),
            reopened.store.agent(&profile.id.to_string()).await.unwrap()
        );
    }

    // An instance editor cannot restore a revoked grant from a stale resource list.
    let rejected = connections::save(
        &owner.service,
        "onepassword",
        &json!({
            "name": "Stale instance editor", "enabled": true, "agentIds": [],
        }),
        Some(&account),
    )
    .await
    .unwrap_err();
    assert_eq!(rejected.status, 400);
    assert_eq!(owner.stored("agents", text(&saved, "id")).await, saved);
}
