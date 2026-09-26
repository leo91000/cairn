use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use leo_agent_manager::{config::Config, http::router, service::Service};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Owner {
    service: std::sync::Arc<Service>,
    _root: TempDir,
    app: axum::Router,
    cookie: String,
    csrf: String,
    host: String,
}
impl Owner {
    async fn new() -> Self {
        Self::at("localhost:4310".into()).await
    }
    async fn at(host: String) -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let s = Service::new(Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: format!("http://{host}"),
            host: "127.0.0.1".into(),
            port: 0,
            setup_token: "fixture".into(),
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            gh_bin: "false".into(),
            concurrency: 4,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let session = s.auth.session().await.unwrap();
        Self {
            service: s.clone(),
            host,
            _root: root,
            app: router(s).await.unwrap(),
            cookie: format!("leo_session={}", session["value"].as_str().unwrap()),
            csrf: session["csrf"].as_str().unwrap().into(),
        }
    }
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Value,
        token: Option<&str>,
    ) -> (u16, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", &self.host)
            .header("content-type", "application/json");
        if path.starts_with("/api/") {
            request = request
                .header("cookie", &self.cookie)
                .header("x-csrf-token", &self.csrf);
        }
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

#[tokio::test]
async fn enrollment_is_single_use_and_revocation_removes_node_access() {
    let owner = Owner::new().await;
    let (status, invitation) = owner
        .call(
            "POST",
            "/api/nodes/enrollments",
            json!({"name":"Desktop"}),
            None,
        )
        .await;
    assert_eq!(status, 200, "{invitation}");
    let input = json!({"code":invitation["code"],"name":"Desktop","protocol":1,"capabilities":{"os":"linux","arch":"x86_64","kvm":true,"cpu":16,"memoryMiB":32768,"diskMiB":131072},"runtimeId":"fixture"});
    let (status, identity) = owner
        .call("POST", "/internal/nodes/enroll", input.clone(), None)
        .await;
    assert_eq!(status, 200, "{identity}");
    let token = identity["token"].as_str().unwrap();
    assert_eq!(
        owner
            .call("POST", "/internal/nodes/enroll", input, None)
            .await
            .0,
        401
    );
    let (status, nodes) = owner.call("GET", "/api/nodes", Value::Null, None).await;
    assert_eq!(status, 200);
    assert!(!nodes.to_string().contains(token));
    assert_eq!(
        owner
            .call(
                "POST",
                "/internal/nodes/heartbeat",
                json!({"runtimeId":"fixture"}),
                Some(token)
            )
            .await
            .0,
        200
    );
    let node = identity["nodeId"].as_str().unwrap();
    assert_eq!(
        owner
            .call(
                "POST",
                &format!("/api/nodes/{node}/revoke"),
                json!({}),
                None
            )
            .await
            .0,
        200
    );
    assert_eq!(
        owner
            .call("POST", "/internal/nodes/heartbeat", json!({}), Some(token))
            .await
            .0,
        401
    );
}

#[tokio::test]
async fn node_configuration_validates_capacity_and_never_grants_agent_access() {
    let owner = Owner::new().await;
    let (_, invitation) = owner
        .call(
            "POST",
            "/api/nodes/enrollments",
            json!({"name":"Small node"}),
            None,
        )
        .await;
    let (_, identity)=owner.call("POST","/internal/nodes/enroll",json!({"code":invitation["code"],"name":"ignored","protocol":1,"capabilities":{"os":"linux","arch":"x86_64","kvm":true,"cpu":8,"memoryMiB":8192,"diskMiB":65536},"runtimeId":"fixture"}),None).await;
    let node = identity["nodeId"].as_str().unwrap();
    let path = format!("/api/nodes/{node}");
    let config = json!({"name":"My node","tags":["fast"],"accepting":true,"limits":{"cpu":4,"memoryMiB":4096,"diskMiB":32768}});
    let (status, saved) = owner.call("PUT", &path, config.clone(), None).await;
    assert_eq!(status, 200, "{saved}");
    let mut invalid = config;
    invalid["limits"]["cpu"] = 9.into();
    assert_eq!(owner.call("PUT", &path, invalid, None).await.0, 400);
    let (_, beat) = owner
        .call(
            "POST",
            "/internal/nodes/heartbeat",
            json!({}),
            identity["token"].as_str(),
        )
        .await;
    assert_eq!(beat["limits"]["cpu"], 4);
    let (_, agents) = owner.call("GET", "/api/agents", Value::Null, None).await;
    for agent in agents.as_array().unwrap() {
        assert_eq!(
            agent["access"]["nodes"],
            json!(["00000000-0000-4000-8000-000000000002"])
        );
    }
}

#[tokio::test]
async fn main_node_policy_survives_an_update_from_an_older_client() {
    let owner = Owner::new().await;
    let path = "/api/agents/00000000-0000-4000-8000-000000000001";
    let (status, restricted) = owner
        .call(
            "PUT",
            path,
            json!({"name":"Main","access":{"nodes":[]}}),
            None,
        )
        .await;
    assert_eq!(status, 200, "{restricted}");
    let (status,saved)=owner.call("PUT",path,json!({"name":"Renamed main","access":{"projects":null,"skills":null,"mcps":null,"github":true}}),None).await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["access"]["nodes"], json!([]));
    let (status, _) = owner
        .call(
            "PUT",
            path,
            json!({"name":"Main","access":{"nodes":["10000000-0000-4000-8000-000000000000"]}}),
            None,
        )
        .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn concurrent_enrollment_has_exactly_one_winner() {
    let owner = Owner::new().await;
    let (_, invitation) = owner
        .call(
            "POST",
            "/api/nodes/enrollments",
            json!({"name":"Race"}),
            None,
        )
        .await;
    let input = json!({"code":invitation["code"],"name":"Race","protocol":1,"capabilities":{"os":"linux","arch":"x86_64","kvm":true,"cpu":2,"memoryMiB":4096,"diskMiB":32768},"runtimeId":"fixture"});
    let (first, second) = tokio::join!(
        owner.call("POST", "/internal/nodes/enroll", input.clone(), None),
        owner.call("POST", "/internal/nodes/enroll", input, None)
    );
    let mut statuses = [first.0, second.0];
    statuses.sort();
    assert_eq!(statuses, [200, 401]);
}

#[tokio::test]
async fn connector_enrolls_over_http_without_printing_or_exposing_its_token() {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = listener.local_addr().unwrap().to_string();
    let owner = Owner::at(host.clone()).await;
    let app = owner.app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_, invitation) = owner
        .call(
            "POST",
            "/api/nodes/enrollments",
            json!({"name":"Linux connector"}),
            None,
        )
        .await;
    let state = owner._root.path().join("node");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_leo"))
        .args([
            "node-enroll",
            &format!("http://{host}"),
            state.to_str().unwrap(),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(invitation["code"].as_str().unwrap().as_bytes())
        .await
        .unwrap();
    let output = child.wait_with_output().await.unwrap();
    server.abort();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = std::fs::read(state.join("identity.json")).unwrap();
    let identity: Value = serde_json::from_slice(&bytes).unwrap();
    let token = identity["token"].as_str().unwrap();
    assert!(!String::from_utf8_lossy(&output.stdout).contains(token));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(token));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(state.join("identity.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let (_, nodes) = owner.call("GET", "/api/nodes", Value::Null, None).await;
    assert!(
        nodes
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["name"] == "Linux connector" && node["capabilities"]["os"] == "linux")
    );
}

#[tokio::test]
async fn a_task_cannot_use_the_local_runner_without_node_permission() {
    let owner = Owner::new().await;
    let main = "00000000-0000-4000-8000-000000000001";
    assert_eq!(
        owner
            .call(
                "PUT",
                &format!("/api/agents/{main}"),
                json!({"name":"Main","access":{"nodes":[]}}),
                None
            )
            .await
            .0,
        200
    );
    let (status,task)=owner.call("POST","/api/tasks",json!({"name":"Restricted node task","prompt":"Do nothing","agentId":main,"enabled":false}),None).await;
    assert_eq!(status, 200, "{task}");
    let (status, error) = owner
        .call(
            "POST",
            &format!("/api/tasks/{}/run", task["id"].as_str().unwrap()),
            json!({}),
            None,
        )
        .await;
    assert_eq!(status, 403, "{error}");
}

#[tokio::test]
async fn node_credentials_cannot_administer_nodes_and_owner_sessions_cannot_impersonate_a_node() {
    let owner = Owner::new().await;
    let (_, invitation) = owner
        .call(
            "POST",
            "/api/nodes/enrollments",
            json!({"name":"Scoped"}),
            None,
        )
        .await;
    let (_, identity)=owner.call("POST","/internal/nodes/enroll",json!({"code":invitation["code"],"name":"Scoped","protocol":1,"capabilities":{"os":"linux","arch":"x86_64","kvm":false,"cpu":2,"memoryMiB":4096,"diskMiB":32768},"runtimeId":"fixture"}),None).await;
    let request = Request::builder()
        .method("GET")
        .uri("/api/nodes")
        .header("host", &owner.host)
        .header(
            "authorization",
            format!("Bearer {}", identity["token"].as_str().unwrap()),
        )
        .body(Body::empty())
        .unwrap();
    let response = owner.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status().as_u16(), 401);
    assert_eq!(
        owner
            .call("POST", "/internal/nodes/heartbeat", json!({}), None)
            .await
            .0,
        401
    );
    assert_eq!(
        owner
            .call(
                "POST",
                "/internal/nodes/heartbeat",
                json!({}),
                Some("not-a-node-token")
            )
            .await
            .0,
        401
    );
}

#[tokio::test]
async fn revocation_removes_node_grants_without_blocking_later_agent_edits() {
    let owner = Owner::new().await;
    let (_, invitation) = owner
        .call(
            "POST",
            "/api/nodes/enrollments",
            json!({"name":"Revocation"}),
            None,
        )
        .await;
    let (_, identity) = owner.call("POST", "/internal/nodes/enroll", json!({"code":invitation["code"],"name":"Revocation","protocol":1,"capabilities":{"os":"linux","arch":"x86_64","kvm":true,"cpu":2,"memoryMiB":4096,"diskMiB":32768},"runtimeId":"fixture"}), None).await;
    let node = identity["nodeId"].as_str().unwrap();
    let main = "/api/agents/00000000-0000-4000-8000-000000000001";
    let local = "00000000-0000-4000-8000-000000000002";
    assert_eq!(
        owner
            .call(
                "PUT",
                main,
                json!({"name":"Main","access":{"nodes":[local,node]}}),
                None
            )
            .await
            .0,
        200
    );
    assert_eq!(
        owner
            .call(
                "POST",
                &format!("/api/nodes/{node}/revoke"),
                json!({}),
                None
            )
            .await
            .0,
        200
    );
    let (_, agents) = owner.call("GET", "/api/agents", Value::Null, None).await;
    let mut main_agent = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "00000000-0000-4000-8000-000000000001")
        .unwrap()
        .clone();
    assert_eq!(main_agent["access"]["nodes"], json!([local]));
    main_agent["name"] = "Renamed after revocation".into();
    assert_eq!(owner.call("PUT", main, main_agent, None).await.0, 200);
}

#[test]
fn mcp_agent_updates_preserve_omitted_node_permissions() {
    use leo_agent_manager::validation::parse;
    let update = parse(
        "mcp:update_agent",
        json!({"id":"00000000-0000-4000-8000-000000000001","agent":{"access":{"mcps":[]}}}),
    )
    .unwrap();
    assert!(update["agent"]["access"].get("nodes").is_none());
    let restricted = parse(
        "mcp:update_agent",
        json!({"id":"00000000-0000-4000-8000-000000000001","agent":{"access":{"nodes":[]}}}),
    )
    .unwrap();
    assert_eq!(restricted["agent"]["access"]["nodes"], json!([]));
    let created = parse("mcp:save_agent", json!({"name":"Default"})).unwrap();
    assert_eq!(
        created["access"]["nodes"],
        json!(["00000000-0000-4000-8000-000000000002"])
    );
}

#[tokio::test]
async fn outbound_transport_streams_only_to_the_authenticated_node() {
    use leo_agent_manager::execution::secret;
    let owner = Owner::new().await;
    let (_, invite) = owner
        .call(
            "POST",
            "/api/nodes/enrollments",
            json!({"name":"Worker"}),
            None,
        )
        .await;
    let (_, identity) = owner.call("POST", "/internal/nodes/enroll", json!({"code":invite["code"],"name":"Worker","protocol":1,"capabilities":{"os":"linux","arch":"x86_64","kvm":true,"cpu":4,"memoryMiB":8192,"diskMiB":65536},"runtimeId":"fixture"}), None).await;
    let credential = secret(&owner._root.path().join("data"), "runner-secret")
        .await
        .unwrap();
    let request = Request::builder()
        .uri(format!(
            "/internal/execution/{}/health",
            identity["nodeId"].as_str().unwrap()
        ))
        .header("host", &owner.host)
        .header("authorization", format!("Bearer {credential}"))
        .body(Body::empty())
        .unwrap();
    let app = owner.app.clone();
    let waiting = tokio::spawn(async move {
        let response = app.oneshot(request).await.unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
        (status, bytes)
    });
    let token = identity["token"].as_str().unwrap();
    let (status, command) = owner
        .call("POST", "/internal/nodes/poll", json!({}), Some(token))
        .await;
    assert_eq!(status, 200, "{command}");
    assert_eq!(command["path"], "/health");
    assert_eq!(
        owner
            .call(
                "POST",
                "/internal/nodes/reply",
                json!({"id":command["id"],"status":200,"data":"b2s=","done":true}),
                Some("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx")
            )
            .await
            .0,
        401
    );
    assert_eq!(
        owner
            .call(
                "POST",
                "/internal/nodes/reply",
                json!({"id":command["id"],"status":200,"data":"b2s=","done":true}),
                Some(token)
            )
            .await
            .0,
        200
    );
    let (status, bytes) = waiting.await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(&bytes[..], b"ok");
}

#[tokio::test]
async fn workspace_transfer_preserves_files_and_links_without_following_them() {
    use leo_agent_manager::nodes::files;
    use std::os::unix::fs::PermissionsExt;
    let root = TempDir::new().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    std::fs::write(source.join("nested/tool"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        source.join("nested/tool"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::os::unix::fs::symlink("/etc", source.join("external")).unwrap();
    let mut bytes = Vec::new();
    files::send(&source, &mut bytes).await.unwrap();
    files::receive(&mut bytes.as_slice(), &target, 1024)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(target.join("nested/tool")).unwrap(),
        b"#!/bin/sh\nexit 0\n"
    );
    assert_eq!(
        std::fs::read_link(target.join("external")).unwrap(),
        std::path::Path::new("/etc")
    );
    assert_eq!(
        std::fs::metadata(target.join("nested/tool"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let malicious =
        b"{\"path\":\"external/passwd\",\"kind\":\"file\",\"size\":0}\n{\"complete\":true}\n";
    assert!(
        files::receive(&mut malicious.as_slice(), &target, 1024)
            .await
            .is_err()
    );
    let incomplete = root.path().join("incomplete");
    assert!(
        files::receive(&mut &bytes[..bytes.len() - 20], &incomplete, 1024)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn concurrent_admission_reserves_capacity_once_and_preserves_agent_grants() {
    use leo_agent_manager::{
        config::{id, now},
        nodes::{LOCAL_NODE_ID, placement},
    };
    let owner = Owner::new().await;
    let node = id();
    let agent = id();
    let run_a = id();
    let run_b = id();
    owner.service.store.put("nodes",json!({"id":node,"local":false,"accepting":true,"executionReady":true,"lastSeen":now(),"capabilities":{"kvm":true},"limits":{"cpu":2,"memoryMiB":4096,"diskMiB":65536}})).await.unwrap();
    owner
        .service
        .store
        .put("agents", json!({"id":agent,"access":{"nodes":[node]}}))
        .await
        .unwrap();
    let a = json!({"id":run_a,"snapshot":{"agent":{"id":agent}}});
    let b = json!({"id":run_b,"snapshot":{"agent":{"id":agent}}});
    let attempt_a = id();
    let attempt_b = id();
    let (first, second) = tokio::join!(
        placement::reserve(&owner.service, &a, &attempt_a),
        placement::reserve(&owner.service, &b, &attempt_b)
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let winner = if first.is_ok() {
        &attempt_a
    } else {
        &attempt_b
    };
    let loser = if first.is_ok() { &b } else { &a };
    placement::release(&owner.service, winner).await.unwrap();
    assert_eq!(
        placement::reserve(&owner.service, loser, &id())
            .await
            .unwrap()["nodeId"],
        node
    );
    owner
        .service
        .store
        .put("agents", json!({"id":agent,"access":{"nodes":[]}}))
        .await
        .unwrap();
    assert!(
        placement::reserve(
            &owner.service,
            &json!({"id":id(),"snapshot":{"agent":{"id":agent}},"preferredNodeId":LOCAL_NODE_ID}),
            &id()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn retained_disks_remain_charged_after_execution_and_cancellation_releases_destination() {
    use leo_agent_manager::{
        config::{id, now},
        nodes::{moves, placement},
    };
    let owner = Owner::new().await;
    let node = id();
    let agent = id();
    let run = id();
    let attempt = id();
    owner.service.store.put("nodes",json!({"id":node,"accepting":true,"executionReady":true,"lastSeen":now(),"capabilities":{"kvm":true},"limits":{"cpu":4,"memoryMiB":8192,"diskMiB":32768}})).await.unwrap();
    owner
        .service
        .store
        .put("agents", json!({"id":agent,"access":{"nodes":[node]}}))
        .await
        .unwrap();
    let execution = json!({"id":run,"snapshot":{"agent":{"id":agent}}});
    placement::reserve(&owner.service, &execution, &attempt)
        .await
        .unwrap();
    placement::materialize(&owner.service, &attempt)
        .await
        .unwrap();
    placement::release(&owner.service, &attempt).await.unwrap();
    assert!(
        placement::reserve(
            &owner.service,
            &json!({"id":id(),"snapshot":{"agent":{"id":agent}}}),
            &id()
        )
        .await
        .is_err(),
        "Idle persistent disks consume the node's disk budget"
    );
    let retry = id();
    placement::reserve(&owner.service, &execution, &retry)
        .await
        .unwrap();
    placement::release(&owner.service, &retry).await.unwrap();
    let pending = id();
    owner
        .service
        .store
        .put(
            "node-attempts",
            json!({"id":pending,"nodeId":node,"runId":run,"role":"destination","released":false}),
        )
        .await
        .unwrap();
    let cancelled =
        json!({"id":run,"cancelRequestedAt":now(),"moveRequest":{"reservation":pending}});
    assert!(moves::advance(&owner.service, &cancelled).await.unwrap());
    assert_eq!(
        owner
            .service
            .store
            .get("node-attempts", &pending)
            .await
            .unwrap()
            .unwrap()["released"],
        true
    );
}

#[tokio::test]
async fn requesting_a_smaller_disk_is_rejected_before_moving() {
    use leo_agent_manager::{config::id, nodes::moves};
    let owner = Owner::new().await;
    let run = json!({"id":id(),"resources":{"cpu":2,"memoryMiB":4096,"diskMiB":32768}});
    let error = moves::request(
        &owner.service,
        &run,
        &json!({"cpu":2,"memoryMiB":4096,"diskMiB":128}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, 400);
    assert!(error.message.contains("shrink"));
}

#[tokio::test]
async fn recovery_settings_validate_and_preserve_the_last_good_configuration() {
    let owner = Owner::new().await;
    let (status, initial) = owner
        .call("GET", "/api/nodes/settings", Value::Null, None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(initial["destination"], "master");
    let settings = json!({"destination":"master","intervalSeconds":30,"retention":3,"budgetMiB":4096,"disconnectTimeoutSeconds":60,"shutdownTimeoutSeconds":300,"maxCapacityWaitSeconds":3600});
    assert_eq!(
        owner
            .call("PUT", "/api/nodes/settings", settings.clone(), None)
            .await
            .0,
        200
    );
    let mut bad = settings.clone();
    bad["retention"] = 0.into();
    assert_eq!(
        owner.call("PUT", "/api/nodes/settings", bad, None).await.0,
        400
    );
    assert_eq!(
        owner
            .call("GET", "/api/nodes/settings", Value::Null, None)
            .await
            .1,
        settings
    );
}

#[tokio::test]
async fn tags_filter_authorized_nodes_without_granting_access() {
    use leo_agent_manager::{
        config::{id, now},
        nodes::placement,
    };
    let owner = Owner::new().await;
    let fast = id();
    let other = id();
    let agent = id();
    for (node, tags) in [(&fast, json!(["fast"])), (&other, json!(["slow"]))] {
        owner.service.store.put("nodes",json!({"id":node,"tags":tags,"accepting":true,"executionReady":true,"lastSeen":now(),"capabilities":{"kvm":true},"limits":{"cpu":4,"memoryMiB":8192,"diskMiB":65536}})).await.unwrap();
    }
    owner
        .service
        .store
        .put("agents", json!({"id":agent,"access":{"nodes":[other]}}))
        .await
        .unwrap();
    let run = json!({"id":id(),"snapshot":{"agent":{"id":agent}},"requiredTags":["fast"]});
    assert!(
        placement::reserve(&owner.service, &run, &id())
            .await
            .is_err()
    );
    owner
        .service
        .store
        .put(
            "agents",
            json!({"id":agent,"access":{"nodes":[fast,other]}}),
        )
        .await
        .unwrap();
    assert_eq!(
        placement::reserve(&owner.service, &run, &id())
            .await
            .unwrap()["nodeId"],
        fast
    );
}

#[tokio::test]
async fn a_remote_only_agent_prepares_a_private_vm_without_a_local_controller() {
    let owner = Owner::new().await;
    std::fs::create_dir_all(owner.service.config.home.join(".codex")).unwrap();
    std::fs::write(
        owner.service.config.home.join(".codex/leo-managed-auth"),
        "1",
    )
    .unwrap();
    let run = json!({"id":"e2000000-0000-4000-8000-000000000001","snapshot":{"agent":{"id":"00000000-0000-4000-8000-000000000001","access":{"nodes":["e2000000-0000-4000-8000-000000000002"]}},"projects":[],"skills":[],"task":{}}});
    owner
        .service
        .store
        .put("agents", run["snapshot"]["agent"].clone())
        .await
        .unwrap();
    let prepared =
        leo_agent_manager::execution::prepare(&run, &owner.service.config, None, None, None)
            .await
            .unwrap();
    assert_eq!(prepared["backend"], "firecracker");
    assert_eq!(prepared["isolated"], true);
    let placement = leo_agent_manager::nodes::placement::reserve(
        &owner.service,
        &run,
        "e2000000-0000-4000-8000-000000000003",
    )
    .await;
    assert!(
        placement.is_err(),
        "An unavailable VM must never fall back to the shared host"
    );
}

#[tokio::test]
async fn owner_placement_obeys_agent_grants_and_preserves_last_good_selection() {
    let owner = Owner::new().await;
    let run = leo_agent_manager::config::id();
    let main = "00000000-0000-4000-8000-000000000001";
    let local = leo_agent_manager::nodes::LOCAL_NODE_ID;
    let record = json!({"id":run,"taskId":"fixture","createdAt":0,"status":"succeeded","snapshot":{"agent":{"id":main}}});
    owner
        .service
        .store
        .write(move |db| db.add_run(&record, None))
        .await
        .unwrap();
    let path = format!("/api/nodes/placement/{run}");
    let (status, _) = owner
        .call(
            "PUT",
            &path,
            json!({"pinnedNodeId":local,"preferredNodeId":null}),
            None,
        )
        .await;
    assert_eq!(status, 200);
    let (status, _) = owner
        .call(
            "PUT",
            &path,
            json!({"pinnedNodeId":leo_agent_manager::config::id(),"preferredNodeId":null}),
            None,
        )
        .await;
    assert_eq!(status, 403);
    let (status, value) = owner.call("GET", &path, Value::Null, None).await;
    assert_eq!(status, 200);
    assert_eq!(value["pinnedNodeId"], local);
    let (status, _) = owner
        .call(
            "PUT",
            &path,
            json!({"pinnedNodeId":null,"preferredNodeId":local}),
            None,
        )
        .await;
    assert_eq!(status, 200);
    let (_, value) = owner.call("GET", &path, Value::Null, None).await;
    assert!(value["pinnedNodeId"].is_null());
    assert_eq!(value["preferredNodeId"], local);
}

#[tokio::test]
async fn native_auth_relays_refresh_both_protocols_and_stop_after_grant_revocation() {
    use leo_agent_manager::{auth, config::id, microvm::wire, nodes::workspace};
    use tokio::{
        io::BufReader,
        net::{TcpListener, UnixListener, UnixStream},
    };
    for claude in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let owner = Owner::at(address.to_string()).await;
        let node = id();
        let attempt = id();
        let run = id();
        let token = auth::token();
        let main = "00000000-0000-4000-8000-000000000001";
        owner
            .service
            .store
            .put("nodes", json!({"id":node,"revoked":false}))
            .await
            .unwrap();
        owner
            .service
            .store
            .set(
                &format!("node-token:{}", auth::digest(&token)),
                json!(node),
                None,
            )
            .await
            .unwrap();
        owner
            .service
            .store
            .put("agents", json!({"id":main,"access":{"nodes":[node]}}))
            .await
            .unwrap();
        owner
            .service
            .store
            .put(
                "node-attempts",
                json!({"id":attempt,"runId":run,"nodeId":node,"released":false}),
            )
            .await
            .unwrap();
        let record = json!({"id":run,"taskId":"fixture","createdAt":0,"status":"running","snapshot":{"agent":{"id":main}}});
        owner
            .service
            .store
            .write(move |db| db.add_run(&record, None))
            .await
            .unwrap();
        owner
            .service
            .store
            .set(
                &format!("run-checkpoint:{run}"),
                json!({"nodeId":node,"runnerId":attempt}),
                None,
            )
            .await
            .unwrap();
        let plans = owner.service.config.data_dir.join("runner-plans");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::write(
            plans.join(format!("{attempt}.json")),
            json!({"runId":run,"chat":{"claudeManagedAuth":claude}}).to_string(),
        )
        .unwrap();
        let home = owner
            .service
            .config
            .data_dir
            .join("runs")
            .join(&run)
            .join("home")
            .join(if claude { ".claude" } else { ".codex" });
        std::fs::create_dir_all(&home).unwrap();
        let provider = UnixListener::bind(home.join("leo-auth.sock")).unwrap();
        let native = tokio::spawn(async move {
            for generation in 1..=2 {
                let (stream, _) = provider.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                if !claude {
                    assert_eq!(
                        wire::read(&mut stream).await.unwrap().unwrap()["method"],
                        "refresh"
                    );
                }
                wire::write(
                    stream.get_mut(),
                    &json!({"accessToken":format!("synthetic-{generation}")}),
                )
                .await
                .unwrap();
            }
        });
        let app = owner.app.clone();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let stop = tokio_util::sync::CancellationToken::new();
        let path = owner._root.path().join("remote.sock");
        let relay = tokio::spawn(workspace::auth_listener(
            path.clone(),
            reqwest::Client::new(),
            format!("http://{address}").parse().unwrap(),
            token,
            attempt,
            stop.clone(),
            claude,
        ));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !path.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        for generation in 1..=2 {
            let mut stream = BufReader::new(UnixStream::connect(&path).await.unwrap());
            if !claude {
                wire::write(stream.get_mut(), &json!({"method":"refresh"}))
                    .await
                    .unwrap();
            }
            let value =
                tokio::time::timeout(std::time::Duration::from_secs(2), wire::read(&mut stream))
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
            assert_eq!(value["accessToken"], format!("synthetic-{generation}"));
        }
        native.await.unwrap();
        owner
            .service
            .store
            .put("agents", json!({"id":main,"access":{"nodes":[]}}))
            .await
            .unwrap();
        let mut stream = BufReader::new(UnixStream::connect(&path).await.unwrap());
        if !claude {
            wire::write(stream.get_mut(), &json!({"method":"refresh"}))
                .await
                .unwrap();
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), wire::read(&mut stream))
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        stop.cancel();
        relay.await.unwrap().unwrap();
        server.abort();
    }
}

#[tokio::test]
async fn archives_transfer_between_distinct_node_and_master_directories() {
    use leo_agent_manager::{config::id, nodes::archive};
    let owner = Owner::new().await;
    let node = tempfile::TempDir::new().unwrap();
    let remote = node.path().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().fallback(axum::routing::any(
        move |request: axum::extract::Request| {
            let remote = remote.clone();
            async move { archive::controller(&remote, request).await }
        },
    ));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let transfer = id();
    let staging = owner._root.path().join(&transfer);
    std::fs::create_dir(&staging).unwrap();
    let bytes = (0..524325).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    let file = staging.join("workspace.tar.gz");
    std::fs::write(&file, &bytes).unwrap();
    let base = format!("http://{address}");
    archive::transfer(&owner.service, &base, "fixture", &staging, true)
        .await
        .unwrap();
    std::fs::remove_file(&file).unwrap();
    archive::transfer(&owner.service, &base, "fixture", &staging, false)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    let response = owner
        .service
        .http
        .post(format!("{base}/archive-transfers/{transfer}/0"))
        .body("cannot overwrite earlier chunks")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    assert!(
        owner
            .service
            .http
            .delete(format!("{base}/archive-transfers/{transfer}/discard"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert!(
        !node
            .path()
            .join("archive-transfers")
            .join(transfer)
            .exists()
    );
    server.abort();
}

#[tokio::test]
async fn encrypted_recovery_points_cross_the_outbound_relay_and_reject_incomplete_publication() {
    use axum::response::IntoResponse;
    use leo_agent_manager::{
        auth,
        config::{id, now},
        nodes::{backups, relay, snapshots},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let owner = Owner::at(address.to_string()).await;
    let node = id();
    let attempt = id();
    let run = id();
    let token = auth::token();
    owner
        .service
        .store
        .put("nodes", json!({"id":node,"revoked":false}))
        .await
        .unwrap();
    owner
        .service
        .store
        .set(
            &format!("node-token:{}", auth::digest(&token)),
            json!(node),
            None,
        )
        .await
        .unwrap();
    let record = json!({"id":run,"taskId":"fixture","createdAt":0,"status":"running","sessionId":"synthetic-native-session"});
    let owned = record.clone();
    owner
        .service
        .store
        .write(move |db| db.add_run(&owned, None))
        .await
        .unwrap();
    owner
        .service
        .store
        .set(
            &format!("run-checkpoint:{run}"),
            json!({"nodeId":node,"runnerId":attempt}),
            None,
        )
        .await
        .unwrap();
    let source = owner._root.path().join("source-disk");
    let mut original = vec![0u8; 8 * 1024 * 1024];
    original[..27].copy_from_slice(b"private-untracked-contents!");
    original[4 * 1024 * 1024] = 1;
    std::fs::write(&source, &original).unwrap();
    let mut manifest = snapshots::index(&source).await.unwrap();
    manifest["capturedAt"] = now().into();
    manifest["runtime"] = json!({"runtimeId":"fixture"});
    let snapshot = id();
    let state = Arc::new(tokio::sync::Mutex::new(manifest));
    let corrupt = Arc::new(AtomicBool::new(false));
    let fixture = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let controller = fixture.local_addr().unwrap();
    let (disk, data, bad) = (source.clone(), state.clone(), corrupt.clone());
    let controller_app = axum::Router::new().fallback(axum::routing::any(
        move |request: axum::extract::Request| {
            let (disk, data, bad, snapshot) =
                (disk.clone(), data.clone(), bad.clone(), snapshot.clone());
            async move {
                assert_eq!(
                    request.headers()["authorization"],
                    "Bearer controller-fixture"
                );
                if request.method() == "DELETE" {
                    return axum::Json(json!({"ok":true})).into_response();
                }
                if request.uri().path().ends_with("/snapshot") {
                    return axum::Json(json!({"id":snapshot,"manifest":data.lock().await.clone()}))
                        .into_response();
                }
                let hash = request.uri().path().rsplit('/').next().unwrap();
                let mut bytes = snapshots::block(&disk, &*data.lock().await, hash)
                    .await
                    .unwrap();
                if bad.load(Ordering::SeqCst) {
                    bytes[0] ^= 255;
                }
                bytes.into_response()
            }
        },
    ));
    let controller_task =
        tokio::spawn(async move { axum::serve(fixture, controller_app).await.unwrap() });
    let app = owner.app.clone();
    let master_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let stop = tokio_util::sync::CancellationToken::new();
    let relay_task = tokio::spawn(relay::run(
        format!("http://{address}").parse().unwrap(),
        token,
        format!("http://{controller}"),
        "controller-fixture".into(),
        stop.clone(),
    ));
    let first = backups::capture(&owner.service, &record).await.unwrap();
    assert_eq!(first["uploadedBytes"], 8 * 1024 * 1024);
    let retained = owner
        .service
        .store
        .get("node-backups", first["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let first_manifest = backups::manifest(&owner.service, &retained).await.unwrap();
    let restored = owner._root.path().join("restored-disk");
    snapshots::restore(&restored, &first_manifest, |hash| {
        let (service, backup) = (owner.service.clone(), retained.clone());
        async move { backups::read_block(&service, &backup, &hash).await }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(restored).unwrap(), original);
    let block = first_manifest["blocks"][0]["hash"].as_str().unwrap();
    let encrypted = std::fs::read(
        owner
            .service
            .config
            .data_dir
            .join("node-backups")
            .join(&run)
            .join("blocks")
            .join(block),
    )
    .unwrap();
    assert!(
        !encrypted
            .windows(27)
            .any(|v| v == b"private-untracked-contents!")
    );
    original[4 * 1024 * 1024] = 2;
    std::fs::write(&source, &original).unwrap();
    let mut second = snapshots::index(&source).await.unwrap();
    second["capturedAt"] = now().into();
    second["runtime"] = json!({"runtimeId":"fixture"});
    *state.lock().await = second;
    corrupt.store(true, Ordering::SeqCst);
    assert!(backups::capture(&owner.service, &record).await.is_err());
    assert_eq!(
        owner
            .service
            .store
            .list("node-backups")
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        owner.service.store.run(&run).await.unwrap()["backup"]["id"],
        first["id"]
    );
    corrupt.store(false, Ordering::SeqCst);
    let second = backups::capture(&owner.service, &record).await.unwrap();
    assert_eq!(second["uploadedBytes"], 4 * 1024 * 1024);
    stop.cancel();
    relay_task.await.unwrap().unwrap();
    controller_task.abort();
    master_task.abort();
}
