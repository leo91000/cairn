pub mod channel;
pub mod record;

use crate::{
    auth::{hex_digest, token},
    config::{id, now},
    error::{Error, Result, required},
    run_status::RunStatus,
    service::{Service, allowed, covers, policy},
    store::Db,
    validation::{parse, text},
};
use record::{AuthKind, ConnectionState, GrantScope, McpSecrets, McpServer, RunGrant};
use serde::{Deserialize, Serialize};
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
    format!("mcp-grant:{}", hex_digest(bearer))
}

/// The run a bearer was granted to, if its grant still exists.
pub(crate) async fn granted_run(s: &Service, bearer: &str) -> Result<Option<String>> {
    let Some(grant) = s.store.kv(&grant_key(bearer)).await? else {
        return Ok(None);
    };
    Ok(Some(RunGrant::deserialize(&grant)?.run_id))
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
    for (key, value) in db.keys("mcp-oauth:")? {
        if value["connectionId"] == id {
            db.delete(&key)?;
        }
    }
    Ok(())
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

/// Secret fields submitted with connection settings; never stored in the record.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretChanges {
    token: Option<String>,
    client_secret: Option<String>,
    env: Option<BTreeMap<String, String>>,
    #[serde(default)]
    remove_env: Vec<String>,
}

impl SecretChanges {
    const KEYS: [&str; 4] = ["token", "clientSecret", "env", "removeEnv"];

    fn take(settings: &mut Value) -> Result<Self> {
        let changes = Self::deserialize(&*settings)?;
        if let Some(settings) = settings.as_object_mut() {
            for key in Self::KEYS {
                settings.remove(key);
            }
        }
        Ok(changes)
    }

    fn apply(self, secrets: &mut McpSecrets) {
        if let Some(token) = self.token {
            secrets.token = Some(token);
        }
        if let Some(client_secret) = self.client_secret {
            secrets.client_secret = Some(client_secret);
        }
        if let Some(env) = self.env {
            secrets.env.get_or_insert_default().extend(env);
        }
        if let Some(env) = secrets.env.as_mut() {
            for key in &self.remove_env {
                env.remove(key);
            }
        }
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpView<'a> {
    #[serde(flatten)]
    server: &'a McpServer,
    has_token: bool,
    has_client_secret: bool,
    env_keys: Vec<String>,
    callback_url: String,
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

/// During a plan turn, only the tools the connection declares read-only
/// (`readOnlyHint`) among those otherwise exposed. Undeclared tools are excluded.
fn read_only_tools(server: &McpServer, exposed: Option<&[String]>) -> Option<Vec<String>> {
    let declared = server
        .tools
        .iter()
        .filter(|tool| tool["annotations"]["readOnlyHint"] == true)
        .map(|tool| text(tool, "name").to_owned());
    Some(
        declared
            .filter(|name| exposed.is_none_or(|tools| tools.contains(name)))
            .collect(),
    )
}

/// Removes a deleted connection from every agent's access policy. Other settings,
/// including live grants such as 1Password, are kept as saved.
fn revoke_agent_access(db: &Db<'_>, id: &str) -> Result<()> {
    for mut agent in db.list("agents")? {
        let mut access = policy(&agent);
        let mut changed = false;
        if let Some(mcps) = access["mcps"].as_array_mut() {
            let before = mcps.len();
            mcps.retain(|m| m != id);
            changed |= mcps.len() != before;
        }
        if let Some(tools) = access["mcpTools"].as_object_mut() {
            changed |= tools.remove(id).is_some();
        }
        if !changed {
            continue;
        }
        agent["access"] = access;
        crate::service::touch(&mut agent);
        db.put("agents", &agent)?;
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

    pub async fn get(&self, s: &Service, id: &str) -> Result<Value> {
        required(s.store.get("mcps", id).await?, NOT_FOUND)
    }

    pub async fn server(&self, s: &Service, id: &str) -> Result<McpServer> {
        Ok(McpServer::deserialize(&self.get(s, id).await?)?)
    }

    pub async fn put_server(&self, s: &Service, server: &McpServer) -> Result<()> {
        s.store.put("mcps", serde_json::to_value(server)?).await?;
        Ok(())
    }

    pub async fn secrets(&self, s: &Service, id: &str) -> Result<Value> {
        Ok(s.vault.get(id).await?.unwrap_or_else(|| json!({})))
    }

    pub async fn server_secrets(&self, s: &Service, id: &str) -> Result<McpSecrets> {
        match s.vault.get(id).await? {
            Some(secrets) => Ok(serde_json::from_value(secrets)?),
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
        let secrets = serde_json::to_value(secrets)?;
        let vault = s.vault.clone();
        let id = id.to_owned();
        s.store
            .transaction(move |db| {
                authorize(db)?;
                vault.set_in(db, &id, &secrets)
            })
            .await
    }

    async fn view(&self, s: &Service, server: &McpServer) -> Result<Value> {
        let secrets = self.server_secrets(s, &server.id).await?;
        let view = McpView {
            server,
            has_token: !secrets.token().is_empty(),
            has_client_secret: !secrets.client_secret().is_empty(),
            env_keys: secrets.env_keys(),
            callback_url: callback_url(s).await.unwrap_or_default(),
        };
        Ok(serde_json::to_value(view)?)
    }

    pub async fn list(&self, s: &Service) -> Result<Vec<Value>> {
        let mut result = Vec::new();
        for item in s.store.list("mcps").await? {
            result.push(self.view(s, &McpServer::deserialize(&item)?).await?);
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

    pub async fn save(&self, s: &Service, input: Value, existing: Option<&str>) -> Result<Value> {
        self.save_authorized(s, input, existing, |_| Ok(())).await
    }

    pub(crate) async fn save_authorized<F>(
        &self,
        s: &Service,
        input: Value,
        existing: Option<&str>,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let mut settings = parse("mcp", input)?;
        let changes = SecretChanges::take(&mut settings)?;
        let mut server = McpServer::deserialize(&settings)?;
        let id = existing.map_or_else(id, str::to_owned);
        let _guard = self.lock(&id).await;
        let previous = match s.store.get("mcps", &id).await? {
            Some(previous) => Some(McpServer::deserialize(&previous)?),
            None => None,
        };
        let endpoint_changed = previous
            .as_ref()
            .is_some_and(|previous| !previous.same_endpoint(&server));
        let mut secrets = if endpoint_changed {
            McpSecrets::default()
        } else {
            self.server_secrets(s, &id).await?
        };
        changes.apply(&mut secrets);
        validate_credentials(&server, &secrets)?;
        server.id.clone_from(&id);
        server.created_at = previous.as_ref().map_or_else(now, |p| p.created_at);
        server.revision = previous.as_ref().map_or(0, |p| p.revision) + 1;
        server.tools = previous.map(|p| p.tools).unwrap_or_default();
        server.reset_state(ConnectionState::Untested);
        let record = serde_json::to_value(&server)?;
        let secrets = serde_json::to_value(&secrets)?;
        let vault = s.vault.clone();
        s.store
            .transaction(move |db| {
                authorize(db)?;
                db.put("mcps", &record)?;
                vault.set_in(db, &id, &secrets)?;
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
                let item = required(db.get("mcps", &id)?, NOT_FOUND)?;
                cancel_pending(db, &id)?;
                db.delete(&format!("mcp-secret:{id}"))?;
                if remove {
                    db.remove("mcps", &id)?;
                    revoke_agent_access(db, &id)?;
                } else {
                    let mut server = McpServer::deserialize(&item)?;
                    server.revision += 1;
                    server.reset_state(ConnectionState::Untested);
                    db.put("mcps", &serde_json::to_value(&server)?)?;
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

    async fn record_discovery_authorized<F>(
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
        let record = serde_json::to_value(server)?;
        s.store
            .transaction(move |db| {
                authorize(db)?;
                db.put("mcps", &record)?;
                Ok(())
            })
            .await
    }

    pub async fn test(&self, s: &Service, id: &str) -> Result<Value> {
        self.test_authorized(s, id, |_| Ok(())).await
    }

    pub(crate) async fn test_authorized<F>(
        &self,
        s: &Service,
        id: &str,
        authorize: F,
    ) -> Result<Value>
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
        let planning = crate::chats::planning(run);
        configuration.native.planning = planning;
        let origin = agent_origin(s, run);
        let mut servers = BTreeMap::new();
        for item in s.store.list("mcps").await? {
            let server = McpServer::deserialize(&item)?;
            if !server.enabled || !allowed(&access["mcps"], &server.id) {
                continue;
            }
            let mut tools = selected_tools(&server, &access["mcpTools"][&server.id]);
            if planning {
                tools = read_only_tools(&server, tools.as_deref());
            }
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
        let workspace = !s.config.runner_url.is_empty();
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
                message_id: crate::waits::execution_id(run).clone(),
                servers,
                workspace,
                installation,
            };
            let expires = crate::run_limits::budget_ms(agent).map(|budget| now() + budget);
            s.store
                .set(&grant_key(&token), serde_json::to_value(grant)?, expires)
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
        let (id, bearer) = (id.to_owned(), bearer.to_owned());
        s.store
            .read(move |db| {
                let expired = || Error::unauthorized("MCP run access expired or was revoked.");
                let grant = db.kv(&grant_key(&bearer))?.ok_or_else(expired)?;
                let grant = RunGrant::deserialize(&grant)?;
                let run = db.run(&grant.run_id)?.ok_or_else(expired)?;
                let item = db.get("mcps", &id)?.ok_or_else(expired)?;
                let server = McpServer::deserialize(&item)?;
                let Some(scope) = grant.servers.get(&id) else {
                    return Err(expired());
                };
                let revoked = grant.message_id != *crate::waits::execution_id(&run)
                    || !run["cancelRequestedAt"].is_null()
                    || run["status"] != RunStatus::Running
                    || !server.enabled
                    || server.revision != scope.revision;
                if revoked {
                    return Err(expired());
                }
                let agent = &run["snapshot"]["agent"];
                let current = db
                    .get("agents", text(agent, "id"))?
                    .ok_or_else(|| Error::forbidden("Agent permissions changed."))?;
                if !covers(&current, agent) {
                    return Err(Error::forbidden("Agent permissions were reduced."));
                }
                Ok((server, scope.clone()))
            })
            .await
    }

    pub async fn revoke_run(&self, s: &Service, id: &str) -> Result<()> {
        let id = id.to_owned();
        s.store
            .write(move |db| {
                for (key, value) in db.keys("mcp-grant:")? {
                    if value["runId"] == id {
                        db.delete(&key)?;
                    }
                }
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_turns_expose_only_tools_declared_read_only() {
        let server: McpServer = serde_json::from_value(json!({
            "tools": [
                { "name": "search", "annotations": { "readOnlyHint": true } },
                { "name": "delete", "annotations": { "readOnlyHint": false, "destructiveHint": true } },
                { "name": "undeclared" },
                { "name": "lookup", "annotations": { "readOnlyHint": true } },
            ],
        }))
        .unwrap();
        assert_eq!(
            read_only_tools(&server, None),
            Some(vec!["search".to_owned(), "lookup".to_owned()])
        );
        // The agent's own selection still applies.
        assert_eq!(
            read_only_tools(&server, Some(&["search".to_owned(), "delete".to_owned()])),
            Some(vec!["search".to_owned()])
        );
        // Nothing discovered means nothing allowed, never everything.
        assert_eq!(
            read_only_tools(&McpServer::default(), None),
            Some(Vec::new())
        );
    }
}
