use leo_agent_manager::{
    config::{Config, MAIN_AGENT_ID, id},
    execution, project_workspaces,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tempfile::TempDir;

async fn fixture() -> (TempDir, Arc<Service>, Vec<Value>) {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(home.join(".codex/leo-managed-auth"), "1").unwrap();
    let s = Service::new(Config {
        data_dir: root.path().join("data"),
        home,
        workspace_roots: vec![root.path().to_owned()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        setup_token: "fixture".into(),
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        gh_bin: "false".into(),
        concurrency: 2,
        logger: false,
        worker_enabled: false,
        runner_url: "http://runner:4311".into(),
    })
    .await
    .unwrap();
    let mut projects = Vec::new();
    for name in ["First", "Second"] {
        let path = root.path().join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("hello"), name).unwrap();
        projects.push(
            s.project(json!({"name":name,"path":path}), None)
                .await
                .unwrap(),
        );
    }
    (root, s, projects)
}

async fn run(s: &Service, project: Value, agent: &str) -> Value {
    let task = s
        .task(
            json!({"name":"Probe","agentId":agent,"projectId":project,"prompt":"Hello"}),
            None,
        )
        .await
        .unwrap();
    s.enqueue(text(&task, "id"), "manual", None).await.unwrap()
}

#[tokio::test]
async fn empty_start_does_not_touch_unselected_repositories_and_selected_start_loads_only_one() {
    let (_root, s, projects) = fixture().await;
    let mut empty = run(&s, Value::Null, MAIN_AGENT_ID).await;
    // An unavailable, unneeded repository must not prevent a simple chat starting.
    std::fs::remove_dir_all(text(&projects[1], "path")).unwrap();
    let prepared = execution::prepare(&empty, &s.config, None, None, None)
        .await
        .unwrap();
    assert_eq!(prepared["workspaces"], json!([]));
    assert_eq!(
        std::fs::read_dir(text(&prepared, "cwd")).unwrap().count(),
        0
    );
    empty["isolated"] = true.into();
    let prompt = leo_agent_manager::run_output::prompt(&empty, true);
    assert!(prompt.contains("open_project"));
    assert!(
        !prompt.contains(text(&projects[0], "path")),
        "host paths must not be advertised as guest workspaces"
    );
    std::fs::create_dir(text(&projects[1], "path")).unwrap();
    let selected = run(&s, projects[0]["id"].clone(), MAIN_AGENT_ID).await;
    let prepared = execution::prepare(&selected, &s.config, None, None, None)
        .await
        .unwrap();
    assert_eq!(prepared["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(prepared["workspaces"][0]["projectId"], projects[0]["id"]);
    assert_eq!(project_workspaces::catalog(&selected).len(), 2);
    let plan = leo_agent_manager::run_output::chat_plan(
        &selected,
        &prepared,
        Path::new("/tmp"),
        &json!({"args":[]}),
        None,
    );
    assert!(
        plan["writableRoots"]
            .as_array()
            .unwrap()
            .contains(&prepared["projectRoot"])
    );
}

#[tokio::test]
async fn scoped_access_rejects_other_projects_expired_grants_and_changed_permissions() {
    let (_root, s, projects) = fixture().await;
    let agent = s.agent(json!({"name":"Restricted","access":{"projects":[projects[0]["id"]],"skills":[],"mcps":[],"github":false,"sandbox":"read-only"}}),None).await.unwrap();
    let run = run(&s, Value::Null, text(&agent, "id")).await;
    s.store
        .patch_run(text(&run, "id"), json!({"status":"running"}))
        .await
        .unwrap();
    let config = s.mcps.run_configuration(&s, &run).await.unwrap();
    assert!(config["args"].to_string().contains("leo_workspace"));
    let token = text(&config["env"], "LEO_MCP_RUN_TOKEN");
    assert_eq!(
        s.projects
            .open(&s, token, text(&projects[1], "id"))
            .await
            .unwrap_err()
            .status,
        403
    );
    let mut changed = agent.clone();
    changed["access"]["projects"] = json!([]);
    s.store.put("agents", changed).await.unwrap();
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
                axum::Json(json!({"ok":true,"reused":requests.len()>1}))
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    Arc::get_mut(&mut service).unwrap().config.runner_url =
        format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let s = &service;
    let run = run(s, Value::Null, MAIN_AGENT_ID).await;
    let prepared = execution::prepare(&run, &s.config, None, None, None)
        .await
        .unwrap();
    s.store
        .patch_run(
            text(&run, "id"),
            json!({"status":"running","workspaces":[]}),
        )
        .await
        .unwrap();
    s.store
        .set(
            &format!("run-checkpoint:{}", text(&run, "id")),
            json!({"runnerId":id(),"prepared":prepared}),
            None,
        )
        .await
        .unwrap();
    let config = s.mcps.run_configuration(s, &run).await.unwrap();
    let token = text(&config["env"], "LEO_MCP_RUN_TOKEN");
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
    let restored = execution::restore(&saved, prepared, &s.config, None)
        .await
        .unwrap();
    assert_eq!(restored["workspaces"], saved["workspaces"]);
    let mut legacy = restored.clone();
    legacy.as_object_mut().unwrap().remove("projectRoot");
    let upgraded = execution::restore(&saved, legacy, &s.config, None)
        .await
        .unwrap();
    assert_eq!(upgraded["projectRoot"], restored["projectRoot"]);
    assert_eq!(upgraded["workspaces"], restored["workspaces"]);
    assert_eq!(calls.lock().await.len(), 3);
    server.abort();
}

#[tokio::test]
async fn delivered_message_retains_its_original_submission_timestamp() {
    let (_root, s, _) = fixture().await;
    let chat = s
        .chat_create(json!({"agentId":MAIN_AGENT_ID}))
        .await
        .unwrap();
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    let message = s
        .chat_send(text(&chat, "id"), json!({"id":id(),"text":"Hello"}))
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    Arc::get_mut(&mut s).unwrap().config.public_url = url.clone();
    let router = leo_agent_manager::http::router(s.clone()).await.unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    s.store
        .patch_run(text(&run, "id"), json!({"status":"running"}))
        .await
        .unwrap();
    let config = s.mcps.run_configuration(&s, &run).await.unwrap();
    let token = text(&config["env"], "LEO_MCP_RUN_TOKEN");
    let client = reqwest::Client::new();
    let endpoint = format!("{url}/mcp-workspace");
    for (method, params) in [
        ("initialize", json!({"protocolVersion":"2025-11-25"})),
        ("tools/list", json!({})),
    ] {
        let response = client
            .post(&endpoint)
            .bearer_auth(token)
            .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(response.headers()["cache-control"], "no-store");
        let value: Value = response.json().await.unwrap();
        assert!(value["error"].is_null(), "{value}");
        if method == "tools/list" {
            assert_eq!(value["result"]["tools"].as_array().unwrap().len(), 7);
            assert_eq!(value["result"]["tools"][2]["name"], "list_nodes");
            assert_eq!(value["result"]["tools"][1]["name"], "publish_artifact");
            assert_eq!(value["result"]["tools"][0]["name"], "open_project");
        }
    }
    s.mcps.revoke_run(&s, text(&run, "id")).await.unwrap();
    let response = client
        .post(endpoint)
        .bearer_auth(token)
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    server.abort();
}

#[tokio::test]
async fn seed_cleanup_does_not_follow_repository_symlinks_outside_its_private_copy() {
    let (root, s, projects) = fixture().await;
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    let prepared = execution::prepare(&run, &s.config, None, None, None)
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
    )
    .await
    .unwrap();
    assert!(outside.join("skills/keep").exists());
    assert!(!Path::new(text(&entry, "path")).join(".agents").exists());
    assert!(!Path::new(text(&entry, "path")).join(".codex").exists());
}

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
        .project(json!({"name":"Git","path":source}), None)
        .await
        .unwrap();
    let fresh = root.path().join("fresh");
    leo_agent_manager::project_git::clone(&source, &fresh, &project, &s.config)
        .await
        .unwrap();
    assert_eq!(git(&fresh, &["rev-parse", "HEAD"]), current);
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), old);
    assert_eq!(
        std::fs::read_to_string(source.join("file")).unwrap(),
        "uncommitted"
    );
    project["sourceMode"] = "local".into();
    let local = root.path().join("local");
    leo_agent_manager::project_git::clone(&source, &local, &project, &s.config)
        .await
        .unwrap();
    assert_eq!(git(&local, &["rev-parse", "HEAD"]), old);
    assert_eq!(std::fs::read_to_string(local.join("file")).unwrap(), "old");
    project["sourceMode"] = "remote".into();
    std::fs::rename(&origin, root.path().join("offline")).unwrap();
    assert!(
        leo_agent_manager::project_git::clone(
            &source,
            &root.path().join("failed"),
            &project,
            &s.config
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
        .project(json!({"name":"Partial","path":source}), None)
        .await
        .unwrap();
    for mode in ["remote", "local"] {
        project["sourceMode"] = mode.into();
        let target = root.path().join(mode);
        leo_agent_manager::project_git::clone(&source, &target, &project, &s.config)
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
    s.store
        .patch_run(text(&run, "id"), json!({"status":"running"}))
        .await
        .unwrap();
    let config = s.mcps.run_configuration(&s, &run).await.unwrap();
    let bearer = text(&config["env"], "LEO_MCP_RUN_TOKEN");
    let input = json!({"status":"blocked","reason":"GitHub workflow permission is missing.","evidence":["Local tests passed; push was refused."]});
    leo_agent_manager::outcome::report(&s, bearer, &input)
        .await
        .unwrap();
    assert_eq!(
        s.store.run(text(&run, "id")).await.unwrap()["outcome"]["status"],
        "blocked"
    );
    assert!(
        leo_agent_manager::outcome::report(
            &s,
            bearer,
            &json!({"status":"completed","reason":" " ,"evidence":[]})
        )
        .await
        .is_err()
    );
    s.store
        .patch_run(text(&run, "id"), json!({"status":"succeeded"}))
        .await
        .unwrap();
    assert!(
        leo_agent_manager::outcome::report(&s, bearer, &input)
            .await
            .is_err()
    );
    assert_eq!(
        s.store.run(text(&run, "id")).await.unwrap()["outcome"]["status"],
        "blocked"
    );
}

#[tokio::test]
async fn github_sign_in_requests_workflow_and_releases_the_startup_fence() {
    use std::os::unix::fs::PermissionsExt;
    let (root, mut s, _) = fixture().await;
    let script = root.path().join("gh-fixture");
    std::fs::write(&script,"#!/bin/sh\nif [ \"$1\" = '--version' ]; then echo 'gh fixture'; elif [ \"$1\" = api ]; then printf 'HTTP/2 200\\r\\nX-OAuth-Scopes: repo\\r\\n\\r\\nfixture\\n'; else printf '%s\\n' \"$@\" > \"$HOME/login-args\"; fi\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    Arc::get_mut(&mut s).unwrap().config.gh_bin = script.to_string_lossy().into();
    let status = s.connections.status(&s, true).await.unwrap();
    assert_eq!(status[0]["provider"], "github");
    assert_eq!(status[0]["connected"], true);
    assert_eq!(status[0]["workflowPermission"], false);
    assert_eq!(status[0]["account"], "fixture");
    s.connections.start(&s).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while s.store.kv("deployment-lease").await.unwrap().is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        std::fs::read_to_string(s.config.home.join("login-args"))
            .unwrap()
            .contains("--scopes\nworkflow")
    );
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    s.store
        .patch_run(
            text(&run, "id"),
            json!({"status":"running","recoveryPending":true}),
        )
        .await
        .unwrap();
    assert_eq!(s.connections.start(&s).await.unwrap_err().status, 409);
    assert!(s.store.kv("deployment-lease").await.unwrap().is_none());
}

#[tokio::test]
async fn an_old_message_grant_cannot_report_an_outcome_for_a_new_turn() {
    let (_root, s, _) = fixture().await;
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    s.store
        .patch_run(
            text(&run, "id"),
            json!({"status":"running","chatExecution":{"messageId":"first"}}),
        )
        .await
        .unwrap();
    let first = s.store.run(text(&run, "id")).await.unwrap();
    let old = s.mcps.run_configuration(&s, &first).await.unwrap();
    s.store
        .patch_run(
            text(&run, "id"),
            json!({"chatExecution":{"messageId":"second"},"outcome":null}),
        )
        .await
        .unwrap();
    let input = json!({"status":"completed","reason":"Validated and delivered.","evidence":["Tests passed"]});
    assert!(
        leo_agent_manager::outcome::report(&s, text(&old["env"], "LEO_MCP_RUN_TOKEN"), &input)
            .await
            .is_err()
    );
    let second = s.store.run(text(&run, "id")).await.unwrap();
    assert!(second["outcome"].is_null());
    let current = s.mcps.run_configuration(&s, &second).await.unwrap();
    let outcome =
        leo_agent_manager::outcome::report(&s, text(&current["env"], "LEO_MCP_RUN_TOKEN"), &input)
            .await
            .unwrap();
    assert_eq!(outcome["messageId"], "second");
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
    let agent = s.agent(json!({"name":"Another agent","access":{"projects":[],"skills":[],"mcps":[],"github":false}}), None).await.unwrap();
    for agent_id in [MAIN_AGENT_ID, text(&agent, "id")] {
        let run = run(&s, Value::Null, agent_id).await;
        let prepared = execution::prepare(&run, &s.config, None, None, None)
            .await
            .unwrap();
        let home = s
            .config
            .data_dir
            .join("runs")
            .join(text(&run, "id"))
            .join("home");
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
async fn isolated_commits_fall_back_to_the_connected_github_account() {
    use std::os::unix::fs::PermissionsExt;
    let (root, mut s, _) = fixture().await;
    let script = root.path().join("gh-identity");
    std::fs::write(
        &script,
        r#"#!/bin/sh
[ "$*" = 'api --hostname github.com user' ] || exit 1
[ "$GH_CONFIG_DIR" = "$HOME/.config/gh" ] || exit 1
[ -z "$GH_TOKEN$GITHUB_TOKEN" ] || exit 1
[ -f "$GH_CONFIG_DIR/hosts.yml" ] || exit 1
cat "$HOME/github-profile.json"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    Arc::get_mut(&mut s).unwrap().config.gh_bin = script.to_string_lossy().into_owned();
    std::fs::create_dir_all(s.config.home.join(".config/gh")).unwrap();
    std::fs::write(
        s.config.home.join(".config/gh/hosts.yml"),
        "github.com: {}\n",
    )
    .unwrap();
    // An incomplete host identity must not be mixed with a different account.
    std::fs::write(
        s.config.home.join(".gitconfig"),
        "[user]\n name = Incomplete Owner\n",
    )
    .unwrap();
    let agent = s.agent(json!({"name":"Dedicated agent","access":{"projects":[],"skills":[],"mcps":[],"github":false}}), None).await.unwrap();
    for (agent_id, token, name, expected) in [
        (MAIN_AGENT_ID, None, json!("GitHub Owner"), "GitHub Owner"),
        (
            text(&agent, "id"),
            Some("fixture-token"),
            Value::Null,
            "owner-login",
        ),
    ] {
        let run = run(&s, Value::Null, agent_id).await;
        let home = s
            .config
            .data_dir
            .join("runs")
            .join(text(&run, "id"))
            .join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("github-profile.json"),
            json!({"id":12345,"login":"owner-login","name":name,"email":"public@example.test"})
                .to_string(),
        )
        .unwrap();
        // Also cover replacing the synthetic identity when preparation is retried.
        std::fs::write(
            home.join(".gitconfig"),
            "[user]\n name = Main agent\n email = agent@localhost\n",
        )
        .unwrap();
        let prepared = execution::prepare(&run, &s.config, token, None, None)
            .await
            .unwrap();
        assert_commit_identity(
            &home,
            Path::new(text(&prepared, "cwd")),
            expected,
            "12345+owner-login@users.noreply.github.com",
        );
    }
}

#[tokio::test]
async fn missing_account_identity_never_fabricates_an_agent_author() {
    let (_root, s, _) = fixture().await;
    let run = run(&s, Value::Null, MAIN_AGENT_ID).await;
    let home = s
        .config
        .data_dir
        .join("runs")
        .join(text(&run, "id"))
        .join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join(".gitconfig"),
        "[user]\n name = Main agent\n email = agent@localhost\n",
    )
    .unwrap();
    let prepared = execution::prepare(&run, &s.config, None, None, None)
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
