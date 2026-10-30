//! Bounded resource discovery and host-resolved ancestry for pickers and grants.
use super::{
    connections, management, registry,
    scope::{self, Scope},
};
use crate::{
    error::{Error, Result},
    service::Service,
};
use cairn_contracts::{
    ListPluginResourcesInput, ListPluginResourcesOutput, PluginResource, PluginResourceRef,
    ResolvePluginResourceInput, ResolvePluginResourceOutput,
};
use cairn_plugin_api::Manifest;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn plain(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn invalid() -> Error {
    Error::bad_gateway("The plugin returned invalid or unresolved resources.")
}

fn item(manifest: &Manifest, raw: Value, kind: &str) -> Result<PluginResource> {
    let resource: PluginResource = serde_json::from_value(raw).map_err(|_| invalid())?;
    let declaration = manifest.resource_type(kind).ok_or_else(invalid)?;
    if resource.resource_type != kind || !plain(&resource.id, 200) || !plain(&resource.name, 300) {
        return Err(invalid());
    }
    match (&declaration.parent_type, &resource.parent) {
        (None, None) => {}
        (Some(kind), Some(parent)) if parent.resource_type == *kind && plain(&parent.id, 200) => {}
        _ => return Err(invalid()),
    }
    Ok(resource)
}

/// Resource reads use credentials. Installation access alone must not give an
/// agent access to an ungranted instance's service account.
async fn instance(
    s: &Service,
    plugin: &str,
    instance: &str,
    kind: &str,
    agent: Option<&str>,
    authority: Option<management::Authority>,
) -> Result<(registry::Package, Scope)> {
    let package = registry::active(s, plugin).await?;
    if package.manifest.resource_type(kind).is_none() {
        return Err(Error::not_found("This plugin has no such resource type."));
    }
    let connection = connections::get(s, plugin, instance).await?;
    if !connection.enabled {
        return Err(Error::bad(
            "Enable this instance before reading its resources.",
        ));
    }
    let scope = if let Some(agent) = agent {
        if !connection.grants(agent) {
            return Err(Error::forbidden(
                "This agent is not granted that plugin instance.",
            ));
        }
        Scope::Agent {
            agent_id: agent.to_owned(),
            instances: vec![connection],
        }
    } else {
        Scope::Instance(Box::new(connection))
    };
    let scope = match authority {
        Some(authority) => Scope::Managed(Box::new(scope), authority),
        None => scope,
    };
    Ok((package, scope))
}

pub async fn list(
    s: &Service,
    input: ListPluginResourcesInput,
    agent: Option<&str>,
) -> Result<ListPluginResourcesOutput> {
    list_authorized(s, input, agent, None).await
}

pub(crate) async fn list_authorized(
    s: &Service,
    input: ListPluginResourcesInput,
    agent: Option<&str>,
    authority: Option<management::Authority>,
) -> Result<ListPluginResourcesOutput> {
    if !(1..=10_000).contains(&input.page)
        || input.query.chars().count() > 200
        || input.query.chars().any(char::is_control)
    {
        return Err(Error::bad(
            "Choose a page from 1 to 10000 and a query of at most 200 characters.",
        ));
    }
    let (package, scope) = instance(
        s,
        &input.plugin_id,
        &input.instance_id.to_string(),
        &input.resource_type,
        agent,
        authority,
    )
    .await?;
    let result = scope::invoke(
        s,
        &package,
        "resources",
        json!({
            "op": "list",
            "resourceType": input.resource_type,
            "page": input.page,
            "query": input.query,
            "instance": scope.instances()[0],
        }),
        &scope,
    )
    .await?;
    let raw = result["items"]
        .as_array()
        .filter(|items| items.len() <= 200)
        .ok_or_else(invalid)?;
    let mut seen = BTreeSet::new();
    let mut items = Vec::with_capacity(raw.len());
    for raw in raw {
        let resource = item(&package.manifest, raw.clone(), &input.resource_type)?;
        if !seen.insert(resource.id.clone()) {
            return Err(invalid());
        }
        items.push(resource);
    }
    let next_page = match result.get("nextPage") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_i64()
                .filter(|page| *page > input.page && *page <= 10_000)
                .ok_or_else(invalid)?,
        ),
    };
    Ok(ListPluginResourcesOutput { items, next_page })
}

pub async fn resolve(
    s: &Service,
    input: ResolvePluginResourceInput,
    agent: Option<&str>,
) -> Result<ResolvePluginResourceOutput> {
    resolve_authorized(s, input, agent, None).await
}

pub(crate) async fn resolve_authorized(
    s: &Service,
    input: ResolvePluginResourceInput,
    agent: Option<&str>,
    authority: Option<management::Authority>,
) -> Result<ResolvePluginResourceOutput> {
    if !plain(&input.id, 200) {
        return Err(Error::bad("Choose a resource ID of at most 200 bytes."));
    }
    let (package, scope) = instance(
        s,
        &input.plugin_id,
        &input.instance_id.to_string(),
        &input.resource_type,
        agent,
        authority,
    )
    .await?;
    let mut target = PluginResourceRef {
        resource_type: input.resource_type,
        id: input.id,
    };
    let mut chain = Vec::new();
    // The manifest is an acyclic graph of at most 32 types. Each parent is
    // independently resolved; caller-provided ancestors are never trusted.
    for _ in 0..package.manifest.resources.len() {
        let raw = scope::invoke(
            s,
            &package,
            "resources",
            json!({
                "op": "resolve",
                "resourceType": target.resource_type,
                "id": target.id,
                "instance": scope.instances()[0],
            }),
            &scope,
        )
        .await?;
        let resource = item(&package.manifest, raw, &target.resource_type)?;
        if resource.id != target.id {
            return Err(invalid());
        }
        let parent = resource.parent.clone();
        chain.push(resource);
        match parent {
            Some(parent) => target = parent,
            None => {
                let resource = chain.remove(0);
                return Ok(ResolvePluginResourceOutput {
                    resource,
                    ancestors: chain,
                });
            }
        }
    }
    Err(invalid())
}
