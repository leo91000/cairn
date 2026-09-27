use leo_agent_manager::{config::Config, mcp_client::Client, network, service::Service};
use serde_json::json;
use tempfile::TempDir;
fn config(root: &TempDir) -> Config {
    Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().to_owned()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        setup_token: String::new(),
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        gh_bin: "gh".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    }
}

#[tokio::test]
async fn run_history_pages_fit_the_mcp_transport_without_losing_events() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mut settings = config(&root);
    settings.public_url = origin.clone();
    let s = Service::new(settings).await.unwrap();
    let router = leo_agent_manager::http::router(s.clone()).await.unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let token = s
        .auth
        .personal("History reader", vec!["read"])
        .await
        .unwrap();
    let task = s.task(json!({"name":"History","agentId":leo_agent_manager::config::MAIN_AGENT_ID,"prompt":"Read history","worktree":false}),None).await.unwrap();
    let run = s
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    let run_id = run["id"].as_str().unwrap();
    let output = "x".repeat(128 * 1024);
    for index in 0..40 {
        s.store.event(run_id, "item.completed", "tool output", Some(json!({"item":{"id":index,"type":"command_execution","aggregated_output":output}}))).await.unwrap();
    }
    let http = reqwest::Client::new();
    let request = |name: &str, arguments: serde_json::Value| {
        http.post(format!("{origin}/mcp"))
        .bearer_auth(token["token"].as_str().unwrap())
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
    };
    let mut after = 0;
    let mut seen = Vec::new();
    loop {
        let bytes = request("get_run", json!({"runId":run_id,"after":after}))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert!(
            bytes.len() < 8 * 1024 * 1024,
            "MCP response exceeds 8 MB: {} bytes",
            bytes.len()
        );
        let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_ne!(response["result"]["isError"], true);
        let page = &response["result"]["structuredContent"]["result"];
        for event in page["events"].as_array().unwrap() {
            if event["type"] == "item.completed" {
                assert_eq!(event["payload"]["item"]["aggregated_output"], output);
                seen.push(event["payload"]["item"]["id"].as_i64().unwrap());
            }
        }
        let next = page["nextAfter"].as_i64().unwrap();
        assert!(next > after, "pagination must make progress");
        after = next;
        if page["hasMore"] == false {
            break;
        }
        assert!(seen.len() <= 40, "pagination repeated events");
    }
    assert_eq!(seen, (0..40).collect::<Vec<_>>());

    // One event can exceed the transport ceiling on its own. Its original content
    // must remain available, including escaped characters and multi-byte Unicode.
    let large = "\0\"\\🦊é".repeat(250_000);
    let payload = json!({"item":{"type":"agent_message","text":large}});
    s.store
        .event(
            run_id,
            "item.completed",
            "Large answer",
            Some(payload.clone()),
        )
        .await
        .unwrap();
    s.store
        .event(run_id, "turn.completed", "Done", None)
        .await
        .unwrap();
    let bytes = request("get_run", json!({"runId":run_id,"after":after}))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert!(
        bytes.len() < 8 * 1024 * 1024,
        "single event exceeds transport: {} bytes",
        bytes.len()
    );
    let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let page = &response["result"]["structuredContent"]["result"];
    let event = &page["events"][0];
    assert_eq!(event["truncated"], true);
    let event_id = event["id"].as_i64().unwrap();
    let mut offset = 0;
    let mut digest = serde_json::Value::Null;
    let mut original = String::new();
    loop {
        let mut arguments = json!({"runId":run_id,"eventId":event_id,"offset":offset});
        if !digest.is_null() {
            arguments["sha256"] = digest.clone();
        }
        let bytes = request("read_run_content", arguments)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert!(bytes.len() < 8 * 1024 * 1024);
        let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_ne!(response["result"]["isError"], true, "{response}");
        let chunk = &response["result"]["structuredContent"]["result"];
        digest = chunk["sha256"].clone();
        original.push_str(chunk["data"].as_str().unwrap());
        if chunk["nextOffset"].is_null() {
            break;
        }
        let next = chunk["nextOffset"].as_u64().unwrap();
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&original).unwrap()["payload"],
        payload
    );
    let response: serde_json::Value = request("read_run_content", json!({"runId":run_id,"eventId":event_id,"offset":original.find('🦊').unwrap()+1,"sha256":digest}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(
        response["result"]["isError"], true,
        "offset inside UTF-8 must be rejected"
    );
    let response: serde_json::Value =
        request("get_run", json!({"runId":run_id,"after":page["nextAfter"]}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let page = &response["result"]["structuredContent"]["result"];
    assert_eq!(page["events"][0]["type"], "turn.completed");
    assert_eq!(page["hasMore"], false);

    let other_task = s.task(json!({"name":"Other history","agentId":leo_agent_manager::config::MAIN_AGENT_ID,"prompt":"Other run","worktree":false}),None).await.unwrap();
    let other_run = s
        .enqueue(other_task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    let response: serde_json::Value = request(
        "read_run_content",
        json!({"runId":other_run["id"],"eventId":event_id,"offset":0}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(
        response["result"]["isError"], true,
        "events must belong to the requested run"
    );

    // Run metadata (for example a snapshot of many skills) needs the same escape
    // hatch. A changing run must never silently mix two versions across chunks.
    s.store
        .patch_run(run_id, json!({"summary":"\0".repeat(800_000)}))
        .await
        .unwrap();
    let bytes = request("get_run", json!({"runId":run_id,"after":page["nextAfter"]}))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert!(bytes.len() < 8 * 1024 * 1024);
    let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let empty = &response["result"]["structuredContent"]["result"];
    assert_eq!(empty["events"], json!([]));
    assert_eq!(empty["nextAfter"], page["nextAfter"]);
    assert_eq!(empty["hasMore"], false);
    assert_eq!(
        response["result"]["structuredContent"]["result"]["run"]["truncated"],
        true
    );
    let response: serde_json::Value =
        request("read_run_content", json!({"runId":run_id,"offset":0}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let chunk = &response["result"]["structuredContent"]["result"];
    assert!(chunk["nextOffset"].as_u64().unwrap() > 0);
    s.store
        .patch_run(run_id, json!({"summary":"Updated result"}))
        .await
        .unwrap();
    let response: serde_json::Value = request(
        "read_run_content",
        json!({"runId":run_id,"offset":chunk["nextOffset"],"sha256":chunk["sha256"]}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(
        response["result"]["isError"], true,
        "changed metadata must require a fresh read"
    );
    server.abort();
}
#[tokio::test]
async fn stdio_discovers_and_calls_the_official_sdk_fixture() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/mcp.mjs");
    let item = s.mcps.save(&s, json!({"name":"Command fixture","transport":"stdio","command":"node","args":[fixture],"env":{"TEST_PREFIX":"configured:"}}), None).await.unwrap();
    let id = item["id"].as_str().unwrap();
    let tested = s.mcps.test(&s, id).await.unwrap();
    assert_eq!(tested["state"], "connected", "{tested}");
    assert_eq!(tested["tools"][0]["name"], "fixture_echo");
    assert_eq!(tested["envKeys"], json!(["TEST_PREFIX"]));
    assert!(!tested.to_string().contains("configured:"));
    let mut client = Client::connect(&s, &s.mcps.get(&s, id).await.unwrap())
        .await
        .unwrap();
    let result = client
        .request(
            "tools/call",
            json!({"name":"fixture_echo","arguments":{"message":"hello"}}),
        )
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "configured:hello");
    client.close().await;
    s.mcps.disconnect(&s, id, false).await.unwrap();
    assert!(
        s.mcps
            .secrets(&s, id)
            .await
            .unwrap()
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn legacy_stdio_servers_can_exit_on_the_modern_probe() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    let script = "const{createInterface}=require('node:readline');createInterface({input:process.stdin}).on('line',line=>{const r=JSON.parse(line);if(r.method==='server/discover')process.exit(1);if(!r.id)return;const result=r.method==='initialize'?{protocolVersion:'2025-11-25',capabilities:{tools:{}},serverInfo:{name:'legacy',version:'1'}}:{tools:[{name:'legacy_tool',inputSchema:{type:'object'}}]};console.log(JSON.stringify({jsonrpc:'2.0',id:r.id,result}));});";
    let item = s
        .mcps
        .save(
            &s,
            json!({"name":"Legacy","transport":"stdio","command":"node","args":["-e",script]}),
            None,
        )
        .await
        .unwrap();
    let mut client = Client::connect(&s, &item).await.unwrap();
    let tools = client.discover().await.unwrap();
    assert_eq!(tools[0]["name"], "legacy_tool");
    client.close().await;
}
#[tokio::test]
async fn network_guards_block_metadata_even_when_private_network_is_allowed() {
    for endpoint in [
        "http://169.254.169.254/latest/meta-data",
        "http://[fd00:ec2::254]/",
        "http://[::ffff:169.254.169.254]/",
    ] {
        let error = network::fetch(
            endpoint,
            reqwest::Method::GET,
            Default::default(),
            None,
            true,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            error.message,
            "Instance metadata endpoints are unavailable."
        );
    }
    for endpoint in [
        "http://localhost:1/",
        "https://127.0.0.1:1/",
        "https://[::ffff:127.0.0.1]:1/",
    ] {
        let error = network::fetch(
            endpoint,
            reqwest::Method::GET,
            Default::default(),
            None,
            false,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            error.message,
            "Private network access is disabled for this connection."
        );
    }
}

#[tokio::test]
async fn agents_manage_connections_through_the_self_gateway_without_deadlocks_or_stale_grants() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mut config = config(&root);
    config.public_url = origin.clone();
    let s = Service::new(config).await.unwrap();
    let router = leo_agent_manager::http::router(s.clone()).await.unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let owner = s
        .auth
        .personal("Self access", vec!["read", "manage", "run"])
        .await
        .unwrap();
    let connection = s.mcps.save(&s,json!({"name":"Self","url":format!("{origin}/mcp"),"auth":"bearer","token":owner["token"],"allowPrivateNetwork":true}),None).await.unwrap();
    let task = s.task(json!({"name":"Manage","agentId":leo_agent_manager::config::MAIN_AGENT_ID,"prompt":"Manage connections","worktree":false}),None).await.unwrap();
    let run = s
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    let run_id = run["id"].as_str().unwrap();
    s.store
        .patch_run(run_id, json!({"status":"running"}))
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let token = configuration["env"]["LEO_MCP_RUN_TOKEN"].as_str().unwrap();
    let endpoint = format!(
        "{origin}/mcp-gateway/{}",
        connection["id"].as_str().unwrap()
    );
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let request = |name: &str, arguments: serde_json::Value| {
        http.post(&endpoint).bearer_auth(token).header("mcp-protocol-version","2025-11-25").json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
    };
    let created: serde_json::Value = request(
        "create_mcp",
        json!({"name":"Created through agent","transport":"stdio","command":"node"}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_ne!(created["result"]["isError"], true, "{created}");
    assert_eq!(s.mcps.list(&s).await.unwrap().len(), 2);
    let recursive: serde_json::Value = request("test_mcp", json!({"id":connection["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(recursive["result"]["isError"], true, "{recursive}");
    s.mcps.revoke_run(&s, run_id).await.unwrap();
    assert_eq!(
        request("list_mcps", json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    server.abort();
}
#[tokio::test]
async fn official_clients_negotiate_modern_and_legacy_protocols_and_enforce_scopes() {
    use tokio::io::AsyncWriteExt;
    let root = TempDir::new().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mut config = config(&root);
    config.public_url = url.clone();
    let s = Service::new(config).await.unwrap();
    let router = leo_agent_manager::http::router(s.clone()).await.unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let token = s.auth.personal("Test client", vec!["read"]).await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let script = r#"import{Client,StreamableHTTPClientTransport}from'@modelcontextprotocol/client';let data='';for await(const chunk of process.stdin)data+=chunk;const{url,token}=JSON.parse(data);for(const version of ['2026-07-28','2025-11-25','2025-06-18']){const client=new Client({name:'rust-fixture',version:'1'},version==='2026-07-28'?{versionNegotiation:{mode:{pin:version}}}:{supportedProtocolVersions:[version]});await client.connect(new StreamableHTTPClientTransport(new URL(url+'/mcp'),{requestInit:{headers:{authorization:'Bearer '+token}}}));const catalog=await client.listTools();if(!catalog.tools.some(t=>t.name==='list_agents'))throw Error('missing tool');const agents=await client.callTool({name:'list_agents',arguments:{}});if(agents.isError||agents.structuredContent.result.length!==1)throw Error('invalid result '+JSON.stringify(agents));const denied=await client.callTool({name:'create_task',arguments:{name:'Denied',prompt:'No'}});if(!denied.isError||!denied._meta['mcp/www_authenticate'])throw Error('scope bypass');await client.close();}process.stdout.write('ok');"#;
    let mut child = tokio::process::Command::new("node")
        .args(["--input-type=module", "-e", script])
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap(),
        )
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
        .write_all(json!({"url":url,"token":token}).to_string().as_bytes())
        .await
        .unwrap();
    let output = tokio::time::timeout(std::time::Duration::from_secs(20), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"ok");
    server.abort();
}
#[tokio::test]
async fn oauth_consent_pkce_callback_replay_and_refresh_use_the_existing_provider() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let script = r#"import{mcpProvider}from'./tests/mcp-provider.ts';import{createInterface}from'node:readline';const provider=await mcpProvider();console.log(provider.origin);for await(const line of createInterface({input:process.stdin})){if(line==='expire'){provider.expire();console.log('expired');}else if(line==='stats'){console.log(JSON.stringify({refreshes:provider.refreshes,exchanges:provider.exchanges}));}else break;}await provider.close();"#;
    let mut provider = tokio::process::Command::new("node")
        .args(["--import", "tsx", "--input-type=module", "-e", script])
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap(),
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = provider.stdin.take().unwrap();
    let mut output = BufReader::new(provider.stdout.take().unwrap()).lines();
    let origin = output.next_line().await.unwrap().unwrap();
    let root = TempDir::new().unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    let item = s.mcps.save(&s, json!({"name":"OAuth fixture","url":format!("{origin}/mcp"),"auth":"oauth","allowPrivateNetwork":true}), None).await.unwrap();
    let id = item["id"].as_str().unwrap();
    let consent = s.mcps.connect(&s, id, "fixture-session").await.unwrap();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = http
        .get(consent["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    let callback = url::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
    let params = callback
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect::<std::collections::HashMap<_, _>>();
    assert!(!s.mcps.capture_native_callback(&s, &params).await.unwrap());
    assert!(s.mcps.callback(&s, &params, "wrong-session").await.is_err());
    assert_eq!(
        s.mcps
            .callback(&s, &params, "fixture-session")
            .await
            .unwrap(),
        "connected"
    );
    assert!(
        s.mcps
            .callback(&s, &params, "fixture-session")
            .await
            .is_err()
    );
    stdin.write_all(b"expire\n").await.unwrap();
    assert_eq!(output.next_line().await.unwrap().unwrap(), "expired");
    let tested = s.mcps.test(&s, id).await.unwrap();
    assert_eq!(tested["state"], "connected", "{tested}");
    stdin.write_all(b"stats\n").await.unwrap();
    let stats: serde_json::Value =
        serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(stats, json!({"refreshes":1,"exchanges":1}));
    // A native session can complete OAuth despite an unrelated (or absent) browser cookie.
    let native = s
        .mcps
        .connect_native(&s, id, "native-session")
        .await
        .unwrap();
    assert_eq!(
        s.mcps
            .finish_native_callback(&s, id, "native-session")
            .await
            .unwrap(),
        json!({"pending":true})
    );
    let response = http
        .get(native["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    let callback = url::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
    let params = callback
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect::<std::collections::HashMap<_, _>>();
    assert!(s.mcps.capture_native_callback(&s, &params).await.unwrap());
    let mut replay = params.clone();
    replay.insert("code".into(), "attacker-replacement".into());
    assert!(s.mcps.capture_native_callback(&s, &replay).await.unwrap());
    assert_eq!(
        s.mcps
            .finish_native_callback(&s, id, "browser-session")
            .await
            .unwrap(),
        json!({"pending":false,"result":"expired"})
    );
    stdin.write_all(b"stats\n").await.unwrap();
    let stats: serde_json::Value =
        serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        stats["exchanges"], 1,
        "Capturing a callback must not exchange credentials"
    );
    assert_eq!(
        s.mcps
            .finish_native_callback(&s, id, "native-session")
            .await
            .unwrap(),
        json!({"pending":false,"result":"connected"})
    );
    assert!(!s.mcps.capture_native_callback(&s, &params).await.unwrap());
    assert_eq!(
        s.mcps
            .finish_native_callback(&s, id, "native-session")
            .await
            .unwrap()["result"],
        "expired"
    );
    stdin.write_all(b"stop\n").await.unwrap();
    drop(stdin);
    if tokio::time::timeout(std::time::Duration::from_secs(2), provider.wait())
        .await
        .is_err()
    {
        provider.kill().await.unwrap();
    }
}
