//! GitHub is the bundled GitHub package: Luau over generic plugin APIs. The owner signs
//! in through the host's device flow, granted agents' runs receive the account's
//! credentials, projects are imported through its project source, and data saved
//! before the package exists moves into instances. GitHub is simulated locally.
mod common;

use cairn_installation::{
    config::{MAIN_AGENT_ID, id},
    execution,
    plugins::{connections, device_flow, projects, registry, run_credentials},
    project_workspaces, run_output,
    service::Service,
    validation::text,
};
use common::github::{self, Github};
use serde_json::{Value, json};
use std::{
    collections::HashSet, io::Write, os::unix::fs::PermissionsExt, path::Path, sync::Arc,
    time::Duration,
};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
    s: Arc<Service>,
    github: Github,
}

async fn fixture() -> Fixture {
    let root = TempDir::new().unwrap();
    common::managed_codex_home(&root.path().join("home"));
    let github = Github::start(root.path()).await;
    let s = Service::new(cairn_installation::config::Config {
        runner_url: "http://runner:4311".into(),
        ..common::config(root.path())
    })
    .await
    .unwrap();
    github.attach(&s);
    Fixture { root, s, github }
}

fn restricted(name: &str) -> Value {
    json!({ "name": name, "access": { "projects": [], "skills": [], "mcps": [] } })
}

async fn run(s: &Service, agent: &str) -> Value {
    let task = s
        .task(
            json!({ "name": "Probe", "agentId": agent, "prompt": "Hello" }),
            None,
        )
        .await
        .unwrap();
    s.enqueue(text(&task, "id"), "manual", None).await.unwrap()
}

fn run_directory(s: &Service, run: &Value) -> std::path::PathBuf {
    s.config.data_dir.join("runs").join(text(run, "id"))
}

#[tokio::test]
async fn the_owner_signs_in_through_the_host_and_the_token_never_leaves_it() {
    let f = fixture().await;
    let started = device_flow::start(&f.s, "github").await.unwrap();
    assert_eq!(started["state"], "pending");
    assert_eq!(started["userCode"], "WDJB-MJHT");
    assert_eq!(
        started["verificationUri"],
        "https://github.com/login/device"
    );
    let flow = text(&started, "id").to_owned();
    let request = f
        .github
        .calls()
        .into_iter()
        .find(|call| call["path"] == "login/device/code")
        .unwrap();
    let body = text(&request, "body");
    assert!(body.contains("client_id=") && body.contains("scope=repo+workflow+read%3Aorg"));

    // Saving before the owner approves is refused.
    let early = connections::save(
        &f.s,
        "github",
        &json!({ "name": "GitHub", "enabled": true, "credentialFlow": flow }),
        None,
    )
    .await;
    assert_eq!(early.unwrap_err().status, 400);

    f.github.state(&json!({ "device": "approved" }));
    let completed = common::eventually(
        Duration::from_secs(10),
        Duration::from_millis(50),
        || async {
            let status = device_flow::status(&f.s, "github", &flow).unwrap();
            (status["state"] == "completed").then_some(status)
        },
    )
    .await;
    assert!(!completed.to_string().contains(github::USER_TOKEN));

    // The package's form checks the account with the signed-in token, unsaved.
    let form = connections::save(
        &f.s,
        "github",
        &json!({ "name": "GitHub", "enabled": true, "credentialFlow": flow, "agentIds": [MAIN_AGENT_ID] }),
        None,
    )
    .await
    .unwrap();
    assert!(!form.to_string().contains(github::USER_TOKEN));
    assert_eq!(form["settings"]["login"], "octo");
    assert_eq!(form["settings"]["gitName"], "Octo Cat");
    assert_eq!(
        form["settings"]["gitEmail"],
        "583231+octo@users.noreply.github.com"
    );
    let account = f
        .github
        .calls()
        .into_iter()
        .rfind(|call| call["path"] == "user")
        .unwrap();
    assert_eq!(account["token"], github::USER_TOKEN);
    assert_eq!(account["userAgent"], "Cairn");
    // The sign-in is consumed by the save.
    assert_eq!(
        device_flow::status(&f.s, "github", &flow)
            .unwrap_err()
            .status,
        404
    );

    // A declined sign-in fails with a message and keeps nothing.
    let declined = device_flow::start(&f.s, "github").await.unwrap();
    let declined = text(&declined, "id").to_owned();
    f.github.state(&json!({ "device": "denied" }));
    let failed = common::eventually(
        Duration::from_secs(10),
        Duration::from_millis(50),
        || async {
            let status = device_flow::status(&f.s, "github", &declined).unwrap();
            (status["state"] == "failed").then_some(status)
        },
    )
    .await;
    assert_eq!(failed["message"], "The sign-in was declined.");
    assert!(device_flow::start(&f.s, "onepassword").await.is_err());
}

#[tokio::test]
async fn granted_runs_receive_the_account_and_lose_it_with_the_grant() {
    let f = fixture().await;
    let agent = f.s.agent(restricted("Builder"), None).await.unwrap();
    let agent_id = text(&agent, "id").to_owned();
    let other = f.s.agent(restricted("Bystander"), None).await.unwrap();
    let instance = github::instance(
        &f.s,
        "GitHub",
        "gho_fixture_run_token_1234567890",
        &[&agent_id],
    )
    .await;

    let granted = run(&f.s, &agent_id).await;
    let credentials = run_credentials::for_run(&f.s, &agent_id, &[])
        .await
        .unwrap();
    execution::prepare(&granted, &f.s.config, &credentials, None, None)
        .await
        .unwrap();
    let directory = run_directory(&f.s, &granted);
    let home = directory.join("home");
    let hosts = std::fs::read_to_string(home.join(".config/gh/hosts.yml")).unwrap();
    assert!(hosts.contains("oauth_token: gho_fixture_run_token_1234567890"));
    let store = std::fs::read_to_string(home.join(".config/cairn/git-credentials")).unwrap();
    assert_eq!(
        store,
        "https://x-access-token:gho_fixture_run_token_1234567890@github.com\n"
    );
    assert!(
        credentials
            .secrets
            .contains(&"gho_fixture_run_token_1234567890".to_owned())
    );
    let main = std::fs::read_to_string(home.join(".gitconfig")).unwrap();
    assert!(main.contains("path = .config/cairn/gitconfig"));
    let gitconfig = std::fs::read_to_string(home.join(".config/cairn/gitconfig")).unwrap();
    assert!(gitconfig.contains("[credential \"https://github.com\"]"));
    assert!(gitconfig.contains("helper = store --file ~/.config/cairn/git-credentials"));
    assert!(gitconfig.contains("name = Octo Cat"));
    let mode = std::fs::metadata(home.join(".config/gh/hosts.yml"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "credential files are private");
    // Git itself answers with the account's token from the run's home.
    let mut fill = std::process::Command::new("git")
        .args(["credential", "fill"])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", std::env::var("PATH").unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fill.stdin
        .take()
        .unwrap()
        .write_all(b"protocol=https\nhost=github.com\npath=fixture/repo.git\n\n")
        .unwrap();
    let answer = String::from_utf8(fill.wait_with_output().unwrap().stdout).unwrap();
    assert!(answer.contains("username=x-access-token"), "{answer}");
    assert!(
        answer.contains("password=gho_fixture_run_token_1234567890"),
        "{answer}"
    );

    // An agent without a grant gets nothing.
    let ungranted = run(&f.s, text(&other, "id")).await;
    let none = run_credentials::for_run(&f.s, text(&other, "id"), &[])
        .await
        .unwrap();
    execution::prepare(&ungranted, &f.s.config, &none, None, None)
        .await
        .unwrap();
    let other_home = run_directory(&f.s, &ungranted).join("home");
    assert!(!other_home.join(".config/gh/hosts.yml").exists());
    assert_eq!(
        std::fs::read_to_string(other_home.join(".config/cairn/git-credentials")).unwrap(),
        ""
    );

    // Withdrawing the grant empties the files when the run resumes.
    connections::set_enabled(&f.s, "github", &instance, &json!({ "enabled": false }))
        .await
        .unwrap();
    let withdrawn = run_credentials::for_run(&f.s, &agent_id, &[])
        .await
        .unwrap();
    assert!(withdrawn.files.is_empty() && withdrawn.git.is_empty());
    let prepared = execution::prepare(&granted, &f.s.config, &withdrawn, None, None)
        .await
        .unwrap();
    execution::restore(&granted, prepared, &f.s.config, &withdrawn)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(home.join(".config/gh/hosts.yml")).unwrap(),
        ""
    );
    // The resumed run's Git loses the helper and the account's identity with it.
    let gitconfig = std::fs::read_to_string(home.join(".config/cairn/gitconfig")).unwrap();
    assert!(!gitconfig.contains("helper"), "{gitconfig}");
    assert!(!gitconfig.contains("Octo Cat"), "{gitconfig}");
    assert_eq!(
        execution::credential_files(&directory).await,
        [".config/gh/hosts.yml"]
    );
    execution::clear_run_credentials(&directory).await;
    assert!(!home.join(".config/gh/hosts.yml").exists());
    assert!(!home.join(".config/cairn/git-credentials").exists());
}

#[tokio::test]
async fn each_agent_runs_with_one_account_and_tokens_are_masked() {
    let f = fixture().await;
    let agent = f.s.agent(restricted("Builder"), None).await.unwrap();
    let first = github::instance(&f.s, "First", "fixture-first", &[]).await;
    let second = github::instance(&f.s, "Second", "fixture-second", &[]).await;
    let mut edited = agent.clone();
    edited["plugins"] = json!([first, second]);
    let error =
        f.s.agent(edited.clone(), Some(text(&agent, "id")))
            .await
            .unwrap_err();
    assert!(
        error.message.contains("one GitHub instance per agent"),
        "{}",
        error.message
    );
    edited["plugins"] = json!([first]);
    f.s.agent(edited, Some(text(&agent, "id"))).await.unwrap();
    // Granting from the instance side is refused the same way.
    let refused = connections::save(
        &f.s,
        "github",
        &json!({ "name": "Second", "enabled": true, "agentIds": [agent["id"]] }),
        Some(&second),
    )
    .await
    .unwrap_err();
    assert!(refused.message.contains("one GitHub instance per agent"));

    // Long display names are cut on character boundaries.
    f.github
        .state(&json!({ "user": { "id": 7, "login": "long", "name": "é".repeat(150) } }));
    let long = github::instance(&f.s, "Long", "fixture-long", &[]).await;
    let saved = f.s.get(connections::KIND, &long).await.unwrap();
    assert_eq!(saved["settings"]["gitName"], "é".repeat(100));

    let package = registry::builtin("github").unwrap();
    run_output::set_plugin_secret_patterns(package.manifest.secret_regexes());
    let output = run_output::redact(
        "pushed with ghp_abcdefghijklmnopqrstuvwxyz and github_pat_11ABCDEFGHIJ0123456789_abc",
        &[],
    );
    assert_eq!(output, "pushed with [redacted] and [redacted]");
    run_output::set_plugin_secret_patterns(Vec::new());
}

#[tokio::test]
async fn granted_agents_see_github_conditions_and_others_do_not() {
    let f = fixture().await;
    let agent = f.s.agent(restricted("Watcher"), None).await.unwrap();
    let agent_id = text(&agent, "id").to_owned();
    let instance = github::instance(&f.s, "GitHub", "fixture-token", &[&agent_id]).await;
    let run = run(&f.s, &agent_id).await;
    let run =
        f.s.store
            .patch_run(text(&run, "id"), json!({ "status": "running" }))
            .await
            .unwrap();
    let config = f.s.mcps.run_configuration(&f.s, &run).await.unwrap();
    let bearer = text(&config["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
    let types = |listing: &Value| {
        listing["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "wait_for")
            .unwrap()["inputSchema"]["properties"]["conditions"]["items"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|condition| condition["properties"]["type"]["const"].clone())
            .collect::<Vec<_>>()
    };
    let listing = project_workspaces::rpc(&f.s, &bearer, "tools/list", &json!({}))
        .await
        .unwrap();
    assert_eq!(
        types(&listing),
        [
            json!("delay"),
            json!("at"),
            json!("run_finished"),
            json!("github_workflow"),
            json!("github_pr_checks")
        ]
    );
    connections::remove(&f.s, "github", &instance)
        .await
        .unwrap();
    let listing = project_workspaces::rpc(&f.s, &bearer, "tools/list", &json!({}))
        .await
        .unwrap();
    assert_eq!(
        types(&listing),
        [json!("delay"), json!("at"), json!("run_finished")]
    );
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(directory)
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
async fn repositories_are_listed_and_imported_with_the_instance() {
    let f = fixture().await;
    // github.com remotes resolve to a local bare repository for this test.
    let remotes = f.root.path().join("remotes");
    let bare = remotes.join("fixture/repo.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "--bare", "-b", "trunk"]);
    let work = f.root.path().join("work");
    git(
        f.root.path(),
        &["clone", bare.to_str().unwrap(), work.to_str().unwrap()],
    );
    std::fs::write(work.join("README.md"), "hello").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-m", "initial"]);
    git(&work, &["push", "origin", "HEAD:trunk"]);
    std::fs::write(
        f.s.config.home.join(".gitconfig"),
        format!(
            "[url \"file://{}/\"]\n\tinsteadOf = https://github.com/\n",
            remotes.display()
        ),
    )
    .unwrap();
    f.github.state(&json!({
        "repos": [
            { "full_name": "fixture/repo", "name": "repo", "description": "The fixture", "default_branch": "trunk", "private": true, "language": "Rust", "pushed_at": "2026-10-01T10:00:00Z" },
            { "full_name": "fixture/fork", "name": "fork", "fork": true, "archived": true },
            { "full_name": "../escape", "name": "escape" },
        ],
        "repositories": {
            "fixture/repo": { "full_name": "fixture/repo", "name": "repo", "description": "The fixture", "default_branch": "trunk" },
        },
    }));
    let instance = github::instance(&f.s, "GitHub", "fixture-token", &[]).await;
    let input = |query: &str| cairn_installation::http::Input {
        method: "GET".into(),
        path: "/api/plugins/github/projects".into(),
        query: serde_urlencoded::from_str(query).unwrap(),
        headers: axum::http::HeaderMap::default(),
        body: Value::Null,
        identity: None,
    };
    let page = projects::list(
        &f.s,
        "github",
        &input(&format!("source=repositories&instance={instance}")),
    )
    .await
    .unwrap();
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], "fixture/repo");
    assert_eq!(items[0]["projectName"], "repo");
    assert_eq!(items[0]["badges"], json!(["Private", "Rust"]));
    assert_eq!(items[1]["badges"], json!(["Public", "Fork", "Archived"]));
    assert_eq!(items[0]["imported"], false);
    assert!(page["nextPage"].is_null());

    let request = json!({ "pluginId": "github", "source": "repositories", "instance": instance, "id": "fixture/repo" });
    let project = projects::import(&f.s, &request).await.unwrap();
    assert_eq!(project["name"], "repo");
    assert_eq!(project["baseBranch"], "trunk");
    assert_eq!(project["source"]["pluginId"], "github");
    assert_eq!(project["source"]["instance"], instance);
    assert_eq!(project["source"]["id"], "fixture/repo");
    assert_eq!(
        std::fs::read_to_string(Path::new(text(&project, "path")).join("README.md")).unwrap(),
        "hello"
    );
    // Imports are idempotent and the listing marks the repository.
    assert_eq!(
        projects::import(&f.s, &request).await.unwrap()["id"],
        project["id"]
    );
    let page = projects::list(
        &f.s,
        "github",
        &input(&format!("source=repositories&instance={instance}")),
    )
    .await
    .unwrap();
    assert_eq!(page["items"][0]["imported"], true);
    let mut unknown = request.clone();
    unknown["source"] = "elsewhere".into();
    unknown["id"] = "fixture/other".into();
    assert_eq!(
        projects::import(&f.s, &unknown).await.unwrap_err().status,
        404
    );
}

#[tokio::test]
async fn data_saved_before_the_package_moves_into_instances() {
    let root = TempDir::new().unwrap();
    common::managed_codex_home(&root.path().join("home"));
    let config = common::config(root.path());
    let s = Service::new(config.clone()).await.unwrap();
    s.agent(json!({ "name": "Main agent" }), Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    let scoped = s.agent(restricted("Scoped"), None).await.unwrap();
    let unused = s
        .agent(json!({ "name": "Unused token" }), None)
        .await
        .unwrap();
    let scoped_id = text(&scoped, "id").to_owned();
    let unused_id = text(&unused, "id").to_owned();
    // Earlier releases stored the setting and plaintext dedicated tokens.
    for (agent, github) in [
        (MAIN_AGENT_ID, true),
        (scoped_id.as_str(), false),
        (unused_id.as_str(), true),
    ] {
        let mut record = s.get("agents", agent).await.unwrap();
        record["access"]["github"] = github.into();
        if !github {
            record["access"].as_object_mut().unwrap().remove("mcps");
        }
        s.store.put("agents", record).await.unwrap();
    }
    for agent in [&scoped_id, &unused_id] {
        s.store
            .set(
                &format!("agent-github:{agent}"),
                json!(format!("github_pat_{agent}")),
                None,
            )
            .await
            .unwrap();
    }
    std::fs::create_dir_all(config.home.join(".config/gh")).unwrap();
    std::fs::write(
        config.home.join(".config/gh/hosts.yml"),
        "github.com:\n    users:\n        octo:\n            oauth_token: gho_shared_login\n    git_protocol: https\n    user: octo\n",
    )
    .unwrap();
    let project = id();
    s.store
        .put(
            "projects",
            json!({ "id": project, "name": "Repo", "path": "/srv/repo", "origin": "github.com:fixture/repo.git", "baseBranch": "main", "sourceMode": "remote" }),
        )
        .await
        .unwrap();
    s.store
        .set(
            "skill-source:global/review",
            json!({ "type": "github", "repository": "fixture/skills", "path": "skills/review", "ref": "main", "commit": "a".repeat(40), "importedAt": 1, "revision": "r1" }),
            None,
        )
        .await
        .unwrap();
    // The scoped agent answers anyone in a channel: it must hold no plugin grant.
    s.store
        .put(
            connections::KIND,
            json!({ "id": id(), "pluginId": "discord", "name": "#support", "enabled": true, "agentId": scoped_id, "accessMode": "everyone", "agentIds": [], "grantedAccess": { "github": false } }),
        )
        .await
        .unwrap();
    let chat = id();
    s.store
        .put(
            "chats",
            json!({ "id": chat, "wakeup": { "condition": { "type": "github_pr_checks", "projectId": project, "prNumber": 7, "headSha": "1".repeat(40) } } }),
        )
        .await
        .unwrap();
    s.store
        .transaction(|db| {
            db.0.execute_batch("PRAGMA user_version=7;")?;
            Ok(())
        })
        .await
        .unwrap();
    drop(s);

    let s = Service::new(config.clone()).await.unwrap();
    let database = rusqlite::Connection::open(config.data_dir.join("manager.db")).unwrap();
    let version: i64 = database
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert!(version > 7, "The old manager must refuse migrated accounts");
    let mut instances = s
        .store
        .list(connections::KIND)
        .await
        .unwrap()
        .into_iter()
        .filter(|instance| instance["pluginId"] == "github")
        .collect::<Vec<_>>();
    instances.sort_by_key(|instance| text(instance, "name").to_owned());
    let names = instances
        .iter()
        .map(|instance| text(instance, "name"))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["GitHub", "GitHub (Scoped)", "GitHub (Unused token)"]
    );
    let shared = &instances[0];
    let mut shared_agents = shared["agentIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect::<HashSet<_>>();
    assert!(shared_agents.remove(MAIN_AGENT_ID));
    assert!(shared_agents.remove(&unused_id));
    assert!(shared_agents.is_empty());
    assert_eq!(instances[1]["agentIds"], json!([]));
    assert_eq!(instances[2]["agentIds"], json!([]));
    let token = |instance: &Value| {
        let id = text(instance, "id").to_owned();
        let vault = s.vault.clone();
        async move {
            vault
                .get(&connections::credential_key(&id))
                .await
                .unwrap()
                .unwrap()["token"]
                .clone()
        }
    };
    assert_eq!(token(shared).await, "gho_shared_login");
    assert_eq!(
        token(&instances[1]).await,
        format!("github_pat_{scoped_id}")
    );
    // Plaintext tokens are gone; the vault holds them encrypted.
    assert!(s.store.keys("agent-github:").await.unwrap().is_empty());

    for agent in [MAIN_AGENT_ID, &scoped_id, &unused_id] {
        let record = s.get("agents", agent).await.unwrap();
        assert!(record["access"].get("github").is_none(), "{record}");
    }
    let scoped = s.get("agents", &scoped_id).await.unwrap();
    assert_eq!(scoped["plugins"], json!([]));
    // A GitHub-only restriction keeps restricting MCP connections.
    assert_eq!(scoped["access"]["mcps"], json!([]));

    let migrated = s.get("projects", &project).await.unwrap();
    assert_eq!(migrated["source"]["id"], "fixture/repo");
    assert_eq!(migrated["source"]["instance"], shared["id"]);
    let source = s
        .store
        .kv("skill-source:global/review")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(source["pluginId"], "github");
    assert_eq!(
        source["location"],
        json!({ "repository": "fixture/skills", "ref": "main" })
    );
    assert_eq!(source["folder"], "skills/review");
    assert_eq!(source["version"], "a".repeat(40));
    assert_eq!(source["revision"], "r1");
    let wait = s.get("chats", &chat).await.unwrap();
    assert_eq!(wait["wakeup"]["condition"]["pluginId"], "github");

    // A second start changes nothing.
    drop(s);
    let s = Service::new(config).await.unwrap();
    assert_eq!(s.store.list(connections::KIND).await.unwrap().len(), 4);
}

/// The bundled package against the real GitHub: run with `CAIRN_LIVE_GITHUB=<file>`;
/// the user code is written to that file for someone to approve on github.com.
#[tokio::test]
#[ignore = "needs github.com and a person approving the sign-in"]
async fn live_device_sign_in_against_github() {
    let file = std::env::var("CAIRN_LIVE_GITHUB").unwrap();
    let root = TempDir::new().unwrap();
    common::managed_codex_home(&root.path().join("home"));
    let s = Service::new(common::config(root.path())).await.unwrap();
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let started = device_flow::start(&s, "github").await.unwrap();
    std::fs::write(&file, text(&started, "userCode")).unwrap();
    let flow = text(&started, "id").to_owned();
    let status = common::eventually(Duration::from_secs(300), Duration::from_secs(2), || async {
        let status = device_flow::status(&s, "github", &flow).unwrap();
        (status["state"] != "pending").then_some(status)
    })
    .await;
    assert_eq!(status["state"], "completed", "{status}");
    let saved = connections::save(
        &s,
        "github",
        &json!({ "name": "GitHub", "enabled": true, "credentialFlow": flow }),
        None,
    )
    .await
    .unwrap();
    println!(
        "LIVE_GITHUB {}",
        json!({ "login": saved["settings"]["login"], "gitEmail": saved["settings"]["gitEmail"] })
    );
    let test = cairn_installation::plugins::panels::test(&s, "github", text(&saved, "id"))
        .await
        .unwrap();
    println!("LIVE_GITHUB_TEST {test}");
}
