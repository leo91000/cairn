use super::{
    ExecutionAdapter,
    mcp::{Configuration, Server},
};
use crate::{
    accounts::Lease,
    chat_process,
    config::Config,
    error::{Error, Result},
    execution,
    process::Environment,
    rpc::Session,
    service::Service,
    skills::{atomic_write, private_dir},
    validation::text,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) struct Codex;

#[async_trait::async_trait]
impl ExecutionAdapter for Codex {
    fn binary<'a>(&self, config: &'a Config) -> &'a str {
        &config.codex_bin
    }

    fn set_binary(&self, config: &mut Config, binary: String) {
        config.codex_bin = binary;
    }

    fn state_directory(&self) -> &'static str {
        ".codex"
    }

    fn state_home(&self, directory: &Path, isolated: bool) -> PathBuf {
        directory.join(if isolated { "home/.codex" } else { "codex" })
    }

    fn isolate_environment(&self, env: &mut Environment, home: &Path) {
        env.insert("CODEX_HOME".into(), home.to_string_lossy().into_owned());
        for key in ["OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_THREAD_ID"] {
            env.remove(key);
        }
    }

    fn validate_agent(&self, _agent: &Value) -> Result<()> {
        Ok(())
    }

    fn refresh_state_on_resume(&self) -> bool {
        false
    }

    async fn prepare_isolated_home(
        &self,
        config: &Config,
        home: &Path,
        account_home: Option<&Path>,
    ) -> Result<()> {
        private_dir(&home.join(".codex")).await?;
        install_auth(home, config, account_home).await
    }

    async fn prepare_home(
        &self,
        s: &Service,
        home: &Path,
        isolated: bool,
        account: Option<&mut Lease>,
    ) -> Result<()> {
        private_dir(home).await?;
        if isolated {
            if let Some(account) = account {
                s.accounts.relocate(s, account, home).await?;
            }
        } else {
            execution::codex_home(&s.config, home).await?;
        }
        Ok(())
    }

    async fn rotate_account(
        &self,
        s: &Service,
        account: &mut Lease,
        previous_home: &Path,
        isolated: bool,
    ) -> Result<()> {
        if isolated {
            s.accounts.relocate(s, account, previous_home).await
        } else {
            execution::codex_home(&s.config, &account.home).await
        }
    }

    async fn configure_plan(
        &self,
        s: &Service,
        plan: &mut Value,
        account: Option<&Lease>,
    ) -> Result<()> {
        if text(plan, "reasoning").is_empty() {
            let source = account.map_or("", |account| account.account_id.as_str());
            if let Some((model, effort)) =
                crate::models::cached_defaults(s, source, text(plan, "model")).await?
            {
                plan["model"] = model.into();
                plan["reasoning"] = effort.into();
            }
        }
        Ok(())
    }

    async fn model_catalog(&self, s: &Service) -> Result<Value> {
        s.models.list(s).await
    }

    async fn resume_session(
        &self,
        s: &Service,
        run: &Value,
        home: &Path,
        cwd: &Path,
    ) -> Result<String> {
        // The retained guest disk is authoritative; its thread/resume verifies the session.
        if (run["isolated"] == true || !s.config.runner_url.is_empty())
            && let Some(id) = run["sessionId"].as_str().filter(|id| !id.is_empty())
        {
            return Ok(id.to_owned());
        }
        let result = async {
            let mut rpc = Session::codex(&s.config, home, &[], None).await?;
            let result = find_session(&mut rpc, run, cwd).await;
            rpc.close().await;
            result
        }
        .await;
        result.map_err(|error| {
            if crate::run_retry::startup_failure(&error).is_some() {
                return error;
            }
            Error::conflict("The saved Codex conversation is unavailable. Working files were preserved; review them before starting a new run.")
        })
    }

    async fn run(
        &self,
        config: &Config,
        plan: Value,
        events: mpsc::Sender<Value>,
        stop: CancellationToken,
    ) -> Result<()> {
        if let Ok(socket) = std::env::var("CAIRN_CODEX_SERVICE") {
            return chat_process::resident::run(Path::new(&socket), plan, events, stop).await;
        }
        let home = std::env::var("CODEX_HOME").map_err(|_| Error::bad("Missing Codex home."))?;
        chat_process::run(config, Path::new(&home), plan, events, stop).await
    }

    fn chat_mcp(&self, mcp: &Value) -> Value {
        // MCP access belongs to the thread lease, not the resident CLI arguments.
        json!({
            "args": [],
            "codexConfig": { "mcp_servers": mcp["codexConfig"] },
        })
    }

    fn mcp_configuration(&self, configuration: &Configuration) -> Result<Value> {
        let mut servers = BTreeMap::new();
        for (name, server) in &configuration.servers {
            let value = match server {
                Server::Http {
                    url,
                    timeout,
                    enabled_tools,
                } => {
                    let mut value = json!({
                        "url": url,
                        "http_headers": configuration.bearer_headers(),
                    });
                    if let Some(timeout) = timeout {
                        value["tool_timeout_sec"] = (*timeout).into();
                    }
                    if let Some(tools) = enabled_tools {
                        value["enabled_tools"] = json!(tools);
                    }
                    value
                }
                Server::Stdio {
                    command,
                    args,
                    env,
                    enabled_tools,
                    ..
                } => {
                    let mut value = json!({
                        "command": command,
                        "args": args,
                        "env": env,
                    });
                    if let Some(tools) = enabled_tools {
                        value["enabled_tools"] = json!(tools);
                    }
                    value
                }
            };
            servers.insert(name, value);
        }
        Ok(json!({ "codexConfig": servers }))
    }
}

async fn install_auth(home: &Path, config: &Config, codex_home: Option<&Path>) -> Result<()> {
    let auth = codex_home
        .map_or_else(|| config.home.join(".codex"), Path::to_owned)
        .join("auth.json");
    let managed = auth
        .parent()
        .is_some_and(|p| p.join("cairn-managed-auth").exists());
    if managed {
        atomic_write(&home.join(".codex/cairn-managed-auth"), b"1").await?;
    } else {
        let bytes = tokio::fs::read(&auth)
            .await
            .map_err(|_| Error::bad("Connect Codex before starting an isolated agent."))?;
        atomic_write(&home.join(".codex/auth.json"), &bytes).await?;
    }
    atomic_write(
        &home.join(".codex/config.toml"),
        b"cli_auth_credentials_store = \"file\"\n",
    )
    .await
}

async fn find_session(rpc: &mut Session, run: &Value, cwd: &Path) -> Result<String> {
    if let Some(id) = run["sessionId"].as_str() {
        let params = json!({ "threadId": id, "includeTurns": false });
        let result = rpc.request("thread/read", params).await?;
        if result["thread"]["id"] == id {
            return Ok(id.to_owned());
        }
        return Err(Error::bad("Session mismatch"));
    }
    let params = json!({
        "limit": 2,
        "cwd": cwd,
        "sourceKinds": ["appServer"],
        "archived": false,
    });
    let result = rpc.request("thread/list", params).await?;
    let cwd = cwd.to_string_lossy();
    let matches = result["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["cwd"] == cwd.as_ref() && t["parentThreadId"].is_null())
        .collect::<Vec<_>>();
    if let [thread] = matches.as_slice()
        && !text(thread, "id").is_empty()
    {
        return Ok(text(thread, "id").to_owned());
    }
    Err(Error::bad("No unique session"))
}
