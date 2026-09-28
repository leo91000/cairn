use crate::{
    auth::{hex_digest, token},
    config::{id, now},
    error::{Error, Result, required},
    service::{Service, allowed, policy},
    store::{Db, merge},
    validation::{parse, text},
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
#[derive(Default)]
pub struct Mcps {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}
fn cancel_pending(db: &Db<'_>, id: &str) -> Result<()> {
    for (key, value) in db.keys("mcp-oauth:")? {
        if value["connectionId"] == id {
            db.delete(&key)?;
        }
    }
    Ok(())
}
pub fn toml(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            format!("[{}]", items.iter().map(toml).collect::<Vec<_>>().join(","))
        }
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}={}", json!(k), toml(v)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => value.to_string(),
    }
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
        required(s.store.get("mcps", id).await?, "MCP connection not found.")
    }
    pub async fn secrets(&self, s: &Service, id: &str) -> Result<Value> {
        Ok(s.vault.get(id).await?.unwrap_or_else(|| json!({})))
    }
    pub async fn change_secrets(&self, s: &Service, id: &str, patch: Value) -> Result<()> {
        let mut secrets = self.secrets(s, id).await?;
        merge(&mut secrets, &patch);
        s.vault.set(id, &secrets).await
    }
    pub async fn view(&self, s: &Service, mut item: Value) -> Result<Value> {
        let secret = self.secrets(s, text(&item, "id")).await?;
        merge(
            &mut item,
            &json!({
            "hasToken":!text(&secret,"token").is_empty(),"hasClientSecret":!text(&secret,"clientSecret").is_empty(),"envKeys":secret["env"].as_object().map(|env|env.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),"callbackUrl":format!("{}/oauth/mcp/callback",s.config.public_url)}
            ),
        );
        Ok(item)
    }
    pub async fn list(&self, s: &Service) -> Result<Vec<Value>> {
        let mut result = Vec::new();
        for item in s.store.list("mcps").await? {
            result.push(self.view(s, item).await?);
        }
        Ok(result)
    }
    pub async fn assert_management(&self, s: &Service, id: &str) -> Result<()> {
        let item = self.get(s, id).await?;
        if item["transport"] != "http" {
            return Ok(());
        }
        let url =
            url::Url::parse(text(&item, "url")).map_err(|_| Error::bad("Invalid MCP endpoint."))?;
        let busy = self
            .locks
            .lock()
            .await
            .get(id)
            .is_some_and(|lock| lock.try_lock().is_err());
        if busy && url.origin().ascii_serialization() == s.config.public_url && url.path() == "/mcp"
        {
            return Err(Error::new(
                409,
                "This self-connection is serving an active request. Manage other connections here; test or change this connection directly from the MCPs UI after the request finishes.",
            ));
        }
        Ok(())
    }
    pub async fn save(&self, s: &Service, input: Value, existing: Option<&str>) -> Result<Value> {
        let mut settings = parse("mcp", input)?;
        let id = existing.map(str::to_owned).unwrap_or_else(id);
        let _guard = self.lock(&id).await;
        let previous = s.store.get("mcps", &id).await?;
        let changed = previous.as_ref().is_some_and(|p| {
            ["url", "transport", "auth", "clientId", "scopes"]
                .iter()
                .any(|k| p[k] != settings[k])
        });
        let mut secrets = if changed {
            json!({})
        } else {
            self.secrets(s, &id).await?
        };
        for key in ["token", "clientSecret"] {
            if let Some(value) = settings.as_object_mut().unwrap().remove(key) {
                secrets[key] = value;
            }
        }
        if let Some(env) = settings.as_object_mut().unwrap().remove("env") {
            if !secrets["env"].is_object() {
                secrets["env"] = json!({});
            }
            merge(&mut secrets["env"], &env);
        }
        if let Some(remove) = settings.as_object_mut().unwrap().remove("removeEnv") {
            for key in remove.as_array().into_iter().flatten() {
                if let Some(env) = secrets["env"].as_object_mut() {
                    env.remove(key.as_str().unwrap_or(""));
                }
            }
        }
        if settings["auth"] == "bearer" && text(&secrets, "token").is_empty() {
            return Err(Error::bad("Enter a bearer token."));
        }
        if settings["transport"] == "stdio"
            && secrets["env"].as_object().is_some_and(|env| {
                env.keys().any(|k| {
                    [
                        "HOME",
                        "CODEX_HOME",
                        "PATH",
                        "LD_PRELOAD",
                        "LD_LIBRARY_PATH",
                        "NODE_OPTIONS",
                    ]
                    .contains(&k.as_str())
                        || k.starts_with("LEO_")
                        || k.starts_with("RUNNER_")
                })
            })
        {
            return Err(Error::bad(
                "Environment variables cannot override the agent runtime or home.",
            ));
        }
        merge(
            &mut settings,
            &json!({
            "id":id,"createdAt":previous.as_ref().map(|p|p["createdAt"].clone()).unwrap_or_else(||now().into()),"revision":previous.as_ref().and_then(|p|p["revision"].as_u64()).unwrap_or(0)+1,"state":"untested","tools":previous.as_ref().map(|p|p["tools"].clone()).unwrap_or_else(||json!([])),"checkedAt":null,"error":""}
            ),
        );
        let vault = s.vault.clone();
        let item = s
            .store
            .transaction(move |db| {
                db.put("mcps", &settings)?;
                vault.set_in(db, &id, &secrets)?;
                cancel_pending(db, &id)?;
                db.audit(
                    "mcp.saved",
                    &json!({
                    "id":id}
                    ),
                )?;
                Ok(settings)
            })
            .await?;
        self.view(s, item).await
    }
    pub async fn disconnect(&self, s: &Service, id: &str, remove: bool) -> Result<()> {
        let _guard = self.lock(id).await;
        let id = id.to_owned();
        s.store
            .transaction(move |db| {
                let mut item = required(db.get("mcps", &id)?, "MCP connection not found.")?;
                cancel_pending(db, &id)?;
                db.delete(&format!("mcp-secret:{id}"))?;
                if remove {
                    db.remove("mcps", &id)?;
                    for mut agent in db.list("agents")? {
                        let mut access = policy(&agent);
                        if let Some(mcps) = access["mcps"].as_array_mut() {
                            mcps.retain(|m| m != &id);
                        }
                        if let Some(tools) = access["mcpTools"].as_object_mut() {
                            tools.remove(&id);
                        }
                        agent["access"] = access;
                        db.put("agents", &agent)?;
                    }
                } else {
                    let revision = item["revision"].as_u64().unwrap_or(0) + 1;
                    merge(
                        &mut item,
                        &json!({
                        "revision":revision,"state":"untested","error":"","checkedAt":null}
                        ),
                    );
                    db.put("mcps", &item)?;
                }
                db.audit(
                    if remove {
                        "mcp.deleted"
                    } else {
                        "mcp.disconnected"
                    },
                    &json!({
                    "id":id}
                    ),
                )
            })
            .await
    }
    pub async fn failure(&self, s: &Service, mut item: Value, error: &Error) -> Result<()> {
        let known = [
            "Private network access is disabled for this connection.",
            "The endpoint redirects. Configure its final URL.",
            "Instance metadata endpoints are unavailable.",
        ];
        let message = if known.contains(&error.message.as_str()) {
            &error.message
        } else if error.status == 401 {
            "Sign in to connect this server."
        } else {
            "Could not connect. Check the endpoint, credentials, and server availability."
        };
        merge(
            &mut item,
            &json!({
            "state":if error.status==401{
            "needs-auth"}
            else{
            "error"}
            ,"error":message,"checkedAt":now()}
            ),
        );
        s.store.put("mcps", item).await?;
        Ok(())
    }
    pub async fn test(&self, s: &Service, id: &str) -> Result<Value> {
        let _guard = self.lock(id).await;
        let mut item = self.get(s, id).await?;
        let result = async {
            let mut client = crate::mcp_client::Client::connect(s, &item).await?;
            let result = client.discover().await;
            client.close().await;
            result
        }
        .await;
        match result {
            Ok(tools) => {
                merge(
                    &mut item,
                    &json!({
                    "tools":tools,"state":"connected","error":"","checkedAt":now()}
                    ),
                );
                s.store.put("mcps", item).await?;
            }
            Err(error) => self.failure(s, item, &error).await?,
        }
        self.view(s, self.get(s, id).await?).await
    }
    pub async fn run_configuration(&self, s: &Service, run: &Value) -> Result<Value> {
        let access = policy(&run["snapshot"]["agent"]);
        let token = token();
        let mut servers = json!({});
        let mut args = Vec::new();
        let mut redactions = vec![token.clone()];
        let mut claude_mcps = json!({"mcpServers":{}});
        let mut claude_denied = Vec::<String>::new();
        for item in s.store.list("mcps").await? {
            let id = text(&item, "id");
            if item["enabled"] != true || !allowed(&access["mcps"], id) {
                continue;
            }
            let tools = if let Some(selected) = access["mcpTools"][id].as_array() {
                Value::Array(
                    selected
                        .iter()
                        .filter(|name| {
                            item["enabledTools"].is_null()
                                || item["enabledTools"]
                                    .as_array()
                                    .is_some_and(|t| t.contains(name))
                        })
                        .cloned()
                        .collect(),
                )
            } else {
                item["enabledTools"].clone()
            };
            let secrets = self.secrets(s, id).await?;
            let mut config = if item["transport"] == "http" {
                servers[id] = json!({
                "revision":item["revision"],"tools":tools}
                );
                json!({
                "url":format!("{}/mcp-gateway/{id}",s.config.public_url),"bearer_token_env_var":"LEO_MCP_RUN_TOKEN"}
                )
            } else {
                json!({
                "command":item["command"],"args":item["args"],"env":secrets.get("env").cloned().unwrap_or_else(||json!({
                }
                ))}
                )
            };
            let claude_name = format!("leo_{}", id.replace('-', "_"));
            let claude_config = if item["transport"] == "http" {
                json!({"type":"http","url":config["url"],"headers":{"Authorization":format!("Bearer {token}")}})
            } else {
                if let Some(selected) = tools.as_array() {
                    for tool in item["tools"].as_array().into_iter().flatten() {
                        if !selected.iter().any(|name| name == &tool["name"]) {
                            claude_denied
                                .push(format!("mcp__{claude_name}__{}", text(tool, "name")));
                        }
                    }
                }
                json!({"type":"stdio","command":config["command"],"args":config["args"],"env":config["env"]})
            };
            claude_mcps["mcpServers"][&claude_name] = claude_config;
            if !tools.is_null() {
                config["enabled_tools"] = tools;
            }
            redactions.extend(
                secrets["env"]
                    .as_object()
                    .into_iter()
                    .flat_map(|env| env.values())
                    .filter_map(Value::as_str)
                    .map(str::to_owned),
            );
            args.push("-c".to_owned());
            args.push(format!(
                "mcp_servers.leo_{}={}",
                id.replace('-', "_"),
                toml(&config)
            ));
        }
        let workspace = !s.config.runner_url.is_empty();
        if workspace {
            claude_mcps["mcpServers"]["leo_workspace"] = json!({"type":"http","url":format!("{}/mcp-workspace",s.config.public_url),"headers":{"Authorization":format!("Bearer {token}")}});
            args.extend(["-c".into(),format!("mcp_servers.leo_workspace={}",toml(&json!({"url":format!("{}/mcp-workspace",s.config.public_url),"bearer_token_env_var":"LEO_MCP_RUN_TOKEN","tool_timeout_sec":3660}))) ]);
        }
        let mut env = json!({});
        if workspace || !servers.as_object().unwrap().is_empty() {
            env["LEO_MCP_RUN_TOKEN"] = token.clone().into();
            s.store
                .set(
                    &format!("mcp-grant:{}", hex_digest(&token)),
                    json!({
                    "runId":run["id"],"messageId":run["chatExecution"]["messageId"],"servers":servers,"workspace":workspace}
                    ),
                    crate::run_limits::budget_ms(&run["snapshot"]["agent"]).map(|budget| now() + budget),
                )
                .await?;
        }
        redactions.retain(|r| r.len() > 3);
        Ok(json!({
        "args":args,"env":env,"redactions":redactions,"claudeMcps":claude_mcps,"claudeDeniedTools":claude_denied}
        ))
    }
    pub async fn grant(&self, s: &Service, id: &str, bearer: &str) -> Result<(Value, Value)> {
        let (id, bearer) = (id.to_owned(), bearer.to_owned());
        s.store
            .read(move |db| {
                let expired = || Error::new(401, "MCP run access expired or was revoked.");
                let grant = db
                    .kv(&format!("mcp-grant:{}", hex_digest(&bearer)))?
                    .ok_or_else(expired)?;
                let run = db.run(text(&grant, "runId"))?.ok_or_else(expired)?;
                let scope = grant["servers"][&id].clone();
                let item = db.get("mcps", &id)?.ok_or_else(expired)?;
                if scope.is_null()
                    || !run["cancelRequestedAt"].is_null()
                    || run["status"] != "running"
                    || item["enabled"] != true
                    || item["revision"] != scope["revision"]
                {
                    return Err(expired());
                }
                let current = db
                    .get("agents", text(&run["snapshot"]["agent"], "id"))?
                    .ok_or_else(|| Error::new(403, "Agent permissions changed."))?;
                if policy(&current) != policy(&run["snapshot"]["agent"]) {
                    return Err(Error::new(403, "Agent permissions changed."));
                }
                Ok((item, scope))
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
