//! Plugin API v2 end to end, through the built-in 1Password package: an ordinary
//! package with a secret setting, agent grants, commands, a tool, instructions, a
//! declared program, a view, actions and a test.
#[allow(unused_imports)]
use common::{PluginServiceFixture as _, PluginStoreFixture as _};

use common::AgentFixture;
use common::MissionFixture;

mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    config::{MAIN_AGENT_ID, id},
    plugins::{agents, connections, panels, registry},
    run_output,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc, time::Duration};
use tempfile::TempDir;

const OP: &str = "#!/bin/sh\n\
[ \"$OP_SERVICE_ACCOUNT_TOKEN\" = ops_fixture ] || exit 1\n\
[ -z \"$OP_CONNECT_TOKEN\" ] || exit 2\n\
[ \"$OP_CACHE\" = true ] || exit 3\n\
case \"$1\" in\n\
read) printf 'secret-with-no-newline' ;;\n\
item) [ \"$2\" = get ] && printf '{\"fields\":[{\"id\":\"password\",\"label\":\"password\",\"value\":\"hidden-secret\",\"reference\":\"op://vault/item/password\"}],\"args\":\"%s\"}' \"$*\" || printf '[{\"id\":\"vault\"}]' ;;\n\
*) printf '[{\"id\":\"vault\",\"name\":\"Production\"}]' ;;\n\
esac\n";

struct Fixture {
    root: TempDir,
    s: Arc<Service>,
    bearer: String,
    program: PathBuf,
}

async fn fixture() -> Fixture {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    let mut config = common::config(root.path());
    config.runner_url = "http://runner:4311".into();
    let s = Service::new(config).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let program = root.path().join("op");
    write_program(&program, OP);
    s.plugins.use_program("onepassword", "op", program.clone());
    let bearer = bearer_for_agent(&s, MAIN_AGENT_ID).await;
    Fixture {
        root,
        s,
        bearer,
        program,
    }
}

async fn bearer_for_agent(s: &Service, agent: &str) -> String {
    let task = s
        .task_fixture(
            json!({ "name": "Probe", "agentId": agent, "prompt": "Hello" }),
            None,
        )
        .await
        .unwrap();
    let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&run, "id"),
            json!({ "status": "running", "isolated": true }),
        )
        .await
        .unwrap();
    let config = s.mcps.run_configuration(s, &run).await.unwrap();
    text(&config["env"], "CAIRN_MCP_RUN_TOKEN").to_owned()
}

fn write_program(path: &PathBuf, script: &str) {
    let daemon = path.with_extension("daemon.py");
    let trace = path.with_extension("daemons.jsonl");
    let source = format!(
        r#"import json,os,socket
with open({trace:?},'a') as trace:
 trace.write(json.dumps({{'pid':os.getpid(),'home':os.environ['HOME'],'socket':os.environ['OP_SOCK'],'hasToken':'OP_SERVICE_ACCOUNT_TOKEN' in os.environ}})+'\n')
server=socket.socket(socket.AF_UNIX)
server.bind(os.environ['OP_SOCK'])
server.listen(128)
while True:
 client,_=server.accept()
 client.close()
"#,
        trace = trace.to_str().unwrap()
    );
    std::fs::write(&daemon, source).unwrap();
    let prefix = format!(
        "#!/bin/sh\nif [ \"$1\" = daemon ]; then exec /usr/bin/python3 '{daemon}'; fi\n",
        daemon = daemon.display()
    );
    let script = script.replacen("#!/bin/sh\n", &prefix, 1);
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

async fn save(s: &Service, input: Value, existing: Option<&str>) -> Value {
    common::save_plugin(s, "onepassword", &input, existing)
        .await
        .unwrap()
}

fn command(name: &str, args: &[&str]) -> Value {
    json!({ "plugin": "onepassword", "command": name, "args": args })
}

fn daemons(f: &Fixture) -> Vec<Value> {
    std::fs::read_to_string(f.program.with_extension("daemons.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn caches_are_shared_by_authorized_agents_and_isolated_between_connections() {
    let f = fixture().await;
    let other =
        f.s.agent_fixture(json!({"name":"Other"}), None)
            .await
            .unwrap();
    let first = save(
        &f.s,
        json!({
            "name": "First", "enabled": true,
            "secrets": { "token": "ops_fixture" },
            "agentIds": [MAIN_AGENT_ID, other["id"]],
        }),
        None,
    )
    .await;
    let second = save(
        &f.s,
        json!({
            "name": "Second", "enabled": true,
            "secrets": { "token": "ops_fixture" }, "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await;
    let other_bearer = bearer_for_agent(&f.s, text(&other, "id")).await;
    write_program(
        &f.program,
        "#!/bin/sh\nprintf '{\"home\":\"%s\",\"socket\":\"%s\"}' \"$HOME\" \"$OP_SOCK\"\n",
    );
    let read = command(
        "read",
        &["--account", text(&first, "id"), "op://vault/item/password"],
    );
    let (a, b) = tokio::join!(
        f.s.plugins.command(&f.s, &f.bearer, &read),
        f.s.plugins.command(&f.s, &other_bearer, &read)
    );
    assert_eq!(a.as_ref().unwrap(), b.as_ref().unwrap());
    let first_value: Value = serde_json::from_str(a.unwrap()["value"].as_str().unwrap()).unwrap();
    assert_eq!(
        daemons(&f).len(),
        1,
        "Concurrent callers must share one foreground daemon"
    );
    let second_value = run(
        &f,
        command(
            "read",
            &["--account", text(&second, "id"), "op://vault/item/password"],
        ),
    )
    .await
    .unwrap();
    let second_value: Value =
        serde_json::from_str(second_value["value"].as_str().unwrap()).unwrap();
    assert_ne!(first_value["home"], second_value["home"]);
    assert_ne!(first_value["socket"], second_value["socket"]);
    for entry in daemons(&f) {
        assert_eq!(
            entry["hasToken"], false,
            "The long-lived daemon must never inherit the service token"
        );
        let permissions = std::fs::metadata(entry["home"].as_str().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(permissions & 0o777, 0o700);
    }
    f.s.agent_fixture(json!({"plugins":[]}), Some(text(&other, "id")))
        .await
        .unwrap();
    let error =
        f.s.plugins
            .command(&f.s, &other_bearer, &read)
            .await
            .unwrap_err();
    assert_eq!(error.status, 403);
    assert_eq!(
        run(&f, read).await.unwrap()["value"],
        first_value.to_string()
    );
    assert_eq!(
        daemons(&f).len(),
        2,
        "Revoking one agent cannot grant access or require a different cache for another"
    );
}

#[tokio::test]
async fn rotating_disabling_and_removing_connections_destroy_their_cache_homes() {
    let f = fixture().await;
    let saved = save(
        &f.s,
        json!({
            "name": "Production", "enabled": true,
            "secrets": { "token": "ops_fixture" }, "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await;
    let account = text(&saved, "id");
    let read = command("read", &["--account", account, "op://vault/item/password"]);
    assert!(run(&f, read.clone()).await.is_ok());
    let old = daemons(&f)[0]["home"].as_str().unwrap().to_owned();
    save(
        &f.s,
        json!({"name":"Production","enabled":true,"secrets":{"token":"ops_rotated"}}),
        Some(account),
    )
    .await;
    assert!(!std::path::Path::new(&old).exists());
    assert_eq!(run(&f, read.clone()).await, Err(502));
    let rotated = daemons(&f)[1]["home"].as_str().unwrap().to_owned();
    assert_ne!(old, rotated);
    connections::set_enabled(
        &f.s,
        "onepassword",
        account,
        &cairn_contracts::PluginEnabledInput { enabled: false },
    )
    .await
    .unwrap();
    assert!(!std::path::Path::new(&rotated).exists());
    assert_eq!(run(&f, read.clone()).await, Err(403));
    // Owner tests on disabled instances have an ephemeral cache, never a resident one.
    save(
        &f.s,
        json!({"name":"Production","enabled":false,"secrets":{"token":"ops_fixture"}}),
        Some(account),
    )
    .await;
    assert!(
        !panels::test(&f.s, "onepassword", account)
            .await
            .unwrap()
            .is_empty()
    );
    let transient = daemons(&f).last().unwrap()["home"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!std::path::Path::new(&transient).exists());
    connections::set_enabled(
        &f.s,
        "onepassword",
        account,
        &cairn_contracts::PluginEnabledInput { enabled: true },
    )
    .await
    .unwrap();
    assert!(run(&f, read.clone()).await.is_ok());
    let active = daemons(&f).last().unwrap()["home"]
        .as_str()
        .unwrap()
        .to_owned();
    connections::remove(&f.s, "onepassword", account)
        .await
        .unwrap();
    assert!(!std::path::Path::new(&active).exists());
    assert_eq!(run(&f, read).await, Err(403));
}

#[tokio::test]
async fn disabling_a_connection_interrupts_cached_reads_without_delivering_the_secret() {
    let f = fixture().await;
    let saved = save(
        &f.s,
        json!({
            "name": "Production", "enabled": true,
            "secrets": { "token": "ops_fixture" }, "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await;
    let account = text(&saved, "id");
    let started = f.root.path().join("read-started");
    write_program(
        &f.program,
        &format!(
            "#!/bin/sh\n/usr/bin/touch '{}'\n/bin/sleep 2\nprintf 'never-release-this-secret'\n",
            started.display()
        ),
    );
    let read = command("read", &["--account", account, "op://vault/item/password"]);
    let disable = async {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        connections::set_enabled(
            &f.s,
            "onepassword",
            account,
            &cairn_contracts::PluginEnabledInput { enabled: false },
        )
        .await
        .unwrap();
    };
    let (result, ()) = tokio::join!(f.s.plugins.command(&f.s, &f.bearer, &read), disable);
    let error = result.unwrap_err();
    assert!(!error.message.contains("never-release-this-secret"));
    assert!(matches!(error.status, 403 | 409));
    let home = daemons(&f)[0]["home"].as_str().unwrap().to_owned();
    assert!(!std::path::Path::new(&home).exists());
}

#[tokio::test]
async fn manager_shutdown_interrupts_reads_and_refuses_later_commands() {
    let f = fixture().await;
    let saved = save(
        &f.s,
        json!({
            "name": "Production", "enabled": true,
            "secrets": { "token": "ops_fixture" }, "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await;
    let started = f.root.path().join("read-started");
    write_program(
        &f.program,
        &format!(
            "#!/bin/sh\n/usr/bin/touch '{}'\n/bin/sleep 2\nprintf 'never-release-this-secret'\n",
            started.display()
        ),
    );
    let read = command(
        "read",
        &["--account", text(&saved, "id"), "op://vault/item/password"],
    );
    let shutdown = async {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        f.s.shutdown.cancel();
    };
    let (result, ()) = tokio::join!(f.s.plugins.command(&f.s, &f.bearer, &read), shutdown);
    let error = result.unwrap_err();
    assert_eq!(error.status, 503);
    assert!(!error.message.contains("never-release-this-secret"));
    assert_eq!(run(&f, read).await, Err(503));
    assert_eq!(daemons(&f).len(), 1);
}

#[tokio::test]
async fn package_activation_interrupts_a_read_from_the_previous_approved_version() {
    let f = fixture().await;
    let started = f.root.path().join("package-read-started");
    write_program(
        &f.program,
        &format!(
            "#!/bin/sh\n/usr/bin/touch '{}'\n/bin/sleep 2\nprintf 'never-release-this-secret'\n",
            started.display()
        ),
    );
    let source = "return { command = function(input) local result = cairn.exec.run { program = 'op', instance = input.instances[1].id }; return result.stdout end }";
    let mut next_digest = String::new();
    for version in ["1.0.0", "1.0.1"] {
        let package = registry::install(
            &f.s,
            &json!({
                "manifest": {
                    "id": "cacheprobe", "name": "Cache probe", "version": version,
                    "apiVersion": 2, "capabilities": ["exec"],
                    "agentAccess": { "description": "Fixture reads" },
                    "programs": [{
                        "name": "op", "path": f.program,
                        "cache": { "daemonArgs": ["daemon"], "socketEnv": "OP_SOCK" },
                    }],
                    "commands": [{ "name": "read", "output": "secret" }],
                },
                "source": source,
            }),
        )
        .await
        .unwrap();
        let digest = text(&package, "digest");
        if version == "1.0.0" {
            registry::approve(&f.s, "cacheprobe", version, digest)
                .await
                .unwrap();
        } else {
            next_digest = digest.to_owned();
        }
    }
    common::save_plugin(
        &f.s,
        "cacheprobe",
        &json!({
            "name": "Fixture", "enabled": true, "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await
    .unwrap();
    let activate = async {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        registry::approve(&f.s, "cacheprobe", "1.0.1", &next_digest)
            .await
            .unwrap();
    };
    let read = json!({ "plugin": "cacheprobe", "command": "read", "args": [] });
    let (result, ()) = tokio::join!(f.s.plugins.command(&f.s, &f.bearer, &read), activate);
    let error = result.unwrap_err();
    assert_eq!(error.status, 409);
    assert!(!error.message.contains("never-release-this-secret"));
    assert_eq!(
        registry::active(&f.s, "cacheprobe")
            .await
            .unwrap()
            .manifest
            .version,
        "1.0.1"
    );
    assert!(!std::path::Path::new(daemons(&f)[0]["home"].as_str().unwrap()).exists());
}

#[tokio::test]
async fn quota_failures_explain_the_account_limit_without_echoing_program_output() {
    let f = fixture().await;
    let saved = save(
        &f.s,
        json!({
            "name": "Production", "enabled": true,
            "secrets": { "token": "ops_fixture" }, "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await;
    let account = text(&saved, "id");
    write_program(
        &f.program,
        r#"#!/bin/sh
if [ "$1" = service-account ]; then
    printf '[{"type":"account","action":"read_write","limit":1000,"used":1000,"remaining":0,"reset":18803,"secret":"ops_fixture"}]'
    exit 0
fi
echo 'ops_fixture Too many requests. Your client has been rate-limited. Try again in seconds' >&2
exit 1
"#,
    );
    let read = command("read", &["--account", account, "op://vault/item/password"]);
    let error =
        f.s.plugins
            .command(&f.s, &f.bearer, &read)
            .await
            .unwrap_err();
    assert!(error.message.contains("daily account quota"), "{error:?}");
    assert!(error.message.contains("1000/1000"), "{error:?}");
    assert!(error.message.contains("18803 seconds"), "{error:?}");
    assert!(error.message.contains("shared"), "{error:?}");
    assert!(!error.message.contains("ops_fixture"));

    let limits = run(&f, command("limits", &["--account", account]))
        .await
        .unwrap();
    assert_eq!(limits["value"]["limits"][0]["remaining"], 0);
    assert!(!limits.to_string().contains("ops_fixture"));

    let metadata = mcp(
        &f,
        "tools/call",
        json!({ "name": "onepassword", "arguments": { "operation": "limits", "accountId": account } }),
    )
    .await;
    assert_eq!(
        metadata["result"]["structuredContent"]["limits"][0]["remaining"],
        0
    );
    assert!(!metadata.to_string().contains("ops_fixture"));

    let result = mcp(
        &f, "tools/call",
        json!({ "name": "onepassword", "arguments": { "operation": "fields", "accountId": account, "vault": "vault", "item": "item" } }),
    ).await;
    assert_eq!(result["result"]["isError"], true);
    assert!(
        result["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("daily account quota")
    );
    assert!(!result.to_string().contains("ops_fixture"));
}

#[tokio::test]
async fn rate_limit_diagnostics_fall_back_safely_and_choose_the_longest_exhausted_limit() {
    let f = fixture().await;
    let saved = save(
        &f.s,
        json!({ "name": "Production", "enabled": true, "secrets": { "token": "ops_fixture" }, "agentIds": [MAIN_AGENT_ID] }),
        None,
    ).await;
    let read = command(
        "read",
        &["--account", text(&saved, "id"), "op://vault/item/password"],
    );
    for diagnostic in [
        "printf 'ops_fixture invalid-json'",
        "echo ops_fixture >&2; exit 1",
        // A host-level stdout-limit error must not hide the original rate limit.
        "/usr/bin/head -c 1048577 /dev/zero",
        "printf '[{\"type\":\"account\",\"action\":\"read_write\",\"limit\":\"ops_fixture\",\"used\":1000,\"remaining\":0,\"reset\":1}]'",
    ] {
        write_program(
            &f.program,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = service-account ]; then {diagnostic}; exit 0; fi\necho 'rate limit exceeded ops_fixture' >&2\nexit 1\n"
            ),
        );
        let error =
            f.s.plugins
                .command(&f.s, &f.bearer, &read)
                .await
                .unwrap_err();
        assert!(error.message.contains("rate limit"), "{error:?}");
        assert!(!error.message.contains("ops_fixture"));
    }
    write_program(
        &f.program,
        r#"#!/bin/sh
if [ "$1" = service-account ]; then
    printf '[{"type":"account","action":"read_write","limit":1000,"used":1000,"remaining":0,"reset":18803},{"type":"token","action":"read","limit":1000,"used":1000,"remaining":0,"reset":60}]'
    exit 0
fi
echo '(429) Too Many Requests ops_fixture' >&2
exit 1
"#,
    );
    let error =
        f.s.plugins
            .command(&f.s, &f.bearer, &read)
            .await
            .unwrap_err();
    assert!(error.message.contains("18803 seconds"), "{error:?}");

    write_program(
        &f.program,
        r#"#!/bin/sh
if [ "$1" = service-account ]; then
    printf '[{"type":"token","action":"write","limit":100,"used":100,"remaining":0,"reset":3500},{"type":"token","action":"read","limit":1000,"used":1000,"remaining":0,"reset":60},{"type":"account","action":"read_write","limit":1000,"used":900,"remaining":100,"reset":18000}]'
    exit 0
fi
echo 'rate limit exceeded' >&2
exit 1
"#,
    );
    let error =
        f.s.plugins
            .command(&f.s, &f.bearer, &read)
            .await
            .unwrap_err();
    assert!(
        error.message.contains("hourly service-account quota"),
        "{error:?}"
    );
    assert!(error.message.contains("1000/1000"), "{error:?}");
    assert!(error.message.contains("60 seconds"), "{error:?}");
}

#[tokio::test]
async fn selected_secret_fields_share_one_item_read_and_never_enter_metadata() {
    let f = fixture().await;
    let saved = save(
        &f.s,
        json!({ "name": "Production", "enabled": true, "secrets": { "token": "ops_fixture" }, "agentIds": [MAIN_AGENT_ID] }),
        None,
    ).await;
    let calls = f.root.path().join("calls");
    write_program(
        &f.program,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
[ "$1" = item ] && [ "$2" = get ] || exit 1
printf '{{"fields":[{{"id":"username","label":"username","value":"alice"}},{{"id":"credential","label":"JIRA_API_TOKEN","type":"CONCEALED","value":"hidden-token"}},{{"id":"notesPlain","value":"unrequested-secret"}}]}}'
"#,
            calls.display()
        ),
    );
    let account = text(&saved, "id");
    let read = |fields| {
        command(
            "read-fields",
            &[
                "--account",
                account,
                "--vault",
                "vault",
                "--item",
                "--help",
                "--fields",
                fields,
            ],
        )
    };
    let result = run(&f, read(r#"["username","JIRA_API_TOKEN"]"#))
        .await
        .unwrap();
    assert_eq!(result["output"], "secret");
    let values: Value = serde_json::from_str(result["value"].as_str().unwrap()).unwrap();
    assert_eq!(
        values,
        json!({ "username": "alice", "JIRA_API_TOKEN": "hidden-token" })
    );
    let invoked = std::fs::read_to_string(&calls).unwrap();
    assert_eq!(invoked.lines().count(), 1);
    assert_eq!(
        invoked.trim(),
        "item get --vault=vault --format=json -- --help"
    );
    for fields in [
        "[]",
        "{}",
        "null",
        "not-json",
        "[1]",
        r#"["username","username"]"#,
        r#"["username",null]"#,
        r#"[null,"username"]"#,
    ] {
        assert!(run(&f, read(fields)).await.is_err(), "{fields}");
    }
    assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 1);
    let missing =
        f.s.plugins
            .command(&f.s, &f.bearer, &read(r#"["missing"]"#))
            .await
            .unwrap_err();
    assert!(!missing.message.contains("hidden-token"));
    assert!(!missing.message.contains("unrequested-secret"));
    let metadata = mcp(&f, "tools/call", json!({ "name": "onepassword", "arguments": { "operation": "read-fields", "accountId": account } })).await;
    assert_eq!(metadata["result"]["isError"], true);
    assert!(!metadata.to_string().contains("hidden-token"));
    write_program(
        &f.program,
        r#"#!/bin/sh
printf '{"fields":[{"id":"username","value":"alice"},{"id":"other","label":"username","value":"unrequested-secret"}]}'
"#,
    );
    let by_id = run(&f, read(r#"["username"]"#)).await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(by_id["value"].as_str().unwrap()).unwrap(),
        json!({"username":"alice"})
    );
    for fields in [
        r#"[{"id":"first","label":"duplicate","value":"hidden-token"},{"id":"second","label":"duplicate","value":"unrequested-secret"}]"#,
        r#"[{"id":"duplicate","type":"OTP","value":"totp-seed"}]"#,
    ] {
        write_program(
            &f.program,
            &format!("#!/bin/sh\nprintf '%s' '{{\"fields\":{fields}}}'\n"),
        );
        let error =
            f.s.plugins
                .command(&f.s, &f.bearer, &read(r#"["duplicate"]"#))
                .await
                .unwrap_err();
        assert!(!error.message.contains("hidden-token"));
        assert!(!error.message.contains("totp-seed"));
    }
    f.s.agent_fixture(json!({ "plugins": [] }), Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    assert_eq!(run(&f, read(r#"["username"]"#)).await, Err(403));
}

async fn run(f: &Fixture, input: Value) -> Result<Value, u16> {
    f.s.plugins
        .command(&f.s, &f.bearer, &input)
        .await
        .map_err(|error| error.status)
}

#[tokio::test]
async fn service_accounts_are_instances_with_encrypted_tokens_and_live_grants() {
    let f = fixture().await;
    let s = &f.s;
    let saved = save(
        s,
        json!({ "name": "Production", "enabled": true, "secrets": { "token": "ops_fixture" } }),
        None,
    )
    .await;
    let account = text(&saved, "id").to_owned();
    assert!(!saved.to_string().contains("ops_fixture"));
    assert_eq!(saved["secretKeys"], json!(["token"]));
    let stored = s
        .store
        .kv(&format!(
            "mcp-secret:{}",
            connections::secrets_key(&account)
        ))
        .await
        .unwrap()
        .unwrap();
    assert!(!stored.to_string().contains("ops_fixture"));
    let read = command("read", &["--account", &account, "op://vault/item/password"]);

    // Nothing is granted by default, including Main agent.
    assert_eq!(run(&f, read.clone()).await, Err(403));
    assert_eq!(run(&f, command("accounts", &[])).await, Err(403));
    s.agent_fixture(json!({ "plugins": [account] }), Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    let mut update = json!({ "name": "Production", "enabled": true });
    save(s, update.clone(), Some(&account)).await; // an omitted token is kept
    assert_eq!(
        run(&f, command("accounts", &[])).await.unwrap()["value"]["accounts"],
        json!([{ "id": account, "name": "Production" }])
    );
    assert_eq!(
        run(&f, read.clone()).await.unwrap(),
        json!({ "output": "secret", "value": "secret-with-no-newline" })
    );
    for (name, args) in [
        ("vaults", vec!["--account", account.as_str()]),
        (
            "items",
            vec!["--account", account.as_str(), "--vault", "vault"],
        ),
    ] {
        assert_eq!(
            run(&f, command(name, &args)).await.unwrap()["value"]["items"][0]["id"],
            "vault"
        );
    }
    // Field references never include values, and items cannot become options.
    let fields = run(
        &f,
        command(
            "fields",
            &[
                "--account",
                &account,
                "--vault",
                "vault",
                "--item",
                "--help",
            ],
        ),
    )
    .await
    .unwrap();
    assert!(!fields.to_string().contains("hidden-secret"));
    assert_eq!(
        fields["value"]["fields"][0]["reference"],
        "op://vault/item/password"
    );
    // Only the current CLI frame is understood.
    let earlier_frame = json!({
        "plugin": "onepassword",
        "arguments": { "operation": "read", "accountId": account, "reference": "op://vault/item/password" },
    });
    assert_eq!(run(&f, earlier_frame).await, Err(400));

    update["enabled"] = json!(false);
    save(s, update.clone(), Some(&account)).await;
    assert_eq!(run(&f, read.clone()).await, Err(403));
    update["enabled"] = json!(true);
    s.agent_fixture(json!({ "plugins": [] }), Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    save(s, update.clone(), Some(&account)).await;
    assert_eq!(run(&f, read.clone()).await, Err(403));

    s.agent_fixture(json!({ "plugins": [account] }), Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    update["secrets"] = json!({ "token": "ops_rotated" });
    save(s, update.clone(), Some(&account)).await;
    let secrets = s
        .vault
        .get(&connections::secrets_key(&account))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(secrets["fields"]["token"], "ops_rotated");
    // The fake CLI only accepts the original token.
    assert_eq!(run(&f, read.clone()).await, Err(502));

    let temporary = s
        .agent_fixture(json!({ "name": "Temporary" }), None)
        .await
        .unwrap();
    s.agent_fixture(
        json!({ "plugins": [account] }),
        Some(text(&temporary, "id")),
    )
    .await
    .unwrap();
    update.as_object_mut().unwrap().remove("secrets");
    save(s, update.clone(), Some(&account)).await;
    cairn_installation::api::dispatch(
        s,
        &cairn_installation::http::Input {
            method: "DELETE".into(),
            path: format!("/api/agents/{}", text(&temporary, "id")),
            query: std::collections::HashMap::default(),
            headers: axum::http::HeaderMap::default(),
            body: Value::Null,
            identity: Some(cairn_installation::auth::InstallationIdentity::trusted(
                cairn_installation::auth::InstallationRole::Owner,
                "fixture-owner",
            )),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        s.instance_fixture(&account).await.unwrap()["agentIds"],
        json!([MAIN_AGENT_ID])
    );
    connections::remove(s, "onepassword", &account)
        .await
        .unwrap();
    assert!(
        s.vault
            .get(&connections::secrets_key(&account))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(run(&f, read).await, Err(403));
}

#[tokio::test]
async fn errors_never_echo_secrets_and_revocation_beats_slow_programs() {
    let f = fixture().await;
    let s = &f.s;
    let error = common::save_plugin(
        s,
        "onepassword",
        &json!({ "name": "Bad", "enabled": true, "secrets": { "token": "private-invalid-token" } }),
        None,
    )
    .await
    .unwrap_err();
    assert!(!error.message.contains("private-invalid-token"));
    let unknown_agent = common::save_plugin(
        s,
        "onepassword",
        &json!({ "name": "Bad", "enabled": true, "secrets": { "token": "ops_fixture" }, "agentIds": [id()] }),
        None,
    )
    .await;
    assert!(unknown_agent.is_err());
    let saved = save(
        s,
        json!({
            "name": "Production", "enabled": true,
            "secrets": { "token": "ops_fixture" }, "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await;
    let account = text(&saved, "id").to_owned();
    let read = command("read", &["--account", &account, "op://vault/item/password"]);

    // Arguments are declared: no options smuggled into op, no write commands.
    assert_eq!(
        run(
            &f,
            command("read", &["--account", &account, "--out-file=/tmp/leak"])
        )
        .await,
        Err(400)
    );
    assert_eq!(run(&f, command("write", &[])).await, Err(400));

    write_program(
        &f.program,
        "#!/bin/sh\necho ops_fixture >&2\necho ops_fixture\nexit 1\n",
    );
    let error = f.s.plugins.command(s, &f.bearer, &read).await.unwrap_err();
    assert!(!error.message.contains("ops_fixture"), "{}", error.message);
    assert!(error.message.contains("1Password request failed"));

    let started = f.root.path().join("started");
    let released = f.root.path().join("released");
    write_program(
        &f.program,
        &format!(
            "#!/bin/sh\n: > '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nprintf secret\n",
            started.display(),
            released.display()
        ),
    );
    let revoke = async {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        connections::remove(s, "onepassword", &account)
            .await
            .unwrap();
        std::fs::write(&released, "").unwrap();
    };
    let (result, ()) = tokio::join!(run(&f, read.clone()), revoke);
    assert_eq!(result, Err(403));

    let run_record = cairn_installation::project_workspaces::authorize(s, &f.bearer)
        .await
        .unwrap();
    s.mcps.revoke_run(s, text(&run_record, "id")).await.unwrap();
    assert_eq!(run(&f, command("accounts", &[])).await, Err(401));
}

async fn mcp(f: &Fixture, method: &str, params: Value) -> Value {
    let app = cairn_installation::http::router(f.s.clone()).await.unwrap();
    let response = common::send(
        &app,
        common::request("POST", "/mcp-workspace")
            .header("authorization", format!("Bearer {}", f.bearer))
            .body(Body::from(
                json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
                    .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    common::read_json(response).await
}

#[tokio::test]
async fn granted_agents_get_tools_and_instructions_others_do_not() {
    let f = fixture().await;
    let s = &f.s;
    let names = |listing: &Value| {
        listing["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| text(tool, "name").to_owned())
            .collect::<Vec<_>>()
    };
    let listing = mcp(&f, "tools/list", json!({})).await;
    assert!(!names(&listing).contains(&"onepassword".to_owned()));
    let instructions = agents::instructions(s, MAIN_AGENT_ID).await.unwrap();
    assert!(instructions.is_empty());

    let saved = save(
        s,
        json!({
            "name": "Production", "enabled": true,
            "secrets": { "token": "ops_fixture" }, "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await;
    let listing = mcp(&f, "tools/list", json!({})).await;
    assert!(names(&listing).contains(&"onepassword".to_owned()));
    // Package tools never replace Cairn's own.
    assert_eq!(
        names(&listing)
            .iter()
            .filter(|name| *name == "publish_artifact")
            .count(),
        1
    );
    let result = mcp(
        &f,
        "tools/call",
        json!({ "name": "onepassword", "arguments": { "operation": "vaults", "accountId": saved["id"] } }),
    )
    .await;
    assert_eq!(
        result["result"]["structuredContent"]["items"][0]["id"], "vault",
        "{result}"
    );
    for arguments in [
        json!({ "operation": "read", "accountId": saved["id"] }),
        json!({ "operation": "vaults", "accountId": saved["id"], "extra": true }),
    ] {
        let result = mcp(
            &f,
            "tools/call",
            json!({ "name": "onepassword", "arguments": arguments }),
        )
        .await;
        assert_eq!(result["result"]["isError"], true, "{result}");
        assert!(result["result"]["structuredContent"].is_null());
    }

    let run = cairn_installation::project_workspaces::authorize(s, &f.bearer)
        .await
        .unwrap();
    let prompt = run_output::prompt(&agents::with_instructions(s, &run).await, false);
    assert!(prompt.contains("cairn plugin onepassword read --account ACCOUNT_ID"));
}

#[tokio::test]
async fn metadata_tool_explains_how_to_recover_from_a_missing_account() {
    let f = fixture().await;
    let saved = save(
        &f.s,
        json!({
            "name": "Production",
            "enabled": true,
            "secrets": { "token": "ops_fixture" },
            "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await;

    for operation in ["vaults", "items", "fields"] {
        let mut arguments = json!({
            "operation": operation,
            "vault": "vault",
            "item": "item",
        });
        let result = mcp(
            &f,
            "tools/call",
            json!({ "name": "onepassword", "arguments": arguments }),
        )
        .await;
        assert_eq!(result["result"]["isError"], true, "{result}");
        assert_eq!(
            result["result"]["content"][0]["text"],
            format!(
                "accountId is required for the {operation} operation. Call accounts to list authorized accounts, then retry with the chosen account's id as accountId."
            ),
            "{result}"
        );

        let accounts = mcp(
            &f,
            "tools/call",
            json!({ "name": "onepassword", "arguments": { "operation": "accounts" } }),
        )
        .await;
        let account = &accounts["result"]["structuredContent"]["accounts"][0]["id"];
        assert_eq!(account, &saved["id"], "{accounts}");

        arguments["accountId"] = account.clone();
        let retry = mcp(
            &f,
            "tools/call",
            json!({ "name": "onepassword", "arguments": arguments }),
        )
        .await;
        assert_ne!(retry["result"]["isError"], true, "{retry}");
        assert!(retry["result"]["structuredContent"].is_object(), "{retry}");

        arguments["accountId"] = json!("not-authorized");
        let denied = mcp(
            &f,
            "tools/call",
            json!({ "name": "onepassword", "arguments": arguments }),
        )
        .await;
        assert_eq!(denied["result"]["isError"], true, "{denied}");
        assert_eq!(
            denied["result"]["content"][0]["text"], "This 1Password account is not authorized.",
            "{denied}"
        );
    }
}

#[tokio::test]
async fn owners_see_the_package_view_run_its_actions_and_test_it() {
    let f = fixture().await;
    let s = &f.s;
    let saved = save(
        s,
        json!({ "name": "Production", "enabled": true, "secrets": { "token": "ops_fixture" } }),
        None,
    )
    .await;
    let account = text(&saved, "id");
    let view = panels::view(s, "onepassword", account).await.unwrap();
    assert_eq!(view["view"][0]["type"], "section");
    let result = common::plugin_action(s, "onepassword", account, &json!({ "action": "vaults" }))
        .await
        .unwrap();
    assert_eq!(
        result["view"][0]["children"][0]["items"][0]["title"],
        "Production"
    );
    assert!(
        common::plugin_action(s, "onepassword", account, &json!({ "action": "Bad name" }))
            .await
            .is_err()
    );
    let test = common::plugin_test(s, "onepassword", account)
        .await
        .unwrap();
    assert_eq!(test["message"], "Connected · 1 vault");
    write_program(&f.program, "#!/bin/sh\nexit 1\n");
    assert!(
        common::plugin_test(s, "onepassword", account)
            .await
            .unwrap_err()
            .message
            .contains("1Password request failed")
    );
}

#[tokio::test]
async fn manifest_constraints_are_checked_before_install_and_defaults_can_be_saved() {
    use cairn_installation::plugins::registry;
    let f = fixture().await;
    let s = &f.s;
    let mut package = status_board();
    package["source"] = json!("return {}");
    package["manifest"]["settings"] = json!([
        {
            "key": "note",
            "title": "Note",
            "type": "text",
            "maxLength": 10000,
            "default": "é".repeat(3000),
        },
        {
            "key": "code",
            "title": "Code",
            "type": "string",
            "pattern": "[a-z]+",
            "maxLength": 3,
            "default": "abc",
        },
        {
            "key": "count",
            "title": "Count",
            "type": "number",
            "minimum": 1,
            "maximum": 2,
            "default": 2,
        },
        {
            "key": "apiKey",
            "title": "API key",
            "type": "secret",
            "maxLength": 16384,
        },
    ]);
    for (key, value) in [
        ("maxLength", json!(-1)),
        ("maxLength", json!(1.5)),
        ("maxLength", json!(10001)),
        ("default", json!(false)),
        ("default", json!("x".repeat(10001))),
    ] {
        let mut invalid = package.clone();
        invalid["manifest"]["settings"][0][key] = value;
        assert_eq!(
            registry::install(s, &invalid).await.unwrap_err().status,
            400
        );
        assert!(!f.root.path().join("data/plugins/statusboard").exists());
    }
    registry_install_and_approve(s, package.clone()).await;
    let first = registry::active(s, "statusboard").await.unwrap();
    let reloaded = registry::active(s, "statusboard").await.unwrap();
    assert_eq!(first.digest, reloaded.digest);
    assert_eq!(
        serde_json::to_value(&reloaded.manifest).unwrap(),
        serde_json::to_value(&first.manifest).unwrap()
    );

    let saved = common::save_plugin(
        s,
        "statusboard",
        &json!({ "name": "Defaults", "enabled": true }),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        saved["settings"],
        json!({ "note": "é".repeat(3000), "code": "abc", "count": 2 })
    );
    let input = json!({ "note": "é".repeat(10000) });
    assert_eq!(
        connections::settings(&reloaded.manifest, &input).unwrap()["note"],
        input["note"]
    );
    assert!(
        connections::settings(&reloaded.manifest, &json!({ "note": "x".repeat(10001) })).is_err()
    );
    let empty = serde_json::Map::new();
    assert!(
        connections::secrets(
            &reloaded.manifest,
            &json!({ "apiKey": "x".repeat(16384) }),
            &empty
        )
        .is_ok()
    );
    assert!(
        connections::secrets(
            &reloaded.manifest,
            &json!({ "apiKey": "x".repeat(16385) }),
            &empty
        )
        .is_err()
    );

    // Simulate a previously admitted manifest on disk. Reads must report its
    // unsupported constraint, without rewriting the package or its approval.
    let manifest_path = f
        .root
        .path()
        .join("data/plugins/statusboard/1.0.0/manifest.json");
    package["manifest"]["settings"][0]["maxLength"] = json!(10001);
    let legacy_bytes = serde_json::to_vec_pretty(&package["manifest"]).unwrap();
    std::fs::write(&manifest_path, &legacy_bytes).unwrap();
    let error = registry::active(s, "statusboard").await.err().unwrap();
    assert!(error.to_string().contains("10000"));
    assert_eq!(std::fs::read(&manifest_path).unwrap(), legacy_bytes);

    // A corrected version restores access under the same plugin and instance IDs.
    package["manifest"]["version"] = json!("1.0.1");
    package["manifest"]["settings"][0]["maxLength"] = json!(10000);
    let installed = registry::install(s, &package).await.unwrap();
    registry::approve(s, "statusboard", "1.0.1", text(&installed, "digest"))
        .await
        .unwrap();
    let repaired = registry::active(s, "statusboard").await.unwrap();
    assert_eq!(repaired.manifest.version, "1.0.1");
    let updated = common::save_plugin(
        s,
        "statusboard",
        &json!({ "name": "Defaults", "enabled": true }),
        saved["id"].as_str(),
    )
    .await
    .unwrap();
    assert_eq!(updated["id"], saved["id"]);
    assert_eq!(updated["settings"], saved["settings"]);
}

/// A third-party package: everything a v2 integration needs, without any Cairn change.
fn status_board() -> Value {
    json!({
        "manifest": {
            "id": "statusboard",
            "name": "Status board",
            "version": "1.0.0",
            "apiVersion": 2,
            "capabilities": ["http", "storage"],
            "hosts": ["api.example.com"],
            "instances": { "singular": "Board", "plural": "Boards" },
            "brand": { "color": "#123456", "path": "M0 0h24v24H0z" },
            "settings": [
                { "key": "apiKey", "title": "API key", "type": "secret", "required": true },
                { "key": "region", "title": "Region", "type": "select", "default": "eu",
                  "options": [{ "value": "eu", "label": "Europe" }, { "value": "us", "label": "United States" }] },
                { "key": "labels", "title": "Labels", "type": "multiselect",
                  "options": [{ "value": "prod", "label": "Production" }, { "value": "beta", "label": "Beta" }] },
            ],
            "agentAccess": { "description": "Agents allowed to check statuses." },
            "commands": [{
                "name": "status", "output": "text",
                "arguments": [{ "name": "service", "positional": true, "required": true, "pattern": "[a-z]+" }],
            }],
            "tools": [{
                "name": "statusboard_status",
                "description": "Current state of one service.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "service": { "type": "string" } },
                    "required": ["service"],
                    "additionalProperties": false,
                },
            }],
            "instructions": "Status board: check services with cairn plugin statusboard status <service>.",
            "instanceView": true,
        },
        "source": r#"
            local function state(instance, service)
                local response = cairn.http.request({
                    url = "https://api.example.com/v1/status/" .. service,
                    auth = { secret = "apiKey", header = "authorization", prefix = "Bearer " },
                    instance = instance,
                })
                return response.json and response.json.state or "unknown"
            end
            return {
                command = function(input)
                    local board = input.instances[1]
                    return `{input.arguments.service}: {state(board.id, input.arguments.service)} ({board.settings.region})`
                end,
                tool = function(input)
                    return { state = state(input.instances[1].id, input.input.service) }
                end,
                view = function(input)
                    return {
                        { type = "form", fields = { { key = "note", title = "Note", type = "text" } },
                          submit = { label = "Save", action = "note" } },
                    }
                end,
                action = function(input)
                    cairn.storage.set("note", input.values.note)
                    return { notice = "Saved", tone = "success", view = { { type = "text", text = cairn.storage.get("note") } } }
                end,
            }
        "#,
    })
}

#[tokio::test]
async fn third_party_packages_contribute_forms_commands_tools_and_views() {
    use axum::{Json, Router, http::HeaderMap, routing::get};
    let f = fixture().await;
    let s = &f.s;
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let recorded = seen.clone();
    let app = Router::new().route(
        "/v1/status/{service}",
        get(move |headers: HeaderMap| {
            let recorded = recorded.clone();
            async move {
                let auth = headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                recorded.lock().unwrap().push(auth);
                Json(json!({ "state": "up" }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    s.plugins
        .route_http_to(Some(format!("http://{}", listener.local_addr().unwrap())));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    registry_install_and_approve(s, status_board()).await;
    let invalid = common::save_plugin(
        s,
        "statusboard",
        &json!({ "name": "Main", "enabled": true, "settings": { "region": "mars" }, "secrets": { "apiKey": "k1" } }),
        None,
    )
    .await;
    assert!(invalid.is_err(), "choices come from the manifest");
    let board = common::save_plugin(
        s,
        "statusboard",
        &json!({
            "name": "Main", "enabled": true,
            "settings": { "labels": ["prod", "prod", "beta"] },
            "secrets": { "apiKey": "k1" },
            "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        board["settings"],
        json!({ "region": "eu", "labels": ["prod", "beta"] })
    );
    assert!(!board.to_string().contains("k1"));

    let output = run(
        &f,
        json!({ "plugin": "statusboard", "command": "status", "args": ["api"] }),
    )
    .await
    .unwrap();
    assert_eq!(output, json!({ "output": "text", "value": "api: up (eu)" }));
    assert_eq!(seen.lock().unwrap().as_slice(), ["Bearer k1"]);
    assert_eq!(
        run(
            &f,
            json!({ "plugin": "statusboard", "command": "status", "args": ["API!"] })
        )
        .await,
        Err(400)
    );

    let tool = agents::call_tool(
        s,
        &f.bearer,
        "statusboard_status",
        &json!({ "service": "db" }),
    )
    .await
    .unwrap();
    assert_eq!(tool, Some(json!({ "state": "up" })));
    assert!(
        agents::call_tool(s, &f.bearer, "statusboard_status", &json!({}))
            .await
            .is_err()
    );
    assert!(
        agents::instructions(s, MAIN_AGENT_ID)
            .await
            .unwrap()
            .contains("cairn plugin statusboard status")
    );

    let id = text(&board, "id");
    let view = panels::view(s, "statusboard", id).await.unwrap();
    assert_eq!(view["view"][0]["type"], "form");
    let result = common::plugin_action(
        s,
        "statusboard",
        id,
        &json!({ "action": "note", "values": { "note": "hello" } }),
    )
    .await
    .unwrap();
    assert_eq!(result["notice"], "Saved");
    assert_eq!(result["view"][0]["text"], "hello");
    server.abort();
}

#[tokio::test]
async fn live_form_completion_preserves_cleared_defaulted_fields() {
    let f = fixture().await;
    registry_install_and_approve(
        &f.s,
        json!({
            "manifest": {
                "id": "draftdefaults", "name": "Draft defaults", "version": "1.0.0",
                "apiVersion": 2, "capabilities": [], "instanceForm": true,
                "settings": [
                    { "key": "note", "title": "Note", "type": "string", "default": "Initial note" },
                    { "key": "limit", "title": "Limit", "type": "number", "default": 10 },
                ],
            },
            "source": "return { form = function(input) return { values = input.values } end }",
        }),
    )
    .await;

    for settings in [json!({}), json!({ "note": null, "limit": null })] {
        let completed =
            common::plugin_form(&f.s, "draftdefaults", &json!({ "settings": settings }))
                .await
                .unwrap();
        assert_eq!(completed["values"], json!({}));
    }
    let completed = common::plugin_form(
        &f.s,
        "draftdefaults",
        &json!({ "settings": { "note": "Edited", "limit": 0 } }),
    )
    .await
    .unwrap();
    assert_eq!(completed["values"], json!({ "note": "Edited", "limit": 0 }));
}

async fn registry_install_and_approve(s: &Service, package: Value) {
    use cairn_installation::plugins::registry;
    let installed = registry::install(s, &package).await.unwrap();
    let id = text(&installed["manifest"], "id").to_owned();
    let catalog = registry::catalog(s).await.unwrap();
    let entry = catalog["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id)
        .unwrap()
        .clone();
    registry::approve(s, &id, "1.0.0", text(&entry["versions"][0], "digest"))
        .await
        .unwrap();
}
