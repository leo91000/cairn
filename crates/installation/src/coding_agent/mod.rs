//! Native execution differences between coding agents. Account selection, workspace
//! ownership and execution lifecycle stay in their existing shared services.
mod claude;
mod codex;
pub mod mcp;
mod opencode;

pub(crate) use codex::args_for_run as codex_args;

use crate::{
    accounts::Lease, config::Config, error::Result, process::Environment, provider::Provider,
    service::Service,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The native CLI configuration and session protocol for one coding agent.
/// Credentials and usage remain the responsibility of `accounts::Driver`.
#[async_trait::async_trait]
pub trait ExecutionAdapter: Send + Sync {
    fn binary<'a>(&self, config: &'a Config) -> &'a str;

    fn set_binary(&self, config: &mut Config, binary: String);

    fn state_directory(&self) -> &'static str;

    fn state_home(&self, directory: &Path, isolated: bool) -> PathBuf;

    /// Isolates state and inherited credentials even when this CLI is invoked as a tool.
    fn isolate_environment(&self, env: &mut Environment, home: &Path);

    fn configure_environment(&self, env: &mut Environment, home: &Path) {
        self.isolate_environment(env, home);
    }

    fn uses_chat(&self, chat_execution: bool, managed_account: bool) -> bool;

    /// Whether plan turns can run on this engine: Cairn must be able to enforce
    /// their read-only boundary, not only ask the model to respect it.
    fn plan_turns(&self) -> bool {
        false
    }

    fn legacy_args(
        &self,
        run: &Value,
        mcp: &Value,
        output: &str,
        session: Option<&str>,
    ) -> Vec<String>;

    fn validate_agent(&self, agent: &Value) -> Result<()>;

    fn chat_mcp(&self, mcp: &Value) -> Value;

    fn mcp_configuration(&self, configuration: &mcp::Configuration) -> Result<Value>;

    /// Whether access-only credential files need reimporting on a retained disk.
    fn refresh_state_on_resume(&self) -> bool;

    async fn prepare_isolated_home(
        &self,
        config: &Config,
        home: &Path,
        account_home: Option<&Path>,
    ) -> Result<()>;

    async fn prepare_home(
        &self,
        s: &Service,
        home: &Path,
        isolated: bool,
        account: Option<&mut Lease>,
    ) -> Result<()>;

    async fn rotate_account(
        &self,
        s: &Service,
        account: &mut Lease,
        previous_home: &Path,
        isolated: bool,
    ) -> Result<()>;

    async fn configure_plan(
        &self,
        s: &Service,
        plan: &mut Value,
        account: Option<&Lease>,
    ) -> Result<()>;

    async fn model_catalog(&self, s: &Service) -> Result<Value>;

    async fn resume_session(
        &self,
        s: &Service,
        run: &Value,
        home: &Path,
        cwd: &Path,
    ) -> Result<String>;

    async fn run(
        &self,
        config: &Config,
        plan: Value,
        events: mpsc::Sender<Value>,
        stop: CancellationToken,
    ) -> Result<()>;
}

impl Provider {
    /// Keep every installed coding agent's inherited credentials out of the run.
    /// Other native CLIs invoked as tools must also use this run's private state.
    pub fn configure_run_environment(
        self,
        env: &mut Environment,
        directory: &Path,
        isolated: bool,
        active_home: &Path,
    ) {
        for provider in Self::ALL {
            let adapter = provider.adapter();
            adapter.isolate_environment(env, &adapter.state_home(directory, isolated));
        }
        self.adapter().configure_environment(env, active_home);
    }

    pub fn adapter(self) -> &'static dyn ExecutionAdapter {
        match self {
            Self::Codex => &codex::Codex,
            Self::Claude => &claude::Claude,
            Self::Opencode => &opencode::Opencode,
        }
    }
}
