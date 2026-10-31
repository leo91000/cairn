pub mod channel;
pub mod record;
pub mod sql;

use crate::{
    auth::{hex_digest, token},
    config::{id, now},
    error::{Error, Result, required},
    run_status::RunStatus,
    service::{Service, allowed, covers, policy},
    store::Db,
    validation::text,
};
use cairn_contracts::{McpInput, McpView};
use record::{AuthKind, ConnectionState, GrantScope, McpSecrets, McpServer, RunGrant};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use tokio::sync::Mutex;

const NOT_FOUND: &str = "MCP connection not found.";
pub(crate) const RUN_TOKEN_ENV: &str = "CAIRN_MCP_RUN_TOKEN";

/// Environment variables a command server may not override.
const RESERVED_ENV: [&str; 6] = [
    "HOME",
    "CODEX_HOME",
    "PATH",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "NODE_OPTIONS",
];

/// Failures whose message is safe and useful to show as the connection error.
const KNOWN_FAILURES: [&str; 3] = [
    "Private network access is disabled for this connection.",
    "The endpoint redirects. Configure its final URL.",
    "Instance metadata endpoints are unavailable.",
];

#[derive(Default)]
pub struct Mcps {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

pub async fn callback_url(s: &Service) -> Result<String> {
    let (origin, installation) = crate::relay::beacon_address(&s.config.data_dir)
        .await?
        .ok_or_else(|| {
            Error::unavailable("Claim this installation before connecting an MCP server.")
        })?;
    Ok(format!(
        "{origin}/installations/{installation}/mcps/callback"
    ))
}

pub(crate) fn grant_key(bearer: &str) -> String {
    hex_digest(bearer)
}

/// The run a bearer was granted to, if its grant still exists.
pub(crate) async fn granted_run(s: &Service, bearer: &str) -> Result<Option<String>> {
    let Some(grant) = s.store.mcp_run_grant(&grant_key(bearer)).await? else {
        return Ok(None);
    };
    Ok(Some(grant.run_id))
}

/// Where the agent of `run` reaches run-scoped MCP. Inside a VM, only its
/// relayed loopback origin is reachable; a host execution uses the manager's.
fn agent_origin<'a>(s: &'a Service, run: &Value) -> &'a str {
    if crate::execution::uses_vm(run, &s.config) {
        crate::microvm::mcp::ORIGIN
    } else {
        &s.config.public_url
    }
}

/// Drops OAuth authorizations started for this connection.
pub(crate) fn cancel_pending(db: &Db<'_>, id: &str) -> Result<()> {
    db.cancel_mcp_oauth(id)
}

fn reserved_env(key: &str) -> bool {
    RESERVED_ENV.contains(&key) || key.starts_with("CAIRN_") || key.starts_with("RUNNER_")
}

fn failure_message(error: &Error) -> &str {
    if KNOWN_FAILURES.contains(&error.message.as_str()) {
        &error.message
    } else if error.is_unauthorized() {
        "Sign in to connect this server."
    } else {
        "Could not connect. Check the endpoint, credentials, and server availability."
    }
}

fn validate_credentials(server: &McpServer, secrets: &McpSecrets) -> Result<()> {
    if server.auth == AuthKind::Bearer && secrets.token().is_empty() {
        return Err(Error::bad("Enter a bearer token."));
    }
    let overrides_runtime = secrets
        .env
        .as_ref()
        .is_some_and(|env| env.keys().any(|key| reserved_env(key)));
    if !server.is_http() && overrides_runtime {
        return Err(Error::bad(
            "Environment variables cannot override the agent runtime or home.",
        ));
    }
    Ok(())
}

/// Authorized MCP servers and shared secrets for one execution.
struct RunConfiguration {
    native: crate::coding_agent::mcp::Configuration,
    env: BTreeMap<&'static str, String>,
    redactions: Vec<String>,
}

fn server_name(server: &McpServer) -> String {
    format!("cairn_{}", server.id.replace('-', "_"))
}

impl RunConfiguration {
    fn new(token: String) -> Self {
        Self {
            native: crate::coding_agent::mcp::Configuration::new(token.clone()),
            env: BTreeMap::new(),
            redactions: vec![token],
        }
    }

    /// HTTP servers are reached through this server's gateway with the run token.
    fn add_http(&mut self, origin: &str, server: &McpServer, tools: Option<Vec<String>>) {
        self.native.servers.insert(
            server_name(server),
            crate::coding_agent::mcp::Server::Http {
                url: format!("{origin}/mcp-gateway/{}", server.id),
                timeout: None,
                enabled_tools: tools,
            },
        );
    }

    /// Command servers run inside the task environment with their own secrets.
    fn add_stdio(
        &mut self,
        server: &McpServer,
        tools: Option<Vec<String>>,
        env: BTreeMap<String, String>,
    ) {
        self.native.servers.insert(
            server_name(server),
            crate::coding_agent::mcp::Server::Stdio {
                command: server.command.clone(),
                args: server.args.clone(),
                env,
                enabled_tools: tools,
                tools: server
                    .tools
                    .iter()
                    .map(|tool| text(tool, "name").to_owned())
                    .collect(),
            },
        );
    }

    fn add_workspace(&mut self, origin: &str) {
        self.add_internal(origin, "cairn_workspace", "/mcp-workspace", Some(3660));
    }

    fn add_internal(&mut self, origin: &str, name: &str, path: &str, timeout: Option<u64>) {
        self.native.servers.insert(
            name.into(),
            crate::coding_agent::mcp::Server::Http {
                url: format!("{origin}{path}"),
                timeout,
                enabled_tools: None,
            },
        );
    }
}

/// Tools exposed to the agent: its per-connection selection, restricted to the
/// connection's enabled tools. `None` exposes everything.
fn selected_tools(server: &McpServer, selection: &Value) -> Option<Vec<String>> {
    let Some(selected) = selection.as_array() else {
        return server.enabled_tools.clone();
    };
    let enabled = |name: &str| {
        server
            .enabled_tools
            .as_ref()
            .is_none_or(|tools| tools.iter().any(|tool| tool == name))
    };
    Some(
        selected
            .iter()
            .filter_map(Value::as_str)
            .filter(|name| enabled(name))
            .map(str::to_owned)
            .collect(),
    )
}

/// Removes a deleted connection from every agent's access policy. Other settings,
/// including live grants such as 1Password, are kept as saved.
fn revoke_agent_access(db: &Db<'_>, id: &str) -> Result<()> {
    for mut agent in db.agents()? {
        let mut changed = false;
        if let Some(mcps) = &mut agent.settings.access.mcps {
            let before = mcps.len();
            mcps.retain(|mcp| mcp != id);
            changed |= before != mcps.len();
        }
        changed |= agent.settings.access.mcp_tools.remove(id).is_some();
        if changed {
            agent.revision = crate::config::id();
            db.save_agent(&agent)?;
        }
    }
    Ok(())
}

fn record_failure(server: &mut McpServer, error: &Error) {
    server.state = if error.is_unauthorized() {
        ConnectionState::NeedsAuth
    } else {
        ConnectionState::Error
    };
    server.error = failure_message(error).to_owned();
    server.checked_at = Some(now());
}

impl Mcps {
    pub async fn lock(&self, id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        self.locks
            .lock()
            .await
            .entry(id.into())
            .or_default()
            .clone()
            .lock_owned()
            .await
    }

    pub async fn server(&self, s: &Service, id: &str) -> Result<McpServer> {
        required(s.store.mcp_server(id).await?, NOT_FOUND)
    }

    pub async fn put_server(&self, s: &Service, server: &McpServer) -> Result<()> {
        let server = server.clone();
        s.store
            .transaction(move |db| db.save_mcp_server(&server))
            .await?;
        Ok(())
    }

    pub async fn server_secrets(&self, s: &Service, id: &str) -> Result<McpSecrets> {
        let id = id.to_owned();
        let ciphertext = s
            .store
            .read({
                let id = id.clone();
                move |db| db.mcp_credentials(&id)
            })
            .await?;
        match ciphertext {
            Some(bytes) => Ok(serde_json::from_slice(
                &s.vault.decrypt_bytes(&id, &bytes)?,
            )?),
            None => Ok(McpSecrets::default()),
        }
    }

    pub async fn update_secrets(
        &self,
        s: &Service,
        id: &str,
        change: impl FnOnce(&mut McpSecrets),
    ) -> Result<()> {
        self.update_secrets_authorized(s, id, change, |_| Ok(()))
            .await
    }

    pub(crate) async fn update_secrets_authorized<F>(
        &self,
        s: &Service,
        id: &str,
        change: impl FnOnce(&mut McpSecrets),
        authorize: F,
    ) -> Result<()>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let mut secrets = self.server_secrets(s, id).await?;
        change(&mut secrets);
        let secrets = s.vault.encrypt_bytes(id, &serde_json::to_vec(&secrets)?)?;
        let id = id.to_owned();
        s.store
            .transaction(move |db| {
                authorize(db)?;
                db.save_mcp_credentials(&id, &secrets)
            })
            .await
    }

    async fn view(&self, s: &Service, server: &McpServer) -> Result<McpView> {
        let secrets = self.server_secrets(s, &server.id).await?;
        let mut public = server.clone();
        public.tools = server
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name":text(tool,"name"),
                    "title":tool["title"].as_str(),
                    "description":tool["description"].as_str(),
                    "inputSchema":tool.get("inputSchema").cloned().unwrap_or_else(||json!({})),
                })
            })
            .collect();
        let view = McpView {
            server: public,
            has_token: !secrets.token().is_empty(),
            has_client_secret: !secrets.client_secret().is_empty(),
            env_keys: secrets.env_keys(),
            callback_url: callback_url(s).await.unwrap_or_default(),
        };
        Ok(view)
    }

    pub async fn list(&self, s: &Service) -> Result<Vec<McpView>> {
        let mut result = Vec::new();
        for server in s.store.mcp_servers().await? {
            result.push(self.view(s, &server).await?);
        }
        Ok(result)
    }

    pub async fn assert_management(&self, s: &Service, id: &str) -> Result<()> {
        let server = self.server(s, id).await?;
        if !server.is_http() {
            return Ok(());
        }
        let url = url::Url::parse(&server.url).map_err(|_| Error::bad("Invalid MCP endpoint."))?;
        let busy = self
            .locks
            .lock()
            .await
            .get(id)
            .is_some_and(|lock| lock.try_lock().is_err());
        let beacon = crate::relay::beacon_address(&s.config.data_dir).await?;
        let self_connection = beacon.is_some_and(|(origin, _)| {
            url.origin().ascii_serialization() == origin && url.path() == "/mcp"
        });
        if busy && self_connection {
            return Err(Error::conflict(
                "This self-connection is serving an active request. Manage other connections here; test or change this connection directly from the MCPs UI after the request finishes.",
            ));
        }
        Ok(())
    }

    pub async fn save(
        &self,
        s: &Service,
        input: McpInput,
        existing: Option<&str>,
        expected: Option<u64>,
    ) -> Result<McpView> {
        self.save_authorized(s, input, existing, expected, |_| Ok(()))
            .await
    }

    pub(crate) async fn save_authorized<F>(
        &self,
        s: &Service,
        mut input: McpInput,
        existing: Option<&str>,
        expected: Option<u64>,
        authorize: F,
    ) -> Result<McpView>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        validate_input(&mut input)?;
        if existing.is_some() && expected.is_none() {
            return Err(Error::bad("The current connection revision is required."));
        }
        let mut server = McpServer {
            name: input.name,
            transport: input.transport,
            url: input.url,
            command: input.command,
            args: input.args,
            auth: input.auth,
            client_id: input.client_id,
            scopes: input.scopes,
            allow_private_network: input.allow_private_network,
            enabled: input.enabled,
            enabled_tools: input.enabled_tools,
            ..Default::default()
        };
        let id = existing.map_or_else(id, str::to_owned);
        let _guard = self.lock(&id).await;
        let previous = s.store.mcp_server(&id).await?;
        if existing.is_some() && previous.is_none() {
            return Err(Error::not_found(NOT_FOUND));
        }
        if let Some(expected) = expected
            && previous
                .as_ref()
                .is_none_or(|server| server.revision != expected)
        {
            return Err(Error::conflict(
                "This connection was changed elsewhere. Reload it before saving.",
            ));
        }
        let endpoint_changed = previous
            .as_ref()
            .is_some_and(|previous| !previous.same_endpoint(&server));
        let mut secrets = if endpoint_changed {
            McpSecrets::default()
        } else {
            self.server_secrets(s, &id).await?
        };
        if let Some(token) = input.token {
            secrets.token = Some(token);
        }
        if let Some(secret) = input.client_secret {
            secrets.client_secret = Some(secret);
        }
        if let Some(env) = input.env {
            secrets.env.get_or_insert_default().extend(env.0);
        }
        if let Some(env) = secrets.env.as_mut() {
            for key in input.remove_env {
                env.remove(&key);
            }
        }
        validate_credentials(&server, &secrets)?;
        server.id.clone_from(&id);
        server.created_at = previous.as_ref().map_or_else(now, |p| p.created_at);
        server.revision = previous.as_ref().map_or(0, |p| p.revision) + 1;
        server.tools = previous.map(|p| p.tools).unwrap_or_default();
        server.reset_state(ConnectionState::Untested);
        let record = server.clone();
        let secrets = s.vault.encrypt_bytes(&id, &serde_json::to_vec(&secrets)?)?;
        s.store
            .transaction(move |db| {
                authorize(db)?;
                db.save_mcp_server(&record)?;
                db.save_mcp_credentials(&id, &secrets)?;
                cancel_pending(db, &id)?;
                db.audit("mcp.saved", &json!({ "id": id }))
            })
            .await?;
        self.view(s, &server).await
    }

    pub async fn disconnect(&self, s: &Service, id: &str, remove: bool) -> Result<()> {
        self.disconnect_authorized(s, id, remove, |_| Ok(())).await
    }

    pub(crate) async fn disconnect_authorized<F>(
        &self,
        s: &Service,
        id: &str,
        remove: bool,
        authorize: F,
    ) -> Result<()>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let _guard = self.lock(id).await;
        let id = id.to_owned();
        s.store
            .transaction(move |db| {
                authorize(db)?;
                let mut server = required(db.mcp_server(&id)?, NOT_FOUND)?;
                cancel_pending(db, &id)?;
                db.delete_mcp_credentials(&id)?;
                if remove {
                    revoke_agent_access(db, &id)?;
                    db.delete_mcp_server(&id)?;
                } else {
                    server.revision += 1;
                    server.reset_state(ConnectionState::Untested);
                    db.save_mcp_server(&server)?;
                }
                let action = if remove {
                    "mcp.deleted"
                } else {
                    "mcp.disconnected"
                };
                db.audit(action, &json!({ "id": id }))
            })
            .await
    }

    pub async fn failure(&self, s: &Service, mut server: McpServer, error: &Error) -> Result<()> {
        record_failure(&mut server, error);
        self.put_server(s, &server).await
    }

    /// Records a discovery outcome: the tool catalog on success, the failure otherwise.
    pub async fn record_discovery(
        &self,
        s: &Service,
        server: McpServer,
        result: &Result<Vec<Value>>,
    ) -> Result<()> {
        self.record_discovery_authorized(s, server, result, |_| Ok(()))
            .await
    }

    pub(crate) async fn record_discovery_authorized<F>(
        &self,
        s: &Service,
        mut server: McpServer,
        result: &Result<Vec<Value>>,
        authorize: F,
    ) -> Result<()>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        match result {
            Ok(tools) => server.record_success(tools.clone(), now()),
            Err(error) => record_failure(&mut server, error),
        }
        let record = server;
        s.store
            .transaction(move |db| {
                authorize(db)?;
                db.save_mcp_server(&record)?;
                Ok(())
            })
            .await
    }

    pub async fn test(&self, s: &Service, id: &str) -> Result<McpView> {
        self.test_authorized(s, id, |_| Ok(())).await
    }

    pub(crate) async fn test_authorized<F>(
        &self,
        s: &Service,
        id: &str,
        authorize: F,
    ) -> Result<McpView>
    where
        F: Fn(&Db<'_>) -> Result<()> + Clone + Send + Sync + 'static,
    {
        let _guard = self.lock(id).await;
        let server = self.server(s, id).await?;
        s.store
            .read({
                let authorize = authorize.clone();
                move |db| authorize(db)
            })
            .await?;
        let tools =
            crate::mcp_client::Client::list_tools_authorized(s, &server, authorize.clone()).await;
        self.record_discovery_authorized(s, server, &tools, authorize)
            .await?;
        self.view(s, &self.server(s, id).await?).await
    }

    pub async fn run_configuration(&self, s: &Service, run: &Value) -> Result<Value> {
        let agent = &run["snapshot"]["agent"];
        let provider = crate::provider::Provider::of_agent(agent)?;
        let access = policy(agent);
        let token = token();
        let mut configuration = RunConfiguration::new(token.clone());
        let origin = agent_origin(s, run);
        let mut servers = BTreeMap::new();
        for server in s.store.mcp_servers().await? {
            if !server.enabled || !allowed(&access["mcps"], &server.id) {
                continue;
            }
            let tools = selected_tools(&server, &access["mcpTools"][&server.id]);
            let env = self
                .server_secrets(s, &server.id)
                .await?
                .env
                .unwrap_or_default();
            configuration.redactions.extend(env.values().cloned());
            if server.is_http() {
                let scope = GrantScope {
                    revision: server.revision,
                    tools: tools.clone(),
                };
                servers.insert(server.id.clone(), scope);
                configuration.add_http(origin, &server, tools);
            } else {
                configuration.add_stdio(&server, tools, env);
            }
        }
        let workspace = !s.config.runner_url.is_empty() || run["trigger"] == "chat";
        if workspace {
            configuration.add_workspace(origin);
        }
        let installation = access["installation"] == true;
        if installation {
            configuration.add_internal(origin, "cairn_installation", "/mcp-installation", None);
        }
        if workspace || installation || !servers.is_empty() {
            configuration.env.insert(RUN_TOKEN_ENV, token.clone());
            let grant = RunGrant {
                run_id: text(run, "id").to_owned(),
                message_id: crate::waits::execution_id(run).as_str().map(str::to_owned),
                servers,
                workspace,
                installation,
            };
            let expires = crate::run_limits::budget_ms(agent).map(|budget| now() + budget);
            s.store
                .transaction(move |db| db.save_mcp_run_grant(&grant_key(&token), &grant, expires))
                .await?;
        }
        configuration.redactions.retain(|r| r.len() > 3);
        let mut rendered = provider
            .adapter()
            .mcp_configuration(&configuration.native)?;
        rendered["env"] = serde_json::to_value(configuration.env)?;
        rendered["redactions"] = serde_json::to_value(configuration.redactions)?;
        Ok(rendered)
    }

    pub async fn grant(
        &self,
        s: &Service,
        id: &str,
        bearer: &str,
    ) -> Result<(McpServer, GrantScope)> {
        let (server, scope, _) = self.grant_authorized(s, id, bearer).await?;
        Ok((server, scope))
    }

    pub(crate) async fn grant_authorized(
        &self,
        s: &Service,
        id: &str,
        bearer: &str,
    ) -> Result<(
        McpServer,
        GrantScope,
        crate::capabilities::plugin_packages::Authority,
    )> {
        let (id, bearer) = (id.to_owned(), bearer.to_owned());
        let (key, token) = (id.clone(), bearer.clone());
        let (server, scope, run) = s
            .store
            .read(move |db| {
                let (server, scope) = grant_in(db, &key, &token)?;
                let grant = crate::error::required(
                    db.mcp_run_grant(&grant_key(&token))?,
                    "MCP grant not found",
                )?;
                let run = crate::error::required(db.run(&grant.run_id)?, "Run not found")?;
                Ok((server, scope, run))
            })
            .await?;
        let authority = crate::capabilities::plugin_packages::Authority::for_execution(&run)
            .with_guard(move |db| grant_in(db, &id, &bearer).map(|_| ()));
        let authority = crate::delegation::authorize_async_with(s, &run, authority).await?;
        Ok((server, scope, authority))
    }

    pub async fn revoke_run(&self, s: &Service, id: &str) -> Result<()> {
        let id = id.to_owned();
        s.store.write(move |db| db.revoke_mcp_run(&id)).await
    }
}

fn validate_input(input: &mut McpInput) -> Result<()> {
    input.name = input.name.trim().to_owned();
    input.command = input.command.trim().to_owned();
    input.client_id = input.client_id.trim().to_owned();
    input.scopes = input
        .scopes
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let bounded = |value: &str, max| value.chars().count() <= max;
    if input.name.is_empty()
        || !bounded(&input.name, 100)
        || !bounded(&input.url, 2000)
        || !bounded(&input.command, 1000)
        || !bounded(&input.client_id, 500)
        || !bounded(&input.scopes, 2000)
        || input.args.len() > 100
        || input.args.iter().any(|a| !bounded(a, 4000))
        || input.enabled_tools.as_ref().is_some_and(|tools| {
            tools.len() > 500 || tools.iter().any(|t| t.is_empty() || !bounded(t, 200))
        })
        || input.token.as_ref().is_some_and(|t| !bounded(t, 10000))
        || input
            .client_secret
            .as_ref()
            .is_some_and(|t| !bounded(t, 10000))
        || input.remove_env.len() > 100
    {
        return Err(Error::bad(
            "MCP settings exceed the supported content limits.",
        ));
    }
    if let Some(env) = &input.env {
        for (key, value) in &env.0 {
            let mut chars = key.chars();
            if !chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
                || !bounded(value, 10000)
            {
                return Err(Error::bad("Invalid environment variable."));
            }
        }
    }
    if input.transport == record::Transport::Http {
        let invalid =
            || Error::bad("Enter an HTTP(S) URL without embedded credentials or a fragment.");
        let url = url::Url::parse(&input.url).map_err(|_| invalid())?;
        if !["http", "https"].contains(&url.scheme())
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid());
        }
    } else if input.command.is_empty() || input.auth != AuthKind::None {
        return Err(Error::bad(
            "Command servers require an executable and use environment variables for authentication.",
        ));
    }
    Ok(())
}

pub(crate) fn grant_in(db: &Db<'_>, id: &str, bearer: &str) -> Result<(McpServer, GrantScope)> {
    let expired = || Error::unauthorized("MCP run access expired or was revoked.");
    let grant = db.mcp_run_grant(&grant_key(bearer))?.ok_or_else(expired)?;
    let run = db.run(&grant.run_id)?.ok_or_else(expired)?;
    let server = db.mcp_server(id)?.ok_or_else(expired)?;
    let Some(scope) = grant.servers.get(id) else {
        return Err(expired());
    };
    let revoked = grant.message_id.as_deref() != crate::waits::execution_id(&run).as_str()
        || !run["cancelRequestedAt"].is_null()
        || run["status"] != RunStatus::Running
        || !server.enabled
        || server.revision != scope.revision;
    if revoked {
        return Err(expired());
    }
    let agent = &run["snapshot"]["agent"];
    let current = db
        .agent_snapshot(text(agent, "id"))?
        .ok_or_else(|| Error::forbidden("Agent permissions changed."))?;
    if !covers(&current, agent) {
        return Err(Error::forbidden("Agent permissions were reduced."));
    }
    crate::delegation::authorize_run(db, &run)?;
    Ok((server, scope.clone()))
}
