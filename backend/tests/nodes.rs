use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use leo_agent_manager::{config::Config, http::router, service::Service};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Owner {
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
