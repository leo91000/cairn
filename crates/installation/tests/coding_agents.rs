mod common;

use cairn_installation::{
    coding_agent::mcp::{Configuration, Server},
    provider::Provider,
    run_output,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tempfile::TempDir;

#[test]
fn adapters_preserve_native_mcp_permissions_and_thread_credentials() {
    let mut configuration = Configuration::new("synthetic-run-token".into());
    configuration.servers.insert(
        "cairn_workspace".into(),
        Server::Http {
            url: "http://127.0.0.1:5202/mcp-workspace".into(),
            timeout: Some(3660),
            enabled_tools: None,
        },
    );
    configuration.servers.insert(
        "cairn_selected".into(),
        Server::Stdio {
            command: "fixture-tool".into(),
            args: vec!["--fixture".into()],
            env: BTreeMap::from([("FIXTURE".into(), "synthetic-tool-secret".into())]),
            enabled_tools: Some(vec!["read".into()]),
            tools: vec!["read".into(), "write".into()],
        },
    );
    for provider in Provider::ALL {
        let native = provider
            .adapter()
            .mcp_configuration(&configuration)
            .unwrap();
        let chat = provider.adapter().chat_mcp(&native);
        match provider {
            Provider::Codex => {
                assert!(native.get("claudeMcps").is_none());
                let workspace = &chat["codexConfig"]["mcp_servers"]["cairn_workspace"];
                assert_eq!(workspace["tool_timeout_sec"], 3660);
                assert_eq!(
                    workspace["http_headers"]["Authorization"],
                    "Bearer synthetic-run-token"
                );
                assert!(workspace.get("bearer_token_env_var").is_none());
                assert_eq!(
                    chat["codexConfig"]["mcp_servers"]["cairn_selected"]["enabled_tools"],
                    json!(["read"])
                );
                assert_eq!(chat["args"], json!([]));
                let args = native["args"].as_array().unwrap();
                assert!(
                    args.iter()
                        .filter_map(Value::as_str)
                        .any(|arg| arg.contains("bearer_token_env_var"))
                );
                assert!(
                    !args
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|arg| arg.contains("synthetic-run-token"))
                );
            }
            Provider::Claude => {
                assert!(native.get("codexConfig").is_none());
                let workspace = &chat["claudeMcps"]["mcpServers"]["cairn_workspace"];
                assert_eq!(workspace["type"], "http");
                assert_eq!(
                    workspace["headers"]["Authorization"],
                    "Bearer synthetic-run-token"
                );
                assert_eq!(
                    chat["claudeDeniedTools"],
                    json!(["mcp__cairn_selected__write"])
                );
                assert_eq!(
                    chat["claudeMcps"]["mcpServers"]["cairn_selected"]["env"]["FIXTURE"],
                    "synthetic-tool-secret"
                );
            }
        }
    }
}

#[test]
fn malformed_engine_selection_cannot_produce_a_launch_plan() {
    let root = TempDir::new().unwrap();
    let prepared = json!({ "output": root.path().join("output/result.md") });
    for value in [json!("opencode"), json!(""), json!(null), json!(42)] {
        let run = json!({ "snapshot": { "agent": { "provider": value } } });
        assert!(run_output::chat_plan(&run, &prepared, root.path(), &json!({}), None).is_err());
    }
}

#[test]
fn engine_configuration_and_validation_do_not_depend_on_the_other_engine() {
    let root = TempDir::new().unwrap();
    let config = common::config(root.path());
    for provider in Provider::ALL {
        let adapter = provider.adapter();
        let mut isolated_config = config.clone();
        adapter.set_binary(&mut isolated_config, "fixture-native-cli".into());
        assert_eq!(adapter.binary(&isolated_config), "fixture-native-cli");
        let home = adapter.state_home(root.path(), true);
        let mut environment = cairn_installation::process::Environment::from([
            ("OPENAI_API_KEY".into(), "synthetic-openai-key".into()),
            ("ANTHROPIC_API_KEY".into(), "synthetic-anthropic-key".into()),
        ]);
        provider.configure_run_environment(&mut environment, root.path(), true, &home);
        assert!(!environment.contains_key("OPENAI_API_KEY"));
        assert!(!environment.contains_key("ANTHROPIC_API_KEY"));
        match provider {
            Provider::Codex => {
                assert!(!environment.contains_key("OPENAI_API_KEY"));
                assert_eq!(environment["CODEX_HOME"], home.to_string_lossy());
                assert!(!environment.contains_key("BROWSER"));
                assert!(!environment.contains_key("DISABLE_AUTOUPDATER"));
                assert!(!adapter.uses_chat(false, false));
                assert!(adapter.uses_chat(false, true));
                assert!(adapter.uses_chat(true, false));
            }
            Provider::Claude => {
                assert!(!environment.contains_key("ANTHROPIC_API_KEY"));
                assert_eq!(environment["CLAUDE_CONFIG_DIR"], home.to_string_lossy());
                assert_eq!(environment["BROWSER"], "true");
                assert_eq!(environment["DISABLE_AUTOUPDATER"], "1");
                assert!(adapter.uses_chat(false, false));
                assert!(
                    adapter
                        .validate_agent(&json!({ "provider": "claude", "reasoning": "ultra" }))
                        .is_err()
                );
                assert_eq!(
                    environment["CODEX_HOME"],
                    root.path().join("home/.codex").to_string_lossy()
                );
            }
        }
    }
}
