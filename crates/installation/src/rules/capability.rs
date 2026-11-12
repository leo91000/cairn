use super::*;
use crate::capabilities::ResumeContext;
use cairn_contracts::{Contract, schema_for};
use serde_json::Value;

pub(crate) struct RuleOperation {
    pub name: &'static str,
    scope: &'static str,
    description: &'static str,
}

pub(crate) const RULE_OPERATIONS: &[RuleOperation] = &[
    RuleOperation {
        name: "list_hook_rules",
        scope: "read",
        description: "List every hook rule of the installation with its state, optionally those that use an agent or a plugin instance. Every member sees every rule.",
    },
    RuleOperation {
        name: "get_hook_rule",
        scope: "read",
        description: "Read a hook rule: its hook, execution agent, tree of steps, limits, state and what the caller may do with it.",
    },
    RuleOperation {
        name: "list_hook_rule_versions",
        scope: "read",
        description: "List a rule's immutable versions, newest first, with who saved each one, from where, in which conversation and with which native authority.",
    },
    RuleOperation {
        name: "list_hook_rule_triggerings",
        scope: "read",
        description: "List a rule's triggerings, newest first, with how many ended without any action in the last day. Every member sees their state; the event, actions and errors only its creator, the owner and members with access to its execution agent.",
    },
    RuleOperation {
        name: "get_hook_rule_triggering",
        scope: "read",
        description: "Read one triggering: its state and, for those who may see them, its event payload, each action's input, result and error.",
    },
    RuleOperation {
        name: "get_hook_rule_catalog",
        scope: "read",
        description: "What a rule may use: hooks with payload schemas, plugin tools with input schemas, action kinds, conditions, operators, Cairn CEL functions, variables, designatable agents and limits.",
    },
    RuleOperation {
        name: "validate_hook_rule",
        scope: "read",
        description: "Check a rule definition as saving would, without writing: problems located by JSON Pointer, and warnings.",
    },
    RuleOperation {
        name: "create_hook_rule",
        scope: "manage",
        description: "Create a hook rule for the owner who authorized this connection; creationId makes retries return the same rule. Installation agents write rules only for the person of a conversation that this person alone wrote, never in tasks or missions.",
    },
    RuleOperation {
        name: "replace_hook_rule",
        scope: "manage",
        description: "Save a new version of a rule at its current revision. Only its creator changes a rule; a triggering keeps the version it started with.",
    },
    RuleOperation {
        name: "set_hook_rule_enabled",
        scope: "manage",
        description: "Enable or disable a rule. Only its creator does; disabling stops its triggerings in progress.",
    },
    RuleOperation {
        name: "set_hook_rule_emergency_pause",
        scope: "manage",
        description: "Pause or resume any rule in an emergency. Only the installation owner does; pausing stops its triggerings in progress.",
    },
    RuleOperation {
        name: "lift_hook_rule_budget",
        scope: "manage",
        description: "Resume a rule suspended for exceeding an hourly cap. Its creator or the installation owner does.",
    },
    RuleOperation {
        name: "duplicate_hook_rule",
        scope: "manage",
        description: "Copy a rule for yourself, with access to every agent it uses. The copy starts disabled.",
    },
    RuleOperation {
        name: "delete_hook_rule",
        scope: "manage",
        description: "Delete a rule and stop its triggerings in progress. Its creator or the installation owner does.",
    },
];

pub(crate) fn rule_operation(name: &str) -> Option<&'static RuleOperation> {
    RULE_OPERATIONS.iter().find(|op| op.name == name)
}

fn decode<T: serde::de::DeserializeOwned>(input: Value) -> Result<T> {
    serde_json::from_value(input).map_err(|error| Error::bad(error.to_string()))
}

impl RuleOperation {
    pub(crate) fn descriptor(&self) -> Value {
        let (input, output) = match self.name {
            "list_hook_rules" => (
                schema_for::<ListHookRulesInput>(Contract::Deserialize),
                schema_for::<ListHookRulesOutput>(Contract::Serialize),
            ),
            "list_hook_rule_versions" => (
                schema_for::<HookRuleInput>(Contract::Deserialize),
                schema_for::<ListHookRuleVersionsOutput>(Contract::Serialize),
            ),
            "list_hook_rule_triggerings" => (
                schema_for::<ListHookRuleTriggeringsInput>(Contract::Deserialize),
                schema_for::<ListHookRuleTriggeringsOutput>(Contract::Serialize),
            ),
            "get_hook_rule_triggering" => (
                schema_for::<HookRuleTriggeringInput>(Contract::Deserialize),
                schema_for::<HookRuleTriggering>(Contract::Serialize),
            ),
            "get_hook_rule_catalog" => (
                schema_for::<GetHookRuleCatalogInput>(Contract::Deserialize),
                schema_for::<HookRuleCatalog>(Contract::Serialize),
            ),
            "validate_hook_rule" => (
                schema_for::<ValidateHookRuleInput>(Contract::Deserialize),
                schema_for::<ValidateHookRuleOutput>(Contract::Serialize),
            ),
            "create_hook_rule" => (
                schema_for::<CreateHookRuleInput>(Contract::Deserialize),
                schema_for::<HookRule>(Contract::Serialize),
            ),
            "replace_hook_rule" => (
                schema_for::<ReplaceHookRuleInput>(Contract::Deserialize),
                schema_for::<HookRule>(Contract::Serialize),
            ),
            "set_hook_rule_enabled" => (
                schema_for::<SetHookRuleEnabledInput>(Contract::Deserialize),
                schema_for::<HookRule>(Contract::Serialize),
            ),
            "set_hook_rule_emergency_pause" => (
                schema_for::<SetHookRuleEmergencyPauseInput>(Contract::Deserialize),
                schema_for::<HookRule>(Contract::Serialize),
            ),
            "duplicate_hook_rule" => (
                schema_for::<DuplicateHookRuleInput>(Contract::Deserialize),
                schema_for::<HookRule>(Contract::Serialize),
            ),
            "delete_hook_rule" => (
                schema_for::<HookRuleInput>(Contract::Deserialize),
                schema_for::<DeleteHookRuleOutput>(Contract::Serialize),
            ),
            _ => (
                schema_for::<HookRuleInput>(Contract::Deserialize),
                schema_for::<HookRule>(Contract::Serialize),
            ),
        };
        let read_only = self.scope == "read";
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": input,
            "outputSchema": output,
            "_meta": { "securitySchemes": [{ "type": "oauth2", "scopes": [self.scope] }] },
            "annotations": {
                "readOnlyHint": read_only,
                "destructiveHint": !read_only,
                "openWorldHint": false,
            },
        })
    }

    /// The public MCP and `cairn_installation`. The apps call [`Self::invoke_as`] with
    /// the person's identity.
    pub(crate) async fn invoke(
        &self,
        s: &Service,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        let actor = match context {
            ResumeContext::Human(account) => Actor::Person(InstallationIdentity::trusted(
                InstallationRole::Owner,
                &account,
            )),
            ResumeContext::Public(scopes, _) => Actor::Public(scopes, None),
            ResumeContext::Installation(bearer) => {
                let authorize =
                    crate::installation_mcp::management_authorization(s, Some(&bearer)).await?;
                Actor::Installation(bearer, authorize)
            }
        };
        self.invoke_as(s, input, actor).await
    }

    pub(crate) async fn invoke_as(&self, s: &Service, input: Value, actor: Actor) -> Result<Value> {
        Ok(match self.name {
            "list_hook_rules" => serde_json::to_value(list(s, decode(input)?, actor).await?)?,
            "get_hook_rule" => serde_json::to_value(get(s, decode(input)?, actor).await?)?,
            "list_hook_rule_versions" => {
                serde_json::to_value(versions(s, decode(input)?, actor).await?)?
            }
            "get_hook_rule_catalog" => {
                serde_json::to_value(catalog(s, decode(input)?, actor).await?)?
            }
            "list_hook_rule_triggerings" => {
                serde_json::to_value(triggerings(s, decode(input)?, actor).await?)?
            }
            "get_hook_rule_triggering" => {
                serde_json::to_value(triggering(s, decode(input)?, actor).await?)?
            }
            "validate_hook_rule" => {
                serde_json::to_value(validate(s, decode(input)?, actor).await?)?
            }
            "create_hook_rule" => serde_json::to_value(create(s, decode(input)?, actor).await?)?,
            "replace_hook_rule" => serde_json::to_value(replace(s, decode(input)?, actor).await?)?,
            "set_hook_rule_enabled" => {
                serde_json::to_value(set_enabled(s, decode(input)?, actor).await?)?
            }
            "set_hook_rule_emergency_pause" => {
                serde_json::to_value(set_emergency_pause(s, decode(input)?, actor).await?)?
            }
            "lift_hook_rule_budget" => {
                serde_json::to_value(lift_budget(s, decode(input)?, actor).await?)?
            }
            "duplicate_hook_rule" => {
                serde_json::to_value(duplicate(s, decode(input)?, actor).await?)?
            }
            "delete_hook_rule" => {
                delete(s, decode(input)?, actor).await?;
                serde_json::to_value(DeleteHookRuleOutput { deleted: true })?
            }
            name => return Err(Error::not_found(format!("Unknown rule operation {name}"))),
        })
    }
}
