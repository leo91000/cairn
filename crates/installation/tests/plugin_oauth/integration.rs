//! Exercise security boundaries where the two plugin API families meet.
use super::*;

#[tokio::test]
async fn scoped_transfer_bodies_cannot_replace_verified_request_bindings() {
    let (_root, s, instance, resource, _path, app) = configured_fixture(false, false).await;
    let base = registry::active(&s, "example").await.unwrap();
    let mut manifest = serde_json::to_value(base.manifest).unwrap();
    manifest["version"] = json!("2");
    manifest["capabilities"]
        .as_array_mut()
        .unwrap()
        .push(json!("files"));
    manifest["resources"][0]["settings"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "key": "token", "title": "Token", "type": "secret"
        }));
    manifest["operations"][0]["scopes"] = json!(["channel"]);
    manifest["tools"][0]["inputSchema"]["required"] = json!(["resource", "channel"]);
    manifest["tools"][0]["inputSchema"]["properties"]["channel"] = json!({"type": "string"});
    manifest["operations"][0]["requests"] = json!([
        {"method": "GET", "host": "api.example.test", "path": "/messages"},
        {"method": "POST", "host": "api.example.test", "path": "/read-messages", "bindings": {"channel": "channel"}}
    ]);
    let installed = registry::install(&s, &json!({
        "manifest": manifest,
        "source": r#"return {tool = function(event)
            local auth = {resource = event.input.resource, secret = 'token', prefix = 'Bearer '}
            local downloaded = cairn.http.request({url = 'https://api.example.test/messages', auth = auth,
                responseFile = {name = 'payload.json'}}).file
            local request = {method = 'POST', auth = auth, headers = {['content-type'] = 'application/json'},
                bodyFile = {id = downloaded.id, format = event.input.format}}
            if event.input.format == 'base64url' then
                request.url = 'https://api.example.test/read-messages'
                request.json = {channel = event.input.channel}
                request.bodyFile.jsonPointer = '/channel'
            else
                request.url = 'https://api.example.test/read-messages?channel=' .. event.input.channel
            end
            return cairn.http.request(request).json
        end}"#,
    })).await.unwrap();
    registry::approve(&s, "example", "2", text(&installed, "digest"))
        .await
        .unwrap();
    let previous = s
        .store
        .get("plugin-resources", &resource)
        .await
        .unwrap()
        .unwrap();
    resources::save(
        &s,
        "example",
        &instance,
        "workspace",
        &json!({
            "revision": previous["revision"], "name": "Engineering", "settings": {},
            "secrets": {"token": "scoped-transfer-fixture"}
        }),
        Some(&resource),
    )
    .await
    .unwrap();
    let grant_path = format!(
        "/api/plugins/example/connections/{instance}/resources/workspace/{resource}/grants"
    );
    let response = common::send(
        &app,
        request(
            "POST",
            &grant_path,
            json!({"agentId": MAIN_AGENT_ID, "profile": "read", "scopes": {"channel": ["C1"]}}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let posts = Arc::new(AtomicUsize::new(0));
    let count = posts.clone();
    let provider = axum::Router::new()
        .route(
            "/messages",
            axum::routing::get(|| async { axum::Json(json!({"channel": "C2"})) }),
        )
        .route(
            "/read-messages",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    axum::Json(body)
                }
            }),
        );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let task = s
        .task(
            json!({"name": "Read C1", "prompt": "Read C1", "agentId": MAIN_AGENT_ID}),
            None,
        )
        .await
        .unwrap();
    let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&run, "id"),
            json!({"status": "running", "isolated": true}),
        )
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let mut escaped = Vec::new();
    for format in ["raw", "json", "base64url"] {
        let result = agents::call_tool(
            &s,
            bearer,
            "example_read",
            &json!({
                "resource": resource, "channel": "C1", "format": format
            }),
        )
        .await;
        match result {
            Err(error) => assert_eq!(error.status, 403),
            result => escaped.push((format, format!("{result:?}"))),
        }
    }
    assert!(
        escaped.is_empty(),
        "Opaque bodies escaped native channel binding: {escaped:?}"
    );
    assert_eq!(posts.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn plugin_management_retains_the_chat_members_original_access_generation() {
    let (_root, s, instance, _resource, _path, app) = configured_fixture(false, false).await;
    let base = registry::active(&s, "example").await.unwrap();
    let mut manifest = serde_json::to_value(base.manifest).unwrap();
    manifest["version"] = json!("2");
    manifest["instanceTest"] = json!(true);
    let installed = registry::install(
        &s,
        &json!({
            "manifest": manifest,
            "source": r#"return {test = function()
            cairn.http.request({url = 'https://api.example.test/messages'})
            return {ok = true, message = 'Checked'}
        end}"#
        }),
    )
    .await
    .unwrap();
    registry::approve(&s, "example", "2", text(&installed, "digest"))
        .await
        .unwrap();
    connections::save(
        &s,
        "example",
        &json!({"name": "Example app", "enabled": true,
        "settings": {"clientId": "client-fixture"}, "agentIds": [MAIN_AGENT_ID]}),
        Some(&instance),
    )
    .await
    .unwrap();
    s.agent(
        json!({"name": "Main agent", "access": {"installation": true}}),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let revoked = Arc::new(AtomicBool::new(false));
    let member = revoked.clone();
    let authority = axum::Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(move || {
            let member = member.clone();
            async move {
                axum::Json(json!({
                    "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
                    "members": if member.load(Ordering::SeqCst) {json!([])} else {json!([
                        {"account_id": "alice", "access_id": "alice-membership-1"}
                    ])}
                }))
            }
        }),
    );
    let (origin, authority_server) = common::serve_locally(authority).await;
    let identity_file = s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: Value =
        serde_json::from_slice(&std::fs::read(&identity_file).unwrap()).unwrap();
    identity["origin"] = json!(origin);
    std::fs::write(identity_file, serde_json::to_vec(&identity).unwrap()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let hold = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let provider_hold = hold.clone();
    let provider_entered = entered.clone();
    let provider_resume = resume.clone();
    let provider = axum::Router::new().route(
        "/messages",
        axum::routing::get(move || {
            let count = count.clone();
            let hold = provider_hold.clone();
            let entered = provider_entered.clone();
            let resume = provider_resume.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                if hold.swap(false, Ordering::SeqCst) {
                    entered.notify_one();
                    resume.notified().await;
                }
                axum::Json(json!({"ok": true}))
            }
        }),
    );
    let (origin, provider_server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let chat = s
        .chat_create(json!({"agentId": MAIN_AGENT_ID}))
        .await
        .unwrap();
    let path = format!("/api/chats/{}/messages", text(&chat, "id"));
    let response = common::send(
        &app,
        member_request(
            "POST",
            &path,
            json!({"id": cairn_installation::config::id(), "text": "Check this granted account"}),
            "alice",
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    s.chat_tick(&HashSet::new()).await.unwrap();
    let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&detail["run"], "id"),
            json!({"status": "running", "isolated": true}),
        )
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let invoke = || {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": common::mcp_arguments(
            "manage_plugin", &json!({"pluginId": "example", "action": "test", "instanceId": instance}))});
        common::request("POST", "/mcp-installation")
            .header("authorization", format!("Bearer {bearer}"))
            .body(axum::body::Body::from(body.to_string()))
            .unwrap()
    };
    let before = common::read_json(common::send(&app, invoke()).await).await;
    assert_ne!(before["result"]["isError"], true, "{before}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    revoked.store(true, Ordering::SeqCst);
    let after = common::read_json(common::send(&app, invoke()).await).await;
    assert_eq!(
        after["result"]["isError"], true,
        "A revoked member's management call escaped: {after}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "Revoked requester reached provider I/O"
    );
    revoked.store(false, Ordering::SeqCst);
    hold.store(true, Ordering::SeqCst);
    let revoke_during_io = async {
        entered.notified().await;
        revoked.store(true, Ordering::SeqCst);
        resume.notify_one();
    };
    let (during, ()) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(common::send(&app, invoke()), revoke_during_io)
    })
    .await
    .unwrap();
    let during = common::read_json(during).await;
    assert_eq!(
        during["result"]["isError"], true,
        "Revoked requester received a slow provider result: {during}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    provider_server.abort();
    authority_server.abort();
}
