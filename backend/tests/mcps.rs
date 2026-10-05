mod common;

use common::browser_http::router;

use axum::{body::Body, http::StatusCode};
use leo_agent_manager::{
    auth::{InstallationIdentity, InstallationRole},
    config::{Config, MAIN_AGENT_ID},
    mcp_client::Client,
    network,
    run_status::RunStatus,
    service::Service,
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    task::JoinHandle,
};

/// Every MCP response must fit the 8 MB transport.
const TRANSPORT_LIMIT: usize = 8 * 1024 * 1024;

fn config(root: &TempDir) -> Config {
    Config {
        ..common::config(root.path())
    }
}

/// A service served over HTTP on the origin it advertises.
async fn served(root: &TempDir) -> (Arc<Service>, String, JoinHandle<()>) {
    let (listener, address) = common::bind().await;
    let origin = format!("http://{address}");
    let s = Service::new(Config {
        public_url: origin.clone(),
        ..config(root)
    })
    .await
    .unwrap();
    let server = common::serve(listener, router(s.clone()).await.unwrap());
    (s, origin, server)
}

/// Enqueues a manual run of a new task of the main agent.
async fn manual_run(s: &Service, name: &str, prompt: &str) -> Value {
    let task = json!({
        "name": name,
        "agentId": MAIN_AGENT_ID,
        "prompt": prompt,
        "worktree": false,
    });
    let task = s.task(task, None).await.unwrap();
    s.enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap()
}

fn tool_call(name: &str, arguments: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    })
}

/// Calls the tools of the `/mcp` endpoint with a personal access token.
struct Mcp {
    http: reqwest::Client,
    endpoint: String,
    token: String,
}

impl Mcp {
    async fn bytes(&self, name: &str, arguments: Value) -> Vec<u8> {
        let bytes = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .json(&tool_call(name, &arguments))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        bytes.to_vec()
    }

    /// Calls a tool and checks the response fits the transport.
    async fn bounded(&self, name: &str, arguments: Value) -> Value {
        let bytes = self.bytes(name, arguments).await;
        assert!(
            bytes.len() < TRANSPORT_LIMIT,
            "MCP response exceeds 8 MB: {} bytes",
            bytes.len()
        );
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn call(&self, name: &str, arguments: Value) -> Value {
        serde_json::from_slice(&self.bytes(name, arguments).await).unwrap()
    }
}

fn result(response: &Value) -> &Value {
    &response["result"]["structuredContent"]["result"]
}

/// Pages through every event of `run` and returns the cursor after the last one.
async fn read_every_page(mcp: &Mcp, run: &str, output: &str) -> i64 {
    let mut after = 0;
    let mut seen = Vec::new();
    loop {
        let response = mcp
            .bounded("get_run", json!({ "runId": run, "after": after }))
            .await;
        assert_ne!(response["result"]["isError"], true);
        let page = result(&response);
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
    after
}

/// Reads a truncated event back in chunks, and returns its content and digest.
async fn read_whole_event(mcp: &Mcp, run: &str, event: i64) -> (String, Value) {
    let mut offset = 0;
    let mut digest = Value::Null;
    let mut original = String::new();
    loop {
        let mut arguments = json!({ "runId": run, "eventId": event, "offset": offset });
        if !digest.is_null() {
            arguments["sha256"] = digest.clone();
        }
        let response = mcp.bounded("read_run_content", arguments).await;
        assert_ne!(response["result"]["isError"], true, "{response}");
        let chunk = result(&response);
        digest = chunk["sha256"].clone();
        original.push_str(chunk["data"].as_str().unwrap());
        if chunk["nextOffset"].is_null() {
            return (original, digest);
        }
        let next = chunk["nextOffset"].as_u64().unwrap();
        assert!(next > offset);
        offset = next;
    }
}

/// Run metadata (for example a snapshot of many skills) needs the same escape
/// hatch. A changing run must never silently mix two versions across chunks.
async fn assert_large_metadata_is_chunked_consistently(
    s: &Service,
    mcp: &Mcp,
    run: &str,
    cursor: &Value,
) {
    s.store
        .patch_run(run, json!({ "summary": "\0".repeat(800_000) }))
        .await
        .unwrap();
    let response = mcp
        .bounded("get_run", json!({ "runId": run, "after": cursor }))
        .await;
    let empty = result(&response);
    assert_eq!(empty["events"], json!([]));
    assert_eq!(empty["nextAfter"], *cursor);
    assert_eq!(empty["hasMore"], false);
    assert_eq!(empty["run"]["truncated"], true);
    let response = mcp
        .call("read_run_content", json!({ "runId": run, "offset": 0 }))
        .await;
    let chunk = result(&response);
    assert!(chunk["nextOffset"].as_u64().unwrap() > 0);
    s.store
        .patch_run(run, json!({ "summary": "Updated result" }))
        .await
        .unwrap();
    let continuation = json!({
        "runId": run,
        "offset": chunk["nextOffset"],
        "sha256": chunk["sha256"],
    });
    assert_eq!(
        mcp.call("read_run_content", continuation).await["result"]["isError"],
        true,
        "changed metadata must require a fresh read"
    );
}

#[tokio::test]
async fn run_history_pages_fit_the_mcp_transport_without_losing_events() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let (s, origin, server) = served(&root).await;
    let token = s
        .auth
        .personal("History reader", vec!["read"])
        .await
        .unwrap();
    let run = manual_run(&s, "History", "Read history").await;
    let run_id = run["id"].as_str().unwrap();
    let output = "x".repeat(128 * 1024);
    for index in 0..40 {
        let item = json!({
            "item": { "id": index, "type": "command_execution", "aggregated_output": output },
        });
        s.store
            .event(run_id, "item.completed", "tool output", Some(item))
            .await
            .unwrap();
    }
    let mcp = Mcp {
        http: reqwest::Client::new(),
        endpoint: format!("{origin}/mcp"),
        token: token["token"].as_str().unwrap().to_owned(),
    };
    let after = read_every_page(&mcp, run_id, &output).await;

    // One event can exceed the transport ceiling on its own. Its original content
    // must remain available, including escaped characters and multi-byte Unicode.
    let large = "\0\"\\🦊é".repeat(250_000);
    let payload = json!({ "item": { "type": "agent_message", "text": large } });
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
    let response = mcp
        .bounded("get_run", json!({ "runId": run_id, "after": after }))
        .await;
    let page = result(&response);
    let event = &page["events"][0];
    assert_eq!(event["truncated"], true);
    let event_id = event["id"].as_i64().unwrap();
    let (original, digest) = read_whole_event(&mcp, run_id, event_id).await;
    assert_eq!(
        serde_json::from_str::<Value>(&original).unwrap()["payload"],
        payload
    );
    let inside_character = json!({
        "runId": run_id,
        "eventId": event_id,
        "offset": original.find('🦊').unwrap() + 1,
        "sha256": digest,
    });
    assert_eq!(
        mcp.call("read_run_content", inside_character).await["result"]["isError"],
        true,
        "offset inside UTF-8 must be rejected"
    );
    let response = mcp
        .call(
            "get_run",
            json!({ "runId": run_id, "after": page["nextAfter"] }),
        )
        .await;
    let last = result(&response);
    assert_eq!(last["events"][0]["type"], "turn.completed");
    assert_eq!(last["hasMore"], false);

    let other_run = manual_run(&s, "Other history", "Other run").await;
    let foreign = json!({ "runId": other_run["id"], "eventId": event_id, "offset": 0 });
    assert_eq!(
        mcp.call("read_run_content", foreign).await["result"]["isError"],
        true,
        "events must belong to the requested run"
    );
    assert_large_metadata_is_chunked_consistently(&s, &mcp, run_id, &last["nextAfter"]).await;
    server.abort();
}

#[tokio::test]
async fn stdio_discovers_and_calls_the_official_sdk_fixture() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    let fixture = common::fixture_path("mcp.mjs");
    let connection = json!({
        "name": "Command fixture",
        "transport": "stdio",
        "command": "node",
        "args": [fixture],
        "env": { "TEST_PREFIX": "configured:" },
    });
    let item = s.mcps.save(&s, connection, None).await.unwrap();
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
            json!({ "name": "fixture_echo", "arguments": { "message": "hello" } }),
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

/// A stdio server that answers the legacy handshake but exits on `server/discover`.
const LEGACY_SERVER: &str = r"
const { createInterface } = require('node:readline');
createInterface({ input: process.stdin }).on('line', line => {
    const r = JSON.parse(line);
    if (r.method === 'server/discover') process.exit(1);
    if (!r.id) return;
    const result = r.method === 'initialize'
        ? { protocolVersion: '2025-11-25', capabilities: { tools: {} }, serverInfo: { name: 'legacy', version: '1' } }
        : { tools: [{ name: 'legacy_tool', inputSchema: { type: 'object' } }] };
    console.log(JSON.stringify({ jsonrpc: '2.0', id: r.id, result }));
});
";

#[tokio::test]
async fn legacy_stdio_servers_can_exit_on_the_modern_probe() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    let connection = json!({
        "name": "Legacy",
        "transport": "stdio",
        "command": "node",
        "args": ["-e", LEGACY_SERVER],
    });
    let item = s.mcps.save(&s, connection, None).await.unwrap();
    let mut client = Client::connect(&s, &item).await.unwrap();
    let tools = client.discover().await.unwrap();
    assert_eq!(tools[0]["name"], "legacy_tool");
    client.close().await;
}

async fn fetch_error(endpoint: &str, allow_private: bool) -> String {
    network::fetch(
        endpoint,
        reqwest::Method::GET,
        reqwest::header::HeaderMap::new(),
        None,
        allow_private,
    )
    .await
    .err()
    .unwrap()
    .message
}

#[tokio::test]
async fn network_guards_block_metadata_even_when_private_network_is_allowed() {
    for endpoint in [
        "http://169.254.169.254/latest/meta-data",
        "http://[fd00:ec2::254]/",
        "http://[::ffff:169.254.169.254]/",
    ] {
        assert_eq!(
            fetch_error(endpoint, true).await,
            "Instance metadata endpoints are unavailable."
        );
    }
    for endpoint in [
        "http://localhost:1/",
        "https://127.0.0.1:1/",
        "https://[::ffff:127.0.0.1]:1/",
    ] {
        assert_eq!(
            fetch_error(endpoint, false).await,
            "Private network access is disabled for this connection."
        );
    }
}

#[tokio::test]
async fn agents_manage_connections_through_the_self_gateway_without_deadlocks_or_stale_grants() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let (s, origin, server) = served(&root).await;
    let owner = s
        .auth
        .personal("Self access", vec!["read", "manage", "run"])
        .await
        .unwrap();
    let self_connection = json!({
        "name": "Self",
        "url": format!("{origin}/mcp"),
        "auth": "bearer",
        "token": owner["token"],
        "allowPrivateNetwork": true,
    });
    let connection = s.mcps.save(&s, self_connection, None).await.unwrap();
    let run = manual_run(&s, "Manage", "Manage connections").await;
    let run_id = run["id"].as_str().unwrap();
    s.store
        .patch_run(run_id, json!({ "status": RunStatus::Running }))
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let token = configuration["env"]["LEO_MCP_RUN_TOKEN"].as_str().unwrap();
    let endpoint = format!(
        "{origin}/mcp-gateway/{}",
        connection["id"].as_str().unwrap()
    );
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let request = |name: &str, arguments: Value| {
        http.post(&endpoint)
            .bearer_auth(token)
            .header("mcp-protocol-version", "2025-11-25")
            .json(&tool_call(name, &arguments))
    };
    let call = async |name: &str, arguments: Value| -> Value {
        request(name, arguments)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    };
    let created = call(
        "create_mcp",
        json!({ "name": "Created through agent", "transport": "stdio", "command": "node" }),
    )
    .await;
    assert_ne!(created["result"]["isError"], true, "{created}");
    assert_eq!(s.mcps.list(&s).await.unwrap().len(), 2);
    let recursive = call("test_mcp", json!({ "id": connection["id"] })).await;
    assert_eq!(recursive["result"]["isError"], true, "{recursive}");
    s.mcps.revoke_run(&s, run_id).await.unwrap();
    assert_eq!(
        request("list_mcps", json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    server.abort();
}

/// Connects with the official SDK client for every supported protocol version.
const OFFICIAL_CLIENTS: &str = r"
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
let data = '';
for await (const chunk of process.stdin) data += chunk;
const { url, token } = JSON.parse(data);
for (const version of ['2026-07-28', '2025-11-25', '2025-06-18']) {
    const options = version === '2026-07-28'
        ? { versionNegotiation: { mode: { pin: version } } }
        : { supportedProtocolVersions: [version] };
    const client = new Client({ name: 'rust-fixture', version: '1' }, options);
    await client.connect(new StreamableHTTPClientTransport(new URL(url + '/mcp'), {
        requestInit: { headers: { authorization: 'Bearer ' + token } },
    }));
    const catalog = await client.listTools();
    if (!catalog.tools.some(t => t.name === 'list_agents')) throw Error('missing tool');
    const agents = await client.callTool({ name: 'list_agents', arguments: {} });
    if (agents.isError || agents.structuredContent.result.length !== 1) throw Error('invalid result ' + JSON.stringify(agents));
    const denied = await client.callTool({ name: 'create_task', arguments: { name: 'Denied', prompt: 'No' } });
    if (!denied.isError || !denied._meta['mcp/www_authenticate']) throw Error('scope bypass');
    await client.close();
}
process.stdout.write('ok');
";

#[tokio::test]
async fn official_clients_negotiate_modern_and_legacy_protocols_and_enforce_scopes() {
    let root = TempDir::new().unwrap();
    let (s, url, server) = served(&root).await;
    let token = s.auth.personal("Test client", vec!["read"]).await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut child = tokio::process::Command::new("node")
        .args(["--input-type=module", "-e", OFFICIAL_CLIENTS])
        .current_dir(common::repository())
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
        .write_all(json!({ "url": url, "token": token }).to_string().as_bytes())
        .await
        .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
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

/// Runs the OAuth provider fixture and reports its counters on demand.
const OAUTH_PROVIDER: &str = r"
import { mcpProvider } from './tests/mcp-provider.ts';
import { createInterface } from 'node:readline';
const provider = await mcpProvider();
console.log(provider.origin);
for await (const line of createInterface({ input: process.stdin })) {
    if (line === 'expire') {
        provider.expire();
        console.log('expired');
    } else if (line === 'stats') {
        console.log(JSON.stringify({ refreshes: provider.refreshes, exchanges: provider.exchanges }));
    } else break;
}
await provider.close();
";

/// Follows a consent URL and returns the query of the callback it redirects to.
async fn consent_callback(http: &reqwest::Client, consent: &Value) -> HashMap<String, String> {
    let response = http
        .get(consent["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    let callback = url::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
    callback
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[tokio::test]
async fn oauth_consent_pkce_callback_replay_and_refresh_use_the_existing_provider() {
    let mut provider = tokio::process::Command::new("node")
        .args([
            "--import",
            "tsx",
            "--input-type=module",
            "-e",
            OAUTH_PROVIDER,
        ])
        .current_dir(common::repository())
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
    let connection = json!({
        "name": "OAuth fixture",
        "url": format!("{origin}/mcp"),
        "auth": "oauth",
        "allowPrivateNetwork": true,
    });
    let item = s.mcps.save(&s, connection, None).await.unwrap();
    let id = item["id"].as_str().unwrap();
    let consent = s.mcps.connect(&s, id, "fixture-session").await.unwrap();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let params = consent_callback(&http, &consent).await;
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
    let mut command = async |line: &[u8]| {
        stdin.write_all(line).await.unwrap();
        output.next_line().await.unwrap().unwrap()
    };
    assert_eq!(command(b"expire\n").await, "expired");
    let tested = s.mcps.test(&s, id).await.unwrap();
    assert_eq!(tested["state"], "connected", "{tested}");
    let stats: Value = serde_json::from_str(&command(b"stats\n").await).unwrap();
    assert_eq!(stats, json!({ "refreshes": 1, "exchanges": 1 }));
    // A native session can complete OAuth despite an unrelated (or absent) browser cookie.
    let app = router(s.clone()).await.unwrap();
    let mut request = common::request("POST", &format!("/api/mcps/{id}/connect"))
        .body(Body::from(r#"{"native":true}"#))
        .unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "native-account",
        ));
    let response = common::send(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let native = common::read_json(response).await;
    let finish = async |account: &str| {
        let mut request = common::request("POST", &format!("/api/mcps/{id}/callback"))
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(
                InstallationRole::Owner,
                account,
            ));
        let response = common::send(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        common::read_json(response).await
    };
    assert_eq!(finish("native-account").await, json!({ "pending": true }));
    let params = consent_callback(&http, &native).await;
    assert!(s.mcps.capture_native_callback(&s, &params).await.unwrap());
    let mut replay = params.clone();
    replay.insert("code".into(), "attacker-replacement".into());
    assert!(s.mcps.capture_native_callback(&s, &replay).await.unwrap());
    assert_eq!(
        finish("another-account").await,
        json!({ "pending": false, "result": "expired" })
    );
    let stats: Value = serde_json::from_str(&command(b"stats\n").await).unwrap();
    assert_eq!(
        stats["exchanges"], 1,
        "Capturing a callback must not exchange credentials"
    );
    assert_eq!(
        finish("native-account").await,
        json!({ "pending": false, "result": "connected" })
    );
    assert!(!s.mcps.capture_native_callback(&s, &params).await.unwrap());
    assert_eq!(finish("native-account").await["result"], "expired");
    stdin.write_all(b"stop\n").await.unwrap();
    drop(stdin);
    if tokio::time::timeout(Duration::from_secs(2), provider.wait())
        .await
        .is_err()
    {
        provider.kill().await.unwrap();
    }
}
