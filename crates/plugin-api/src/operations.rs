//! Approved operations associate resource grants with concrete external requests.
use crate::{Manifest, hostname, slug};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum Profile {
    Read,
    Contribute,
    Manage,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestRule {
    pub method: String,
    pub host: String,
    /// Exact URL path, or a prefix ending in `*`.
    pub path: String,
    /// Request field → admitted operation scope. Fields must be present and equal.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Operation {
    pub name: String,
    pub collection: String,
    pub profile: Profile,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<RequestRule>,
}

fn field_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 40
        && value.starts_with(|c: char| c.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

pub(crate) fn validate(manifest: &Manifest) -> Result<(), &'static str> {
    if manifest.operations.len() > 128 {
        return Err("Plugin manifest declares too many operations.");
    }
    let mut names = HashSet::new();
    for operation in &manifest.operations {
        if !slug(&operation.name, 40)
            || !names.insert(operation.name.as_str())
            || !manifest
                .resources
                .iter()
                .any(|resource| resource.name == operation.collection)
            || operation.scopes.len() > 8
            || operation.requests.len() > 8
        {
            return Err("Plugin manifest has an invalid resource operation.");
        }
        let mut scopes = HashSet::new();
        if !operation.scopes.iter().all(|scope| {
            field_name(scope)
                && !matches!(scope.as_str(), "resource" | "instance")
                && scopes.insert(scope)
        }) {
            return Err("Plugin manifest has an invalid operation scope.");
        }
        for request in &operation.requests {
            if request.bindings.len() > 8
                || !request.bindings.iter().all(|(field, scope)| {
                    let resource_setting = scope.strip_prefix("resource.").is_some_and(|key| {
                        manifest
                            .resources
                            .iter()
                            .find(|collection| collection.name == operation.collection)
                            .is_some_and(|collection| {
                                collection.settings.iter().any(|field| {
                                    field["key"] == key
                                        && matches!(
                                            field["type"].as_str(),
                                            Some("string" | "text" | "select" | "url")
                                        )
                                })
                            })
                    });
                    field_name(field) && (operation.scopes.contains(scope) || resource_setting)
                })
            {
                return Err("Plugin manifest has an invalid operation request binding.");
            }
            let prefix = request.path.strip_suffix('*').unwrap_or(&request.path);
            if !manifest.permits_capability("http")
                || !hostname(&request.host)
                || request.host.starts_with("*.")
                || !manifest.allows_host(&request.host)
                || !matches!(
                    request.method.as_str(),
                    "GET" | "POST" | "PUT" | "PATCH" | "DELETE"
                )
                || !prefix.starts_with('/')
                || prefix.len() > 256
                || prefix
                    .split('/')
                    .any(|segment| segment == "." || segment == "..")
                || !prefix.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.')
                })
            {
                return Err("Plugin manifest has an invalid operation request.");
            }
        }
    }
    for command in &manifest.commands {
        if let Some(name) = &command.operation {
            let operation = manifest
                .operations
                .iter()
                .find(|operation| &operation.name == name)
                .ok_or("Plugin command references an undeclared operation.")?;
            for field in
                std::iter::once("resource").chain(operation.scopes.iter().map(String::as_str))
            {
                if !command
                    .arguments
                    .iter()
                    .any(|argument| argument.name == field && argument.required)
                {
                    return Err(
                        "A resource command must require its resource and scope arguments.",
                    );
                }
            }
        }
    }
    for tool in &manifest.tools {
        if let Some(name) = &tool.operation {
            let operation = manifest
                .operations
                .iter()
                .find(|operation| &operation.name == name)
                .ok_or("Plugin tool references an undeclared operation.")?;
            for field in
                std::iter::once("resource").chain(operation.scopes.iter().map(String::as_str))
            {
                let required = tool.input_schema["required"]
                    .as_array()
                    .is_some_and(|required| required.iter().any(|item| item == field));
                if !required || tool.input_schema["properties"][field]["type"] != "string" {
                    return Err(
                        "A resource tool must require its resource and scope arguments as text.",
                    );
                }
            }
        }
    }
    Ok(())
}
