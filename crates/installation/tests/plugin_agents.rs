//! Plugin API v2 end to end, through the built-in 1Password package: an ordinary
//! package with a secret setting, agent grants, commands, a tool, instructions, a
//! declared program, a view, actions and a test.
mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    config::{MAIN_AGENT_ID, id},
    plugins::{agents, connections, legacy, panels},
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
[ \"$OP_CACHE\" = false ] || exit 3\n\
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
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let program = root.path().join("op");
    write_program(&program, OP);
    s.plugins.use_program("onepassword", "op", program.clone());
    let task = s
        .task(
            json!({ "name": "Probe", "agentId": MAIN_AGENT_ID, "prompt": "Hello" }),
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
    let config = s.mcps.run_configuration(&s, &run).await.unwrap();
    let bearer = text(&config["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
    Fixture {
        root,
        s,
        bearer,
        program,
    }
}

fn write_program(path: &PathBuf, script: &str) {
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

async fn save(s: &Service, input: Value, existing: Option<&str>) -> Value {
    connections::save(s, "onepassword", &input, existing)
        .await
        .unwrap()
}

fn command(name: &str, args: &[&str]) -> Value {
    json!({ "plugin": "onepassword", "command": name, "args": args })
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
    let mut update = json!({ "name": "Production", "enabled": true, "agentIds": [MAIN_AGENT_ID] });
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
    update["agentIds"] = json!([]);
    save(s, update.clone(), Some(&account)).await;
    assert_eq!(run(&f, read.clone()).await, Err(403));

    update["agentIds"] = json!([MAIN_AGENT_ID]);
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

    let temporary = s.agent(json!({ "name": "Temporary" }), None).await.unwrap();
    update["agentIds"] = json!([MAIN_AGENT_ID, temporary["id"]]);
    update.as_object_mut().unwrap().remove("secrets");
    save(s, update.clone(), Some(&account)).await;
    s.remove("agents", text(&temporary, "id")).await.unwrap();
    assert_eq!(
        s.get(connections::KIND, &account).await.unwrap()["agentIds"],
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
    let error = connections::save(
        s,
        "onepassword",
        &json!({ "name": "Bad", "enabled": true, "secrets": { "token": "private-invalid-token" } }),
        None,
    )
    .await
    .unwrap_err();
    assert!(!error.message.contains("private-invalid-token"));
    let unknown_agent = connections::save(
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
    let result = panels::action(s, "onepassword", account, &json!({ "action": "vaults" }))
        .await
        .unwrap();
    assert_eq!(
        result["view"][0]["children"][0]["items"][0]["title"],
        "Production"
    );
    assert!(
        panels::action(s, "onepassword", account, &json!({ "action": "Bad name" }))
            .await
            .is_err()
    );
    let test = panels::test(s, "onepassword", account).await.unwrap();
    assert_eq!(test["message"], "Connected · 1 vault");
    write_program(&f.program, "#!/bin/sh\nexit 1\n");
    assert!(
        panels::test(s, "onepassword", account)
            .await
            .unwrap_err()
            .message
            .contains("1Password request failed")
    );
}

#[tokio::test]
async fn legacy_service_accounts_move_into_instances_once() {
    let f = fixture().await;
    let s = &f.s;
    let account = id();
    s.store
        .put(
            "onepassword",
            json!({
                "id": account, "name": "Legacy", "enabled": true,
                "agentIds": [MAIN_AGENT_ID], "updatedAt": 1,
            }),
        )
        .await
        .unwrap();
    let encrypted = s
        .vault
        .encrypt(
            &format!("onepassword:{account}"),
            &json!({ "token": "ops_fixture" }),
        )
        .unwrap();
    s.store
        .set(
            &format!("mcp-secret:onepassword:{account}"),
            encrypted,
            None,
        )
        .await
        .unwrap();
    legacy::migrate(s).await.unwrap();
    legacy::migrate(s).await.unwrap();
    assert!(s.store.list("onepassword").await.unwrap().is_empty());
    let instance = s.get(connections::KIND, &account).await.unwrap();
    assert_eq!(instance["pluginId"], "onepassword");
    assert_eq!(instance["name"], "Legacy");
    assert_eq!(instance["agentIds"], json!([MAIN_AGENT_ID]));
    // Agents keep their account IDs and the token still reaches op.
    assert_eq!(
        run(
            &f,
            command("read", &["--account", &account, "op://vault/item/password"])
        )
        .await
        .unwrap()["value"],
        "secret-with-no-newline"
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

    let saved = connections::save(
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

    // Simulate a previously admitted manifest on disk. Execution refuses this
    // package; the owner's catalogue retains its exact validation diagnostic.
    let manifest_path = f
        .root
        .path()
        .join("data/plugins/statusboard/1.0.0/manifest.json");
    package["manifest"]["settings"][0]["maxLength"] = json!(10001);
    let legacy_bytes = serde_json::to_vec_pretty(&package["manifest"]).unwrap();
    std::fs::write(&manifest_path, &legacy_bytes).unwrap();
    let error = registry::active(s, "statusboard").await.err().unwrap();
    assert_eq!(error.status, 403);
    assert!(error.to_string().contains("Review it in Plugins"));
    let catalog = registry::catalog(s).await.unwrap();
    let installed = catalog["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["id"] == "statusboard")
        .unwrap();
    assert_eq!(installed["status"], "invalid");
    let version = installed["versions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|version| version["version"] == "1.0.0")
        .unwrap();
    assert!(version["error"].as_str().unwrap().contains("10000"));
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
    let updated = connections::save(
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
    let invalid = connections::save(
        s,
        "statusboard",
        &json!({ "name": "Main", "enabled": true, "settings": { "region": "mars" }, "secrets": { "apiKey": "k1" } }),
        None,
    )
    .await;
    assert!(invalid.is_err(), "choices come from the manifest");
    let board = connections::save(
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
    let result = panels::action(
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
