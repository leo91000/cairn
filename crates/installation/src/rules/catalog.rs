//! What the rule editor offers: hooks with their payload, plugin tools, action kinds,
//! conditions, operators, Cairn functions and the agents a person may designate.
use super::{formula, validation};
use crate::{error::Result, service::Service};
use cairn_contracts::{
    AgentId, AgentPluginGrants, CairnHookKind, Contract, HookRuleCatalog, HookRuleLimits,
    HookRuleTrigger, PluginContributionKind as Kind, PluginGrantAccess, PluginInstanceId,
    RuleAction, RuleCatalogAction, RuleCatalogAgent, RuleCatalogEntry, RuleCatalogFunction,
    RuleCatalogHook, RuleCatalogLimits, RuleCatalogTool, RuleCatalogVariable, RuleOperator,
    schema_for,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn origin_schema() -> Value {
    json!({
        "type": "object",
        "description": "What started the turn.",
        "properties": {
            "kind": { "type": "string", "enum": ["person", "rule", "agent", "installation", "resume"] },
            "accountId": { "type": "string", "description": "The person's Cairn account." },
            "ruleId": { "type": "string" },
        },
        "required": ["kind"],
    })
}

/// The payload Cairn emits for each of its hooks.
pub(crate) fn cairn_payload(hook: CairnHookKind) -> Value {
    let mut properties = json!({
        "key": { "type": "string", "description": "The conversation key, such as discord:123." },
        "conversationId": { "type": "string" },
        "agentId": { "type": "string" },
        "origin": origin_schema(),
    });
    let extra = match hook {
        CairnHookKind::ReplyReady => json!({
            "text": { "type": "string", "description": "The agent's reply." },
            "deliverables": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "mediaType": { "type": "string" },
                        "url": { "type": "string" },
                    },
                },
            },
        }),
        CairnHookKind::QuestionAsked => json!({
            "questionId": { "type": "string" },
            "title": { "type": "string" },
            "text": { "type": "string", "description": "The question as text." },
        }),
        CairnHookKind::Progress => json!({
            "step": { "type": "string", "description": "A short step, without detail." },
        }),
        CairnHookKind::Ended => json!({
            "outcome": { "type": "string", "enum": ["completed", "failed", "cancelled"] },
            "error": { "type": "string" },
        }),
    };
    for (name, schema) in extra.as_object().into_iter().flatten() {
        properties[name] = schema.clone();
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": ["key", "conversationId", "agentId", "origin"],
    })
}

fn cairn_hook(hook: CairnHookKind) -> (&'static str, &'static str) {
    match hook {
        CairnHookKind::ReplyReady => (
            "Reply ready",
            "The agent of a conversation bound to one of your keys replied, with its text and deliverables.",
        ),
        CairnHookKind::QuestionAsked => (
            "Question asked",
            "The agent of a conversation bound to one of your keys asks a question.",
        ),
        CairnHookKind::Progress => (
            "Progress",
            "A short progress step, about every 8 seconds, at most one pending per conversation. It does not count against the triggering cap.",
        ),
        CairnHookKind::Ended => (
            "Turn ended",
            "The turn of a conversation bound to one of your keys completed, failed or was cancelled.",
        ),
    }
}

fn action(kind: &str) -> (&'static str, &'static str, bool) {
    match kind {
        "callTool" => (
            "Call a plugin tool",
            "Calls a tool of a plugin instance through the execution agent's rights, never a Cairn tool.",
            false,
        ),
        "startAgent" => (
            "Start an agent",
            "Starts a conversation with an agent, optionally bound to a key. Fails when the key is already bound: steer it instead.",
            true,
        ),
        "steerConversation" => (
            "Steer a conversation",
            "Sends a message to the conversation bound to a key. Fails while a person keeps it paused or archived.",
            true,
        ),
        "cancelConversation" => (
            "Cancel a conversation",
            "Stops the current turn of the conversation bound to a key.",
            false,
        ),
        "answerQuestion" => (
            "Answer a question",
            "Answers the pending question of the conversation bound to a key.",
            false,
        ),
        "storeValue" => (
            "Remember a value",
            "Keeps a value with the rule, or with the conversation bound to a key, for later triggerings.",
            false,
        ),
        _ => (
            "Wait",
            "Waits, then resumes the next steps, for at most 7 days.",
            false,
        ),
    }
}

/// One JSON Schema per action kind, cut from the rule contract itself.
fn actions() -> Vec<RuleCatalogAction> {
    let schema = schema_for::<RuleAction>(Contract::Deserialize);
    let definitions = schema
        .get("definitions")
        .cloned()
        .unwrap_or_else(|| json!({}));
    schema["oneOf"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|variant| {
            let kind = variant["properties"]["type"]["const"]
                .as_str()
                .or_else(|| variant["properties"]["type"]["enum"][0].as_str())?
                .to_owned();
            let (title, description, conversation) = action(&kind);
            let mut input_schema = variant.clone();
            input_schema["definitions"] = definitions.clone();
            Some(RuleCatalogAction {
                kind,
                title: title.into(),
                description: description.into(),
                input_schema,
                conversation,
            })
        })
        .collect()
}

fn entry(kind: &str, title: &str, description: &str) -> RuleCatalogEntry {
    RuleCatalogEntry {
        kind: kind.into(),
        title: title.into(),
        description: description.into(),
    }
}

fn conditions() -> Vec<RuleCatalogEntry> {
    vec![
        entry("all", "All of", "Every condition of the group holds."),
        entry(
            "any",
            "Any of",
            "At least one condition of the group holds.",
        ),
        entry("not", "Not", "The condition does not hold."),
        entry(
            "compare",
            "Compare",
            "Compares a payload field or a formula with a value.",
        ),
        entry(
            "formula",
            "Formula",
            "A CEL expression that evaluates to true or false.",
        ),
        entry(
            "authorIn",
            "Author is one of",
            "The author is one of the people you choose.",
        ),
        entry(
            "authorStartedConversation",
            "Author started the conversation",
            "The author started the conversation bound to a key.",
        ),
        entry(
            "conversationExists",
            "Conversation exists",
            "A conversation is bound to a key.",
        ),
        entry(
            "authorQuota",
            "Author quota",
            "The author triggered this rule fewer times than a limit within a window.",
        ),
    ]
}

fn operators() -> Vec<RuleCatalogEntry> {
    RuleOperator::ALL
        .iter()
        .map(|operator| {
            let (title, description) = match operator {
                RuleOperator::Equals => ("is", "Equal to the value."),
                RuleOperator::NotEquals => ("is not", "Different from the value."),
                RuleOperator::Contains => (
                    "contains",
                    "Text containing the value, or a list holding it.",
                ),
                RuleOperator::StartsWith => ("starts with", "Text starting with the value."),
                RuleOperator::EndsWith => ("ends with", "Text ending with the value."),
                RuleOperator::Matches => {
                    ("matches", "Text matching a regular expression as a whole.")
                }
                RuleOperator::In => ("is one of", "One of the values of a list."),
                RuleOperator::Present => ("is present", "The field exists and is not null."),
                RuleOperator::Absent => ("is absent", "The field is missing or null."),
                RuleOperator::GreaterThan => {
                    ("is greater than", "A number or date after the value.")
                }
                RuleOperator::LessThan => ("is less than", "A number or date before the value."),
            };
            entry(operator.key(), title, description)
        })
        .collect()
}

fn functions() -> Vec<RuleCatalogFunction> {
    formula::FUNCTIONS
        .iter()
        .map(|function| RuleCatalogFunction {
            name: function.name.into(),
            signature: function.signature.into(),
            description: function.description.into(),
            example: function.example.into(),
        })
        .collect()
}

fn variables() -> Vec<RuleCatalogVariable> {
    [
        ("payload", "The event's payload. Its top-level fields are also variables of their own, such as threadId."),
        ("steps", "The results of earlier actions, by step name: steps.post.messageId."),
        ("values", "The values this rule remembered: values.lastReply."),
        ("event", "The event: event.id, event.hook, event.instanceId and event.receivedAt."),
        ("now", "When the triggering started, as a timestamp."),
    ]
    .into_iter()
    .map(|(name, description)| RuleCatalogVariable {
        name: name.into(),
        description: description.into(),
    })
    .collect()
}

pub(crate) fn limits() -> RuleCatalogLimits {
    RuleCatalogLimits {
        default_limits: HookRuleLimits::default(),
        max_triggerings_per_hour: validation::MAX_TRIGGERINGS_PER_HOUR,
        max_conversations_per_hour: validation::MAX_CONVERSATIONS_PER_HOUR,
        max_wait_seconds: validation::MAX_WAIT_SECONDS,
        max_steps: validation::MAX_STEPS,
        max_tool_calls: validation::MAX_TOOL_CALLS,
        max_nodes: validation::MAX_NODES as u32,
        max_recorded_bytes: Some(super::engine::MAX_RECORDED_BYTES as u32),
        max_cursor_bytes: Some(super::engine::MAX_CURSOR_BYTES as u32),
        max_rule_values_bytes: Some(super::engine::MAX_RULE_VALUES_BYTES as u32),
        max_pending: Some(super::engine::MAX_PENDING as u32),
    }
}

fn granted(grants: &AgentPluginGrants, instance: &str, kind: Kind, name: &str) -> bool {
    grants.grants.iter().any(|grant| {
        grant.instance_id.to_string() == instance
            && match &grant.access {
                PluginGrantAccess::Full => true,
                PluginGrantAccess::Selected { contributions } => contributions
                    .iter()
                    .any(|held| held.kind == kind && held.name == name),
            }
    })
}

/// The catalog for the agents a viewer may designate, or only one of them.
pub(crate) async fn build(
    s: &Service,
    usable: Vec<AgentId>,
    only: Option<AgentId>,
) -> Result<HookRuleCatalog> {
    let agents = usable
        .into_iter()
        .filter(|agent| only.is_none_or(|only| only == *agent))
        .collect::<Vec<_>>();
    let ids = agents.iter().map(ToString::to_string).collect::<Vec<_>>();
    let (profiles, grants, instances) = s
        .store
        .read(move |db| {
            let mut profiles = Vec::new();
            let mut grants = Vec::new();
            for id in &ids {
                if let Some(profile) = db.agent(id)? {
                    grants.push(db.agent_plugin_grants(id)?);
                    profiles.push(profile);
                }
            }
            Ok((profiles, grants, db.plugin_instances()?))
        })
        .await?;

    let mut hooks = Vec::new();
    let mut tools = Vec::new();
    for instance in instances {
        if !grants.iter().any(|grant| {
            grant
                .grants
                .iter()
                .any(|g| g.instance_id.to_string() == instance.id)
        }) {
            continue;
        }
        let Ok(package) = crate::plugins::registry::active(s, &instance.plugin_id).await else {
            continue;
        };
        let Ok(instance_id) = uuid::Uuid::parse_str(&instance.id).map(PluginInstanceId) else {
            continue;
        };
        let manifest = package.manifest;
        for hook in &manifest.hooks {
            let holders = grants
                .iter()
                .filter(|grant| granted(grant, &instance.id, Kind::Hook, &hook.name))
                .map(|grant| grant.agent_id)
                .collect::<Vec<_>>();
            if holders.is_empty() {
                continue;
            }
            hooks.push(RuleCatalogHook {
                trigger: HookRuleTrigger::Plugin {
                    instance_id,
                    hook: hook.name.clone(),
                },
                title: format!("{} · {}", instance.name, hook.name),
                description: hook.description.clone(),
                plugin_id: Some(instance.plugin_id.clone()),
                instance_name: Some(instance.name.clone()),
                payload_schema: hook.payload_schema.clone(),
                resource_fields: hook.resource_fields.clone(),
                agent_ids: holders,
            });
        }
        for tool in &manifest.tools {
            let holders = grants
                .iter()
                .filter(|grant| granted(grant, &instance.id, Kind::Tool, &tool.name))
                .map(|grant| grant.agent_id)
                .collect::<Vec<_>>();
            if holders.is_empty() {
                continue;
            }
            tools.push(RuleCatalogTool {
                instance_id,
                instance_name: instance.name.clone(),
                plugin_id: instance.plugin_id.clone(),
                tool: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
                resource_fields: tool.resource_fields.clone(),
                agent_ids: holders,
            });
        }
    }
    let every = profiles
        .iter()
        .map(|profile| profile.id)
        .collect::<Vec<_>>();
    for hook in CairnHookKind::ALL {
        let (title, description) = cairn_hook(hook);
        hooks.push(RuleCatalogHook {
            trigger: HookRuleTrigger::Cairn { hook },
            title: title.into(),
            description: description.into(),
            plugin_id: None,
            instance_name: None,
            payload_schema: cairn_payload(hook),
            resource_fields: Vec::new(),
            agent_ids: every.clone(),
        });
    }

    let mut by_name = BTreeMap::new();
    for profile in profiles {
        let snapshot = serde_json::to_value(&profile)?;
        by_name.insert(
            (profile.settings.name.to_lowercase(), profile.id.to_string()),
            RuleCatalogAgent {
                agent_id: profile.id,
                name: profile.settings.name,
                native: crate::native::policy::supports(&snapshot, "native"),
            },
        );
    }

    Ok(HookRuleCatalog {
        hooks,
        tools,
        actions: actions(),
        conditions: conditions(),
        operators: operators(),
        functions: functions(),
        variables: variables(),
        agents: by_name.into_values().collect(),
        limits: limits(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_kind_has_its_own_schema() {
        let kinds = actions()
            .into_iter()
            .map(|action| action.kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "callTool",
                "startAgent",
                "steerConversation",
                "cancelConversation",
                "answerQuestion",
                "storeValue",
                "wait"
            ]
        );
    }

    #[test]
    fn cairn_payloads_declare_their_key() {
        for hook in CairnHookKind::ALL {
            let schema = cairn_payload(hook);
            assert_eq!(schema["properties"]["key"]["type"], "string");
            assert!(jsonschema::is_valid(
                &schema,
                &json!({
                    "key": "discord:1", "conversationId": "c", "agentId": "a",
                    "origin": { "kind": "person" },
                })
            ));
        }
    }
}
