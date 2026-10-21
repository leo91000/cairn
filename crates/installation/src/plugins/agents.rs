//! What packages give agents: commands in their conversation, Cairn MCP tools and
//! prompt instructions. Only agents the owner granted on an enabled instance get them,
//! and only through those instances.
use super::{
    connections::{self, Connection},
    registry::{self, Package},
    scope::{self, Scope},
};
use crate::{
    error::{Error, Result},
    project_workspaces::authorize,
    service::Service,
    validation::text,
};
use cairn_plugin_api::{Output, parse_arguments, usage};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Active packages with agent access, each with the enabled instances granting `agent_id`.
async fn granted(
    s: &Service,
    agent_id: &str,
    plugin: Option<&str>,
) -> Result<Vec<(Package, Vec<Connection>)>> {
    let mut by_plugin = BTreeMap::<String, Vec<Connection>>::new();
    for value in s.store.list(connections::KIND).await? {
        let Ok(connection) = serde_json::from_value::<Connection>(value) else {
            continue;
        };
        if connection.grants(agent_id) && plugin.is_none_or(|plugin| plugin == connection.plugin_id)
        {
            by_plugin
                .entry(connection.plugin_id.clone())
                .or_default()
                .push(connection);
        }
    }
    let mut output = Vec::new();
    for (plugin, mut instances) in by_plugin {
        let Ok(package) = registry::active(s, &plugin).await else {
            continue;
        };
        if package.manifest.agent_access.is_none() {
            continue;
        }
        instances.sort_by(|a, b| a.name.cmp(&b.name));
        output.push((package, instances));
    }
    Ok(output)
}

fn agent(run: &Value) -> (String, Value) {
    let agent = &run["snapshot"]["agent"];
    let id = text(agent, "id").to_owned();
    (id.clone(), json!({ "id": id, "name": text(agent, "name") }))
}

/// Prompt instructions from every package granting this agent, one paragraph each.
pub async fn instructions(s: &Service, agent_id: &str) -> Result<String> {
    Ok(granted(s, agent_id, None)
        .await?
        .iter()
        .map(|(package, _)| package.manifest.instructions.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The run with `pluginInstructions` for its agent, for the launch prompt. A lookup
/// failure only leaves the instructions out.
pub async fn with_instructions(s: &Service, run: &Value) -> Value {
    let mut run = run.clone();
    let agent_id = text(&run["snapshot"]["agent"], "id").to_owned();
    run["pluginInstructions"] = json!(instructions(s, &agent_id).await.unwrap_or_default());
    run
}

/// Command-line words from `{ args: [...] }`.
fn words(input: &Value) -> Result<Vec<String>> {
    input["args"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(|word| word.as_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| Error::bad("Command arguments must be text."))
}

/// Runs `cairn plugin <plugin> <command> …` for the run holding `bearer`.
pub async fn command(s: &Service, bearer: &str, input: &Value) -> Result<Value> {
    let run = authorize(s, bearer).await?;
    let (agent_id, agent) = agent(&run);
    let plugin = text(input, "plugin");
    let unavailable = || Error::not_found("Plugin command not available.");
    let package = registry::active(s, plugin)
        .await
        .map_err(|_| unavailable())?;
    if let Some(file) = input.get("file").filter(|value| value.is_object()) {
        let instances = granted(s, &agent_id, Some(plugin))
            .await?
            .into_iter()
            .flat_map(|(_, instances)| instances)
            .collect::<Vec<_>>();
        if !package.manifest.permits_capability("files") {
            return Err(Error::forbidden(
                "This plugin has no approved file capability.",
            ));
        }
        let method = match text(file, "method") {
            "create" => "files.create",
            "append" => "files.append",
            "read" => "files.read",
            "delete" => "files.delete",
            _ => return Err(Error::bad("Unknown file transfer operation.")),
        };
        let scope = Scope::Agent {
            agent_id,
            instances,
        };
        let result = scope::call(s, &package, &scope, method, file).await?;
        return Ok(json!({ "output": "json", "value": result }));
    }
    let name = text(input, "command");
    let Some(command) = package.manifest.command(name) else {
        let available = package
            .manifest
            .commands
            .iter()
            .map(usage)
            .collect::<Vec<_>>();
        if available.is_empty() {
            return Err(unavailable());
        }
        return Err(Error::bad(format!(
            "Unknown command. Use cairn plugin {plugin} {}",
            available.join(" | ")
        )));
    };
    let values = parse_arguments(command, &words(input)?).map_err(Error::bad)?;
    let instances = granted(s, &agent_id, Some(plugin))
        .await?
        .into_iter()
        .flat_map(|(_, instances)| instances)
        .collect::<Vec<_>>();
    if instances.is_empty() {
        return Err(Error::forbidden(format!(
            "No enabled {} instance grants this agent.",
            package.manifest.name
        )));
    }
    let scope = Scope::Agent {
        agent_id,
        instances,
    };
    let payload = json!({
        "command": name,
        "arguments": values,
        "agent": agent,
        "instances": scope.instances(),
    });
    let value = scope::invoke(s, &package, "command", payload, &scope).await?;
    Ok(match command.output {
        Output::Secret => {
            let value = value
                .as_str()
                .ok_or_else(|| Error::bad_gateway("The plugin returned no secret value."))?;
            json!({ "output": "secret", "value": value })
        }
        Output::Text => {
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            json!({ "output": "text", "value": value })
        }
        Output::Json => json!({ "output": "json", "value": value }),
    })
}

/// Tool definitions for the run's agent, as Cairn MCP lists them.
pub async fn tools(s: &Service, bearer: &str) -> Result<Vec<Value>> {
    let run = authorize(s, bearer).await?;
    let (agent_id, _) = agent(&run);
    Ok(granted(s, &agent_id, None)
        .await?
        .iter()
        .flat_map(|(package, _)| package.manifest.tools.iter())
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
            })
        })
        .collect())
}

/// Calls a package tool, or returns `None` when no granted package declares `name`.
pub async fn call_tool(
    s: &Service,
    bearer: &str,
    name: &str,
    input: &Value,
) -> Result<Option<Value>> {
    let run = authorize(s, bearer).await?;
    let (agent_id, agent) = agent(&run);
    let Some((package, instances)) = granted(s, &agent_id, None)
        .await?
        .into_iter()
        .find(|(package, _)| package.manifest.tool(name).is_some())
    else {
        return Ok(None);
    };
    let tool = package.manifest.tool(name).expect("found above");
    if !jsonschema::is_valid(&tool.input_schema, input) {
        return Err(Error::bad(format!("Invalid input for {name}.")));
    }
    let scope = Scope::Agent {
        agent_id,
        instances,
    };
    let payload = json!({
        "tool": name,
        "input": input,
        "agent": agent,
        "instances": scope.instances(),
    });
    scope::invoke(s, &package, "tool", payload, &scope)
        .await
        .map(Some)
}
