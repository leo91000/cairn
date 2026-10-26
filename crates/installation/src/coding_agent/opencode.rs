use super::{
    ExecutionAdapter,
    mcp::{Configuration, Server},
};
use crate::{
    accounts::Lease,
    config::Config,
    error::{Error, Result},
    process::Environment,
    service::Service,
    skills::private_dir,
    validation::text,
};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) struct Opencode;

#[async_trait::async_trait]
impl ExecutionAdapter for Opencode {
    fn binary<'a>(&self, config: &'a Config) -> &'a str {
        &config.opencode_bin
    }

    fn set_binary(&self, config: &mut Config, binary: String) {
        config.opencode_bin = binary;
    }

    fn state_directory(&self) -> &'static str {
        ".opencode"
    }

    fn state_home(&self, directory: &Path, _isolated: bool) -> PathBuf {
        directory.join("home/.opencode")
    }

    fn isolate_environment(&self, env: &mut Environment, home: &Path) {
        crate::opencode::isolate(env, home);
    }

    fn validate_agent(&self, agent: &Value) -> Result<()> {
        crate::opencode::model_id(text(agent, "model"))?;
        if crate::service::policy(agent)["sandbox"] != "yolo" {
            return Err(Error::bad(
                "OpenCode currently requires YOLO access in an isolated execution environment. It does not implement the read-only or workspace-write sandbox.",
            ));
        }
        Ok(())
    }

    fn chat_mcp(&self, mcp: &Value) -> Value {
        json!({
            "args": [],
            "opencodeConfig": mcp["opencodeConfig"],
        })
    }

    fn mcp_configuration(&self, configuration: &Configuration) -> Result<Value> {
        let mut servers = Map::new();
        let mut native_names = std::collections::HashSet::new();
        let mut permissions = Map::new();
        permissions.insert("*".into(), "allow".into());
        for (name, server) in &configuration.servers {
            if !native_names.insert(native_tool_name(name)) {
                return Err(Error::bad(
                    "OpenCode MCP server names must remain distinct after native normalization.",
                ));
            }
            let (value, allowed) = match server {
                Server::Http {
                    url,
                    timeout,
                    enabled_tools,
                } => (
                    json!({
                        "type": "remote",
                        "url": url,
                        "headers": configuration.bearer_headers(),
                        "oauth": false,
                        "enabled": true,
                        "timeout": timeout.unwrap_or(60) * 1000,
                    }),
                    enabled_tools,
                ),
                Server::Stdio {
                    command,
                    args,
                    env,
                    enabled_tools,
                    tools,
                } => {
                    let mut names = std::collections::HashSet::new();
                    if tools
                        .iter()
                        .any(|tool| !names.insert(native_tool_name(tool)))
                    {
                        return Err(Error::bad(
                            "OpenCode MCP tool names must remain distinct after native normalization.",
                        ));
                    }
                    let mut command_line = vec![command.clone()];
                    command_line.extend(args.iter().cloned());
                    (
                        json!({
                            "type": "local",
                            "command": command_line,
                            "environment": env,
                            "enabled": true,
                        }),
                        enabled_tools,
                    )
                }
            };
            servers.insert(name.clone(), value);
            if let Some(allowed) = allowed {
                let native_name = native_tool_name(name);
                permissions.insert(format!("{native_name}_*"), "deny".into());
                for tool in allowed {
                    let native_tool = native_tool_name(tool);
                    permissions.insert(format!("{native_name}_{native_tool}"), "allow".into());
                }
            }
        }
        Ok(json!({
            "args": [],
            "opencodeConfig": { "mcp": servers, "permission": permissions },
        }))
    }

    fn refresh_state_on_resume(&self) -> bool {
        true
    }

    async fn prepare_isolated_home(
        &self,
        _config: &Config,
        home: &Path,
        _account_home: Option<&Path>,
    ) -> Result<()> {
        private_dir(&home.join(".opencode")).await
    }

    async fn prepare_home(
        &self,
        _s: &Service,
        home: &Path,
        _isolated: bool,
        _account: Option<&mut Lease>,
    ) -> Result<()> {
        private_dir(home).await
    }

    async fn rotate_account(
        &self,
        s: &Service,
        account: &mut Lease,
        previous_home: &Path,
        _isolated: bool,
    ) -> Result<()> {
        s.accounts.relocate(s, account, previous_home).await
    }

    async fn configure_plan(
        &self,
        s: &Service,
        plan: &mut Value,
        account: Option<&Lease>,
    ) -> Result<()> {
        let Some(account) = account else {
            return Err(Error::bad(
                "Connect an OpenCode model-provider API key before starting this agent.",
            ));
        };
        let selected = text(plan, "model").to_owned();
        let record = s.accounts.account(s, &account.account_id).await?;
        let field = |name: &str| {
            record
                .extra
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let (name, via) = if selected.starts_with(crate::opencode::AUTO) {
            let route = s
                .opencode
                .route(s, &selected, &field("modelProvider"))
                .await?
                .ok_or_else(|| {
                    Error::conflict(
                        "This OpenCode connection no longer offers the selected model. Choose another model or connection.",
                    )
                })?;
            plan["model"] = route["model"].clone();
            // A fallback route may not offer the saved level; it then uses its default.
            let reasoning = text(plan, "reasoning");
            if !reasoning.is_empty()
                && !route["supportedReasoningEfforts"]
                    .as_array()
                    .is_some_and(|e| e.iter().any(|e| e["reasoningEffort"] == reasoning))
            {
                plan["reasoning"] = "".into();
            }
            (
                text(&route, "displayName").to_owned(),
                text(&route, "providerName").to_owned(),
            )
        } else {
            let name = s
                .opencode
                .display_name(&selected)
                .unwrap_or(selected.clone());
            (name, field("modelProviderName"))
        };
        // Conversations label the replies that follow with the provider that wrote them.
        let payload = json!({ "route": { "model": plan["model"], "providerName": via } });
        s.store
            .event(
                text(plan, "runId"),
                "status",
                &format!("{name} via {via}"),
                Some(payload),
            )
            .await?;
        plan["opencodeManagedAuth"] = true.into();
        Ok(())
    }

    async fn model_catalog(&self, s: &Service) -> Result<Value> {
        s.opencode.models(s).await
    }

    async fn resume_session(
        &self,
        _s: &Service,
        run: &Value,
        _home: &Path,
        _cwd: &Path,
    ) -> Result<String> {
        let id = text(run, "sessionId");
        if !id.starts_with("ses_") || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(Error::conflict(
                "The saved OpenCode session is unavailable. Working files were preserved.",
            ));
        }
        Ok(id.into())
    }

    async fn run(
        &self,
        config: &Config,
        plan: Value,
        events: mpsc::Sender<Value>,
        stop: CancellationToken,
    ) -> Result<()> {
        crate::opencode::run(config, plan, events, stop).await
    }
}

fn native_tool_name(value: &str) -> String {
    value
        .encode_utf16()
        .map(|unit| {
            // OpenCode's non-Unicode JavaScript regex replaces each UTF-16 unit.
            char::from_u32(u32::from(unit))
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .unwrap_or('_')
        })
        .collect()
}
