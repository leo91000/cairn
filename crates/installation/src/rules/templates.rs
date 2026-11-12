//! Rule templates (ADR-0075, « Modèles de règles »). Installing one checks its
//! parameters, then creates ordinary rules owned by the installer through the same
//! validation and authority as `create_hook_rule`. Nothing links them to the template:
//! a plugin update never changes them.
use super::*;
use cairn_plugin_api::{
    Manifest, RuleTemplate, RuleTemplateParameter, RuleTemplateParameterKind as ParameterKind,
    placeholder,
};
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_RESOURCES: usize = 200;

/// The instance a template is installed from: its plugin trigger and tool calls.
fn imply_instance(rule: &mut Value, instance: &str) -> std::result::Result<(), String> {
    fn nodes(nodes: &mut Value, instance: &str) -> std::result::Result<(), String> {
        let Some(nodes) = nodes.as_array_mut() else {
            return Ok(());
        };
        for node in nodes {
            match node["type"].as_str() {
                Some("if") => {
                    nodes_in(node, "then", instance)?;
                    nodes_in(node, "otherwise", instance)?;
                }
                Some("forEach") => nodes_in(node, "body", instance)?,
                Some("action") => {
                    if let Some(action) = node.get_mut("action")
                        && action["type"] == "callTool"
                    {
                        set_instance(action, instance)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn nodes_in(node: &mut Value, key: &str, instance: &str) -> std::result::Result<(), String> {
        match node.get_mut(key) {
            Some(children) => nodes(children, instance),
            None => Ok(()),
        }
    }
    fn set_instance(target: &mut Value, instance: &str) -> std::result::Result<(), String> {
        let Some(fields) = target.as_object_mut() else {
            return Ok(());
        };
        if fields.contains_key("instanceId") {
            return Err(
                "A template's rules name no instance: they use the one they are installed from."
                    .into(),
            );
        }
        fields.insert("instanceId".into(), instance.into());
        Ok(())
    }

    if let Some(trigger) = rule.get_mut("trigger")
        && trigger["type"] == "plugin"
    {
        set_instance(trigger, instance)?;
    }
    if let Some(steps) = rule.get_mut("steps") {
        nodes(steps, instance)?;
    }
    if let Some(fields) = rule.as_object_mut()
        && !fields.contains_key("limits")
    {
        fields.insert(
            "limits".into(),
            serde_json::to_value(HookRuleLimits::default()).expect("limits"),
        );
    }
    Ok(())
}

fn substitute(
    value: &mut Value,
    values: &BTreeMap<String, Value>,
) -> std::result::Result<(), String> {
    if let Some(found) = placeholder(value) {
        let found = found.map_err(str::to_owned)?;
        let mut replaced = values.get(found.name).cloned().unwrap_or(Value::Null);
        if found.ids {
            replaced = match replaced {
                Value::Array(items) => items.into_iter().map(|item| item["id"].clone()).collect(),
                Value::Null => Value::Null,
                resource => resource["id"].clone(),
            };
        }
        *value = replaced;
        return Ok(());
    }
    match value {
        Value::Array(items) => items
            .iter_mut()
            .try_for_each(|item| substitute(item, values)),
        Value::Object(fields) => fields
            .values_mut()
            .try_for_each(|item| substitute(item, values)),
        _ => Ok(()),
    }
}

/// The template's rules for these parameter values, which must already be checked.
pub(crate) fn instantiate(
    manifest: &Manifest,
    template: &RuleTemplate,
    instance: &str,
    values: &BTreeMap<String, Value>,
) -> std::result::Result<Vec<HookRuleDefinition>, String> {
    let mut definitions = Vec::new();
    for (index, rule) in template.rules.iter().enumerate() {
        let number = index + 1;
        let mut rule = rule.clone();
        imply_instance(&mut rule, instance)
            .map_err(|message| format!("Rule {number}: {message}"))?;
        substitute(&mut rule, values).map_err(|message| format!("Rule {number}: {message}"))?;
        let definition: HookRuleDefinition = serde_json::from_value(rule)
            .map_err(|error| format!("Rule {number} is not a valid hook rule: {error}"))?;
        if let HookRuleTrigger::Plugin { hook, .. } = &definition.trigger
            && manifest.hook(hook).is_none()
        {
            return Err(format!(
                "Rule {number} listens to the hook {hook}, which the plugin does not declare."
            ));
        }
        let mut undeclared = None;
        validation::visit_actions(&definition.steps, &mut |action| {
            if let RuleAction::CallTool { tool, .. } = action
                && manifest.tool(tool).is_none()
            {
                undeclared.get_or_insert_with(|| tool.clone());
            }
        });
        if let Some(tool) = undeclared {
            return Err(format!(
                "Rule {number} calls the tool {tool}, which the plugin does not declare."
            ));
        }
        definitions.push(definition);
    }
    Ok(definitions)
}

fn sample(parameter: &RuleTemplateParameter) -> Value {
    let resource = || {
        json!({
            "resourceType": parameter.resource_type.clone().unwrap_or_default(),
            "id": "sample",
        })
    };
    match parameter.kind {
        ParameterKind::Agent => json!(uuid::Uuid::nil()),
        ParameterKind::Resource => resource(),
        ParameterKind::Resources => json!([resource()]),
        ParameterKind::Text => json!("sample"),
        ParameterKind::Number => json!(1),
        ParameterKind::Boolean => json!(false),
    }
}

/// Package admission: every rule of every template, with all parameters given and with
/// only the required ones, is a hook rule using the hooks and tools this manifest
/// declares, with a valid tree and formulas. Installing checks the rest against the
/// installation.
pub(crate) fn check_manifest(manifest: &Manifest) -> std::result::Result<(), String> {
    let instance = uuid::Uuid::nil().to_string();
    for template in &manifest.rule_templates {
        let full = template
            .parameters
            .iter()
            .map(|parameter| (parameter.name.clone(), sample(parameter)))
            .collect::<BTreeMap<_, _>>();
        let minimal = template
            .parameters
            .iter()
            .filter_map(|parameter| {
                let value = match &parameter.default {
                    Some(default) => default.clone(),
                    None if parameter.required => sample(parameter),
                    None => return None,
                };
                Some((parameter.name.clone(), value))
            })
            .collect::<BTreeMap<_, _>>();
        for values in [full, minimal] {
            let definitions = instantiate(manifest, template, &instance, &values)
                .map_err(|message| format!("Rule template {}: {message}", template.name))?;
            for (index, definition) in definitions.iter().enumerate() {
                let payload = match &definition.trigger {
                    HookRuleTrigger::Plugin { hook, .. } => manifest
                        .hook(hook)
                        .map(|declaration| declaration.payload_schema.clone()),
                    HookRuleTrigger::Cairn { hook } => Some(catalog::cairn_payload(*hook)),
                };
                if let Some(problem) = validation::structure(definition, payload.as_ref()).first() {
                    return Err(format!(
                        "Rule template {}: Rule {}: {} [{}]",
                        template.name,
                        index + 1,
                        problem.message,
                        problem.path
                    ));
                }
            }
        }
    }
    Ok(())
}

fn resource(value: &Value, parameter: &RuleTemplateParameter) -> Result<()> {
    let reference: PluginResourceRef = serde_json::from_value(value.clone()).map_err(|_| {
        Error::bad(format!(
            "{} takes resources written {{resourceType, id}}.",
            parameter.title
        ))
    })?;
    let id = reference.id.trim();
    if Some(&reference.resource_type) != parameter.resource_type.as_ref()
        || id.is_empty()
        || reference.id.chars().count() > 200
        || reference.id.chars().any(char::is_control)
    {
        return Err(Error::bad(format!(
            "{} takes a resource of type {}.",
            parameter.title,
            parameter.resource_type.as_deref().unwrap_or_default()
        )));
    }
    Ok(())
}

/// Checks the values a person gives, applying defaults. Agents are checked against the
/// installer in the write transaction ([`authorize_agents`]).
pub(crate) fn values(
    template: &RuleTemplate,
    given: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, Value>> {
    if let Some(unknown) = given.keys().find(|name| template.parameter(name).is_none()) {
        return Err(Error::bad(format!(
            "This template has no parameter {unknown}."
        )));
    }
    let mut values = BTreeMap::new();
    for parameter in &template.parameters {
        let value = given
            .get(&parameter.name)
            .filter(|value| !value.is_null())
            .filter(|value| {
                parameter.kind != ParameterKind::Resources
                    || value.as_array().is_none_or(|items| !items.is_empty())
            })
            .or(parameter.default.as_ref());
        let Some(value) = value else {
            if parameter.required {
                return Err(Error::bad(format!("Enter {}.", parameter.title)));
            }
            continue;
        };
        let title = &parameter.title;
        match parameter.kind {
            ParameterKind::Agent => {
                let Some(agent) = value.as_str().and_then(|id| uuid::Uuid::parse_str(id).ok())
                else {
                    return Err(Error::bad(format!("{title} takes an agent identifier.")));
                };
                values.insert(parameter.name.clone(), json!(agent));
                continue;
            }
            ParameterKind::Resource => resource(value, parameter)?,
            ParameterKind::Resources => {
                let Some(items) = value.as_array() else {
                    return Err(Error::bad(format!("{title} takes a list of resources.")));
                };
                if items.len() > MAX_RESOURCES {
                    return Err(Error::bad(format!(
                        "{title} takes at most {MAX_RESOURCES} resources."
                    )));
                }
                for item in items {
                    resource(item, parameter)?;
                }
            }
            ParameterKind::Text => {
                if value
                    .as_str()
                    .is_none_or(|text| text.len() > validation::MAX_LITERAL)
                {
                    return Err(Error::bad(format!(
                        "{title} takes a text of at most 16 KB."
                    )));
                }
            }
            ParameterKind::Number => {
                if !value.is_number() {
                    return Err(Error::bad(format!("{title} takes a number.")));
                }
            }
            ParameterKind::Boolean => {
                if !value.is_boolean() {
                    return Err(Error::bad(format!("{title} takes true or false.")));
                }
            }
        }
        values.insert(parameter.name.clone(), value.clone());
    }
    Ok(values)
}

/// The installer must have access to every agent the parameters name, wherever the
/// template uses them.
pub(crate) fn authorize_agents(
    db: &Db<'_>,
    creator: &Creator,
    template: &RuleTemplate,
    values: &BTreeMap<String, Value>,
) -> Result<()> {
    for parameter in &template.parameters {
        if parameter.kind != ParameterKind::Agent {
            continue;
        }
        let Some(agent) = values.get(&parameter.name).and_then(Value::as_str) else {
            continue;
        };
        if db.agent(agent)?.is_none() {
            return Err(Error::bad(format!(
                "{}: this agent no longer exists.",
                parameter.title
            )));
        }
        if !creator.uses_agent(db, agent)? {
            return Err(Error::bad(format!(
                "{}: you don't have access to this agent. Ask the installation owner to give you access.",
                parameter.title
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(rules: &Value) -> Manifest {
        serde_json::from_value(json!({
            "id": "chat", "name": "Chat", "version": "1", "apiVersion": 2,
            "capabilities": ["hooks"], "agentAccess": {},
            "resources": [{ "name": "channel", "description": "Channel" }],
            "hooks": [{
                "name": "message",
                "description": "Message",
                "payloadSchema": { "type": "object", "properties": { "channel": { "type": "string" } } },
            }],
            "tools": [{ "name": "chat_post", "description": "Post", "inputSchema": { "type": "object" } }],
            "ruleTemplates": [{
                "name": "support",
                "title": "Support",
                "parameters": [
                    { "name": "agent", "title": "Agent", "kind": "agent", "required": true },
                    { "name": "channels", "title": "Channels", "kind": "resources", "resourceType": "channel" },
                    { "name": "greeting", "title": "Greeting", "kind": "text", "default": "Hello" },
                ],
                "rules": rules,
            }],
        }))
        .unwrap()
    }

    fn rule() -> Value {
        json!({
            "name": "Support",
            "trigger": { "type": "plugin", "hook": "message" },
            "executionAgentId": { "$parameter": "agent" },
            "steps": [{
                "type": "if", "id": "here",
                "condition": {
                    "type": "compare",
                    "left": { "type": "formula", "expression": "channel" },
                    "operator": "in",
                    "right": { "type": "literal", "value": { "$parameter": "channels", "field": "id" } },
                },
                "then": [{ "type": "action", "id": "post", "action": {
                    "type": "callTool",
                    "tool": "chat_post",
                    "input": { "text": { "type": "literal", "value": { "$parameter": "greeting" } } },
                }}],
            }],
        })
    }

    #[test]
    fn instantiation_substitutes_parameters_and_implies_the_instance() {
        let manifest = manifest(&json!([rule()]));
        manifest.validate().unwrap();
        check_manifest(&manifest).unwrap();
        let template = &manifest.rule_templates[0];
        let agent = uuid::Uuid::new_v4().to_string();
        let instance = uuid::Uuid::new_v4().to_string();
        let given = BTreeMap::from([
            ("agent".to_owned(), json!(agent)),
            (
                "channels".to_owned(),
                json!([{ "resourceType": "channel", "id": "support" }]),
            ),
        ]);
        let values = values(template, &given).unwrap();
        assert_eq!(values["greeting"], "Hello");
        let definitions = instantiate(&manifest, template, &instance, &values).unwrap();
        let definition = serde_json::to_value(&definitions[0]).unwrap();
        assert_eq!(definition["executionAgentId"], agent);
        assert_eq!(definition["trigger"]["instanceId"], instance);
        assert_eq!(definition["limits"]["triggeringsPerHour"], 60);
        let then = &definition["steps"][0]["then"][0]["action"];
        assert_eq!(then["instanceId"], instance);
        assert_eq!(then["input"]["text"]["value"], "Hello");
        assert_eq!(
            definition["steps"][0]["condition"]["right"]["value"],
            json!(["support"])
        );
    }

    #[test]
    fn admission_refuses_rules_that_are_not_rules_of_this_plugin() {
        let mut named = rule();
        named["trigger"]["instanceId"] = json!(uuid::Uuid::new_v4());
        let mut hook = rule();
        hook["trigger"]["hook"] = "reaction".into();
        let mut tool = rule();
        tool["steps"][0]["then"][0]["action"]["tool"] = "chat_other".into();
        let mut shape = rule();
        shape["steps"][0]["then"] = json!({ "$parameter": "greeting" });
        let mut optional = rule();
        optional["name"] = json!({ "$parameter": "greeting" });
        optional["steps"][0]["then"][0]["action"]["input"] = json!({ "$parameter": "channels" });
        let mut formula = rule();
        formula["steps"][0]["condition"]["left"]["expression"] = "unknown_variable".into();
        let mut limits = rule();
        limits["limits"] = json!({ "triggeringsPerHour": 1000, "conversationsPerHour": 5 });
        for (label, invalid) in [
            ("formula", formula),
            ("limits", limits),
            ("instance", named),
            ("hook", hook),
            ("tool", tool),
            ("shape", shape),
            ("optional", optional),
        ] {
            let manifest = manifest(&json!([invalid]));
            manifest.validate().unwrap();
            assert!(check_manifest(&manifest).is_err(), "{label}");
        }
    }

    #[test]
    fn values_follow_the_parameter_kinds() {
        let manifest = manifest(&json!([rule()]));
        let template = &manifest.rule_templates[0];
        let agent = json!(uuid::Uuid::new_v4());
        let refused = [
            json!({}),
            json!({ "agent": "not-an-id" }),
            json!({ "agent": agent, "other": 1 }),
            json!({ "agent": agent, "greeting": 3 }),
            json!({ "agent": agent, "greeting": [] }),
            json!({ "agent": [] }),
            json!({ "agent": agent, "channels": [{ "resourceType": "server", "id": "a" }] }),
            json!({ "agent": agent, "channels": [{ "resourceType": "channel", "id": " " }] }),
            json!({ "agent": agent, "channels": { "resourceType": "channel", "id": "a" } }),
        ];
        for given in refused {
            let given = serde_json::from_value(given.clone()).unwrap();
            assert!(values(template, &given).is_err(), "{given:?}");
        }
        let given = serde_json::from_value(json!({ "agent": agent, "channels": [] })).unwrap();
        let accepted = values(template, &given).unwrap();
        assert!(!accepted.contains_key("channels"));
    }
}
