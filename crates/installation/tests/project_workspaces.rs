use crate::common::run_fixture::RunFixture;
use common::AgentFixture;
use common::MissionFixture;
use common::ProjectFixture;
use common::StoreAgentFixture;

mod common;

use cairn_installation::plugins::run_credentials::RunCredentials;
use cairn_installation::{
    config::{Config, MAIN_AGENT_ID, id},
    execution, outcome, project_git, project_workspaces, run_output,
    run_status::RunStatus,
    service::{Service, covers},
    validation::text,
};
use common::reconfigure;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tempfile::TempDir;

/// A service with two local projects, `First` and `Second`, and a remote runner.
async fn fixture() -> (TempDir, Arc<Service>, Vec<Value>) {
    let root = TempDir::new().unwrap();
    common::managed_codex_home(&root.path().join("home"));
    let s = Service::new(Config {
        concurrency: 2,
        runner_url: "http://runner:4311".into(),
        ..common::config(root.path())
    })
    .await
    .unwrap();
    let mut projects = Vec::new();
    for name in ["First", "Second"] {
        let path = root.path().join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("hello"), name).unwrap();
        projects.push(
            s.project_fixture(json!({ "name": name, "path": path }), None)
                .await
                .unwrap(),
        );
    }
    (root, s, projects)
}

/// Enqueues a manual run of `agent` on `project`.
async fn run(s: &Service, project: Value, agent: &str) -> Value {
    let task = json!({
        "name": "Probe",
        "agentId": agent,
        "projectId": project,
        "prompt": "Hello",
    });
    let task = s.task_fixture(task, None).await.unwrap();
    s.enqueue(text(&task, "id"), "manual", None).await.unwrap()
}

async fn patch_run(s: &Service, run: &Value, patch: Value) {
    s.store
        .change_run_fixture(text(run, "id"), patch)
        .await
        .unwrap();
}

/// Marks `run` running and returns the MCP token of its grant.
async fn running_run_token(s: &Arc<Service>, run: &Value) -> String {
    patch_run(s, run, json!({ "status": RunStatus::Running })).await;
    run_token(s, run).await
}

async fn run_token(s: &Arc<Service>, run: &Value) -> String {
    let config = s.mcps.run_configuration(s, run).await.unwrap();
    text(&config["env"], "CAIRN_MCP_RUN_TOKEN").to_owned()
}

fn run_home(s: &Service, run: &Value) -> PathBuf {
    s.config
        .data_dir
        .join("runs")
        .join(text(run, "id"))
        .join("home")
}

/// An agent restricted to no project, skill or MCP.
fn restricted_agent(name: &str) -> Value {
    json!({
        "name": name,
        "access": { "projects": [], "skills": [], "mcps": [] },
    })
}

#[tokio::test]
async fn empty_start_does_not_touch_unselected_repositories_and_selected_start_loads_only_one() {
    let (_root, s, projects) = fixture().await;
    let mut empty = run(&s, Value::Null, MAIN_AGENT_ID).await;
    // An unavailable, unneeded repository must not prevent a simple chat starting.
    std::fs::remove_dir_all(text(&projects[1], "path")).unwrap();
    let prepared = execution::prepare(&empty, &s.config, &RunCredentials::default(), None, None)
        .await
        .unwrap();
    assert_eq!(prepared["workspaces"], json!([]));
    assert_eq!(
        std::fs::read_dir(text(&prepared, "cwd")).unwrap().count(),
        0
    );
    empty["isolated"] = true.into();
    let prompt = run_output::prompt(&empty, true);
    assert!(prompt.contains("open_project"));
    assert!(
        !prompt.contains(text(&projects[0], "path")),
        "host paths must not be advertised as guest workspaces"
    );
    std::fs::create_dir(text(&projects[1], "path")).unwrap();
    let selected = run(&s, projects[0]["id"].clone(), MAIN_AGENT_ID).await;
    let prepared = execution::prepare(&selected, &s.config, &RunCredentials::default(), None, None)
        .await
        .unwrap();
    assert_eq!(prepared["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(prepared["workspaces"][0]["projectId"], projects[0]["id"]);
    assert_eq!(project_workspaces::catalog(&selected).len(), 2);
    let plan = run_output::chat_plan(
        &selected,
        &prepared,
        Path::new("/tmp"),
        &json!({ "args": [] }),
        None,
    )
    .unwrap();
    assert!(
        plan["writableRoots"]
            .as_array()
            .unwrap()
            .contains(&prepared["projectRoot"])
    );
}

#[tokio::test]
async fn managed_resume_does_not_depend_on_unopened_project_sources() {
    for backend in ["firecracker", "native"] {
        for selected in [false, true] {
            let (_root, s, projects) = fixture().await;
            let project = if selected {
                projects[0]["id"].clone()
            } else {
                Value::Null
            };
            let mut run = run(&s, project, MAIN_AGENT_ID).await;
            run["snapshot"]["agent"]["access"]["backend"] = backend.into();
            let prepared =
                execution::prepare(&run, &s.config, &RunCredentials::default(), None, None)
                    .await
                    .unwrap();
            assert_eq!(prepared["backend"], backend);
            assert_eq!(
                prepared["workspaces"].as_array().unwrap().len(),
                usize::from(selected)
            );
            let working_file = Path::new(text(&prepared, "cwd")).join("retained-untracked");
            std::fs::write(&working_file, b"retained native or VM work").unwrap();
            // This authorized repository was never opened. Its removal must not
            // prevent continuing the private working tree or an empty conversation.
            std::fs::remove_dir_all(text(&projects[1], "path")).unwrap();
            if !selected {
                std::fs::remove_dir_all(text(&projects[0], "path")).unwrap();
            }
            let restored = execution::restore(
                &run,
                prepared.clone(),
                &s.config,
                &RunCredentials::default(),
            )
            .await
            .unwrap();
            assert_eq!(restored, prepared);
            assert_eq!(
                std::fs::read(&working_file).unwrap(),
                b"retained native or VM work"
            );
            if selected {
                // Keep the existing location check for an actually opened project.
                std::fs::remove_dir_all(text(&projects[0], "path")).unwrap();
                assert!(
                    execution::restore(&run, restored, &s.config, &RunCredentials::default())
                        .await
                        .is_err()
                );
                assert!(working_file.exists());
            }
        }
    }
}

#[test]
fn access_covers_unchanged_or_wider_grants_but_not_reduced_ones() {
    let agent = |access: Value| json!({ "id": "agent", "access": access });
    let granted = agent(json!({
        "projects": ["a"],
        "skills": null,
        "mcps": ["m"],
        "mcpTools": { "m": ["read"] },
        "sandbox": "workspace-write",
        "nodes": ["laptop"],
    }));
    let same = agent(json!({
        "projects": ["a"],
        "mcps": ["m"],
        "mcpTools": { "m": ["read"] },
        "sandbox": "workspace-write",
        "nodes": null,
    }));
    assert!(covers(&same, &granted));
    let wider = agent(json!({
        "projects": ["a", "b"],
        "mcps": null,
        "mcpTools": {},
        "sandbox": "yolo",
    }));
    assert!(covers(&wider, &granted));

    let reductions = [
        ("projects", json!(["b"])),
        ("skills", json!([])),
        ("mcps", json!([])),
        ("mcpTools", json!({ "m": [] })),
        ("sandbox", json!("read-only")),
    ];
    for (key, value) in reductions {
        let mut reduced = same.clone();
        reduced["access"][key] = value;
        assert!(!covers(&reduced, &granted), "{key}");
    }
}

#[tokio::test]
async fn scoped_access_rejects_other_projects_expired_grants_and_reduced_permissions() {
    let (_root, s, projects) = fixture().await;
    let mut restricted = restricted_agent("Restricted");
    restricted["access"]["projects"] = json!([projects[0]["id"]]);
    restricted["access"]["sandbox"] = "read-only".into();
    let agent = s.agent_fixture(restricted, None).await.unwrap();
    let run = run(&s, Value::Null, text(&agent, "id")).await;
    patch_run(&s, &run, json!({ "status": RunStatus::Running })).await;
    let config = s.mcps.run_configuration(&s, &run).await.unwrap();
    assert!(config["codexConfig"].get("cairn_workspace").is_some());
    let token = text(&config["env"], "CAIRN_MCP_RUN_TOKEN");
    assert_eq!(
        s.projects
            .open(&s, token, text(&projects[1], "id"))
            .await
            .unwrap_err()
            .status,
        403
    );
    // Wider access and settings older versions saved do not revoke the run's grant.
    let mut widened = agent.clone();
    widened["access"]["projects"] = json!([projects[0]["id"], projects[1]["id"]]);
    widened["access"]["sandbox"] = "workspace-write".into();
    s.store.put_agent_fixture(widened).await.unwrap();
    project_workspaces::authorize(&s, token).await.unwrap();
    let mut changed = agent.clone();
    changed["access"]["projects"] = json!([]);
    s.store.put_agent_fixture(changed).await.unwrap();
    assert_eq!(
        project_workspaces::authorize(&s, token)
            .await
            .unwrap_err()
            .status,
        403
    );
    s.mcps.revoke_run(&s, text(&run, "id")).await.unwrap();
    assert_eq!(
        project_workspaces::authorize(&s, token)
            .await
            .unwrap_err()
            .status,
        401
    );
}

#[tokio::test]
async fn open_imports_once_reuses_seed_and_restores_loaded_catalog_without_checkpoint_races() {
    let (_root, mut service, projects) = fixture().await;
    let calls = Arc::new(tokio::sync::Mutex::new(Vec::<Value>::new()));
    let requests = calls.clone();
    let router = axum::Router::new().fallback(axum::routing::post(
        move |axum::Json(value): axum::Json<Value>| {
            let requests = requests.clone();
            async move {
                let mut requests = requests.lock().await;
                requests.push(value);
                axum::Json(json!({ "ok": true, "reused": requests.len() > 1 }))
            }
        },
    ));
    let (listener, address) = common::bind().await;
    reconfigure(&mut service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, router);
    let s = &service;
    let run = run(s, Value::Null, MAIN_AGENT_ID).await;
    let prepared = execution::prepare(&run, &s.config, &RunCredentials::default(), None, None)
        .await
        .unwrap();
    patch_run(
        s,
        &run,
        json!({ "status": RunStatus::Running, "workspaces": [] }),
    )
    .await;
    common::set_checkpoint(
        &s.store,
        text(&run, "id"),
        json!({ "runnerId": id(), "prepared": prepared }),
    )
    .await;
    let token = &run_token(s, &run).await;
    let project_id = text(&projects[0], "id");
    let (a, b) = tokio::join!(
        s.projects.open(s, token, project_id),
        s.projects.open(s, token, project_id)
    );
    assert!(a.is_ok() && b.is_ok());
    let a = a.unwrap();
    assert_eq!(a["path"], b.unwrap()["path"]);
    std::fs::write(
        Path::new(text(&projects[0], "path")).join("hello"),
        "changed upstream",
    )
    .unwrap();
    let again = s.projects.open(s, token, project_id).await.unwrap();
    assert_eq!(again["reused"], true);
    assert_eq!(
        std::fs::read_to_string(Path::new(text(&again, "path")).join("hello")).unwrap(),
        "First"
    );
    let saved = s.store.run(text(&run, "id")).await.unwrap();
    assert_eq!(saved["workspaces"].as_array().unwrap().len(), 1);
    let restored = execution::restore(&saved, prepared, &s.config, &RunCredentials::default())
        .await
        .unwrap();
    assert_eq!(restored["workspaces"], saved["workspaces"]);
    assert_eq!(calls.lock().await.len(), 3);
    server.abort();
}

#[tokio::test]
async fn delivered_message_retains_its_original_submission_timestamp() {
    let (_root, s, _) = fixture().await;
    let chat = s
        .chat_create(json!({ "agentId": MAIN_AGENT_ID }))
        .await
        .unwrap();
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    let message = s
        .chat_send(text(&chat, "id"), json!({ "id": id(), "text": "Hello" }))
        .await
        .unwrap();
    let mut chat = chat;
    chat["runId"] = run["id"].clone();
    s.store.put("chats", chat).await.unwrap();
    s.chat_acknowledge(text(&run, "id"), text(&message, "id"))
        .await
        .unwrap();
    let run_id = text(&run, "id").to_owned();
    let events = s
        .store
        .read(move |db| db.events(&run_id, 0, 100))
        .await
        .unwrap();
    let event = events.iter().find(|e| e["type"] == "chat.user").unwrap();
    assert_eq!(event["payload"]["createdAt"], message["createdAt"]);
}

#[tokio::test]
async fn builtin_mcp_endpoint_exposes_scoped_workspace_tools_and_checks_the_run_grant() {
    let (_root, mut s, _) = fixture().await;
    let (listener, address) = common::bind().await;
    let url = format!("http://{address}");
    reconfigure(&mut s, |config| config.public_url = url.clone()).await;
    let router = cairn_installation::http::router(s.clone()).await.unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    let token = &running_run_token(&s, &run).await;
    let client = reqwest::Client::new();
    let endpoint = format!("{url}/mcp-workspace");
    for (method, params) in [
        ("initialize", json!({ "protocolVersion": "2025-11-25" })),
        ("tools/list", json!({})),
    ] {
        let response = client
            .post(&endpoint)
            .bearer_auth(token)
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(response.headers()["cache-control"], "no-store");
        let value: Value = response.json().await.unwrap();
        assert!(value["error"].is_null(), "{value}");
        if method == "tools/list" {
            // Plugin tools such as 1Password's appear only for agents granted an instance.
            let tools = value["result"]["tools"].as_array().unwrap();
            assert_eq!(tools.len(), 13);
            assert!(
                !tools
                    .iter()
                    .any(|tool| tool["name"] == "onepassword" || tool["name"] == "sleep")
            );
            for name in [
                "wait_for",
                "acquire_exclusive",
                "release_exclusive",
                "exclusive_status",
                "renew_exclusive",
            ] {
                assert!(tools.iter().any(|tool| tool["name"] == name));
            }
            assert_eq!(value["result"]["tools"][2]["name"], "list_nodes");
            assert_eq!(value["result"]["tools"][1]["name"], "publish_artifact");
            assert_eq!(value["result"]["tools"][0]["name"], "open_project");
        }
    }
    s.mcps.revoke_run(&s, text(&run, "id")).await.unwrap();
    let response = client
        .post(endpoint)
        .bearer_auth(token)
        .json(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    server.abort();
}

/// Sends one JSON-RPC request to the built-in workspace MCP.
async fn workspace_rpc(s: &Arc<Service>, token: &str, method: &str, params: Value) -> Value {
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let request = common::request("POST", "/mcp-workspace")
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    common::read_json(common::send(&app, request).await).await
}

fn tool_names(listing: &Value) -> Vec<String> {
    let mut names = listing["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| text(tool, "name").to_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[tokio::test]
async fn plan_turns_list_and_call_only_workspace_tools_without_effects() {
    let (_root, s, _) = fixture().await;
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    let planning = json!({ "messageId": "plan-message", "text": "Plan", "plan": true });
    patch_run(
        &s,
        &run,
        json!({ "status": RunStatus::Running, "chatExecution": planning }),
    )
    .await;
    let run = s.store.run(text(&run, "id")).await.unwrap();
    let token = run_token(&s, &run).await;

    let listing = workspace_rpc(&s, &token, "tools/list", json!({})).await;
    assert_eq!(
        tool_names(&listing),
        [
            "exclusive_status",
            "list_nodes",
            "open_project",
            "pending_questions"
        ]
    );
    for (name, arguments) in [
        ("publish_artifact", json!({ "path": "result.md" })),
        ("wait_for", json!({ "delay": 60 })),
        (
            "report_outcome",
            json!({ "status": "completed", "reason": "Done" }),
        ),
        ("acquire_exclusive", json!({})),
        ("onepassword", json!({})),
    ] {
        let response = workspace_rpc(
            &s,
            &token,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await;
        assert_eq!(response["result"]["isError"], true, "{name}: {response}");
        assert!(
            response.to_string().contains("plan turn"),
            "{name}: {response}"
        );
    }
    let pending = workspace_rpc(
        &s,
        &token,
        "tools/call",
        json!({ "name": "pending_questions", "arguments": {} }),
    )
    .await;
    assert!(!pending.to_string().contains("plan turn"), "{pending}");

    // The next turn's grant offers the whole catalog again.
    let next = json!({ "messageId": "next-message", "text": "Implement", "afterPlan": true });
    patch_run(&s, &run, json!({ "chatExecution": next })).await;
    let run = s.store.run(text(&run, "id")).await.unwrap();
    let token = run_token(&s, &run).await;
    let listing = workspace_rpc(&s, &token, "tools/list", json!({})).await;
    assert_eq!(tool_names(&listing).len(), 13);
}

#[tokio::test]
async fn seed_cleanup_does_not_follow_repository_symlinks_outside_its_private_copy() {
    let (root, s, projects) = fixture().await;
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    let prepared = execution::prepare(&run, &s.config, &RunCredentials::default(), None, None)
        .await
        .unwrap();
    let outside = root.path().join("outside");
    std::fs::create_dir_all(outside.join("skills")).unwrap();
    std::fs::write(outside.join("skills/keep"), "host file").unwrap();
    let source = Path::new(text(&projects[0], "path"));
    std::fs::remove_dir_all(source.join(".agents")).unwrap();
    std::os::unix::fs::symlink(&outside, source.join(".agents")).unwrap();
    std::os::unix::fs::symlink(&outside, source.join(".codex")).unwrap();
    let entry = execution::project_seed(
        &run,
        &projects[0],
        &s.config,
        Path::new(text(&prepared, "projectRoot")),
        &RunCredentials::default(),
    )
    .await
    .unwrap();
    assert!(outside.join("skills/keep").exists());
    assert!(!Path::new(text(&entry, "path")).join(".agents").exists());
    assert!(!Path::new(text(&entry, "path")).join(".codex").exists());
}

/// Runs `git` in `directory` with a fixture identity and returns its trimmed output.
fn git(directory: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.test")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

async fn archive_seed(s: &Service, project: &Value, root: &Path) -> PathBuf {
    std::fs::create_dir(root).unwrap();
    let run = json!({
        "id": id(),
        "snapshot": { "agent": { "access": { "backend": "native" } }, "task": { "worktree": true } },
    });
    let entry = execution::project_seed(&run, project, &s.config, root, &RunCredentials::default())
        .await
        .unwrap();
    PathBuf::from(text(&entry, "path"))
}

#[tokio::test]
async fn new_git_work_is_fresh_and_local_snapshots_preserve_the_registered_checkout() {
    let (root, s, _) = fixture().await;
    let origin = root.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    git(&origin, &["init", "-b", "main"]);
    std::fs::write(origin.join("file"), "old").unwrap();
    git(&origin, &["add", "."]);
    git(&origin, &["commit", "-m", "old"]);
    let source = root.path().join("source");
    git(
        root.path(),
        &["clone", origin.to_str().unwrap(), source.to_str().unwrap()],
    );
    let old = git(&source, &["rev-parse", "HEAD"]);
    std::fs::write(origin.join("file"), "current").unwrap();
    git(&origin, &["commit", "-am", "new"]);
    let current = git(&origin, &["rev-parse", "HEAD"]);
    std::fs::write(source.join("file"), "uncommitted").unwrap();
    let mut project = s
        .project_fixture(json!({ "name": "Git", "path": source }), None)
        .await
        .unwrap();
    let fresh = root.path().join("fresh");
    project_git::clone(&source, &fresh, &project, &s.config, &[])
        .await
        .unwrap();
    assert_eq!(git(&fresh, &["rev-parse", "HEAD"]), current);
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), old);
    assert_eq!(
        std::fs::read_to_string(source.join("file")).unwrap(),
        "uncommitted"
    );
    // Exercise the actual isolated seed seam; ordinary clones keep their contract.
    let seed = archive_seed(&s, &project, &root.path().join("seed")).await;
    assert_eq!(
        std::fs::read_to_string(seed.join("file")).unwrap(),
        "current"
    );
    // The same commit reuses a complete archive, while mutable checkouts remain private.
    let cache = s.config.data_dir.join("project-archives");
    let archives_before = std::fs::read_dir(&cache)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "zip")
        })
        .count();
    assert_eq!(archives_before, 1);
    let archive = std::fs::read_dir(&cache)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "zip")
        })
        .unwrap()
        .path();
    let mut damaged = std::fs::read(&archive).unwrap();
    damaged[0] ^= 0xff;
    std::fs::write(&archive, damaged).unwrap();
    // Same-length corruption must rebuild from the authorized remote, not poison later starts.
    std::fs::write(fresh.join("file"), "conversation edit").unwrap();
    let warm = archive_seed(&s, &project, &root.path().join("warm")).await;
    assert_eq!(
        std::fs::read_to_string(warm.join("file")).unwrap(),
        "current"
    );
    assert_eq!(
        std::fs::read_to_string(fresh.join("file")).unwrap(),
        "conversation edit"
    );
    // Changing a live remote to a fork at the same commit must invalidate the
    // archive's Git configuration, including the archive later sent to nodes.
    let fork = root.path().join("fork");
    git(
        root.path(),
        &[
            "clone",
            "--bare",
            origin.to_str().unwrap(),
            fork.to_str().unwrap(),
        ],
    );
    git(
        &source,
        &["remote", "set-url", "origin", fork.to_str().unwrap()],
    );
    for name in ["fork-seed", "fork-warm"] {
        let fork_seed = archive_seed(&s, &project, &root.path().join(name)).await;
        assert_eq!(
            git(&fork_seed, &["config", "--get", "remote.origin.url"]),
            fork.to_str().unwrap()
        );
        assert_eq!(git(&fork_seed, &["rev-parse", "HEAD"]), current);
    }
    git(
        &source,
        &["remote", "set-url", "origin", origin.to_str().unwrap()],
    );
    std::fs::write(origin.join("file"), "next version").unwrap();
    git(&origin, &["commit", "-am", "next version"]);
    let next = archive_seed(&s, &project, &root.path().join("next")).await;
    assert_eq!(
        std::fs::read_to_string(next.join("file")).unwrap(),
        "next version"
    );
    assert_eq!(
        std::fs::read_to_string(warm.join("file")).unwrap(),
        "current"
    );
    project["sourceMode"] = "local".into();
    let local = root.path().join("local");
    project_git::clone(&source, &local, &project, &s.config, &[])
        .await
        .unwrap();
    assert_eq!(git(&local, &["rev-parse", "HEAD"]), old);
    assert_eq!(std::fs::read_to_string(local.join("file")).unwrap(), "old");
    project["sourceMode"] = "remote".into();
    std::fs::rename(&origin, root.path().join("offline")).unwrap();
    assert!(
        project_git::clone(
            &source,
            &root.path().join("failed"),
            &project,
            &s.config,
            &[]
        )
        .await
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(source.join("file")).unwrap(),
        "uncommitted"
    );
}

#[tokio::test]
async fn partial_clone_missing_blobs_are_fetched_from_the_real_promisor() {
    let (root, s, _) = fixture().await;
    let origin = root.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    git(&origin, &["init", "-b", "main"]);
    git(&origin, &["config", "uploadpack.allowFilter", "true"]);
    std::fs::write(origin.join("blob"), "not in the partial object store").unwrap();
    git(&origin, &["add", "."]);
    git(&origin, &["commit", "-m", "initial"]);
    let source = root.path().join("partial");
    git(
        root.path(),
        &[
            "clone",
            "--filter=blob:none",
            "--no-checkout",
            &format!("file://{}", origin.display()),
            source.to_str().unwrap(),
        ],
    );
    let missing = git(
        &source,
        &["rev-list", "--objects", "--missing=print", "HEAD"],
    );
    assert!(
        missing.lines().any(|line| line.starts_with('?')),
        "fixture must actually omit blobs"
    );
    let mut project = s
        .project_fixture(json!({ "name": "Partial", "path": source }), None)
        .await
        .unwrap();
    for mode in ["remote", "local"] {
        project["sourceMode"] = mode.into();
        let target = root.path().join(mode);
        project_git::clone(&source, &target, &project, &s.config, &[])
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("blob")).unwrap(),
            "not in the partial object store"
        );
    }
    assert_eq!(
        git(
            &source,
            &["rev-list", "--objects", "--missing=print", "HEAD"]
        ),
        missing
    );
}

#[tokio::test]
async fn outcome_is_explicit_validated_and_rejected_after_run_revocation() {
    let (_root, s, _) = fixture().await;
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    let bearer = &running_run_token(&s, &run).await;
    let input = json!({
        "status": "blocked",
        "reason": "GitHub workflow permission is missing.",
        "evidence": ["Local tests passed; push was refused."],
    });
    outcome::report(&s, bearer, &input).await.unwrap();
    assert_eq!(
        s.store.run(text(&run, "id")).await.unwrap()["outcome"]["status"],
        "blocked"
    );
    let blank = json!({ "status": "completed", "reason": " ", "evidence": [] });
    assert!(outcome::report(&s, bearer, &blank).await.is_err());

    // Who must act next is kept for work that waits, and dropped for completed work.
    let mut waiting = input.clone();
    waiting["waitingOn"] = "requester".into();
    assert_eq!(
        outcome::report(&s, bearer, &waiting).await.unwrap()["waitingOn"],
        "requester"
    );
    waiting["waitingOn"] = "someone".into();
    assert!(outcome::report(&s, bearer, &waiting).await.is_err());
    let done = json!({
        "status": "completed",
        "reason": "Shipped.",
        "evidence": [],
        "waitingOn": "external",
    });
    assert!(outcome::report(&s, bearer, &done).await.unwrap()["waitingOn"].is_null());
    outcome::report(&s, bearer, &input).await.unwrap();

    patch_run(&s, &run, json!({ "status": RunStatus::Succeeded })).await;
    assert!(outcome::report(&s, bearer, &input).await.is_err());
    assert_eq!(
        s.store.run(text(&run, "id")).await.unwrap()["outcome"]["status"],
        "blocked"
    );
}

#[tokio::test]
async fn an_old_message_grant_cannot_report_an_outcome_for_a_new_turn() {
    let (_root, s, _) = fixture().await;
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    patch_run(
        &s,
        &run,
        json!({ "status": RunStatus::Running, "chatExecution": { "messageId": "first" } }),
    )
    .await;
    let first = s.store.run(text(&run, "id")).await.unwrap();
    let old = run_token(&s, &first).await;
    patch_run(
        &s,
        &run,
        json!({ "chatExecution": { "messageId": "second" }, "outcome": null }),
    )
    .await;
    let input = json!({
        "status": "completed",
        "reason": "Validated and delivered.",
        "evidence": ["Tests passed"],
    });
    assert!(outcome::report(&s, &old, &input).await.is_err());
    let second = s.store.run(text(&run, "id")).await.unwrap();
    assert!(second["outcome"].is_null());
    let current = run_token(&s, &second).await;
    let reported = outcome::report(&s, &current, &input).await.unwrap();
    assert_eq!(reported["messageId"], "second");
}

fn identity_git(home: &Path, directory: &Path, args: &[&str]) -> std::process::Output {
    let mut command = std::process::Command::new("git");
    command
        .args(args)
        .current_dir(directory)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for key in [
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
        "EMAIL",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(key);
    }
    command.output().unwrap()
}

fn assert_commit_identity(home: &Path, directory: &Path, name: &str, email: &str) {
    for args in [
        vec!["init"],
        vec!["commit", "--allow-empty", "-m", "Identity regression"],
    ] {
        let output = identity_git(home, directory, &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = identity_git(
        home,
        directory,
        &["log", "-1", "--format=%an <%ae>%n%cn <%ce>"],
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("{name} <{email}>\n{name} <{email}>")
    );
}

#[tokio::test]
async fn isolated_commits_keep_the_configured_owner_for_main_and_restricted_agents() {
    let (_root, s, _) = fixture().await;
    let owner_config =
        "[include]\n path = identity.gitconfig\n[credential]\n helper = host-only-helper\n";
    std::fs::write(s.config.home.join(".gitconfig"), owner_config).unwrap();
    std::fs::write(
        s.config.home.join("identity.gitconfig"),
        "[user]\n name = Account Owner\n email = owner@example.test\n",
    )
    .unwrap();
    let agent = s
        .agent_fixture(restricted_agent("Another agent"), None)
        .await
        .unwrap();
    for agent_id in [MAIN_AGENT_ID, text(&agent, "id")] {
        let run = run(&s, Value::Null, agent_id).await;
        let prepared = execution::prepare(&run, &s.config, &RunCredentials::default(), None, None)
            .await
            .unwrap();
        let home = run_home(&s, &run);
        assert_commit_identity(
            &home,
            Path::new(text(&prepared, "cwd")),
            "Account Owner",
            "owner@example.test",
        );
        let config = std::fs::read_to_string(home.join(".gitconfig")).unwrap();
        assert!(!config.contains("host-only-helper"));
        assert!(!config.contains("identity.gitconfig"));
    }
    assert_eq!(
        std::fs::read_to_string(s.config.home.join(".gitconfig")).unwrap(),
        owner_config
    );
}

#[tokio::test]
async fn isolated_commits_fall_back_to_the_granted_plugin_account() {
    let (root, s, _) = fixture().await;
    let github = common::github::Github::start(root.path()).await;
    github.attach(&s);
    // An incomplete host identity must not be mixed with a different account.
    std::fs::write(
        s.config.home.join(".gitconfig"),
        "[user]\n name = Incomplete Owner\n",
    )
    .unwrap();
    let agent = s
        .agent_fixture(restricted_agent("Dedicated agent"), None)
        .await
        .unwrap();
    for (agent_id, name, expected) in [
        (MAIN_AGENT_ID, json!("GitHub Owner"), "GitHub Owner"),
        (text(&agent, "id"), Value::Null, "owner-login"),
    ] {
        github.state(&json!({ "user": { "id": 12345, "login": "owner-login", "name": name } }));
        let instance = common::github::instance(&s, expected, "fixture-token", &[agent_id]).await;
        let run = run(&s, Value::Null, agent_id).await;
        let home = run_home(&s, &run);
        std::fs::create_dir_all(&home).unwrap();
        // Also cover replacing the synthetic identity when preparation is retried.
        std::fs::write(
            home.join(".gitconfig"),
            "[user]\n name = Main agent\n email = agent@localhost\n",
        )
        .unwrap();
        let credentials = cairn_installation::plugins::run_credentials::for_run(&s, agent_id, &[])
            .await
            .unwrap();
        let prepared = execution::prepare(&run, &s.config, &credentials, None, None)
            .await
            .unwrap();
        assert_commit_identity(
            &home,
            Path::new(text(&prepared, "cwd")),
            expected,
            "12345+owner-login@users.noreply.github.com",
        );
        cairn_installation::plugins::connections::remove(&s, "github", &instance)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn missing_account_identity_never_fabricates_an_agent_author() {
    let (_root, s, _) = fixture().await;
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    let home = run_home(&s, &run);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join(".gitconfig"),
        "[user]\n name = Main agent\n email = agent@localhost\n",
    )
    .unwrap();
    let prepared = execution::prepare(&run, &s.config, &RunCredentials::default(), None, None)
        .await
        .unwrap();
    let cwd = Path::new(text(&prepared, "cwd"));
    assert!(identity_git(&home, cwd, &["init"]).status.success());
    assert!(
        !identity_git(
            &home,
            cwd,
            &["commit", "--allow-empty", "-m", "No identity"]
        )
        .status
        .success()
    );
    let config = std::fs::read_to_string(home.join(".gitconfig")).unwrap();
    assert!(!config.contains("agent@localhost"));
    assert!(!config.contains("Main agent"));
}

#[tokio::test]
async fn question_tools_only_withdraw_the_authorized_conversations_pending_questions() {
    let (_root, s, _) = fixture().await;
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    let bearer = running_run_token(&s, &run).await;
    let mut chat = s.chat_create(json!({})).await.unwrap();
    chat["runId"] = run["id"].clone();
    let chat_id = text(&chat, "id").to_owned();
    s.store.put("chats", chat).await.unwrap();
    let question = "a".repeat(64);
    s.question_receive(text(&run, "id"), json!({
        "id": question, "blocking": true, "fields": [{ "id": "scope", "title": "Still needed?" }],
    })).await.unwrap();
    let call = |name: &str, args: Value| json!({ "name": name, "arguments": args });
    let pending = project_workspaces::rpc(
        &s,
        &bearer,
        "tools/call",
        &call("pending_questions", json!({})),
    )
    .await
    .unwrap();
    assert_eq!(
        pending["structuredContent"]["questions"][0]["questionId"],
        question
    );
    let cancelled = project_workspaces::rpc(
        &s,
        &bearer,
        "tools/call",
        &call("withdraw_question", json!({ "questionId": question })),
    )
    .await
    .unwrap();
    assert_eq!(cancelled["structuredContent"]["status"], "cancelled");
    // Retrying a withdrawal must not reopen it or send a user message.
    let retried = s.question_cancel(&chat_id, &question).await.unwrap();
    assert_eq!(retried["status"], "cancelled");
    assert_eq!(
        s.chat_detail(&chat_id).await.unwrap()["messages"],
        json!([])
    );

    let other_run = crate::run(&s, Value::Null, MAIN_AGENT_ID).await;
    let other_token = running_run_token(&s, &other_run).await;
    let rejected = project_workspaces::rpc(
        &s,
        &other_token,
        "tools/call",
        &call("withdraw_question", json!({ "questionId": question })),
    )
    .await
    .unwrap();
    assert_eq!(rejected["isError"], true);
    patch_run(&s, &run, json!({ "status": RunStatus::Succeeded })).await;
    let revoked = project_workspaces::rpc(
        &s,
        &bearer,
        "tools/call",
        &call("pending_questions", json!({})),
    )
    .await;
    // Authorization errors can be returned as tool errors, never as question data.
    assert!(revoked.is_err() || revoked.unwrap()["isError"] == true);
}
