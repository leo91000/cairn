//! Exclusive node reservations and plugin grants (such as 1Password) are edited on
//! the agent. Nodes and plugin instances keep a synchronized copy that enforcement
//! and older releases read.
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
        gh_bin: "false".into(),
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
        self.service.store.get(kind, id).await.unwrap().unwrap()
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
            .send("POST", "/api/agents", json!({ "name": name }))
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

    // Nothing new is granted by default; empty grants are stored as absent keys.
    assert!(agent["access"].get("exclusiveNodes").is_none());
    assert!(agent["access"].get("plugins").is_none());

    agent["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    agent["access"]["exclusiveNodes"] = json!([NODE]);
    agent["access"]["plugins"] = json!([account]);
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

    // An older client that omits the new fields keeps them.
    let mut older = saved.clone();
    for key in ["exclusiveNodes", "plugins"] {
        older["access"].as_object_mut().unwrap().remove(key);
    }
    older["description"] = "Edited by an older app".into();
    let (status, kept) = owner.send("PUT", &path(&saved), older).await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(kept["access"]["exclusiveNodes"], json!([NODE]));
    assert_eq!(kept["access"]["plugins"], json!([account]));

    // Removing both grants removes the copies.
    let mut revoked = kept.clone();
    revoked["access"]["exclusiveNodes"] = json!([]);
    revoked["access"]["plugins"] = json!([]);
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
    main["access"]["exclusiveNodes"] = json!([NODE]);
    main["access"]["plugins"] = json!([account]);
    let (status, saved) = owner.send("PUT", &path(&main), main).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["access"]["plugins"], json!([account]));

    // Live grants are not captured by runs: they never block queued work.
    assert!(policy(&saved).get("exclusiveNodes").is_none());
    assert!(policy(&saved).get("plugins").is_none());
    let mut fewer = saved.clone();
    fewer["access"]["plugins"] = json!([]);
    assert!(covers(&fewer, &saved));
}

#[tokio::test]
async fn invalid_live_grants_are_refused() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let mut agent = owner.create_agent("Careful").await;
    for (key, value) in [
        ("exclusiveNodes", json!([GHOST])),
        ("plugins", json!([GHOST])),
    ] {
        let mut draft = agent.clone();
        draft["access"][key] = value;
        let (status, _) = owner.send("PUT", &path(&agent), draft).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{key}");
    }

    owner
        .send("POST", &format!("/api/nodes/{NODE}/revoke"), json!({}))
        .await;
    agent["access"]["exclusiveNodes"] = json!([NODE]);
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

    // An empty revision, as for agents saved before revisions existed, is not checked.
    let mut empty = agent.clone();
    empty["revision"] = "".into();
    assert_eq!(
        owner.send("PUT", &path(&agent), empty).await.0,
        StatusCode::OK
    );

    // Clients that never send a revision keep working.
    let mut unversioned = agent.clone();
    unversioned.as_object_mut().unwrap().remove("revision");
    assert_eq!(
        owner.send("PUT", &path(&agent), unversioned).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn older_clients_editing_resources_update_the_agent() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let agent = owner.create_agent("Android QA").await;
    let id = text(&agent, "id").to_owned();

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
    assert_eq!(status, StatusCode::OK, "{node}");
    assert_eq!(
        owner.stored("agents", &id).await["access"]["exclusiveNodes"],
        json!([NODE])
    );

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
        owner.stored("agents", &id).await["access"]["plugins"],
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
    assert!(
        owner.stored("agents", &id).await["access"]
            .get("plugins")
            .is_none()
    );
}

#[tokio::test]
async fn deleting_an_agent_or_revoking_a_node_removes_its_live_grants() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let mut agent = owner.create_agent("Temporary").await;
    agent["access"]["exclusiveNodes"] = json!([NODE]);
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
        "exclusiveAgents": node["exclusiveAgents"],
    });
    let (status, error) = owner
        .send("PUT", &format!("/api/nodes/{NODE}"), configuration)
        .await;
    assert_eq!(status, StatusCode::OK, "{error}");

    let mut kept = owner.create_agent("Kept").await;
    kept["access"]["exclusiveNodes"] = json!([NODE]);
    let (_, kept) = owner.send("PUT", &path(&kept), kept).await;
    owner
        .send("POST", &format!("/api/nodes/{NODE}/revoke"), json!({}))
        .await;
    assert!(
        owner.stored("agents", text(&kept, "id")).await["access"]
            .get("exclusiveNodes")
            .is_none()
    );
}

#[tokio::test]
async fn startup_rebuilds_agent_grants_from_resources_an_older_release_edited() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let service = start(root.path()).await;
    let owner = Owner::serve(service, TempDir::new().unwrap()).await;
    owner.add_node().await;
    let account = owner.add_service_account(json!([])).await;
    let agent = owner.create_agent("Legacy").await;
    let id = text(&agent, "id").to_owned();

    // An older release only writes the resource side, possibly naming deleted agents.
    let store = &owner.service.store;
    let mut node = owner.stored("nodes", NODE).await;
    node["exclusiveAgents"] = json!([id, GHOST]);
    store.put("nodes", node).await.unwrap();
    let mut instance = owner.stored(connections::KIND, &account).await;
    instance["agentIds"] = json!([GHOST, id]);
    store.put(connections::KIND, instance).await.unwrap();
    let mut legacy = owner.stored("agents", &id).await;
    for key in ["exclusiveNodes", "plugins"] {
        legacy["access"].as_object_mut().unwrap().remove(key);
    }
    store.put("agents", legacy).await.unwrap();
    owner.service.shutdown.cancel();
    drop(owner);

    let restarted = start(root.path()).await;
    let agent = restarted.store.get("agents", &id).await.unwrap().unwrap();
    assert_eq!(agent["access"]["exclusiveNodes"], json!([NODE]));
    assert_eq!(agent["access"]["plugins"], json!([account]));
    let node = restarted.store.get("nodes", NODE).await.unwrap().unwrap();
    assert_eq!(node["exclusiveAgents"], json!([id]));
    let instance = restarted
        .store
        .get(connections::KIND, &account)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(instance["agentIds"], json!([id]));

    // A second startup changes nothing.
    let revision = agent["revision"].clone();
    restarted.shutdown.cancel();
    let again = start(root.path()).await;
    let agent = again.store.get("agents", &id).await.unwrap().unwrap();
    assert_eq!(agent["revision"], revision);
}

#[tokio::test]
async fn preview_counts_work_that_reduced_access_would_stop() {
    let owner = Owner::new().await;
    let agent = owner.create_agent("Busy").await;
    let task = owner
        .service
        .task(
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
    agent["access"]["exclusiveNodes"] = json!([NODE]);
    agent["access"]["plugins"] = json!([account]);
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
    assert_eq!(after["access"]["exclusiveNodes"], json!([NODE]));
    assert_eq!(after["access"]["plugins"], json!([account]));
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
async fn startup_gives_existing_agents_a_revision_and_keeps_empty_grants_absent() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let service = start(root.path()).await;
    let mut legacy = service
        .store
        .get("agents", MAIN_AGENT_ID)
        .await
        .unwrap()
        .unwrap();
    legacy.as_object_mut().unwrap().remove("revision");
    for key in ["exclusiveNodes", "plugins"] {
        legacy["access"].as_object_mut().unwrap().remove(key);
    }
    service.store.put("agents", legacy).await.unwrap();
    service.shutdown.cancel();
    drop(service);

    let restarted = start(root.path()).await;
    let main = restarted
        .store
        .get("agents", MAIN_AGENT_ID)
        .await
        .unwrap()
        .unwrap();
    assert!(main["revision"].as_str().is_some_and(|r| !r.is_empty()));
    // The previous release merges every access key into its policy: empty grants stay absent.
    assert!(main["access"].get("exclusiveNodes").is_none());
    assert!(main["access"].get("plugins").is_none());
}

#[tokio::test]
async fn approved_delegation_access_matches_what_the_previous_release_computes() {
    let owner = Owner::new().await;
    owner.add_node().await;
    let mut agent = owner.create_agent("Bot").await;
    agent["access"]["projects"] = json!([]);
    agent["access"]["nodes"] = json!([LOCAL_NODE_ID, NODE]);
    agent["access"]["exclusiveNodes"] = json!([NODE]);
    agent["access"]["github"] = false.into();
    let (status, agent) = owner.send("PUT", &path(&agent), agent).await;
    assert_eq!(status, StatusCode::OK, "{agent}");

    // A delegation approved for this agent, as the instance editor saves it.
    let instance = json!({
        "id": GHOST,
        "pluginId": "discord",
        "name": "#support",
        "enabled": true,
        "revision": "r1",
        "settings": {},
        "agentId": agent["id"],
        "projectId": null,
        "grantedAccess": cairn_installation::agent_grants::approved_access(&agent),
        "ownerAccessId": null,
        "accessMode": "allowlist",
        "userIds": ["1"],
        "people": [],
        "maxMessagesPerHour": 20,
        "updatedAt": 0,
    });
    owner
        .service
        .store
        .put(connections::KIND, instance)
        .await
        .unwrap();

    // The previous release's policy() merges every saved access key.
    let mut previous = json!({
        "projects": null, "skills": null, "mcps": null, "mcpTools": {},
        "installation": false, "github": true, "sandbox": "yolo",
        "nodes": [LOCAL_NODE_ID],
    });
    cairn_installation::store::merge(&mut previous, &agent["access"]);
    let stored = owner.stored(connections::KIND, GHOST).await;
    assert_eq!(stored["grantedAccess"], previous);

    // Removing the live grant keeps the delegation approved, on both releases.
    let mut fewer = agent.clone();
    fewer["access"]["exclusiveNodes"] = json!([]);
    let (status, fewer) = owner.send("PUT", &path(&agent), fewer).await;
    assert_eq!(status, StatusCode::OK, "{fewer}");
    let stored = owner.stored(connections::KIND, GHOST).await;
    let mut previous = json!({
        "projects": null, "skills": null, "mcps": null, "mcpTools": {},
        "installation": false, "github": true, "sandbox": "yolo",
        "nodes": [LOCAL_NODE_ID],
    });
    cairn_installation::store::merge(&mut previous, &fewer["access"]);
    assert_eq!(stored["grantedAccess"], previous);
    assert!(cairn_installation::agent_grants::same_approval(
        &fewer,
        &stored["grantedAccess"]
    ));
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
            json!({ "id": GHOST, "pluginId": "third-party", "name": "Pending", "agentIds": [id] }),
        )
        .await
        .unwrap();
    let mut held = owner.stored("agents", &id).await;
    held["access"]["plugins"] = json!([GHOST]);
    owner
        .service
        .store
        .put("agents", held.clone())
        .await
        .unwrap();

    let mut renamed = held.clone();
    renamed["description"] = "Still editable".into();
    let (status, saved) = owner.send("PUT", &path(&held), renamed).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["access"]["plugins"], json!([GHOST]));

    // Granting it to another agent still requires an active package.
    let mut other = owner.create_agent("Other").await;
    other["access"]["plugins"] = json!([GHOST]);
    let (status, _) = owner.send("PUT", &path(&other), other).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
