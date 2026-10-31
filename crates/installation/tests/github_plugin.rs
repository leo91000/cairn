//! GitHub is the bundled GitHub package: Luau over generic plugin APIs. The owner signs
//! in through the host's device flow, granted agents' runs receive the account's
//! credentials, projects are imported through its project source, and data saved
//! before the package exists moves into instances. GitHub is simulated locally.
#[allow(unused_imports)]
use common::{PluginServiceFixture as _, PluginStoreFixture as _};

use common::AgentFixture;
use common::MissionFixture;

mod common;

use cairn_installation::{
    config::MAIN_AGENT_ID,
    execution,
    plugins::{connections, device_flow, registry, run_credentials},
    project_workspaces, run_output,
    service::Service,
    validation::text,
};
use common::github::{self, Github};
use serde_json::{Value, json};
use std::{io::Write, os::unix::fs::PermissionsExt, path::Path, sync::Arc, time::Duration};
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
    common::relay_fixture::claimed(&s).await.unwrap();
    Fixture { root, s, github }
}

fn restricted(name: &str) -> Value {
    json!({ "name": name, "access": { "projects": [], "skills": [], "mcps": [] } })
}

async fn run(s: &Service, agent: &str) -> Value {
    let task = s
        .task_fixture(
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
    let early = common::save_plugin(
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
    let form = common::save_plugin(
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
    let agent =
        f.s.agent_fixture(restricted("Builder"), None)
            .await
            .unwrap();
    let agent_id = text(&agent, "id").to_owned();
    let other =
        f.s.agent_fixture(restricted("Bystander"), None)
            .await
            .unwrap();
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
    common::enable_plugin(&f.s, "github", &instance, &json!({ "enabled": false }))
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
    let agent =
        f.s.agent_fixture(restricted("Builder"), None)
            .await
            .unwrap();
    let first = github::instance(&f.s, "First", "fixture-first", &[]).await;
    let second = github::instance(&f.s, "Second", "fixture-second", &[]).await;
    let mut edited = agent.clone();
    edited["plugins"] = json!([first, second]);
    let error =
        f.s.agent_fixture(edited.clone(), Some(text(&agent, "id")))
            .await
            .unwrap_err();
    assert!(
        error.message.contains("one GitHub instance per agent"),
        "{}",
        error.message
    );
    edited["plugins"] = json!([first]);
    f.s.agent_fixture(edited, Some(text(&agent, "id")))
        .await
        .unwrap();
    // Existing instance editors must use a revision-guarded profile command.
    let refused = common::save_plugin(
        &f.s,
        "github",
        &json!({ "name": "Second", "enabled": true, "agentIds": [agent["id"]] }),
        Some(&second),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.status, 400);
    assert!(refused.message.contains("current revision"));
    assert_eq!(
        f.s.agent_snapshot(text(&agent, "id")).await.unwrap()["plugins"],
        json!([first])
    );

    // Long display names are cut on character boundaries.
    f.github
        .state(&json!({ "user": { "id": 7, "login": "long", "name": "é".repeat(150) } }));
    let long = github::instance(&f.s, "Long", "fixture-long", &[]).await;
    let saved = f.s.instance_fixture(&long).await.unwrap();
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
    let agent =
        f.s.agent_fixture(restricted("Watcher"), None)
            .await
            .unwrap();
    let agent_id = text(&agent, "id").to_owned();
    let instance = github::instance(&f.s, "GitHub", "fixture-token", &[&agent_id]).await;
    let run = run(&f.s, &agent_id).await;
    let run =
        f.s.store
            .change_run(
                text(&run, "id"),
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Running),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    let run = f.s.store.run(&run.id).await.unwrap();
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
        identity: Some(cairn_installation::auth::InstallationIdentity::trusted(
            cairn_installation::auth::InstallationRole::Owner,
            "fixture-owner",
        )),
    };
    let page = cairn_installation::api::dispatch(
        &f.s,
        &input(&format!("source=repositories&instance={instance}")),
    )
    .await
    .unwrap();
    let page = common::resource_result(&f.s, page).await.unwrap();
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], "fixture/repo");
    assert_eq!(items[0]["projectName"], "repo");
    assert_eq!(items[0]["badges"], json!(["Private", "Rust"]));
    assert_eq!(items[1]["badges"], json!(["Public", "Fork", "Archived"]));
    assert_eq!(items[0]["imported"], false);
    assert!(page["nextPage"].is_null());

    let request = json!({ "pluginId": "github", "source": "repositories", "instance": instance, "id": "fixture/repo" });
    let project = common::owner_api_route(&f.s, "POST", "/api/projects/import", request.clone())
        .await
        .unwrap();
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
        common::owner_api_route(&f.s, "POST", "/api/projects/import", request.clone())
            .await
            .unwrap()["id"],
        project["id"]
    );
    let page = cairn_installation::api::dispatch(
        &f.s,
        &input(&format!("source=repositories&instance={instance}")),
    )
    .await
    .unwrap();
    let page = common::resource_result(&f.s, page).await.unwrap();
    assert_eq!(page["items"][0]["imported"], true);
    let mut unknown = request.clone();
    unknown["source"] = "elsewhere".into();
    unknown["id"] = "fixture/other".into();
    assert_eq!(
        common::owner_api_route(&f.s, "POST", "/api/projects/import", unknown)
            .await
            .unwrap_err()
            .status,
        404
    );
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
    let saved = common::save_plugin(
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
    let test = common::plugin_test(&s, "github", text(&saved, "id"))
        .await
        .unwrap();
    println!("LIVE_GITHUB_TEST {test}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pending_project_import_excludes_only_its_own_installation() {
    let first = fixture().await;
    let other = fixture().await;
    let started = first.root.path().join("import-started");
    let released = first.root.path().join("import-released");
    let mut requests = Vec::new();
    for fixture in [&first, &other] {
        std::fs::write(
            fixture.s.config.home.join(".gitconfig"),
            format!(
                "[url \"file://{}/missing/\"]\n\tinsteadOf = https://github.com/\n",
                fixture.root.path().display()
            ),
        )
        .unwrap();
        fixture.github.state(&json!({"repositories":{"fixture/repo":{"full_name":"fixture/repo","name":"repo","default_branch":"main"}}}));
        let instance = github::instance(&fixture.s, "Source", "synthetic-project-token", &[]).await;
        requests.push(json!({"pluginId":"github","source":"repositories","instance":instance,"id":"fixture/repo"}));
    }
    first.github.state(&json!({"repositories":{"fixture/repo":{"full_name":"fixture/repo","name":"repo","default_branch":"main"}}, "gate":{"path":"repos/fixture/repo","started":started,"released":released}}));
    let service = first.s.clone();
    let body = requests[0].clone();
    let pending = tokio::spawn(async move {
        common::owner_api_route(&service, "POST", "/api/projects/import", body).await
    });
    tokio::time::timeout(Duration::from_secs(15), async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        common::owner_api_route(
            &first.s,
            "POST",
            "/api/projects/import",
            requests[0].clone()
        )
        .await
        .unwrap_err()
        .status,
        409
    );
    // The other installation reaches its own local Git fixture, rather than
    // inheriting the first installation's import conflict. No real remote is used.
    let independent = tokio::time::timeout(
        Duration::from_secs(15),
        common::owner_api_route(
            &other.s,
            "POST",
            "/api/projects/import",
            requests[1].clone(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(independent.unwrap_err().status, 400);
    assert!(
        other
            .github
            .api_calls()
            .iter()
            .any(|call| call["path"] == "repos/fixture/repo")
    );
    std::fs::write(released, "release").unwrap();
    assert_eq!(pending.await.unwrap().unwrap_err().status, 400);
    assert!(first.s.store.projects().await.unwrap().is_empty());
    assert!(other.s.store.projects().await.unwrap().is_empty());
}
