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
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) struct Claude;

#[async_trait::async_trait]
impl ExecutionAdapter for Claude {
    fn binary<'a>(&self, config: &'a Config) -> &'a str {
        &config.claude_bin
    }

    fn set_binary(&self, config: &mut Config, binary: String) {
        config.claude_bin = binary;
    }

    fn state_directory(&self) -> &'static str {
        ".claude"
    }

    fn state_home(&self, directory: &Path, _isolated: bool) -> PathBuf {
        directory.join("home/.claude")
    }

    fn isolate_environment(&self, env: &mut Environment, home: &Path) {
        crate::claude::sanitize(env);
        env.insert(
            "CLAUDE_CONFIG_DIR".into(),
            home.to_string_lossy().into_owned(),
        );
    }

    fn configure_environment(&self, env: &mut Environment, home: &Path) {
        crate::claude::configure(env, home);
    }

    fn validate_agent(&self, agent: &Value) -> Result<()> {
        crate::claude::validate_agent(agent)
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
        private_dir(&home.join(".claude")).await
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
        _s: &Service,
        plan: &mut Value,
        account: Option<&Lease>,
    ) -> Result<()> {
        plan["claudeManagedAuth"] = account.is_some().into();
        Ok(())
    }

    async fn model_catalog(&self, s: &Service) -> Result<Value> {
        crate::claude::model_catalog(s).await
    }

    async fn resume_session(
        &self,
        _s: &Service,
        run: &Value,
        _home: &Path,
        _cwd: &Path,
    ) -> Result<String> {
        run["sessionId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::conflict(
                    "The saved Claude session is unavailable. Working files were preserved.",
                )
            })
    }

    async fn run(
        &self,
        config: &Config,
        plan: Value,
        events: mpsc::Sender<Value>,
        stop: CancellationToken,
    ) -> Result<()> {
        crate::claude_process::run(config, plan, events, stop).await
    }

    fn chat_mcp(&self, mcp: &Value) -> Value {
        json!({
            "args": mcp["args"],
            "claudeMcps": mcp["claudeMcps"],
            "claudeDeniedTools": mcp["claudeDeniedTools"],
        })
    }

    fn mcp_configuration(&self, configuration: &Configuration) -> Result<Value> {
        let mut servers = BTreeMap::new();
        let mut denied_tools = Vec::new();
        for (name, server) in &configuration.servers {
            let value = match server {
                Server::Http { url, .. } => json!({
                    "type": "http",
                    "url": url,
                    "headers": configuration.bearer_headers(),
                }),
                Server::Stdio {
                    command,
                    args,
                    env,
                    enabled_tools,
                    tools,
                } => {
                    if let Some(selected) = enabled_tools {
                        for tool in tools {
                            if !selected.contains(tool) {
                                denied_tools.push(format!("mcp__{name}__{tool}"));
                            }
                        }
                    }
                    json!({
                        "type": "stdio",
                        "command": command,
                        "args": args,
                        "env": env,
                    })
                }
            };
            servers.insert(name, value);
        }
        Ok(json!({
            "args": [],
            "claudeMcps": { "mcpServers": servers },
            "claudeDeniedTools": denied_tools,
        }))
    }
}
