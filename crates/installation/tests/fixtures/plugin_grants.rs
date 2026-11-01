//! Shared real-service grant fixture for integration and publication-boundary regressions.
#![allow(dead_code)]

use crate::common;

use axum::{Router, body::Body, http::StatusCode, routing::any};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::MAIN_AGENT_ID,
    error::Result,
    http::Input,
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

pub(crate) struct Fixture {
    pub(crate) _root: TempDir,
    pub(crate) s: Arc<Service>,
    pub(crate) bearer: String,
    pub(crate) run: String,
    pub(crate) instance: String,
    pub(crate) posts: Arc<AtomicUsize>,
    pub(crate) pause: Arc<AtomicBool>,
    pub(crate) entered: Arc<tokio::sync::Notify>,
    pub(crate) release: Arc<tokio::sync::Notify>,
    pub(crate) moved_parent: Arc<AtomicBool>,
    pub(crate) uploads: Arc<AtomicUsize>,
}

impl Fixture {
    pub(crate) async fn delegated_manager() -> (Self, String) {
        let f = Self::new().await;
        f.marker("live-parent").await;
        f.set(json!([contribution("tool", "grant_probe_post", support())]))
            .await;
        let child =
            f.s.agent_fixture(
                json!({
                    "name": "Child",
                    "access": { "installation": true },
                }),
                None,
            )
            .await
            .unwrap();
        let child_id = text(&child, "id");
        f.set_for(
            child_id,
            json!([contribution(
                "tool",
                "grant_probe_post",
                json!([{ "resourceType": "thread", "id": "support-thread" }])
            )]),
        )
        .await;
        let (_, bearer) = f.delegated(MAIN_AGENT_ID, child_id).await;
        (f, bearer)
    }

    pub(crate) async fn new() -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let s = Service::new(cairn_installation::config::Config {
            runner_url: "http://runner:4311".into(),
            ..common::config(root.path())
        })
        .await
        .unwrap();
        common::relay_fixture::claimed(&s).await.unwrap();
        s.plugins.use_worker(
            option_env!("CARGO_BIN_EXE_cairn")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("CAIRN_TEST_CODE_WORKER").map(PathBuf::from))
                .unwrap_or_else(|| {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/cairn")
                }),
        );
        let posts = Arc::new(AtomicUsize::new(0));
        let count = posts.clone();
        let pause = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let moved_parent = Arc::new(AtomicBool::new(false));
        let remote_parent = moved_parent.clone();
        let uploads = Arc::new(AtomicUsize::new(0));
        let uploaded = uploads.clone();
        let hold_entered = entered.clone();
        let hold_release = release.clone();
        let (paused, arrived, resume) = (pause.clone(), entered.clone(), release.clone());
        let (origin, _server) = common::serve_locally(
            Router::new()
                .route(
                    "/effect",
                    any(move || {
                        let moved = remote_parent.clone();
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
                            if moved.load(Ordering::SeqCst) {
                                "general"
                            } else {
                                "ok"
                            }
                        }
                    }),
                )
                .route(
                    "/hold",
                    any(move || {
                        let entered = hold_entered.clone();
                        let release = hold_release.clone();
                        async move {
                            entered.notify_one();
                            release.notified().await;
                            "held"
                        }
                    }),
                )
                .route(
                    "/upload",
                    any(move || {
                        let uploaded = uploaded.clone();
                        async move {
                            uploaded.fetch_add(1, Ordering::SeqCst);
                            "uploaded"
                        }
                    }),
                ),
        )
        .await;
        s.plugins.route_http_to(Some(origin));
        let shape = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "download": { "type": "boolean" },
                "two": { "type": "boolean" },
                "channel": { "type": "string" },
                "*": { "type": "string" },
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
            { "path": "/*", "resourceType": "channel" },
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
        if input.instance.settings.marker == "resolver-effects" then
            pcall(function() cairn.http.request({url="https://service.example.test/effect"}) end)
            pcall(function() cairn.http.request({url="https://service.example.test/effect"}) end)
        end
        local parent = nil
        if input.resourceType == "channel" then parent={ resourceType="server",id="guild" } end
        if input.resourceType == "thread" then parent={ resourceType="channel",id=if input.id == "support-thread" and input.instance.settings.marker ~= "different-parent" then "support" else "general" } end
        if input.resourceType == "thread" and input.instance.settings.marker == "live-parent" then
            local response = cairn.http.request({url="https://service.example.test/effect"})
            parent={resourceType="channel",id=if response.text == "general" then "general" else "support"}
        end
        return { resourceType=input.resourceType,id=input.id,name=input.id,parent=parent }
    end,
    tool = function(input)
        if input.input.chunks then
            local destination = if input.input.target then cairn.files.read({instance=input.instances[1].id,id=input.input.target}) else cairn.files.create({instance=input.instances[1].id,name="chunks.txt",data=""})
            for _ = 1, input.input.chunks do
                destination = cairn.files.append({instance=input.instances[1].id,id=destination.id,offset=destination.size,data="chunk"})
            end
            return destination
        end
        if input.input.copy then
            local destination = cairn.files.create({instance=input.instances[1].id,name="copy.txt",data=""})
            local original = cairn.files.read({instance=input.instances[1].id,id=input.input.copy})
            return cairn.files.append({instance=input.instances[1].id,id=destination.id,offset=0,data=original.data,encoding=original.encoding})
        end
        if input.input.consume then
            local original = cairn.files.read({instance=input.instances[1].id,id=input.input.consume})
            cairn.http.request({instance=input.instances[1].id,url="https://service.example.test/hold"})
            return cairn.http.request({instance=input.instances[1].id,method="POST",url="https://service.example.test/upload",body=original.data})
        end
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
    condition = function(input)
        local marker = input.instances[1].settings.marker
        if marker == "condition-pending" then return {state="waiting"} end
        if marker == "condition-effects" then
            pcall(function() cairn.http.request({instance=input.instances[1].id,url="https://service.example.test/effect"}) end)
            pcall(function() cairn.http.request({instance=input.instances[1].id,url="https://service.example.test/effect"}) end)
        end
        return {state="completed"}
    end,
}
"#;
        let installed = common::package_install(
            &s,
            &json!({
                "manifest": manifest,
                "source": source,
            }),
        )
        .await
        .unwrap();
        common::package_approve(&s, "grant-probe", "1", text(&installed, "digest"))
            .await
            .unwrap();
        let instance = common::save_plugin(
            &s,
            "grant-probe",
            &json!({
                "name": "Probe",
                "enabled": true,
            }),
            None,
        )
        .await
        .unwrap();
        s.agent_fixture(json!({"access":{"installation":true}}), Some(MAIN_AGENT_ID))
            .await
            .unwrap();
        let task = s
            .task_fixture(
                json!({
                    "name": "Probe",
                    "agentId": MAIN_AGENT_ID,
                    "prompt": "Probe",
                }),
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
            run: text(&run, "id").into(),
            instance: text(&instance, "id").into(),
            posts,
            pause,
            entered,
            release,
            moved_parent,
            uploads,
        }
    }

    pub(crate) async fn marker(&self, value: &str) {
        let mut instance = self
            .s
            .store
            .plugin_instance(&self.instance)
            .await
            .unwrap()
            .unwrap();
        instance.settings["marker"] = value.into();
        instance.revision = cairn_installation::config::id();
        self.s
            .store
            .transaction(move |db| db.save_plugin_instance(&instance))
            .await
            .unwrap();
    }

    pub(crate) async fn delegated(&self, requester: &str, target: &str) -> (String, String) {
        self.s
            .agent_fixture(
                json!({
                    "access": {
                        "delegation": true,
                        "installation": true,
                    },
                }),
                Some(requester),
            )
            .await
            .unwrap();
        let parent = self
            .s
            .chat_create(json!({ "agentId": requester }))
            .await
            .unwrap();
        let parent_id = text(&parent, "id");
        self.s
            .chat_send(
                parent_id,
                json!({
                    "id": cairn_installation::config::id(),
                    "text": "Delegate",
                }),
            )
            .await
            .unwrap();
        let parent_token = running_conversation(&self.s, parent_id).await;
        let task = cairn_installation::delegation::delegate(
            &self.s,
            &parent_token,
            &json!({
                "agentId": target,
                "task": "Scoped task",
            }),
        )
        .await
        .unwrap();
        let chat = text(&task, "chatId").to_owned();
        let token = running_conversation(&self.s, &chat).await;
        (chat, token)
    }

    pub(crate) async fn api(&self, method: &str, body: Value) -> Result<Value> {
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

    pub(crate) async fn set(&self, contributions: Value) -> Value {
        self.set_for(MAIN_AGENT_ID, contributions).await
    }

    pub(crate) async fn remove_for(&self, agent: &str) -> Value {
        let current = self.api("GET", json!({ "agentId": agent })).await.unwrap();
        self.api(
            "PUT",
            json!({
                "agentId": agent, "revision": current["revision"],
                "grants": [], "runCredentialInstances": [],
            }),
        )
        .await
        .unwrap()
    }

    pub(crate) async fn set_for(&self, agent: &str, contributions: Value) -> Value {
        let current = self.api("GET", json!({ "agentId": agent })).await.unwrap();
        self.api(
            "PUT",
            json!({
                "agentId": agent,
                "revision": current["revision"],
                "grants": [{
                    "instanceId": self.instance,
                    "access": {
                        "type": "selected",
                        "contributions": contributions,
                    },
                }],
                "runCredentialInstances": [],
            }),
        )
        .await
        .unwrap()
    }

    pub(crate) async fn mcp(&self, endpoint: &str, token: &str, name: &str, input: Value) -> Value {
        let app = common::relay_fixture::router(self.s.clone()).await.unwrap();
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": common::mcp_arguments(name, &input),
        });
        let request = common::request("POST", endpoint)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        common::read_json(response).await
    }
}

pub(crate) fn contribution(kind: &str, name: &str, resources: Value) -> Value {
    let mut fields = serde_json::Map::new();
    fields.insert("kind".into(), kind.into());
    fields.insert("name".into(), name.into());
    fields.insert("resources".into(), resources);
    Value::Object(fields)
}

pub(crate) fn support() -> Value {
    json!([{"resourceType":"channel","id":"support"}])
}

pub(crate) async fn running_conversation(s: &Arc<Service>, chat: &str) -> String {
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
    s.store
        .save_checkpoint_boundary(
            run_id,
            json!({
                "completed": true,
                "launched": true,
            }),
        )
        .await
        .unwrap();
    text(&config["env"], "CAIRN_MCP_RUN_TOKEN").into()
}
