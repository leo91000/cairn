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

pub(super) struct Antigravity;

#[async_trait::async_trait]
impl ExecutionAdapter for Antigravity {
    fn binary<'a>(&self, config: &'a Config) -> &'a str {
        &config.antigravity_bin
    }

    fn set_binary(&self, config: &mut Config, binary: String) {
        config.antigravity_bin = binary;
    }

    fn state_directory(&self) -> &'static str {
        ".antigravity"
    }

    fn state_home(&self, directory: &Path, _isolated: bool) -> PathBuf {
        directory.join("home/.antigravity")
    }

    fn isolate_environment(&self, env: &mut Environment, home: &Path) {
        crate::antigravity::isolate(env, home);
    }

    /// Cairn's hook allows only reading during a plan turn; `agy` has no read-only
    /// sandbox, so plan turns have no shell.
    fn plan_turns(&self) -> bool {
        true
    }

    fn validate_agent(&self, agent: &Value) -> Result<()> {
        let model = text(agent, "model");
        if !model.is_empty() && !crate::antigravity::valid_slug(model) {
            return Err(Error::bad("Choose a supported Antigravity model."));
        }
        if crate::service::policy(agent)["sandbox"] != "yolo" {
            return Err(Error::bad(
                "Antigravity currently requires YOLO access in an isolated execution environment. It does not implement the read-only or workspace-write sandbox.",
            ));
        }
        Ok(())
    }

    fn chat_mcp(&self, mcp: &Value) -> Value {
        json!({
            "args": [],
            "antigravityMcp": mcp["antigravityMcp"],
            "antigravityPolicyMcp": mcp["antigravityPolicyMcp"],
        })
    }

    fn mcp_configuration(&self, configuration: &Configuration) -> Result<Value> {
        let mut servers = Map::new();
        let mut policy = Map::new();
        for (name, server) in &configuration.servers {
            let (mut value, allowed) = match server {
                Server::Http {
                    url,
                    timeout,
                    enabled_tools,
                } => (
                    json!({
                        "serverUrl": url,
                        "headers": configuration.bearer_headers(),
                        "timeoutSeconds": timeout.unwrap_or(180),
                    }),
                    enabled_tools,
                ),
                Server::Stdio {
                    command,
                    args,
                    env,
                    enabled_tools,
                    ..
                } => (
                    json!({
                        "command": command,
                        "args": args,
                        "env": env,
                    }),
                    enabled_tools,
                ),
            };
            if let Some(allowed) = allowed {
                value["enabledTools"] = json!(allowed);
            }
            servers.insert(name.clone(), value);
            policy.insert(name.clone(), json!(allowed));
        }
        Ok(json!({
            "args": [],
            "antigravityMcp": { "mcpServers": servers },
            "antigravityPolicyMcp": policy,
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
        private_dir(&home.join(".antigravity")).await
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
                "Connect an Antigravity account (Gemini API key, Google Cloud credentials or Google account) before starting this agent.",
            ));
        };
        let record = s.accounts.account(s, &account.account_id).await?;
        let kind = record.model_provider.as_deref().unwrap_or_default();
        let mut model = text(plan, "model").to_owned();
        // Each account kind has its own catalog: the leased account's default applies.
        if model.is_empty() {
            model = s
                .antigravity
                .default_model(s, kind)
                .await?
                .unwrap_or_default();
        }
        let slug = s
            .antigravity
            .resolve(s, kind, &model, text(plan, "reasoning"))
            .await?
            .ok_or_else(|| {
                Error::conflict(
                    "This Antigravity account does not offer the selected model or reasoning level. Choose another model or connect another account.",
                )
            })?;
        plan["antigravityModel"] = slug.into();
        Ok(())
    }

    async fn model_catalog(&self, s: &Service) -> Result<Value> {
        s.antigravity.models(s).await
    }

    async fn resume_session(
        &self,
        _s: &Service,
        run: &Value,
        _home: &Path,
        _cwd: &Path,
    ) -> Result<String> {
        let id = text(run, "sessionId");
        if crate::validation::uuid(id).is_err() {
            return Err(Error::conflict(
                "The saved Antigravity conversation is unavailable. Working files were preserved.",
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
        crate::antigravity::run(config, plan, events, stop).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn mcp_servers_keep_their_allowlists_natively_and_in_the_hook_policy() {
        let mut configuration = Configuration::new("token".into());
        configuration.servers.insert(
            "cairn_workspace".into(),
            Server::Http {
                url: "http://127.0.0.1/mcp".into(),
                timeout: None,
                enabled_tools: None,
            },
        );
        configuration.servers.insert(
            "files".into(),
            Server::Stdio {
                command: "files-mcp".into(),
                args: vec!["--stdio".into()],
                env: BTreeMap::from([("A".into(), "1".into())]),
                enabled_tools: Some(vec!["read".into()]),
                tools: vec!["read".into(), "write".into()],
            },
        );
        let rendered = Antigravity.mcp_configuration(&configuration).unwrap();
        let servers = &rendered["antigravityMcp"]["mcpServers"];
        assert_eq!(
            servers["cairn_workspace"]["serverUrl"],
            "http://127.0.0.1/mcp"
        );
        assert_eq!(
            servers["cairn_workspace"]["headers"]["Authorization"],
            "Bearer token"
        );
        assert!(servers["cairn_workspace"].get("enabledTools").is_none());
        assert_eq!(servers["files"]["command"], "files-mcp");
        assert_eq!(servers["files"]["enabledTools"], json!(["read"]));
        assert_eq!(
            rendered["antigravityPolicyMcp"]["cairn_workspace"],
            Value::Null
        );
        assert_eq!(rendered["antigravityPolicyMcp"]["files"], json!(["read"]));
    }

    #[test]
    fn agents_require_yolo_access_and_a_native_model_name() {
        let agent = |model: &str, sandbox: &str| json!({ "provider": "antigravity", "model": model, "access": { "sandbox": sandbox } });
        assert!(
            Antigravity
                .validate_agent(&agent("gemini-4-neon", "yolo"))
                .is_ok()
        );
        assert!(Antigravity.validate_agent(&agent("", "yolo")).is_ok());
        assert!(
            Antigravity
                .validate_agent(&agent("gemini 4", "yolo"))
                .is_err()
        );
        assert!(
            Antigravity
                .validate_agent(&agent("gemini-4-neon", "workspace-write"))
                .is_err()
        );
    }
}
