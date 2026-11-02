//! Resource types and explicit references, shared by grant enforcement and hooks.
use crate::{Manifest, client_default, short, slug};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, schemars::JsonSchema, ts_rs::TS)]
#[schemars(rename = "PluginResourceType")]
#[ts(rename = "PluginResourceType")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceType {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_type: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, schemars::JsonSchema, ts_rs::TS)]
#[schemars(rename = "PluginResourceField")]
#[ts(rename = "PluginResourceField")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceField {
    /// JSON Pointer through properties; `*` traverses array items.
    pub path: String,
    pub resource_type: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, schemars::JsonSchema, ts_rs::TS)]
#[schemars(rename = "PluginHook")]
#[ts(rename = "PluginHook")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hook {
    pub name: String,
    pub description: String,
    #[ts(type = "any ")]
    pub payload_schema: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(transform = client_default::<Vec<ResourceField>>)]
    pub resource_fields: Vec<ResourceField>,
}

/// A path must resolve unambiguously to a declared string property. No inferred
/// fields, `$ref`, unions or recursive schema traversal at this authorization seam.
fn field_schema<'a>(schema: &'a Value, path: &str) -> Option<&'a Value> {
    if path.len() > 400 || !path.starts_with('/') {
        return None;
    }
    let parts = path[1..].split('/').collect::<Vec<_>>();
    if parts.len() > 16 {
        return None;
    }
    let mut node = schema;
    for part in parts {
        if part.is_empty() || part.chars().any(char::is_control) {
            return None;
        }
        // Reject invalid JSON Pointer escapes rather than silently interpreting them.
        let mut chars = part.chars();
        while let Some(c) = chars.next() {
            if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
                return None;
            }
        }
        node = if part == "*" && node["type"] == "array" {
            node.get("items")?
        } else if node["type"] == "object" {
            node.get("properties")?
                .get(part.replace("~1", "/").replace("~0", "~"))?
        } else {
            return None;
        };
    }
    (node["type"] == "string").then_some(node)
}

impl Manifest {
    pub fn resource_type(&self, name: &str) -> Option<&ResourceType> {
        self.resources.iter().find(|kind| kind.name == name)
    }

    pub fn hook(&self, name: &str) -> Option<&Hook> {
        self.hooks.iter().find(|hook| hook.name == name)
    }

    pub(crate) fn validate_resources(&self) -> Result<(), &'static str> {
        if self.resources.len() > 32 || self.hooks.len() > 32 {
            return Err("Plugin declares too many resource types or hooks.");
        }
        let mut names = BTreeSet::new();
        for kind in &self.resources {
            if !slug(&kind.name, 40)
                || !names.insert(&kind.name)
                || kind.description.trim().is_empty()
                || !short(&kind.description, 300)
            {
                return Err("Plugin declares an invalid resource type.");
            }
            let mut seen = BTreeSet::from([kind.name.as_str()]);
            let mut parent = kind.parent_type.as_deref();
            while let Some(name) = parent {
                let Some(kind) = self.resource_type(name) else {
                    return Err("Plugin resource parent type is not declared.");
                };
                if !seen.insert(name) {
                    return Err("Plugin resource hierarchy contains a cycle.");
                }
                parent = kind.parent_type.as_deref();
            }
        }
        if self.hooks.is_empty() == self.permits_capability("hooks") {
            return Err("Plugin must declare hooks exactly when requesting the hooks capability.");
        }
        let mut names = BTreeSet::new();
        for hook in &self.hooks {
            if !slug(&hook.name, 40)
                || !names.insert(&hook.name)
                || hook.description.trim().is_empty()
                || !short(&hook.description, 4000)
                || hook.payload_schema["type"] != "object"
                || serde_json::to_vec(&hook.payload_schema).map_or(true, |v| v.len() > 16_384)
            {
                return Err("Plugin declares an invalid hook.");
            }
            self.validate_resource_fields(&hook.payload_schema, &hook.resource_fields)?;
        }
        for tool in &self.tools {
            self.validate_resource_fields(&tool.input_schema, &tool.resource_fields)?;
        }
        for argument in self.commands.iter().flat_map(|command| &command.arguments) {
            if argument
                .resource_type
                .as_deref()
                .is_some_and(|name| self.resource_type(name).is_none())
            {
                return Err("Plugin command argument names an undeclared resource type.");
            }
        }
        Ok(())
    }

    fn validate_resource_fields(
        &self,
        schema: &Value,
        fields: &[ResourceField],
    ) -> Result<(), &'static str> {
        if fields.len() > 32 {
            return Err("Plugin declares too many resource fields.");
        }
        let mut paths = BTreeSet::new();
        for field in fields {
            if self.resource_type(&field.resource_type).is_none()
                || !paths.insert(&field.path)
                || field_schema(schema, &field.path).is_none()
            {
                return Err("Plugin declares an invalid resource field.");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn declaration() -> Value {
        json!({
            "id": "probe", "name": "Probe", "version": "1", "apiVersion": 2,
            "capabilities": ["hooks"], "agentAccess": {},
            "resources": [
                { "name": "server", "description": "Server", "parentType": null },
                { "name": "channel", "description": "Channel", "parentType": "server" }
            ],
            "hooks": [{
                "name": "message", "description": "Message received",
                "payloadSchema": { "type": "object", "properties": {
                    "channel": { "type": "string" },
                    "people": { "type": "array", "items": { "type": "object", "properties": { "id": { "type": "string" } } } }
                } },
                "resourceFields": [{ "path": "/channel", "resourceType": "channel" }]
            }],
            "tools": [{
                "name": "probe_post", "description": "Post",
                "inputSchema": { "type": "object", "properties": { "channel": { "type": "string" } } },
                "resourceFields": [{ "path": "/channel", "resourceType": "channel" }]
            }],
            "commands": [{ "name": "post", "arguments": [{ "name": "channel", "positional": true, "resourceType": "channel" }] }]
        })
    }

    #[test]
    fn declarations_validate_in_json_and_toml() {
        let expected = Manifest::parse("json", &declaration().to_string()).unwrap();
        let toml = toml::to_string(&expected).unwrap();
        let actual = Manifest::parse("toml", &toml).unwrap();
        assert_eq!(actual, expected);
        assert!(actual.permits("hooks.emit"));
        assert!(actual.permits("instance.status"));
        assert!(!actual.permits("inbound.accept"));
    }

    #[test]
    fn invalid_hierarchies_hooks_and_references_are_refused() {
        for (path, value) in [
            ("/resources/0/parentType", json!("channel")),
            ("/resources/1/parentType", json!("missing")),
            ("/resources/1/name", json!("server")),
            ("/hooks/0/resourceFields/0/resourceType", json!("missing")),
            ("/hooks/0/payloadSchema/type", json!("string")),
            ("/hooks/0/name", json!("bad name")),
            ("/hooks/0/resourceFields/0/path", json!("/missing")),
            ("/hooks/0/resourceFields/0/path", json!("/people")),
            ("/tools/0/resourceFields/0/path", json!("/missing")),
            ("/commands/0/arguments/0/resourceType", json!("missing")),
            ("/capabilities", json!([])),
            ("/hooks", json!([])),
            ("/agentAccess", Value::Null),
        ] {
            let mut value_to_test = declaration();
            *value_to_test
                .pointer_mut(path)
                .unwrap_or_else(|| panic!("missing {path}")) = value;
            assert!(
                Manifest::parse("json", &value_to_test.to_string()).is_err(),
                "{path}"
            );
        }
        let mut duplicate = declaration();
        let fields = duplicate["hooks"][0]["resourceFields"]
            .as_array_mut()
            .unwrap();
        fields.push(fields[0].clone());
        assert!(Manifest::parse("json", &duplicate.to_string()).is_err());
    }

    #[test]
    fn resource_paths_traverse_nested_objects_arrays_and_escaped_properties() {
        let schema = json!({ "type": "object", "properties": {
            "a/b~c": { "type": "string" },
            "groups": { "type": "array", "items": { "type": "object", "properties": { "id": { "type": "string" } } } }
        } });
        assert!(field_schema(&schema, "/a~1b~0c").is_some());
        assert!(field_schema(&schema, "/groups/*/id").is_some());
        for path in [
            "groups/*/id",
            "/groups/id",
            "/groups/0/id",
            "/a~2b",
            "/groups/*",
            "",
        ] {
            assert!(field_schema(&schema, path).is_none(), "{path}");
        }
    }
}
