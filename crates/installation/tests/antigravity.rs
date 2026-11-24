mod common;

use cairn_installation::{
    accounts::broker, antigravity, config::Config, provider::Provider, service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const KEY: &str = "synthetic-gemini-key-never-a-real-credential";
const MODEL: &str = "gemini-4-neon";
const RUN: &str = "antigravity-test";
const PLAN_REFUSAL: &str = "This is a Cairn plan turn";
const NATIVE_QUESTION_REFUSAL: &str = "Antigravity's own question tool is unavailable in Cairn";

/// The executable Cairn launches as `agy`. Integration tests run from a test
/// executable whose own `antigravity-hook` mode does not exist, so the hook
/// configuration Cairn writes is pointed at the package's `cairn` binary first.
/// `base_url` redirects a native release to a local model server: Cairn strips
/// ambient `GOOGLE_GEMINI_*` variables from the native environment.
fn launcher(root: &Path, binary: &str, base_url: Option<&str>) -> String {
    let test = std::env::current_exe().unwrap();
    let cairn = env!("CARGO_BIN_EXE_cairn");
    let base = base_url.map_or_else(String::new, |url| {
        format!("export GOOGLE_GEMINI_BASE_URL='{url}'\n")
    });
    let script = format!(
        "#!/bin/sh\nhooks=\"$HOME/.gemini/config/hooks.json\"\n\
         if [ -f \"$hooks\" ]; then sed -i 's|{test}|{cairn}|g' \"$hooks\"; fi\n\
         {base}exec '{binary}' \"$@\"\n",
        test = test.display(),
    );
    let path = root.join("agy-launcher.sh");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

async fn service(root: &TempDir) -> Arc<Service> {
    service_with(root, &common::fixture("antigravity.mjs")).await
}

async fn service_with(root: &TempDir, binary: &str) -> Arc<Service> {
    Service::new(Config {
        antigravity_bin: launcher(root.path(), binary, None),
        ..common::config(root.path())
    })
    .await
    .unwrap()
}

async fn connect(s: &Service, kind: &str, secret: &str) -> Value {
    serde_json::to_value(
        s.accounts
            .connect_api_key(s, Provider::Antigravity, "Antigravity", kind, secret, None)
            .await
            .unwrap(),
    )
    .unwrap()
}

fn service_account() -> Value {
    json!({
        "type": "service_account",
        "project_id": "synthetic-project",
        "private_key": "-----BEGIN PRIVATE KEY-----\nsynthetic-private-key\n-----END PRIVATE KEY-----\n",
        "client_email": "agent@synthetic-project.iam.gserviceaccount.com",
    })
}

fn google_cloud_secret() -> String {
    json!({ "credentials": service_account(), "location": "us-central1" }).to_string()
}

fn plan(root: &TempDir, slug: &str, body: &str) -> Value {
    json!({
        "sandbox": "yolo",
        "model": MODEL,
        "antigravityModel": slug,
        "cwd": root.path(),
        "inputDirectory": root.path().join("inbox"),
        "output": root.path().join("result.md"),
        "instructions": "Be concise.",
        "reasoning": "high",
        "execution": { "messageId": "initial", "text": body, "attachments": [] },
    })
}

/// The lease's native home, where the fixture keeps its log and conversations.
fn native_home(s: &Service) -> PathBuf {
    Provider::Antigravity.driver().home(s, RUN)
}

fn records(home: &Path, kind: &str) -> Vec<Value> {
    std::fs::read_to_string(home.join("agy-fixture.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record["type"] == kind)
        .collect()
}

fn write_inbox(root: &TempDir, messages: &Value) {
    let inbox = root.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let temporary = inbox.join(".messages.json.tmp");
    std::fs::write(&temporary, messages.to_string()).unwrap();
    std::fs::rename(temporary, inbox.join("messages.json")).unwrap();
}

/// Runs one Cairn turn through the adapter with a leased account and its broker.
/// `react` sees each event as it arrives, to steer the turn or ask a side question.
async fn run_with(
    s: &Service,
    plan: Value,
    mut react: impl FnMut(&Value) + Send,
) -> (cairn_installation::error::Result<()>, Vec<Value>) {
    let lease = s
        .accounts
        .acquire(s, RUN, Provider::Antigravity, text(&plan, "model"))
        .await
        .unwrap()
        .expect("an Antigravity account is leased");
    let broker = broker::serve(s, &lease).await.unwrap();
    let mut config = s.config.clone();
    config.home = lease.home.parent().unwrap().to_owned();
    let (tx, mut rx) = mpsc::channel(256);
    let task =
        tokio::spawn(
            async move { antigravity::run(&config, plan, tx, CancellationToken::new()).await },
        );
    let mut events = Vec::new();
    let collected = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = rx.recv().await {
            react(&event);
            events.push(event);
        }
    })
    .await;
    collected.expect("native turn must finish or fail without hanging");
    let result = task.await.unwrap();
    // The launch removed its credential files itself: on an execution node the lease's
    // manager-side cleanup never reaches that disk.
    for file in [
        "cairn-adc.json",
        ".gemini/antigravity-cli/antigravity-oauth-token",
    ] {
        assert!(
            !lease.home.join(file).exists(),
            "{file} outlived the launch"
        );
    }
    drop(broker);
    s.accounts.release(s, &lease).await.unwrap();
    (result, events)
}

async fn run(s: &Service, plan: Value) -> (cairn_installation::error::Result<()>, Vec<Value>) {
    run_with(s, plan, |_| {}).await
}

fn thread(events: &[Value]) -> String {
    text(
        events
            .iter()
            .find(|e| e["type"] == "thread.started")
            .expect("the native conversation started"),
        "thread_id",
    )
    .to_owned()
}

fn delivered(events: &[Value], id: &str) -> bool {
    events
        .iter()
        .any(|e| e["type"] == "chat.delivered" && e["messageId"] == id)
}

fn completed<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|e| e["type"] == "item.completed" && e["item"]["type"] == kind)
        .map(|e| &e["item"])
        .collect()
}

fn efforts(model: &Value) -> Vec<&str> {
    model["supportedReasoningEfforts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| text(e, "reasoningEffort"))
        .collect()
}

#[tokio::test]
async fn gemini_keys_stay_private_and_catalogs_group_levels_across_account_kinds() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let providers = Provider::Antigravity
        .driver()
        .api_key_providers(&s)
        .await
        .unwrap();
    assert_eq!(
        providers,
        json!([{ "id": "gemini", "name": "Gemini API" }, { "id": "google-cloud", "name": "Google Cloud" }])
    );
    let empty = s.antigravity.models(&s).await.unwrap();
    assert_eq!(empty["models"], json!([]));
    assert!(text(&empty, "error").contains("Connect an Antigravity account"));

    let account = connect(&s, "gemini", KEY).await;
    assert_eq!(account["modelProvider"], "gemini");
    assert_eq!(account["modelProviderName"], "Gemini API");
    assert_eq!(account["state"], "ready");
    assert!(!account.to_string().contains(KEY));
    assert!(
        s.accounts
            .connect_api_key(&s, Provider::Antigravity, "Again", "gemini", KEY, None)
            .await
            .unwrap_err()
            .message
            .contains("already connected")
    );
    assert!(
        s.accounts
            .connect_api_key(
                &s,
                Provider::Antigravity,
                "Bad",
                "gemini",
                "two words",
                None
            )
            .await
            .is_err()
    );

    let catalog = s.antigravity.models(&s).await.unwrap();
    assert_eq!(catalog["error"], "", "{catalog}");
    let models = catalog["models"].as_array().unwrap();
    let ids = models.iter().map(|m| text(m, "model")).collect::<Vec<_>>();
    assert_eq!(
        ids,
        [
            "gemini-4-neon",
            "gemini-3.5-flash",
            "gemini-3-flash-preview"
        ]
    );
    assert_eq!(models[0]["displayName"], "Gemini 4 Neon");
    assert_eq!(models[0]["description"], "Via Gemini API");
    assert_eq!(models[0]["defaultReasoningEffort"], "high");
    assert_eq!(models[0]["isDefault"], true);
    assert_eq!(efforts(&models[0]), ["low", "high", "xhigh"]);
    assert_eq!(models[0]["routes"], json!([]));
    assert!(models[0].get("slugs").is_none());
    assert_eq!(efforts(&models[2]), Vec::<&str>::new());
    assert_eq!(
        s.antigravity
            .resolve(&s, "gemini", MODEL, "xhigh")
            .await
            .unwrap()
            .as_deref(),
        Some("gemini-4-neon-xhigh")
    );
    assert_eq!(
        s.antigravity
            .resolve(&s, "gemini", "gemini-3.5-flash", "")
            .await
            .unwrap()
            .as_deref(),
        Some("gemini-3.5-flash")
    );
    // Each listing ran in a private directory that no longer holds credentials.
    let catalogs = s.config.data_dir.join("antigravity-catalog");
    assert!(
        std::fs::read_dir(&catalogs).map_or(true, |mut entries| entries.next().is_none()),
        "catalog listings must not leave credential copies"
    );

    // The broker hands a run the key; the run never reads the vault.
    let lease = s
        .accounts
        .acquire(&s, RUN, Provider::Antigravity, MODEL)
        .await
        .unwrap()
        .unwrap();
    let served = broker::serve(&s, &lease).await.unwrap();
    let credential = broker::request(
        &lease.home.join(broker::SOCKET),
        &broker::Request::default(),
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    assert_eq!(credential, json!({ "kind": "gemini", "key": KEY }));
    drop(served);
    s.accounts.release(&s, &lease).await.unwrap();

    // A Google Cloud account joins the catalog: shared models keep only shared levels.
    let cloud = connect(&s, "google-cloud", &google_cloud_secret()).await;
    assert_eq!(cloud["modelProviderName"], "Google Cloud");
    assert!(!cloud.to_string().contains("synthetic-private-key"));
    let catalog = s.antigravity.models(&s).await.unwrap();
    let models = catalog["models"].as_array().unwrap();
    let neon = models.iter().find(|m| m["model"] == MODEL).unwrap();
    assert_eq!(efforts(neon), ["low", "high"]);
    assert_eq!(neon["description"], "Via Gemini API, Google Cloud");
    let claude = models
        .iter()
        .find(|m| m["model"] == "claude-sonnet-4-6-thinking")
        .unwrap();
    assert_eq!(claude["description"], "Via Google Cloud");
    assert_eq!(claude["displayName"], "Claude Sonnet 4.6 (Thinking)");
    assert!(
        s.antigravity
            .resolve(&s, "gemini", "claude-sonnet-4-6-thinking", "")
            .await
            .unwrap()
            .is_none()
    );

    s.accounts.remove(&s, text(&account, "id")).await.unwrap();
    assert!(
        s.vault
            .get(&format!("antigravity-account:{}", text(&account, "id")))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_turn_streams_delivers_replays_its_receipt_and_resumes_the_conversation() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s, "gemini", KEY).await;
    let input = plan(&root, "gemini-4-neon-high", "Build the status panel");
    let (result, events) = run(&s, input.clone()).await;
    result.unwrap();
    let conversation = thread(&events);
    assert!(cairn_installation::validation::uuid(&conversation).is_ok());
    assert!(delivered(&events, "initial"));
    assert!(events.iter().any(|e| e["type"] == "chat.side.available"));
    let answer = std::fs::read_to_string(root.path().join("result.md")).unwrap();
    assert_eq!(
        answer,
        "I prepared a compact status panel. You asked: \"Build the status panel\". Earlier steps in this conversation: 0."
    );
    let messages = completed(&events, "agent_message");
    assert!(messages.iter().any(|m| m["text"] == answer.as_str()));
    // Streaming updates the item before it completes.
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "item.started" && e["item"]["type"] == "agent_message")
    );
    assert_eq!(
        events.last().unwrap()["type"],
        "turn.completed",
        "{events:?}"
    );
    assert!(!serde_json::to_string(&events).unwrap().contains(KEY));

    let home = native_home(&s);
    let launches = records(&home, "launch");
    assert_eq!(launches.len(), 1);
    let launch = &launches[0];
    let args = launch["args"].as_array().unwrap();
    for expected in [
        "--input-format",
        "stream-json",
        "--output-format",
        "--dangerously-skip-permissions",
        "--disable-slash-commands",
        "-p=",
    ] {
        assert!(args.contains(&json!(expected)), "{expected} in {args:?}");
    }
    assert_eq!(launch["model"], "gemini-4-neon-high");
    assert_eq!(launch["mode"], "default");
    assert_eq!(launch["conversation"], Value::Null);
    assert_eq!(launch["settings"]["modelProvider"], "gemini");
    assert_eq!(launch["settings"]["disableSlashCommands"], true);
    assert_eq!(launch["settings"]["permissions"]["deny"], json!([]));
    assert_eq!(launch["env"]["GEMINI_API_KEY"], KEY);
    assert_eq!(launch["env"]["HOME"], home.to_string_lossy().as_ref());
    assert_eq!(launch["env"]["AGY_CLI_DISABLE_AUTO_UPDATE"], "true");
    assert_eq!(
        launch["env"]["BASH_ENV"],
        home.join("cairn-shell-env.sh").to_string_lossy().as_ref()
    );
    assert!(launch["env"].get("AGY_ADC_AUTH").is_none());
    assert!(launch["env"].get("DBUS_SESSION_BUS_ADDRESS").is_none());
    assert_eq!(launch["geminiMd"], "Be concise.");
    assert_eq!(launch["mcpConfig"], json!({ "mcpServers": {} }));
    assert_eq!(launch["token"], Value::Null);
    assert_eq!(
        records(&home, "message")
            .iter()
            .map(|m| text(m, "content"))
            .collect::<Vec<_>>(),
        ["Build the status panel"]
    );
    // Shell commands get the shell's HOME back, without the run's credentials.
    let shell = std::fs::read_to_string(home.join("cairn-shell-env.sh")).unwrap();
    assert!(shell.contains(&format!(
        "export HOME='{}'",
        home.parent().unwrap().display()
    )));
    assert!(shell.contains("unset GEMINI_API_KEY"));
    let receipt =
        std::fs::read_to_string(root.path().join("result.antigravity-receipt.json")).unwrap();
    assert!(!receipt.contains(KEY));

    // A completed receipt replays without starting the CLI again.
    let (tx, mut rx) = mpsc::channel(128);
    let config = Config {
        antigravity_bin: "/nonexistent-agy".into(),
        ..s.config.clone()
    };
    antigravity::run(&config, input.clone(), tx, CancellationToken::new())
        .await
        .unwrap();
    let mut replayed = Vec::new();
    while let Some(event) = rx.recv().await {
        replayed.push(event);
    }
    assert!(delivered(&replayed, "initial"));
    assert!(
        completed(&replayed, "agent_message")
            .iter()
            .any(|m| m["text"] == answer.as_str())
    );
    assert_eq!(replayed.last().unwrap()["type"], "turn.completed");
    assert_eq!(records(&home, "launch").len(), 1);

    // The next message resumes the same native conversation with its history.
    let mut follow_up = input;
    follow_up["sessionId"] = conversation.clone().into();
    follow_up["execution"]["messageId"] = "follow-up".into();
    follow_up["execution"]["text"] = "Now add the project status.".into();
    follow_up["output"] = root
        .path()
        .join("follow-up.md")
        .to_string_lossy()
        .into_owned()
        .into();
    let (result, events) = run(&s, follow_up).await;
    result.unwrap();
    assert_eq!(thread(&events), conversation);
    assert!(delivered(&events, "follow-up"));
    assert!(!delivered(&events, "initial"));
    let launches = records(&home, "launch");
    assert_eq!(launches.len(), 2);
    assert_eq!(launches[1]["conversation"], conversation.as_str());
    let args = launches[1]["args"].as_array().unwrap();
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--conversation" && w[1] == conversation.as_str())
    );
    assert_eq!(args.last().unwrap(), "-p=");
    assert_eq!(
        std::fs::read_to_string(root.path().join("follow-up.md")).unwrap(),
        "I prepared a compact status panel. You asked: \"Now add the project status.\". Earlier steps in this conversation: 2."
    );
}

#[tokio::test]
async fn steering_messages_join_the_running_process_in_order() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s, "gemini", KEY).await;
    let input = plan(
        &root,
        "gemini-4-neon-high",
        "fixture:agy-slow=1500 Build the status panel",
    );
    let mut steered = false;
    let (result, events) = run_with(&s, input, |event| {
        if event["type"] == "chat.delivered" && event["messageId"] == "initial" && !steered {
            steered = true;
            write_inbox(
                &root,
                &json!([{ "id": "steer", "text": "Also add the project status." }]),
            );
        }
    })
    .await;
    result.unwrap();
    assert!(delivered(&events, "initial") && delivered(&events, "steer"));
    let home = native_home(&s);
    assert_eq!(records(&home, "launch").len(), 1, "one native process");
    assert_eq!(
        records(&home, "message")
            .iter()
            .map(|m| text(m, "content"))
            .collect::<Vec<_>>(),
        [
            "fixture:agy-slow=1500 Build the status panel",
            "Also add the project status."
        ]
    );
    // The reply is the answer to the last message, after both native turns.
    assert_eq!(
        std::fs::read_to_string(root.path().join("result.md")).unwrap(),
        "I prepared a compact status panel. You asked: \"Also add the project status.\". Earlier steps in this conversation: 2."
    );
    let receipt: Value = serde_json::from_slice(
        &std::fs::read(root.path().join("result.antigravity-receipt.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["submissions"], json!(["initial", "steer"]));
    assert_eq!(receipt["complete"], true);
}

#[tokio::test]
async fn ordinary_turns_run_native_tools_through_cairns_hook() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s, "gemini", KEY).await;
    let (result, events) = run(
        &s,
        plan(
            &root,
            "gemini-4-neon-high",
            "fixture:agy-tools Mark the workspace",
        ),
    )
    .await;
    result.unwrap();
    let home = native_home(&s);
    let hooks = records(&home, "hook");
    assert_eq!(
        hooks
            .iter()
            .map(|h| (text(h, "tool"), text(h, "decision")))
            .collect::<Vec<_>>(),
        [
            ("list_dir", "allow"),
            ("run_command", "allow"),
            ("write_file", "allow")
        ],
        "{hooks:?}"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("agy-command.txt")).unwrap(),
        "fixture-command\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("agy-written.txt")).unwrap(),
        "fixture file"
    );
    let commands = completed(&events, "command_execution");
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["status"], "completed");
    assert_eq!(commands[0]["aggregated_output"], "ran\n");
    let tools = completed(&events, "mcp_tool_call");
    assert!(tools.iter().any(|t| t["server"] == "Antigravity"
        && t["tool"] == "write_file"
        && t["status"] == "completed"));
}

#[tokio::test]
async fn plan_turns_refuse_shell_and_writes_in_the_hook_and_native_rules() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s, "gemini", KEY).await;
    let mut input = plan(
        &root,
        "gemini-4-neon-high",
        "fixture:agy-tools Plan the workspace changes",
    );
    input["planning"] = true.into();
    input["sandbox"] = "read-only".into();
    // Native settings from the agent's configuration only add restrictions.
    input["antigravitySettings"] = json!({
        "enableTerminalSandbox": true,
        "permissions": { "deny": ["command(rm)"] },
    });
    let (result, events) = run(&s, input).await;
    result.unwrap();
    let home = native_home(&s);
    let hooks = records(&home, "hook");
    let decisions = hooks
        .iter()
        .map(|h| (text(h, "tool"), text(h, "decision")))
        .collect::<Vec<_>>();
    assert_eq!(
        decisions,
        [
            ("list_dir", "allow"),
            ("run_command", "deny"),
            ("write_file", "deny")
        ],
        "{hooks:?}"
    );
    assert!(text(&hooks[1], "reason").starts_with(PLAN_REFUSAL));
    assert!(!root.path().join("agy-command.txt").exists());
    assert!(!root.path().join("agy-written.txt").exists());
    let commands = completed(&events, "command_execution");
    assert_eq!(commands[0]["status"], "failed");
    assert!(text(commands[0], "aggregated_output").contains(PLAN_REFUSAL));

    let launch = &records(&home, "launch")[0];
    let args = launch["args"].as_array().unwrap();
    assert!(args.windows(2).any(|w| w[0] == "--mode" && w[1] == "plan"));
    let deny = launch["settings"]["permissions"]["deny"]
        .as_array()
        .unwrap();
    for rule in [
        "command(*)",
        "write_file(*)",
        "read_url(*)",
        "execute_url(*)",
        "download_url(*)",
        "command(rm)",
    ] {
        assert!(deny.contains(&json!(rule)), "{rule} in {deny:?}");
    }
    assert_eq!(launch["settings"]["enableTerminalSandbox"], true);
    let message = text(&records(&home, "message")[0], "content").to_owned();
    assert!(message.starts_with("Shell commands, file changes and subagents are unavailable"));
    // The plan turn ran in a fresh native HOME, removed with its credentials afterwards,
    // while the run's own native state kept its ordinary hook policy untouched.
    assert!(
        launch["env"]["HOME"]
            .as_str()
            .unwrap()
            .ends_with("/plan-turn")
    );
    assert!(!home.join("plan-turn").exists());
    assert!(!home.join("cairn-hook-policy.json").exists());
}

#[tokio::test]
async fn plan_turns_refuse_repositories_that_define_antigravity_commands() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s, "gemini", KEY).await;
    std::fs::create_dir_all(root.path().join(".agents")).unwrap();
    std::fs::write(root.path().join(".agents/hooks.json"), "{}").unwrap();
    let mut input = plan(&root, "gemini-4-neon-high", "Plan the workspace changes");
    input["planning"] = true.into();
    input["sandbox"] = "read-only".into();
    let (result, _) = run(&s, input).await;
    let error = result.unwrap_err();
    assert_eq!(error.status, 409);
    assert!(
        error.message.contains(".agents/hooks.json"),
        "{}",
        error.message
    );
    // agy never started: its repository hooks could not run.
    assert!(records(&native_home(&s), "launch").is_empty());
}

#[tokio::test]
async fn side_questions_refuse_repositories_that_define_antigravity_commands() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s, "gemini", KEY).await;
    std::fs::create_dir_all(root.path().join(".agents")).unwrap();
    std::fs::write(root.path().join(".agents/mcp_config.json"), "{}").unwrap();
    let input = plan(
        &root,
        "gemini-4-neon-high",
        "fixture:agy-slow=3000 Build the status panel",
    );
    let mut asked = false;
    let (result, events) = run_with(&s, input, |event| {
        if event["type"] == "chat.delivered" && !asked {
            asked = true;
            write_inbox(
                &root,
                &json!([{ "id": "side-message", "sideQuestion": { "id": "side-1", "text": "Why?" } }]),
            );
        }
    })
    .await;
    // The ordinary turn runs: YOLO turns may use repository configuration.
    result.unwrap();
    let failed = events
        .iter()
        .find(|e| e["type"] == "chat.side.failed")
        .unwrap_or_else(|| panic!("no side failure: {events:?}"));
    assert!(
        text(failed, "error").contains(".agents/mcp_config.json"),
        "{failed}"
    );
    assert!(events.iter().all(|e| e["type"] != "chat.side.answered"));
}

#[tokio::test]
async fn runs_wait_with_an_explanation_when_no_account_offers_the_model() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s, "gemini", KEY).await;
    s.antigravity.models(&s).await.unwrap();
    let error = s
        .accounts
        .acquire(&s, RUN, Provider::Antigravity, "gemini-unknown")
        .await
        .unwrap_err();
    assert!(
        error
            .message
            .contains("No enabled Antigravity account supports model gemini-unknown"),
        "{}",
        error.message
    );
}

/// A copy of `input` that sends `text` as message `id`, writing its own output.
fn next_message(root: &TempDir, input: &Value, id: &str, text: &str) -> Value {
    let mut next = input.clone();
    next["execution"]["messageId"] = id.into();
    next["execution"]["text"] = text.into();
    next["output"] = root
        .path()
        .join(format!("{id}.md"))
        .to_string_lossy()
        .into_owned()
        .into();
    next
}

#[tokio::test]
async fn plan_turns_resume_in_a_fresh_home_and_hand_their_steps_back() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s, "gemini", KEY).await;
    let first = plan(&root, "gemini-4-neon-high", "Build the status panel");
    let (result, events) = run(&s, first.clone()).await;
    result.unwrap();
    let conversation = thread(&events);

    let mut planning = next_message(&root, &first, "plan", "Plan the next step");
    planning["sessionId"] = conversation.clone().into();
    planning["planning"] = true.into();
    planning["sandbox"] = "read-only".into();
    let (result, events) = run(&s, planning).await;
    result.unwrap();
    assert_eq!(thread(&events), conversation);
    // The fresh home resumed the conversation: it saw the first turn's two steps.
    assert!(
        std::fs::read_to_string(root.path().join("plan.md"))
            .unwrap()
            .ends_with("Earlier steps in this conversation: 2.")
    );
    assert!(!native_home(&s).join("plan-turn").exists());

    // The plan turn's steps are back in the run's native state.
    let mut follow_up = next_message(&root, &first, "after-plan", "Implement it");
    follow_up["sessionId"] = conversation.clone().into();
    let (result, _) = run(&s, follow_up).await;
    result.unwrap();
    assert!(
        std::fs::read_to_string(root.path().join("after-plan.md"))
            .unwrap()
            .ends_with("Earlier steps in this conversation: 4.")
    );
}

#[tokio::test]
async fn a_stopped_first_plan_turn_hands_its_conversation_back() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s, "gemini", KEY).await;
    let mut planning = plan(
        &root,
        "gemini-4-neon-high",
        "fixture:agy-slow=60000 Plan the status panel",
    );
    planning["planning"] = true.into();
    planning["sandbox"] = "read-only".into();
    let lease = s
        .accounts
        .acquire(&s, RUN, Provider::Antigravity, MODEL)
        .await
        .unwrap()
        .expect("an Antigravity account is leased");
    let broker = broker::serve(&s, &lease).await.unwrap();
    let mut config = s.config.clone();
    config.home = lease.home.parent().unwrap().to_owned();
    let (tx, mut rx) = mpsc::channel(256);
    let stop = CancellationToken::new();
    let cancel = stop.clone();
    let input = planning.clone();
    let task = tokio::spawn(async move { antigravity::run(&config, input, tx, cancel).await });
    // Stop once the native conversation exists, as a user pressing Stop would.
    let conversation = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(event) = rx.recv().await {
            if event["type"] == "chat.delivered" {
                break;
            }
        }
        let conversations = lease
            .home
            .join("plan-turn/.gemini/antigravity-cli/conversations");
        loop {
            if let Some(name) = std::fs::read_dir(&conversations)
                .ok()
                .and_then(|mut entries| entries.next())
                .and_then(Result::ok)
            {
                return name.file_name().to_string_lossy().replace(".db", "");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the plan turn starts its conversation");
    stop.cancel();
    assert!(task.await.unwrap().is_err());
    drop(broker);
    s.accounts.release(&s, &lease).await.unwrap();
    assert!(
        lease
            .home
            .join(format!(
                ".gemini/antigravity-cli/conversations/{conversation}.db"
            ))
            .exists(),
        "the stopped plan turn lost its conversation"
    );
    assert!(!lease.home.join("plan-turn").exists());

    // The next ordinary turn resumes it.
    let mut follow_up = next_message(&root, &planning, "after-stop", "Build it");
    follow_up["planning"] = Value::Null;
    follow_up["sandbox"] = "yolo".into();
    follow_up["sessionId"] = conversation.clone().into();
    let (result, events) = run(&s, follow_up).await;
    result.unwrap();
    assert_eq!(thread(&events), conversation);
}

/// A plan turn after a first ordinary turn, in the run of `root`.
fn plan_after(root: &TempDir, first: &Value, conversation: &str) -> Value {
    let mut planning = next_message(root, first, "plan", "Plan the next step");
    planning["sessionId"] = conversation.into();
    planning["planning"] = true.into();
    planning["sandbox"] = "read-only".into();
    planning
}

#[tokio::test]
async fn plan_turns_refuse_conversations_redirected_by_a_link() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s, "gemini", KEY).await;
    let first = plan(&root, "gemini-4-neon-high", "Build the status panel");
    let (result, events) = run(&s, first.clone()).await;
    result.unwrap();
    let conversation = thread(&events);
    // An earlier YOLO turn planted the conversation's journal as a link to a working
    // file, which SQLite would write through.
    let journal = native_home(&s).join(format!(
        ".gemini/antigravity-cli/conversations/{conversation}.db-wal"
    ));
    let target = root.path().join("main.rs");
    std::fs::write(&target, "fn main() {}\n").unwrap();
    std::os::unix::fs::symlink(&target, &journal).unwrap();
    let launches = records(&native_home(&s), "launch").len();
    let (result, _) = run(&s, plan_after(&root, &first, &conversation)).await;
    let error = result.unwrap_err();
    assert_eq!(error.status, 409, "{}", error.message);
    assert!(
        error.message.contains("replaced by a link"),
        "{}",
        error.message
    );
    assert_eq!(
        records(&native_home(&s), "launch").len(),
        launches,
        "agy never started"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn main() {}\n");
}

#[tokio::test]
async fn native_configuration_adds_rules_to_ordinary_turns() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    common::save_native_configuration(
        &s,
        "antigravity",
        None,
        &json!({
            "enableTerminalSandbox": true,
            "permissions": { "deny": ["command(rm)"] },
        })
        .to_string(),
    )
    .await;
    connect(&s, "gemini", KEY).await;
    let mut input = plan(&root, "gemini-4-neon-high", "Build the status panel");
    input["antigravitySettings"] = json!({
        "enableTerminalSandbox": true,
        "permissions": { "deny": ["command(rm)"] },
    });
    let (result, _) = run(&s, input.clone()).await;
    result.unwrap();
    let launch = &records(&native_home(&s), "launch")[0];
    assert_eq!(
        launch["settings"]["permissions"],
        json!({ "deny": ["command(rm)"] })
    );
    assert_eq!(launch["settings"]["enableTerminalSandbox"], true);

    input["execution"]["messageId"] = "invalid".into();
    input["antigravitySettings"] = json!({ "permissions": { "deny": [1] } });
    let (result, _) = run(&s, input).await;
    assert!(
        result
            .unwrap_err()
            .message
            .contains("Invalid Antigravity native permissions")
    );
}

#[tokio::test]
async fn side_questions_answer_from_a_copy_without_touching_the_conversation() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s, "gemini", KEY).await;
    let input = plan(
        &root,
        "gemini-4-neon-high",
        "fixture:agy-slow=6000 Build the status panel",
    );
    let mut asked = false;
    let (result, events) = run_with(&s, input, |event| {
        if event["type"] == "chat.delivered" && !asked {
            asked = true;
            write_inbox(
                &root,
                &json!([{ "id": "side-message", "sideQuestion": { "id": "side-1", "text": "What did I ask for?" } }]),
            );
        }
    })
    .await;
    result.unwrap();
    let answered = events
        .iter()
        .find(|e| e["type"] == "chat.side.answered")
        .unwrap_or_else(|| panic!("no side answer: {events:?}"));
    assert_eq!(answered["sideQuestionId"], "side-1");
    // The copy held the running turn's user step, and the question was its last line.
    assert_eq!(
        answered["text"],
        "I prepared a compact status panel. You asked: \"What did I ask for?\". Earlier steps in this conversation: 1."
    );
    assert!(!delivered(&events, "side-message"));
    let home = native_home(&s);
    // The main process received only the user's message.
    assert_eq!(records(&home, "message").len(), 1);
    assert_eq!(records(&home, "launch").len(), 1);
    // The saved conversation has no trace of the side question.
    let database = home.join(format!(
        ".gemini/antigravity-cli/conversations/{}.db",
        thread(&events)
    ));
    let connection = rusqlite::Connection::open(database).unwrap();
    let steps = connection
        .prepare("SELECT role, content FROM steps ORDER BY step_index")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert!(
        steps
            .iter()
            .all(|(_, content)| !content.contains("What did I ask for?"))
    );
    assert!(!home.join("side-questions/side-1").exists());
}

#[tokio::test]
async fn cancelling_a_turn_stops_the_commands_agy_started() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s, "gemini", KEY).await;
    let input = plan(
        &root,
        "gemini-4-neon-high",
        "fixture:agy-command fixture:agy-slow=60000 Run the build",
    );
    let lease = s
        .accounts
        .acquire(&s, RUN, Provider::Antigravity, MODEL)
        .await
        .unwrap()
        .expect("an Antigravity account is leased");
    let broker = broker::serve(&s, &lease).await.unwrap();
    let mut config = s.config.clone();
    config.home = lease.home.parent().unwrap().to_owned();
    let (tx, mut rx) = mpsc::channel(256);
    let stop = CancellationToken::new();
    let cancel = stop.clone();
    let task = tokio::spawn(async move { antigravity::run(&config, input, tx, cancel).await });
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let pid_file = native_home(&s).join("agy-command.pid");
    let pid: i32 = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = pid.trim().parse()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the command starts");
    // A killed command may stay a zombie until its new parent reaps it.
    let alive = |pid: i32| {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z'))
        })
    };
    assert!(alive(pid));

    stop.cancel();
    assert!(task.await.unwrap().is_err());
    let stopped = tokio::time::timeout(Duration::from_secs(10), async {
        while alive(pid) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(stopped.is_ok(), "the command outlived the cancelled turn");
    drain.abort();
    drop(broker);
    s.accounts.release(&s, &lease).await.unwrap();
}

#[tokio::test]
async fn quota_failures_report_exhaustion() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s, "gemini", KEY).await;
    let (result, events) = run(
        &s,
        plan(
            &root,
            "gemini-4-neon-high",
            "fixture:agy-429 Build the status panel",
        ),
    )
    .await;
    let error = result.unwrap_err();
    assert_eq!(
        error.message,
        "Usage limit reached for Antigravity. Check this account's quota or connect another account."
    );
    assert!(delivered(&events, "initial"));
    assert!(!events.iter().any(|e| e["type"] == "turn.completed"));
}

#[tokio::test]
async fn unsupported_releases_are_refused() {
    let root = TempDir::new().unwrap();
    let wrapper = root.path().join("old-agy.sh");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nCAIRN_AGY_FIXTURE_VERSION=1.5.13 exec '{}' \"$@\"\n",
            common::fixture("antigravity.mjs")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let s = service_with(&root, &wrapper.to_string_lossy()).await;
    connect(&s, "gemini", KEY).await;
    let error = s.antigravity.models(&s).await.unwrap_err();
    assert!(
        error.message.contains("Install 1.5.14 or later"),
        "{}",
        error.message
    );
}

async fn sign_in_until(s: &Service, condition: impl Fn(&Value) -> bool) -> Value {
    common::eventually(
        Duration::from_secs(10),
        Duration::from_millis(30),
        async || {
            let view = serde_json::to_value(s.accounts.sign_in().await).unwrap();
            condition(&view).then_some(view)
        },
    )
    .await
}

/// Signs in a Google account through `agy acp --login`.
async fn sign_in_google(s: &Arc<Service>) -> String {
    let started = serde_json::to_value(
        s.accounts
            .add(s, Provider::Antigravity, "Google")
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(started["state"], "pending");
    let view = sign_in_until(s, |v| v["acceptsCode"] == true).await;
    assert!(
        text(&view, "url").starts_with("https://accounts.google.com/o/oauth2/auth?"),
        "{view}"
    );
    s.accounts
        .submit_code("4/fixture-authorization-code")
        .await
        .unwrap();
    let finished = sign_in_until(s, |v| v["state"] != "pending").await;
    assert_eq!(finished["state"], "complete", "{finished}");
    let account = s
        .accounts
        .of(s, Provider::Antigravity)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    account.id.to_string()
}

#[tokio::test]
async fn reconnecting_google_refreshes_a_partial_catalog_immediately() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let id = sign_in_google(&s).await;
    connect(&s, "gemini", KEY).await;
    let secret = format!("antigravity-account:{id}");
    let mut expired = s.vault.get(&secret).await.unwrap().unwrap();
    expired["token"]["token"]["expiry"] = "2000-01-01T00:00:00Z".into();
    expired["token"]["token"]["refresh_token"] = "".into();
    s.vault.set(&secret, &expired).await.unwrap();

    let lists_google = |catalog: &Value| {
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| text(model, "description").contains("Google account"))
    };
    let partial = s.antigravity.models(&s).await.unwrap();
    assert!(text(&partial, "error").contains("Reconnect this Antigravity Google account"));
    assert!(!s.accounts.account(&s, &id).await.unwrap().is_active());
    assert!(!lists_google(&partial), "{partial}");

    s.accounts.reconnect(&s, &id).await.unwrap();
    sign_in_until(&s, |view| view["acceptsCode"] == true).await;
    s.accounts
        .submit_code("4/fixture-authorization-code")
        .await
        .unwrap();
    let finished = sign_in_until(&s, |view| view["state"] != "pending").await;
    assert_eq!(finished["state"], "complete", "{finished}");

    let catalog = s.antigravity.models(&s).await.unwrap();
    assert_eq!(catalog["error"], "", "{catalog}");
    assert!(lists_google(&catalog), "{catalog}");
}

#[tokio::test]
async fn google_accounts_sign_in_and_runs_receive_only_access_tokens() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let id = sign_in_google(&s).await;
    let account = s.accounts.account(&s, &id).await.unwrap();
    assert_eq!(account.email.as_deref(), Some("fixture.user@gmail.com"));
    assert_eq!(account.model_provider.as_deref(), Some("google"));
    let view = serde_json::to_value(
        s.accounts
            .list(&s)
            .await
            .unwrap()
            .into_iter()
            .find(|a| a.id == id)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(view["modelProviderName"], "Google account");
    assert_eq!(view["state"], "ready");
    let stored = s
        .vault
        .get(&format!("antigravity-account:{id}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored["kind"], "google");
    assert_eq!(
        stored["token"]["token"]["refresh_token"],
        "1//fixture-refresh-token"
    );
    // The sign-in home keeps no token once the vault holds it.
    assert!(
        !s.config
            .data_dir
            .join("account-login")
            .join(&id)
            .join(".gemini/antigravity-cli/antigravity-oauth-token")
            .exists()
    );

    let catalog = s.antigravity.models(&s).await.unwrap();
    assert_eq!(catalog["models"][0]["model"], MODEL);
    assert_eq!(catalog["models"][0]["description"], "Via Google account");
    let slug = s
        .antigravity
        .resolve(&s, "google", MODEL, "xhigh")
        .await
        .unwrap()
        .unwrap();
    let (result, _) = run(&s, plan(&root, &slug, "Build the status panel")).await;
    result.unwrap();
    let launch = &records(&native_home(&s), "launch")[0];
    let token = &launch["token"];
    assert_eq!(token["mode"], 0o600);
    assert_eq!(
        token["content"]["token"]["access_token"],
        stored["token"]["token"]["access_token"]
    );
    assert!(
        token["content"]["token"].get("refresh_token").is_none(),
        "{token}"
    );
    assert!(launch["env"].get("GEMINI_API_KEY").is_none());
    assert!(launch["settings"].get("modelProvider").is_none());
    // Releasing the lease removes the token from the run's state.
    assert!(
        !native_home(&s)
            .join(".gemini/antigravity-cli/antigravity-oauth-token")
            .exists()
    );
}

#[tokio::test]
async fn expired_google_tokens_are_renewed_by_the_manager_and_the_turn_relaunched() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let id = sign_in_google(&s).await;
    let secret = format!("antigravity-account:{id}");
    let mut stored = s.vault.get(&secret).await.unwrap().unwrap();
    let original = text(&stored["token"]["token"], "access_token").to_owned();
    stored["token"]["token"]["expiry"] = "2000-01-01T00:00:00Z".into();
    s.vault.set(&secret, &stored).await.unwrap();

    let (result, events) = run(
        &s,
        plan(
            &root,
            "gemini-4-neon-high",
            "fixture:agy-expired Build the status panel",
        ),
    )
    .await;
    result.unwrap();
    let renewed = s.vault.get(&secret).await.unwrap().unwrap();
    let access = text(&renewed["token"]["token"], "access_token").to_owned();
    assert_ne!(access, original);
    assert!(access.starts_with("ya29.fixture-renewed-"), "{renewed}");
    assert_eq!(
        renewed["token"]["token"]["refresh_token"],
        "1//fixture-refresh-token"
    );

    let home = native_home(&s);
    let launches = records(&home, "launch");
    assert_eq!(launches.len(), 2, "the turn relaunched once: {launches:?}");
    let conversation = thread(&events);
    assert_eq!(launches[0]["conversation"], Value::Null);
    assert_eq!(launches[1]["conversation"], conversation.as_str());
    for launch in &launches {
        assert_eq!(
            launch["token"]["content"]["token"]["access_token"],
            access.as_str()
        );
        assert!(
            launch["token"]["content"]["token"]
                .get("refresh_token")
                .is_none()
        );
    }
    let messages = records(&home, "message");
    assert_eq!(messages.len(), 2);
    assert!(text(&messages[1], "content").starts_with("Your access was renewed"));
    assert!(delivered(&events, "initial"));
    assert_eq!(events.last().unwrap()["type"], "turn.completed");
    assert!(
        std::fs::read_to_string(root.path().join("result.md"))
            .unwrap()
            .contains("Earlier steps in this conversation: 1.")
    );
}

#[tokio::test]
async fn google_sign_in_refuses_a_rejected_code() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    s.accounts
        .add(&s, Provider::Antigravity, "Google")
        .await
        .unwrap();
    sign_in_until(&s, |v| v["acceptsCode"] == true).await;
    s.accounts.submit_code("invalid").await.unwrap();
    let finished = sign_in_until(&s, |v| v["state"] != "pending").await;
    assert_eq!(finished["state"], "failed", "{finished}");
    assert!(
        text(&finished, "error").contains("did not complete"),
        "{finished}"
    );
}

#[tokio::test]
async fn google_cloud_runs_receive_a_private_credential_file_and_project() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let account = connect(&s, "google-cloud", &google_cloud_secret()).await;
    assert_eq!(account["modelProvider"], "google-cloud");
    let slug = s
        .antigravity
        .resolve(&s, "google-cloud", MODEL, "low")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(slug, "gemini-4-neon-low");
    let (result, _) = run(&s, plan(&root, &slug, "Build the status panel")).await;
    result.unwrap();
    let home = native_home(&s);
    let launch = &records(&home, "launch")[0];
    assert_eq!(launch["env"]["AGY_ADC_AUTH"], "true");
    assert_eq!(
        launch["env"]["GOOGLE_APPLICATION_CREDENTIALS"],
        home.join("cairn-adc.json").to_string_lossy().as_ref()
    );
    assert_eq!(launch["env"]["GOOGLE_CLOUD_PROJECT"], "synthetic-project");
    assert_eq!(
        launch["env"]["GOOGLE_CLOUD_QUOTA_PROJECT"],
        "synthetic-project"
    );
    assert_eq!(launch["env"]["GOOGLE_CLOUD_LOCATION"], "us-central1");
    assert!(launch["env"].get("GEMINI_API_KEY").is_none());
    assert_eq!(launch["adc"]["mode"], 0o600);
    assert_eq!(launch["adc"]["content"], service_account());
    assert!(launch["settings"].get("modelProvider").is_none());
    // Releasing the lease removes the credential file.
    assert!(!home.join("cairn-adc.json").exists());
}

/// A local Gemini API: `streamGenerateContent?alt=sse` replies, recorded in order.
/// A request whose user text names `PLAN-TOOLS` first receives a `run_command` call,
/// then a `write_file` call, then text.
async fn gemini_server(
    root: &Path,
) -> (
    String,
    Arc<std::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    use axum::{Router, http::HeaderMap, routing::post};
    let requests = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let recorded = requests.clone();
    let marker = root.join("native-marker.txt");
    let router = Router::new().route(
        "/v1beta/models/{*model}",
        post(move |headers: HeaderMap, body: axum::body::Bytes| {
            let recorded = recorded.clone();
            let marker = marker.clone();
            async move {
                assert_eq!(headers["x-goog-api-key"], KEY);
                let request: Value = serde_json::from_slice(&body).unwrap();
                let contents = request["contents"].to_string();
                recorded.lock().unwrap().push(request);
                let responses = contents.matches("functionResponse").count();
                let part = if contents.contains("ASK-NATIVE") && responses == 0 {
                    json!({ "functionCall": { "name": "ask_question", "args": {
                        "questions": [{ "question": "Which color?", "options": ["Red", "Blue"] }],
                    } } })
                } else if contents.contains("PLAN-TOOLS") && responses == 0 {
                    json!({ "functionCall": { "name": "run_command", "args": {
                        "CommandLine": format!("echo native > {}", marker.display()),
                        "WaitMsBeforeAsync": 3000, "toolSummary": "x",
                    } } })
                } else if contents.contains("PLAN-TOOLS") && responses == 1 {
                    json!({ "functionCall": { "name": "write_file", "args": {
                        "Path": marker.to_string_lossy(), "Content": "x", "Overwrite": true,
                    } } })
                } else {
                    json!({ "text": "Native Antigravity passed." })
                };
                let chunk = json!({ "candidates": [{ "content": { "role": "model", "parts": [part] } }] });
                let last = json!({
                    "candidates": [{ "content": { "role": "model", "parts": [{ "text": "" }] }, "finishReason": "STOP" }],
                    "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 5, "totalTokenCount": 15 },
                });
                (
                    [("content-type", "text/event-stream")],
                    format!("data: {chunk}\r\n\r\ndata: {last}\r\n\r\n"),
                )
            }
        }),
    );
    let (url, server) = common::serve_locally(router).await;
    (url, requests, server)
}

async fn native_service(root: &TempDir, base_url: Option<&str>) -> Arc<Service> {
    let binary = std::env::var("ANTIGRAVITY_NATIVE_BIN").expect("set ANTIGRAVITY_NATIVE_BIN");
    Service::new(Config {
        antigravity_bin: launcher(root.path(), &binary, base_url),
        ..common::config(root.path())
    })
    .await
    .unwrap()
}

/// Optional native qualification: the real release passes the version check and lists
/// models for a Gemini API key. Run with ANTIGRAVITY_NATIVE_BIN via the credential-isolated
/// backend launcher.
#[tokio::test]
#[ignore = "requires a native Antigravity binary"]
async fn native_cli_lists_models_for_a_gemini_key() {
    let root = TempDir::new().unwrap();
    let s = native_service(&root, None).await;
    connect(&s, "gemini", KEY).await;
    let catalog = s.antigravity.models(&s).await.unwrap();
    assert_eq!(catalog["error"], "", "{catalog}");
    let models = catalog["models"].as_array().unwrap();
    assert!(!models.is_empty(), "{catalog}");
    assert!(models.iter().all(|m| !text(m, "displayName").is_empty()));
}

/// The first model of the native catalog and its default slug on a Gemini key.
async fn native_model(s: &Service) -> (String, String) {
    let catalog = s.antigravity.models(s).await.unwrap();
    let model = text(&catalog["models"][0], "model").to_owned();
    let slug = s
        .antigravity
        .resolve(s, "gemini", &model, "")
        .await
        .unwrap()
        .unwrap();
    (model, slug)
}

/// Optional native smoke: the real release runs a turn against a local Gemini API with a
/// synthetic key, then resumes the conversation with its history.
#[tokio::test]
#[ignore = "requires a native Antigravity binary"]
async fn native_cli_runs_and_resumes_a_conversation() {
    let root = TempDir::new().unwrap();
    let (url, requests, server) = gemini_server(root.path()).await;
    let s = native_service(&root, Some(&url)).await;
    connect(&s, "gemini", KEY).await;
    let (model, slug) = native_model(&s).await;
    let mut input = plan(
        &root,
        &slug,
        "Reply with the local model result. Do not call tools.",
    );
    input["model"] = model.into();
    let (result, events) = run(&s, input.clone()).await;
    result.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join("result.md")).unwrap(),
        "Native Antigravity passed."
    );
    let conversation = thread(&events);
    let first = requests.lock().unwrap().len();
    assert!(first > 0);

    input["sessionId"] = conversation.clone().into();
    input["execution"]["messageId"] = "follow-up".into();
    input["execution"]["text"] = "Continue in the same conversation. Do not call tools.".into();
    input["output"] = root
        .path()
        .join("follow-up.md")
        .to_string_lossy()
        .into_owned()
        .into();
    let (result, events) = run(&s, input).await;
    server.abort();
    result.unwrap();
    assert_eq!(thread(&events), conversation);
    assert_eq!(
        std::fs::read_to_string(root.path().join("follow-up.md")).unwrap(),
        "Native Antigravity passed."
    );
    assert!(
        requests.lock().unwrap()[first..].iter().any(|request| {
            let contents = request["contents"].to_string();
            contents.contains("Reply with the local model result")
                && contents.contains("Native Antigravity passed.")
                && contents.contains("Continue in the same conversation")
        }),
        "the resumed request did not carry the first turn"
    );
}

/// Optional native qualification of plan-turn isolation: the real release resumes a
/// conversation in a fresh home whose `conversations` directory links to the run's,
/// and records the plan turn's steps there for the next turn.
#[tokio::test]
#[ignore = "requires a native Antigravity binary"]
async fn native_plan_turns_resume_the_conversation_in_a_fresh_home() {
    let root = TempDir::new().unwrap();
    let (url, requests, server) = gemini_server(root.path()).await;
    let s = native_service(&root, Some(&url)).await;
    connect(&s, "gemini", KEY).await;
    let (model, slug) = native_model(&s).await;
    let mut input = plan(&root, &slug, "FIRST-TURN Reply without tools.");
    input["model"] = model.into();
    let (result, events) = run(&s, input.clone()).await;
    result.unwrap();
    let conversation = thread(&events);

    let mut planning = next_message(&root, &input, "plan", "PLAN-RESUME Propose a plan.");
    planning["sessionId"] = conversation.clone().into();
    planning["planning"] = true.into();
    planning["sandbox"] = "read-only".into();
    let (result, events) = run(&s, planning).await;
    result.unwrap();
    assert_eq!(thread(&events), conversation);

    let mut follow_up = next_message(&root, &input, "after", "AFTER-PLAN Continue.");
    follow_up["sessionId"] = conversation.clone().into();
    let (result, _) = run(&s, follow_up).await;
    server.abort();
    result.unwrap();
    let requests = requests.lock().unwrap();
    let carried = |marker: &str, earlier: &[&str]| {
        requests.iter().any(|request| {
            let contents = request["contents"].to_string();
            contents.contains(marker) && earlier.iter().all(|e| contents.contains(e))
        })
    };
    assert!(
        carried("PLAN-RESUME", &["FIRST-TURN"]),
        "the plan turn lost the history"
    );
    assert!(
        carried("AFTER-PLAN", &["FIRST-TURN", "PLAN-RESUME"]),
        "the plan turn's steps did not return"
    );
}

/// Optional native boundary check: in a plan turn the real release asks Cairn's hook
/// before `run_command` and `write_file`, and neither runs.
#[tokio::test]
#[ignore = "requires a native Antigravity binary"]
async fn native_plan_turns_deny_commands_and_writes() {
    let root = TempDir::new().unwrap();
    let (url, requests, server) = gemini_server(root.path()).await;
    let s = native_service(&root, Some(&url)).await;
    connect(&s, "gemini", KEY).await;
    let (model, slug) = native_model(&s).await;
    let mut input = plan(&root, &slug, "PLAN-TOOLS Plan the workspace changes.");
    input["model"] = model.into();
    input["planning"] = true.into();
    input["sandbox"] = "read-only".into();
    let (result, events) = run(&s, input).await;
    server.abort();
    result.unwrap();
    assert!(!root.path().join("native-marker.txt").exists());
    // The release reports each refusal with the reason Cairn's hook gave.
    let requests = requests.lock().unwrap();
    let refused = |tool: &str| {
        requests.iter().any(|request| {
            request["contents"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|content| {
                    content["parts"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|part| {
                            let response = &part["functionResponse"];
                            response["name"] == tool
                                && response["response"].to_string().contains(PLAN_REFUSAL)
                        })
                })
        })
    };
    assert!(refused("run_command"), "{requests:?}");
    assert!(refused("write_file"), "{requests:?}");
    let commands = completed(&events, "command_execution");
    assert_eq!(commands.len(), 1, "{events:?}");
    assert_eq!(commands[0]["status"], "failed");
    assert!(text(commands[0], "aggregated_output").contains(PLAN_REFUSAL));
    assert!(
        completed(&events, "mcp_tool_call")
            .iter()
            .any(|t| t["tool"] == "write_file" && t["status"] == "failed")
    );
    assert_eq!(events.last().unwrap()["type"], "turn.completed");
}

/// The real release sends Cairn's hook the tool call it can read: an ordinary turn
/// refuses Antigravity's own question tool, which only a parsed name can match.
#[tokio::test]
#[ignore = "requires a native Antigravity binary"]
async fn native_ordinary_turns_refuse_the_native_question_tool() {
    let root = TempDir::new().unwrap();
    let (url, requests, server) = gemini_server(root.path()).await;
    let s = native_service(&root, Some(&url)).await;
    connect(&s, "gemini", KEY).await;
    let (model, slug) = native_model(&s).await;
    let mut input = plan(&root, &slug, "ASK-NATIVE Ask me a question.");
    input["model"] = model.into();
    let (result, _) = run(&s, input).await;
    server.abort();
    result.unwrap();
    let requests = requests.lock().unwrap();
    let refused = requests.iter().any(|request| {
        request["contents"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|content| content["parts"].as_array().into_iter().flatten())
            .any(|part| {
                let response = &part["functionResponse"];
                response["name"] == "ask_question"
                    && response["response"]
                        .to_string()
                        .contains(NATIVE_QUESTION_REFUSAL)
            })
    });
    assert!(refused, "{requests:?}");
}
