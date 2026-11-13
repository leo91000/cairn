//! Rule templates (ADR-0075, « Modèles de règles »): ready-made sets of hook rules a
//! plugin declares. Installing one asks for its parameters, then creates ordinary rules
//! owned by whoever installs it, with no link to the template.
use crate::{Manifest, client_default, short, slug};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const MAX_RULE_TEMPLATES: usize = 8;
pub const MAX_TEMPLATE_PARAMETERS: usize = 16;
pub const MAX_TEMPLATE_RULES: usize = 10;

/// What a parameter asks for.
#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, schemars::JsonSchema, ts_rs::TS,
)]
#[schemars(rename = "PluginRuleTemplateParameterKind")]
#[ts(rename = "PluginRuleTemplateParameterKind")]
#[serde(rename_all = "camelCase")]
pub enum RuleTemplateParameterKind {
    /// An agent the installer may use; its identifier.
    Agent,
    /// One resource of `resourceType`: `{resourceType, id}`.
    Resource,
    /// A list of resources of `resourceType`.
    Resources,
    Text,
    Number,
    Boolean,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, schemars::JsonSchema, ts_rs::TS)]
#[schemars(rename = "PluginRuleTemplateParameter")]
#[ts(rename = "PluginRuleTemplateParameter")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleTemplateParameter {
    /// Referenced by `{"$parameter": name}` in the template's rules.
    pub name: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[schemars(transform = client_default::<String>)]
    pub description: String,
    #[schemars(
        with = "String",
        pattern("^(agent|resource|resources|text|number|boolean)$")
    )]
    pub kind: RuleTemplateParameterKind,
    /// A resource type the manifest declares, for `resource` and `resources`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[schemars(transform = client_default::<bool>)]
    pub required: bool,
    /// For `text`, `number` and `boolean` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "any ")]
    pub default: Option<Value>,
}

/// A set of hook rules. Each rule is a hook rule definition without the instance: its
/// plugin trigger and tool calls use the instance it is installed from. Any JSON value
/// may be `{"$parameter": name}`, or `{"$parameter": name, "field": "id"}` for the
/// identifiers of a resource parameter.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, schemars::JsonSchema, ts_rs::TS)]
#[schemars(rename = "PluginRuleTemplate")]
#[ts(rename = "PluginRuleTemplate")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleTemplate {
    pub name: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[schemars(transform = client_default::<String>)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(transform = client_default::<Vec<RuleTemplateParameter>>)]
    pub parameters: Vec<RuleTemplateParameter>,
    #[ts(type = "Array<any> ")]
    pub rules: Vec<Value>,
}

impl RuleTemplate {
    pub fn parameter(&self, name: &str) -> Option<&RuleTemplateParameter> {
        self.parameters
            .iter()
            .find(|parameter| parameter.name == name)
    }
}

/// A placeholder: the parameter it names and whether it selects resource identifiers.
pub struct Placeholder<'a> {
    pub name: &'a str,
    pub ids: bool,
}

/// Reads a placeholder object. `Some(Err)` is a malformed one.
pub fn placeholder(value: &Value) -> Option<Result<Placeholder<'_>, &'static str>> {
    let fields = value.as_object()?;
    let name = fields.get("$parameter")?;
    let Some(name) = name.as_str() else {
        return Some(Err("A rule template placeholder must name a parameter."));
    };
    let ids = match fields.get("field") {
        None => false,
        Some(field) if field == "id" => true,
        Some(_) => {
            return Some(Err(
                "A rule template placeholder selects only the field id.",
            ));
        }
    };
    if fields.len() != 1 + usize::from(ids) {
        return Some(Err("A rule template placeholder has unknown fields."));
    }
    Some(Ok(Placeholder { name, ids }))
}

fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn placeholders(
    value: &Value,
    template: &RuleTemplate,
    used: &mut BTreeSet<String>,
) -> Result<(), &'static str> {
    if let Some(found) = placeholder(value) {
        let found = found?;
        let Some(parameter) = template.parameter(found.name) else {
            return Err("A rule template uses an undeclared parameter.");
        };
        if found.ids
            && !matches!(
                parameter.kind,
                RuleTemplateParameterKind::Resource | RuleTemplateParameterKind::Resources
            )
        {
            return Err("Only resource parameters select the field id.");
        }
        used.insert(found.name.to_owned());
        return Ok(());
    }
    match value {
        Value::Array(items) => items
            .iter()
            .try_for_each(|item| placeholders(item, template, used)),
        Value::Object(fields) => fields
            .values()
            .try_for_each(|item| placeholders(item, template, used)),
        _ => Ok(()),
    }
}

impl Manifest {
    pub fn rule_template(&self, name: &str) -> Option<&RuleTemplate> {
        self.rule_templates
            .iter()
            .find(|template| template.name == name)
    }

    /// The structure of each template. The host also checks that every rule, with sample
    /// parameters, is a hook rule definition using this manifest's hooks and tools.
    pub(crate) fn validate_rule_templates(&self) -> Result<(), &'static str> {
        if self.rule_templates.len() > MAX_RULE_TEMPLATES {
            return Err("Plugin declares too many rule templates.");
        }
        let mut names = BTreeSet::new();
        for template in &self.rule_templates {
            if !slug(&template.name, 40)
                || !names.insert(&template.name)
                || template.title.trim().is_empty()
                || !short(&template.title, 100)
                || template.title.contains('\n')
                || !short(&template.description, 2000)
            {
                return Err("Plugin declares an invalid rule template.");
            }
            if template.parameters.len() > MAX_TEMPLATE_PARAMETERS {
                return Err("A rule template declares too many parameters.");
            }
            let mut parameters = BTreeSet::new();
            for parameter in &template.parameters {
                if !identifier(&parameter.name)
                    || !parameters.insert(&parameter.name)
                    || parameter.title.trim().is_empty()
                    || !short(&parameter.title, 100)
                    || parameter.title.contains('\n')
                    || !short(&parameter.description, 300)
                {
                    return Err("A rule template declares an invalid parameter.");
                }
                let resource = matches!(
                    parameter.kind,
                    RuleTemplateParameterKind::Resource | RuleTemplateParameterKind::Resources
                );
                match &parameter.resource_type {
                    Some(kind) if resource && self.resource_type(kind).is_some() => {}
                    None if !resource => {}
                    _ => {
                        return Err(
                            "A resource parameter names a resource type the plugin declares; other parameters name none.",
                        );
                    }
                }
                if let Some(default) = &parameter.default {
                    let valid = match parameter.kind {
                        RuleTemplateParameterKind::Text => {
                            default.as_str().is_some_and(|text| short(text, 2000))
                        }
                        RuleTemplateParameterKind::Number => default.is_number(),
                        RuleTemplateParameterKind::Boolean => default.is_boolean(),
                        _ => false,
                    };
                    if !valid {
                        return Err(
                            "Only text, number and boolean parameters have a default, of their kind.",
                        );
                    }
                }
            }
            if template.rules.is_empty() || template.rules.len() > MAX_TEMPLATE_RULES {
                return Err("A rule template declares 1 to 10 rules.");
            }
            let mut used = BTreeSet::new();
            for rule in &template.rules {
                if !rule.is_object() {
                    return Err("A rule template's rules are objects.");
                }
                placeholders(rule, template, &mut used)?;
            }
            if template
                .parameters
                .iter()
                .any(|parameter| !used.contains(&parameter.name))
            {
                return Err("A rule template declares a parameter its rules do not use.");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::Manifest;
    use serde_json::{Value, json};

    fn manifest(templates: Value) -> Result<(), &'static str> {
        serde_json::from_value::<Manifest>(json!({
            "id": "chat", "name": "Chat", "version": "1", "apiVersion": 2,
            "capabilities": ["hooks"], "agentAccess": {},
            "resources": [{ "name": "channel", "description": "Channel" }],
            "hooks": [{ "name": "message", "description": "Message", "payloadSchema": { "type": "object" } }],
            "ruleTemplates": templates,
        }))
        .map_err(|_| "parse")?
        .validate()
    }

    fn template() -> Value {
        json!({
            "name": "support",
            "title": "Support",
            "description": "Answers a channel.",
            "parameters": [
                { "name": "agent", "title": "Agent", "kind": "agent", "required": true },
                { "name": "channel", "title": "Channel", "kind": "resource", "resourceType": "channel" },
                { "name": "greeting", "title": "Greeting", "kind": "text", "default": "Hello" },
            ],
            "rules": [{
                "name": "Support",
                "trigger": { "type": "plugin", "hook": "message" },
                "executionAgentId": { "$parameter": "agent" },
                "steps": [{
                    "type": "if", "id": "here",
                    "condition": {
                        "type": "compare",
                        "left": { "type": "formula", "expression": "channel" },
                        "operator": "equals",
                        "right": { "type": "literal", "value": { "$parameter": "channel", "field": "id" } },
                    },
                    "then": [{ "type": "action", "id": "start", "action": {
                        "type": "startAgent",
                        "agentId": { "$parameter": "agent" },
                        "prompt": { "type": "literal", "value": { "$parameter": "greeting" } },
                    }}],
                }],
            }],
        })
    }

    #[test]
    fn templates_declare_used_parameters_of_known_kinds() {
        assert!(manifest(json!([template()])).is_ok());
        assert!(manifest(json!([template(), template()])).is_err());

        let invalid: [(&str, Value); 11] = [
            ("/name", json!("Not a slug")),
            ("/title", json!(" ")),
            ("/rules", json!([])),
            ("/rules/0", json!("rule")),
            ("/parameters/0/name", json!("Agent")),
            ("/parameters/1/name", json!("agent")),
            ("/parameters/1/resourceType", json!("server")),
            ("/parameters/0/resourceType", json!("channel")),
            ("/parameters/0/default", json!("agent-id")),
            ("/parameters/2/default", json!(3)),
            (
                "/rules/0/executionAgentId",
                json!({ "$parameter": "missing" }),
            ),
        ];
        for (pointer, value) in invalid {
            let mut changed = template();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            match changed.pointer_mut(parent).unwrap() {
                Value::Array(items) => items[key.parse::<usize>().unwrap()] = value,
                parent => parent[key] = value,
            }
            assert!(manifest(json!([changed])).is_err(), "{pointer}");
        }

        let mut unused = template();
        unused["parameters"][2]["name"] = "other".into();
        assert!(manifest(json!([unused])).is_err());
        let mut field = template();
        field["rules"][0]["executionAgentId"]["field"] = "id".into();
        assert!(manifest(json!([field])).is_err());
        let mut extra = template();
        extra["rules"][0]["executionAgentId"]["other"] = true.into();
        assert!(manifest(json!([extra])).is_err());
    }

    #[test]
    fn the_approved_digest_covers_the_templates() {
        let parse = |templates: Value| {
            serde_json::from_value::<Manifest>(json!({
                "id": "chat", "name": "Chat", "version": "1", "apiVersion": 2,
                "capabilities": ["hooks"], "agentAccess": {},
                "resources": [{ "name": "channel", "description": "Channel" }],
                "hooks": [{ "name": "message", "description": "Message", "payloadSchema": { "type": "object" } }],
                "ruleTemplates": templates,
            }))
            .unwrap()
        };
        let source = crate::Sources::default();
        let original = source.digest(&parse(json!([template()]))).unwrap();
        let mut changed = template();
        changed["rules"][0]["name"] = "Changed".into();
        assert_ne!(original, source.digest(&parse(json!([changed]))).unwrap());
        assert_eq!(
            original,
            source.digest(&parse(json!([template()]))).unwrap()
        );
    }
}
