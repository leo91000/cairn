//! Grant contracts through the authenticated API and actual plugin dispatcher.
mod common;

use axum::{Router, body::Body, http::StatusCode, routing::any};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::MAIN_AGENT_ID,
    error::Result,
    http::Input,
    plugins::{agents, grant_authorization},
    service::Service,
    validation::text,
};
use common::{AgentFixture, MissionFixture};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    s: Arc<Service>,
    bearer: String,
    instance: String,
    posts: Arc<AtomicUsize>,
    pause: Arc<AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl Fixture {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let s = Service::new(cairn_installation::config::Config {
            runner_url: "http://runner:4311".into(),
            ..common::config(root.path())
        })
        .await
        .unwrap();
        common::relay_fixture::claimed(&s).await.unwrap();
        s.plugins
            .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
        let posts = Arc::new(AtomicUsize::new(0));
        let count = posts.clone();
        let pause = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (paused, arrived, resume) = (pause.clone(), entered.clone(), release.clone());
        let (origin, _server) = common::serve_locally(Router::new().route(
            "/effect",
            any(move || {
                let (count, paused, arrived, resume) = (
                    count.clone(),
                    paused.clone(),
                    arrived.clone(),
                    resume.clone(),
                );
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    if paused.swap(false, Ordering::SeqCst) {
                        arrived.notify_one();
                        resume.notified().await;
                    }
                    "ok"
                }
            }),
        ))
        .await;
        s.plugins.route_http_to(Some(origin));
        let shape = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "download": { "type": "boolean" },
                "two": { "type": "boolean" },
                "channel": { "type": "string" },
                "thread": { "type": "string" },
                "server": { "type": "string" },
                "author": { "type": "string" },
                "instance": { "type": "string" },
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": { "channel": { "type": "string" } },
                    },
                },
            },
        });
        let fields = json!([
            { "path": "/channel", "resourceType": "channel" },
            { "path": "/thread", "resourceType": "thread" },
            { "path": "/server", "resourceType": "server" },
            { "path": "/author", "resourceType": "person" },
            { "path": "/items/*/channel", "resourceType": "channel" },
        ]);
        let manifest = json!({
            "id": "grant-probe",
            "name": "Grant probe",
            "version": "1",
            "apiVersion": 2,
            "agentAccess": {},
            "capabilities": ["http", "hooks", "timers", "files"],
            "settings": [{
                "key": "marker", "title": "Marker", "type": "string", "default": "first",
            }],
            "runCredentials": {
                "files": [{ "path": ".config/probe/value", "content": "{{settings.marker}}" }],
            },
            "hosts": ["service.example.test"],
            "instructions": "probe instructions",
            "resources": [
                { "name": "server", "description": "Server" },
                { "name": "channel", "description": "Channel", "parentType": "server" },
                { "name": "thread", "description": "Thread", "parentType": "channel" },
                { "name": "person", "description": "Person" },
            ],
            "tools": [
                {
                    "name": "grant_probe_post", "description": "Post",
                    "inputSchema": shape, "resourceFields": fields,
                },
                {
                    "name": "grant_probe_other", "description": "Other",
                    "inputSchema": { "type": "object" },
                },
            ],
            "commands": [{
                "name": "post", "description": "Post", "output": "json",
                "arguments": [{
                    "name": "channel", "description": "Channel",
                    "positional": true, "resourceType": "channel",
                }],
            }],
            "hooks": [{
                "name": "message", "description": "Message",
                "payloadSchema": shape, "resourceFields": fields,
            }],
            "conditions": [{
                "name": "ready", "description": "Ready",
                "inputSchema": { "type": "object", "additionalProperties": false },
            }],
        });
        let source = r#"
return {
    resources = function(input)
        if input.op == "list" then
            pcall(function() cairn.http.request({url="https://service.example.test/effect"}) end)
            pcall(function() cairn.http.request({url="https://service.example.test/effect"}) end)
            return {items=cairn.array({})}
        end
        if input.id == "unknown" then return {} end
        local parent = nil
        if input.resourceType == "channel" then parent={ resourceType="server",id="guild" } end
        if input.resourceType == "thread" then parent={ resourceType="channel",id=if input.id == "support-thread" then "support" else "general" } end
        return { resourceType=input.resourceType,id=input.id,name=input.id,parent=parent }
    end,
    tool = function(input)
        if input.input.download then
            return cairn.http.request({instance=input.instances[1].id,url="https://service.example.test/effect",responseFile={name="private.txt"}})
        end
        local response = cairn.http.request({ instance=input.input.instance or input.instances[1].id, method="POST",url="https://service.example.test/effect" })
        if input.input.two then cairn.http.request({ instance=input.instances[1].id,method="POST",url="https://service.example.test/effect" }) end
        return response
    end,
    command = function(input)
        return cairn.http.request({ instance=input.instances[1].id,method="POST",url="https://service.example.test/effect" })
    end,
    condition = function(input) return {state="completed"} end,
}
"#;
        let installed =
            common::package_install(&s, &json!({ "manifest": manifest, "source": source }))
                .await
                .unwrap();
        common::package_approve(&s, "grant-probe", "1", text(&installed, "digest"))
            .await
            .unwrap();
        let instance = common::save_plugin(
            &s,
            "grant-probe",
            &json!({"name":"Probe","enabled":true}),
            None,
        )
        .await
        .unwrap();
        s.agent_fixture(json!({"access":{"installation":true}}), Some(MAIN_AGENT_ID))
            .await
            .unwrap();
        let task = s
            .task_fixture(
                json!({"name":"Probe","agentId":MAIN_AGENT_ID,"prompt":"Probe"}),
                None,
            )
            .await
            .unwrap();
        let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
        s.store
            .change_run(
                text(&run, "id"),
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Running),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let run = s.store.run(text(&run, "id")).await.unwrap();
        let config = s.mcps.run_configuration(&s, &run).await.unwrap();
        Self {
            _root: root,
            s,
            bearer: text(&config["env"], "CAIRN_MCP_RUN_TOKEN").into(),
            instance: text(&instance, "id").into(),
            posts,
            pause,
            entered,
            release,
        }
    }

    async fn api(&self, method: &str, body: Value) -> Result<Value> {
        cairn_installation::api::dispatch(
            &self.s,
            &Input {
                method: method.into(),
                path: format!(
                    "/api/agents/{}/plugin-grants",
                    body["agentId"].as_str().unwrap_or(MAIN_AGENT_ID)
                ),
                query: std::collections::HashMap::default(),
                headers: axum::http::HeaderMap::default(),
                body,
                identity: Some(InstallationIdentity::trusted(
                    InstallationRole::Owner,
                    "fixture-owner",
                )),
            },
        )
        .await
    }

    async fn set(&self, contributions: Value) -> Value {
        self.set_for(MAIN_AGENT_ID, contributions).await
    }

    async fn set_for(&self, agent: &str, contributions: Value) -> Value {
        let current = self.api("GET", json!({"agentId":agent})).await.unwrap();
        self.api("PUT",json!({"agentId":agent,"revision":current["revision"],"grants":[{"instanceId":self.instance,"access":{"type":"selected","contributions":contributions}}],"runCredentialInstances":[]})).await.unwrap()
    }

    async fn mcp(&self, endpoint: &str, token: &str, name: &str, input: Value) -> Value {
        let app = common::relay_fixture::router(self.s.clone()).await.unwrap();
        let request=common::request("POST",endpoint).header("authorization",format!("Bearer {token}")).body(Body::from(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":common::mcp_arguments(name,&input)}).to_string())).unwrap();
        let response = common::send(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        common::read_json(response).await
    }
}

fn contribution(kind: &str, name: &str, resources: Value) -> Value {
    let mut fields = serde_json::Map::new();
    fields.insert("kind".into(), kind.into());
    fields.insert("name".into(), name.into());
    fields.insert("resources".into(), resources);
    Value::Object(fields)
}

fn support() -> Value {
    json!([{"resourceType":"channel","id":"support"}])
}

#[tokio::test]
async fn tools_commands_and_hooks_resolve_every_designation_and_reject_contradictions() {
    let f = Fixture::new().await;
    f.set(json!([
        contribution("tool", "grant_probe_post", support()),
        contribution("command", "post", support()),
        contribution("hook", "message", support())
    ]))
    .await;
    let tools = agents::tools(&f.s, &f.bearer).await.unwrap();
    assert_eq!(
        tools.iter().map(|t| text(t, "name")).collect::<Vec<_>>(),
        vec!["grant_probe_post"]
    );
    assert_eq!(agents::instructions(&f.s, MAIN_AGENT_ID).await.unwrap(), "");
    for input in [
        json!({"channel":"support","author":"anyone"}),
        json!({"thread":"support-thread"}),
        json!({"channel":"support","thread":"support-thread","server":"guild"}),
        json!({"items":[{"channel":"support"}]}),
    ] {
        agents::call_tool(&f.s, &f.bearer, "grant_probe_post", &input)
            .await
            .unwrap();
        grant_authorization::authorize_hook_delivery(
            &f.s,
            MAIN_AGENT_ID,
            &f.instance,
            "message",
            &input,
        )
        .await
        .unwrap();
    }
    let effects = f.posts.load(Ordering::SeqCst);
    for input in [
        json!({}),
        json!({"channel":"general"}),
        json!({"thread":"general-thread"}),
        json!({"channel":"support","thread":"general-thread"}),
        json!({"channel":"support","server":"different"}),
        json!({"channel":"unknown"}),
        json!({"items":[{"channel":"support"},{"channel":"general"}]}),
        json!({"channel":""}),
        json!({"author":"support"}),
    ] {
        assert!(
            agents::call_tool(&f.s, &f.bearer, "grant_probe_post", &input)
                .await
                .is_err(),
            "{input}"
        );
        assert!(
            grant_authorization::authorize_hook_delivery(
                &f.s,
                MAIN_AGENT_ID,
                &f.instance,
                "message",
                &input
            )
            .await
            .is_err(),
            "{input}"
        );
    }
    assert_eq!(
        effects,
        f.posts.load(Ordering::SeqCst),
        "denied input reached a host effect"
    );
    agents::command(
        &f.s,
        &f.bearer,
        &json!({"plugin":"grant-probe","command":"post","args":["support"]}),
    )
    .await
    .unwrap();
    assert!(
        agents::command(
            &f.s,
            &f.bearer,
            &json!({"plugin":"grant-probe","command":"post","args":["general"]})
        )
        .await
        .is_err()
    );
    assert!(
        agents::call_tool(&f.s, &f.bearer, "grant_probe_other", &json!({}))
            .await
            .is_err()
    );
    assert!(
        cairn_installation::plugins::conditions::schemas(&f.s, MAIN_AGENT_ID)
            .await
            .unwrap()
            .is_empty()
    );
    f.set(json!([
        contribution("instructions", "", json!([])),
        contribution("condition", "ready", json!([]))
    ]))
    .await;
    assert_eq!(
        agents::instructions(&f.s, MAIN_AGENT_ID).await.unwrap(),
        "probe instructions"
    );
    assert_eq!(
        cairn_installation::plugins::conditions::schemas(&f.s, MAIN_AGENT_ID)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(agents::tools(&f.s, &f.bearer).await.unwrap().is_empty());
}

#[tokio::test]
async fn grant_replacement_is_revision_guarded_public_manage_and_installation_reduce_only() {
    let f = Fixture::new().await;
    let before = f
        .set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    let body = json!({"agentId":MAIN_AGENT_ID,"revision":before["revision"],"grants":[{"instanceId":f.instance,"access":{"type":"full"}}],"runCredentialInstances":[]});
    let denied = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            body.clone(),
        )
        .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let read_only = common::relay_fixture::grant(&f.s, &["read"]).await;
    let denied = f
        .mcp(
            "/mcp",
            &read_only,
            "replace_agent_plugin_grants",
            body.clone(),
        )
        .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let public = common::relay_fixture::grant(&f.s, &["manage", "read"]).await;
    let changed = f
        .mcp("/mcp", &public, "replace_agent_plugin_grants", body.clone())
        .await;
    assert_ne!(changed["result"]["isError"], true, "{changed}");
    assert_eq!(f.api("PUT", body).await.unwrap_err().status, 409);
    let read = f
        .mcp(
            "/mcp",
            &public,
            "get_agent_plugin_grants",
            json!({"agentId":MAIN_AGENT_ID}),
        )
        .await;
    assert_ne!(read["result"]["isError"], true, "{read}");
    let current = f.api("GET", json!({})).await.unwrap();
    let reduced=f.mcp("/mcp-installation",&f.bearer,"replace_agent_plugin_grants",json!({"agentId":MAIN_AGENT_ID,"revision":current["revision"],"grants":[],"runCredentialInstances":[]})).await;
    assert_ne!(reduced["result"]["isError"], true, "{reduced}");
    assert!(
        f.api("GET", json!({})).await.unwrap()["grants"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn instances_are_filtered_when_another_grants_a_different_contribution() {
    let f = Fixture::new().await;
    let second = common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({"name":"Second","enabled":true}),
        None,
    )
    .await
    .unwrap();
    let current = f.api("GET", json!({})).await.unwrap();
    f.api("PUT",json!({"agentId":MAIN_AGENT_ID,"revision":current["revision"],"grants":[
        {"instanceId":f.instance,"access":{"type":"selected","contributions":[contribution("tool","grant_probe_post",support())]}},
        {"instanceId":second["id"],"access":{"type":"selected","contributions":[contribution("instructions","",json!([]))]}}
    ],"runCredentialInstances":[]})).await.unwrap();
    let effects = f.posts.load(Ordering::SeqCst);
    assert!(
        agents::call_tool(
            &f.s,
            &f.bearer,
            "grant_probe_post",
            &json!({"channel":"support","instance":second["id"]})
        )
        .await
        .is_err()
    );
    assert_eq!(f.posts.load(Ordering::SeqCst), effects);
    agents::call_tool(
        &f.s,
        &f.bearer,
        "grant_probe_post",
        &json!({"channel":"support","instance":f.instance}),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn revocation_during_io_withholds_the_result_and_prevents_the_next_effect() {
    let f = Fixture::new().await;
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    f.pause.store(true, Ordering::SeqCst);
    let (s, bearer) = (f.s.clone(), f.bearer.clone());
    let called = tokio::spawn(async move {
        agents::call_tool(
            &s,
            &bearer,
            "grant_probe_post",
            &json!({"channel":"support","two":true}),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), f.entered.notified())
        .await
        .unwrap();
    f.set(json!([contribution("instructions", "", json!([]))]))
        .await;
    f.release.notify_one();
    assert!(called.await.unwrap().is_err());
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
}

async fn running_conversation(s: &Arc<Service>, chat: &str) -> String {
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let detail = s.chat_detail(chat).await.unwrap();
    let run_id = text(&detail["run"], "id");
    s.store
        .change_run(
            run_id,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let run = s.store.run(run_id).await.unwrap();
    s.chat_acknowledge(run_id, text(&run["chatExecution"], "messageId"))
        .await
        .unwrap();
    let config = s.mcps.run_configuration(s, &run).await.unwrap();
    text(&config["env"], "CAIRN_MCP_RUN_TOKEN").into()
}

#[tokio::test]
async fn delegated_descendant_grants_are_resolved_and_rechecked_after_requester_reduction() {
    let f = Fixture::new().await;
    f.s.agent_fixture(json!({"access":{"delegation":true}}), Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    let child =
        f.s.agent_fixture(json!({"name":"Child"}), None)
            .await
            .unwrap();
    let child_id = text(&child, "id");
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    f.set_for(
        child_id,
        json!([contribution(
            "tool",
            "grant_probe_post",
            json!([{"resourceType":"thread","id":"support-thread"}])
        )]),
    )
    .await;
    let parent =
        f.s.chat_create(json!({"agentId":MAIN_AGENT_ID}))
            .await
            .unwrap();
    let parent_id = text(&parent, "id");
    f.s.chat_send(
        parent_id,
        json!({"id":cairn_installation::config::id(),"text":"Delegate"}),
    )
    .await
    .unwrap();
    let parent_token = running_conversation(&f.s, parent_id).await;
    let task = cairn_installation::delegation::delegate(
        &f.s,
        &parent_token,
        &json!({"agentId":child_id,"task":"Use support"}),
    )
    .await
    .unwrap();
    let child_token = running_conversation(&f.s, text(&task, "chatId")).await;
    agents::call_tool(
        &f.s,
        &child_token,
        "grant_probe_post",
        &json!({"thread":"support-thread"}),
    )
    .await
    .unwrap();
    let effects = f.posts.load(Ordering::SeqCst);
    f.set(json!([contribution(
        "tool",
        "grant_probe_post",
        json!([{"resourceType":"channel","id":"general"}])
    )]))
    .await;
    assert!(
        agents::call_tool(
            &f.s,
            &child_token,
            "grant_probe_post",
            &json!({"thread":"support-thread"})
        )
        .await
        .is_err()
    );
    assert_eq!(effects, f.posts.load(Ordering::SeqCst));
}

#[tokio::test]
async fn credential_supplier_is_explicit_and_reductions_never_select_another() {
    let f = Fixture::new().await;
    let second = common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({"name":"Second","enabled":true,"settings":{"marker":"second"}}),
        None,
    )
    .await
    .unwrap();
    let current = f.api("GET", json!({})).await.unwrap();
    let full = |id: &Value| json!({"instanceId":id,"access":{"type":"full"}});
    let first = json!(f.instance);
    let saved=f.api("PUT",json!({"agentId":MAIN_AGENT_ID,"revision":current["revision"],"grants":[full(&first),full(&second["id"])],"runCredentialInstances":[first]})).await.unwrap();
    let read = cairn_installation::plugins::run_credentials::for_run(&f.s, MAIN_AGENT_ID, &[])
        .await
        .unwrap();
    assert_eq!(
        read.files,
        vec![(".config/probe/value".into(), "first".into())]
    );
    let reduced=f.api("PUT",json!({"agentId":MAIN_AGENT_ID,"revision":saved["revision"],"grants":[full(&second["id"])],"runCredentialInstances":[]})).await.unwrap();
    assert!(
        cairn_installation::plugins::run_credentials::for_run(&f.s, MAIN_AGENT_ID, &[])
            .await
            .unwrap()
            .files
            .is_empty()
    );
    let choice = json!({"agentId":MAIN_AGENT_ID,"revision":reduced["revision"],"grants":[full(&second["id"])],"runCredentialInstances":[second["id"]]});
    let denied = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            choice.clone(),
        )
        .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    f.api("PUT", choice).await.unwrap();
    let read = cairn_installation::plugins::run_credentials::for_run(&f.s, MAIN_AGENT_ID, &[])
        .await
        .unwrap();
    assert_eq!(
        read.files,
        vec![(".config/probe/value".into(), "second".into())]
    );
}

#[tokio::test]
async fn installation_can_narrow_a_resource_to_its_descendant_but_cannot_reverse_it() {
    let f = Fixture::new().await;
    let before = f
        .set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    let replace = |revision: &Value, resources: Value| json!({"agentId":MAIN_AGENT_ID,"revision":revision,"grants":[{"instanceId":f.instance,"access":{"type":"selected","contributions":[contribution("tool","grant_probe_post",resources)]}}],"runCredentialInstances":[]});
    let narrowed = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            replace(
                &before["revision"],
                json!([{"resourceType":"thread","id":"support-thread"}]),
            ),
        )
        .await;
    assert_ne!(narrowed["result"]["isError"], true, "{narrowed}");
    let current = f.api("GET", json!({})).await.unwrap();
    let widened = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            replace(&current["revision"], support()),
        )
        .await;
    assert_eq!(widened["result"]["isError"], true, "{widened}");
}

#[tokio::test]
async fn transfer_reads_reauthorize_the_producing_contribution_and_resources() {
    let f = Fixture::new().await;
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    let response = agents::call_tool(
        &f.s,
        &f.bearer,
        "grant_probe_post",
        &json!({ "channel": "support", "download": true }),
    )
    .await
    .unwrap()
    .unwrap();
    let transfer = json!({ "plugin": "grant-probe", "file": { "method": "read", "instance": f.instance, "id": response["file"]["id"] } });
    let read = agents::command(&f.s, &f.bearer, &transfer).await.unwrap();
    assert_eq!(read["value"]["data"], "b2s");
    f.set(json!([contribution(
        "tool",
        "grant_probe_post",
        json!([{ "resourceType": "channel", "id": "general" }])
    )]))
    .await;
    assert!(agents::command(&f.s, &f.bearer, &transfer).await.is_err());
    f.set(json!([contribution("instructions", "", json!([]))]))
        .await;
    assert!(agents::command(&f.s, &f.bearer, &transfer).await.is_err());
}

#[tokio::test]
async fn unavailable_existing_grants_and_their_supplier_survive_other_reductions() {
    let f = Fixture::new().await;
    let second = common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({ "name": "Second", "enabled": true }),
        None,
    )
    .await
    .unwrap();
    let full = |id: &Value| json!({ "instanceId": id, "access": { "type": "full" } });
    let current = f.api("GET", json!({})).await.unwrap();
    let saved = f.api("PUT", json!({ "agentId": MAIN_AGENT_ID, "revision": current["revision"], "grants": [full(&json!(f.instance)), full(&second["id"])], "runCredentialInstances": [f.instance] })).await.unwrap();
    let path = f.s.config.data_dir.join("plugins/grant-probe/1/main.luau");
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, "return {} -- awaiting review").unwrap();
    let reduced = f.api("PUT", json!({ "agentId": MAIN_AGENT_ID, "revision": saved["revision"], "grants": [full(&json!(f.instance))], "runCredentialInstances": [f.instance] })).await.unwrap();
    assert_eq!(reduced["runCredentialInstances"], json!([f.instance]));
    assert_eq!(reduced["grants"].as_array().unwrap().len(), 1);
    std::fs::write(&path, original).unwrap();
    assert_eq!(
        cairn_installation::plugins::run_credentials::for_run(&f.s, MAIN_AGENT_ID, &[])
            .await
            .unwrap()
            .files
            .len(),
        1
    );
}

#[tokio::test]
async fn legacy_full_grants_without_run_credentials_round_trip_without_a_supplier() {
    let f = Fixture::new().await;
    common::save_plugin(&f.s, "onepassword", &json!({ "name": "Fixture", "enabled": true, "secrets": { "token": "ops_fixture" }, "agentIds": [MAIN_AGENT_ID] }), None).await.unwrap();
    let current = f.api("GET", json!({})).await.unwrap();
    assert_eq!(current["runCredentialInstances"], json!([]));
    let saved = f.api("PUT", current).await.unwrap();
    assert_eq!(saved["runCredentialInstances"], json!([]));
}

#[tokio::test]
async fn merges_accept_semantically_covered_descendant_grants() {
    let f = Fixture::new().await;
    let source =
        f.s.agent_fixture(json!({ "name": "Source" }), None)
            .await
            .unwrap();
    let source_id = text(&source, "id");
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    f.set_for(
        source_id,
        json!([contribution(
            "tool",
            "grant_probe_post",
            json!([{ "resourceType": "thread", "id": "support-thread" }])
        )]),
    )
    .await;
    let merged =
        f.s.merge_agents(
            serde_json::from_value(
                json!({ "sourceId": source_id, "targetId": MAIN_AGENT_ID, "confirm": true }),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(merged.deleted);
    assert!(f.s.store.agent(source_id).await.unwrap().is_none());
    assert_eq!(
        f.api("GET", json!({})).await.unwrap()["grants"][0]["access"]["contributions"][0]["resources"],
        support()
    );
}

#[tokio::test]
async fn deferred_resource_effects_retain_the_delegation_resource_ceiling() {
    let f = Fixture::new().await;
    f.s.agent_fixture(
        json!({ "access": { "delegation": true } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let child =
        f.s.agent_fixture(
            json!({ "name": "Child", "access": { "installation": true } }),
            None,
        )
        .await
        .unwrap();
    let child_id = text(&child, "id");
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    f.set_for(
        child_id,
        json!([contribution(
            "tool",
            "grant_probe_post",
            json!([{ "resourceType": "thread", "id": "support-thread" }])
        )]),
    )
    .await;
    let parent =
        f.s.chat_create(json!({ "agentId": MAIN_AGENT_ID }))
            .await
            .unwrap();
    f.s.chat_send(
        text(&parent, "id"),
        json!({ "id": cairn_installation::config::id(), "text": "Delegate" }),
    )
    .await
    .unwrap();
    let parent_token = running_conversation(&f.s, text(&parent, "id")).await;
    let task = cairn_installation::delegation::delegate(
        &f.s,
        &parent_token,
        &json!({ "agentId": child_id, "task": "Read resources" }),
    )
    .await
    .unwrap();
    let child_token = running_conversation(&f.s, text(&task, "chatId")).await;
    f.pause.store(true, Ordering::SeqCst);
    let started = f.mcp("/mcp-installation", &child_token, "list_plugin_resources", json!({ "pluginId": "grant-probe", "instanceId": f.instance, "resourceType": "channel" })).await;
    assert_ne!(started["result"]["isError"], true, "{started}");
    let operation = text(
        &started["result"]["structuredContent"]["result"],
        "operation",
    );
    assert!(!operation.is_empty(), "{started}");
    tokio::time::timeout(std::time::Duration::from_secs(5), f.entered.notified())
        .await
        .unwrap();
    f.set(json!([contribution(
        "tool",
        "grant_probe_post",
        json!([{ "resourceType": "channel", "id": "general" }])
    )]))
    .await;
    f.release.notify_one();
    let state = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let state = f.s.plugins.operations.get(operation).unwrap();
            if matches!(text(&state, "state"), "completed" | "failed" | "cancelled") {
                break state;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state["state"], "failed", "{state}");
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cross_agent_descendant_reductions_keep_the_callers_instance_boundary() {
    let f = Fixture::new().await;
    let child =
        f.s.agent_fixture(json!({ "name": "Child" }), None)
            .await
            .unwrap();
    let child_id = text(&child, "id");
    let before = f
        .set_for(
            child_id,
            json!([contribution("tool", "grant_probe_post", support())]),
        )
        .await;
    let request = json!({
        "agentId": child_id,
        "revision": before["revision"],
        "grants": [{
            "instanceId": f.instance,
            "access": {
                "type": "selected",
                "contributions": [contribution("tool", "grant_probe_post", json!([
                    { "resourceType": "thread", "id": "support-thread" },
                ]))],
            },
        }],
        "runCredentialInstances": [],
    });
    let denied = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            request.clone(),
        )
        .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);

    f.set(json!([contribution("instructions", "", json!([]))]))
        .await;
    let allowed = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            request,
        )
        .await;
    assert_ne!(allowed["result"]["isError"], true, "{allowed}");
}
