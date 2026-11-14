//! Hook rules (ADR-0075, « Règles de hook »): a hook, an execution agent and a tree
//! applied to each event. These wire models confer no authority by themselves.
use crate::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
#[schemars(inline)]
pub struct HookRuleId(pub Uuid);

impl std::fmt::Display for HookRuleId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The hooks Cairn emits for conversations bound to one of the creator's keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum CairnHookKind {
    /// The agent's reply is ready, with its text and deliverables.
    ReplyReady,
    /// The agent asked a question.
    QuestionAsked,
    /// A short progress step, about every 8 seconds.
    Progress,
    /// The conversation's turn completed, failed or was cancelled.
    Ended,
}

impl CairnHookKind {
    pub const ALL: [Self; 4] = [
        Self::ReplyReady,
        Self::QuestionAsked,
        Self::Progress,
        Self::Ended,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::ReplyReady => "replyReady",
            Self::QuestionAsked => "questionAsked",
            Self::Progress => "progress",
            Self::Ended => "ended",
        }
    }
}

/// What a rule listens to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HookRuleTrigger {
    /// A hook a plugin instance emits. The execution agent must hold it.
    Plugin {
        instance_id: PluginInstanceId,
        #[schemars(length(min = 1, max = 40))]
        hook: String,
    },
    /// A Cairn hook for conversations bound to the creator's keys.
    Cairn {
        #[schemars(
            with = "String",
            pattern("^(replyReady|questionAsked|progress|ended)$")
        )]
        hook: CairnHookKind,
    },
}

/// Any value of an action or a condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RuleValue {
    /// A fixed JSON value.
    Literal {
        #[ts(type = "any ")]
        value: serde_json::Value,
    },
    /// Text where each `{{expression}}` inserts the result of a CEL expression.
    Template {
        #[schemars(length(max = 10000))]
        text: String,
    },
    /// A CEL expression.
    Formula {
        #[schemars(length(min = 1, max = 2000))]
        expression: String,
    },
}

impl RuleValue {
    pub fn text(text: &str) -> Self {
        Self::Literal {
            value: serde_json::Value::String(text.into()),
        }
    }

    pub fn formula(expression: &str) -> Self {
        Self::Formula {
            expression: expression.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum RuleOperator {
    Equals,
    NotEquals,
    Contains,
    StartsWith,
    EndsWith,
    /// The whole value matches a regular expression.
    Matches,
    /// The value is one of a list.
    In,
    Present,
    Absent,
    GreaterThan,
    LessThan,
}

impl RuleOperator {
    pub const ALL: [Self; 11] = [
        Self::Equals,
        Self::NotEquals,
        Self::Contains,
        Self::StartsWith,
        Self::EndsWith,
        Self::Matches,
        Self::In,
        Self::Present,
        Self::Absent,
        Self::GreaterThan,
        Self::LessThan,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Equals => "equals",
            Self::NotEquals => "notEquals",
            Self::Contains => "contains",
            Self::StartsWith => "startsWith",
            Self::EndsWith => "endsWith",
            Self::Matches => "matches",
            Self::In => "in",
            Self::Present => "present",
            Self::Absent => "absent",
            Self::GreaterThan => "greaterThan",
            Self::LessThan => "lessThan",
        }
    }

    /// Whether the operator compares with a second value.
    pub fn binary(self) -> bool {
        !matches!(self, Self::Present | Self::Absent)
    }
}

/// A structured condition: nested AND/OR groups, comparisons, formulas and Cairn
/// predicates. External people are only payload data that conditions examine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RuleCondition {
    /// Every condition holds (AND).
    All {
        #[schemars(length(max = 50))]
        conditions: Vec<RuleCondition>,
    },
    /// At least one condition holds (OR).
    Any {
        #[schemars(length(max = 50))]
        conditions: Vec<RuleCondition>,
    },
    Not {
        condition: Box<RuleCondition>,
    },
    /// `left operator right`, usually a payload field against a value.
    Compare {
        left: RuleValue,
        #[schemars(
            with = "String",
            pattern(
                "^(equals|notEquals|contains|startsWith|endsWith|matches|in|present|absent|greaterThan|lessThan)$"
            )
        )]
        operator: RuleOperator,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        right: Option<RuleValue>,
    },
    /// A CEL expression evaluating to a boolean.
    Formula {
        #[schemars(length(min = 1, max = 2000))]
        expression: String,
    },
    /// The author is one of the chosen people.
    AuthorIn {
        author: RuleValue,
        #[schemars(length(min = 1, max = 200))]
        people: Vec<PluginResourceRef>,
    },
    /// The author started the conversation bound to this key.
    AuthorStartedConversation {
        author: RuleValue,
        key: RuleValue,
    },
    /// A conversation is bound to this key.
    ConversationExists {
        key: RuleValue,
    },
    /// The author triggered this rule fewer than `limit` times in the window.
    AuthorQuota {
        author: RuleValue,
        #[schemars(range(min = 1, max = 10000))]
        limit: u32,
        #[schemars(range(min = 1, max = 10080))]
        window_minutes: u32,
    },
}

/// Where a stored value lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum RuleValueScope {
    /// Kept with the rule, across triggerings.
    Rule,
    /// Kept on the conversation bound to a key, shared by the rules that reach it.
    Conversation,
}

/// One effect. Each action does one thing; the tree chooses between alternatives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RuleAction {
    /// Call a plugin tool through the execution agent's grants. Never a Cairn tool.
    CallTool {
        instance_id: PluginInstanceId,
        #[schemars(length(min = 1, max = 200))]
        tool: String,
        #[schemars(length(max = 50))]
        input: BTreeMap<String, RuleValue>,
    },
    /// Start a conversation with an agent the creator may use, optionally bound to a key.
    /// Fails when the key is already bound.
    StartAgent {
        agent_id: AgentId,
        prompt: RuleValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        project_id: Option<ProjectId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        key: Option<RuleValue>,
    },
    /// Send a message to the conversation bound to a key.
    SteerConversation { key: RuleValue, message: RuleValue },
    /// Cancel the current turn of the conversation bound to a key.
    CancelConversation { key: RuleValue },
    /// Answer the pending question of the conversation bound to a key.
    AnswerQuestion { key: RuleValue, answer: RuleValue },
    /// Remember a value for later triggerings.
    StoreValue {
        #[schemars(with = "String", pattern("^(rule|conversation)$"))]
        scope: RuleValueScope,
        /// The conversation key, for conversation values.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        key: Option<RuleValue>,
        #[schemars(length(min = 1, max = 40), pattern("^[a-zA-Z][a-zA-Z0-9_]*$"))]
        name: String,
        value: RuleValue,
    },
    /// Wait, then resume the remaining steps (at most seven days).
    Wait { seconds: RuleValue },
}

/// A block of the tree. Every node has an identifier, unique in the rule; an action's
/// result is available to later steps as `steps.<id>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RuleNode {
    If {
        #[schemars(length(min = 1, max = 40), pattern("^[a-z][a-zA-Z0-9_]*$"))]
        id: String,
        condition: RuleCondition,
        #[schemars(length(max = 100))]
        then: Vec<RuleNode>,
        #[serde(default)]
        #[schemars(length(max = 100))]
        otherwise: Vec<RuleNode>,
    },
    /// Repeat the body for each item of a list.
    ForEach {
        #[schemars(length(min = 1, max = 40), pattern("^[a-z][a-zA-Z0-9_]*$"))]
        id: String,
        items: RuleValue,
        /// The variable holding the current item in the body.
        #[schemars(length(min = 1, max = 40), pattern("^[a-z][a-zA-Z0-9_]*$"))]
        item_name: String,
        #[schemars(length(max = 100))]
        body: Vec<RuleNode>,
    },
    Action {
        #[schemars(length(min = 1, max = 40), pattern("^[a-z][a-zA-Z0-9_]*$"))]
        id: String,
        /// Shown in the editor and the history instead of the identifier.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        #[schemars(length(max = 100))]
        label: String,
        action: RuleAction,
    },
}

impl RuleNode {
    pub fn id(&self) -> &str {
        match self {
            Self::If { id, .. } | Self::ForEach { id, .. } | Self::Action { id, .. } => id,
        }
    }
}

/// Hourly caps that stop a loop by itself. Cairn fixes their maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleLimits {
    #[schemars(range(min = 1, max = 600))]
    pub triggerings_per_hour: u32,
    #[schemars(range(min = 1, max = 100))]
    pub conversations_per_hour: u32,
}

impl Default for HookRuleLimits {
    fn default() -> Self {
        Self {
            triggerings_per_hour: 60,
            conversations_per_hour: 10,
        }
    }
}

/// Everything its creator writes. A saved definition becomes an immutable version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleDefinition {
    #[schemars(length(min = 1, max = 100))]
    pub name: String,
    #[serde(default)]
    #[schemars(length(max = 2000))]
    pub description: String,
    pub trigger: HookRuleTrigger,
    /// Its rights frame the plugin hook the rule listens to and the tools it calls.
    pub execution_agent_id: AgentId,
    #[schemars(length(max = 100))]
    pub steps: Vec<RuleNode>,
    pub limits: HookRuleLimits,
}

/// What keeps a rule from triggering. Each one is lifted independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum HookRuleBlockKind {
    /// By its creator, or a duplication. Lifted by its creator.
    Disabled,
    /// By the owner. Lifted by the owner.
    EmergencyPaused,
    /// An hourly cap was exceeded. Lifted by its creator or the owner.
    BudgetExhausted,
    /// The creator or the execution agent lost a right the rule uses. Lifted when the
    /// right returns, or by the creator editing the rule.
    RightsLost,
    /// The creator left the installation. Never lifted: duplicate the rule instead.
    MembershipEnded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleBlock {
    #[schemars(
        with = "String",
        pattern("^(disabled|emergencyPaused|budgetExhausted|rightsLost|membershipEnded)$")
    )]
    pub kind: HookRuleBlockKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(optional = nullable, type = "number")]
    pub since: Option<i64>,
    pub reason: String,
}

/// How a version was saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum HookRuleSource {
    /// The web or Android app.
    App,
    /// The public MCP, for the owner who authorized the client.
    PublicMcp,
    /// An agent with installation access, for the person of its conversation.
    InstallationAgent,
    /// A duplicate of another rule.
    Duplicate,
    /// A rule template from a plugin.
    Template,
}

/// What the person viewing the rule may do with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleAllowed {
    pub edit: bool,
    pub enable: bool,
    pub disable: bool,
    pub duplicate: bool,
    pub delete: bool,
    pub emergency_pause: bool,
    pub lift_budget: bool,
    /// Whether the triggering history shows payloads and action results.
    pub view_details: bool,
}

/// A warning shown to the rule's creator, which does not prevent saving.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleWarning {
    #[schemars(pattern("^openToAnyone$"))]
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRule {
    pub id: HookRuleId,
    pub creator_account_id: String,
    /// Changes with every write, including state changes.
    pub revision: String,
    pub version: u32,
    pub definition: HookRuleDefinition,
    /// Empty when the rule triggers.
    pub blocks: Vec<HookRuleBlock>,
    pub allowed: HookRuleAllowed,
    pub warnings: Vec<HookRuleWarning>,
    /// Whether this version may reach native agents and conversations.
    pub native_authority: bool,
    #[schemars(
        with = "String",
        pattern("^(app|publicMcp|installationAgent|duplicate|template)$")
    )]
    pub source: HookRuleSource,
    /// The conversation in which an agent wrote this version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub conversation_id: Option<Uuid>,
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub created_at: i64,
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleSummary {
    pub id: HookRuleId,
    pub name: String,
    pub description: String,
    pub creator_account_id: String,
    pub revision: String,
    pub version: u32,
    pub trigger: HookRuleTrigger,
    pub execution_agent_id: AgentId,
    /// Agents the rule starts, besides its execution agent.
    pub started_agent_ids: Vec<AgentId>,
    /// Plugin instances whose tools the rule calls.
    pub tool_instance_ids: Vec<PluginInstanceId>,
    pub blocks: Vec<HookRuleBlock>,
    pub allowed: HookRuleAllowed,
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleVersion {
    pub version: u32,
    pub saved_by_account_id: String,
    #[schemars(
        with = "String",
        pattern("^(app|publicMcp|installationAgent|duplicate|template)$")
    )]
    pub source: HookRuleSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub conversation_id: Option<Uuid>,
    pub native_authority: bool,
    pub definition: HookRuleDefinition,
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub saved_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListHookRulesInput {
    /// Rules that use this agent, as execution agent or started agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub agent_id: Option<AgentId>,
    /// Rules that listen to this instance or call its tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub instance_id: Option<PluginInstanceId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListHookRulesOutput {
    pub rules: Vec<HookRuleSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleInput {
    pub rule_id: HookRuleId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateHookRuleInput {
    /// A random UUID: retrying returns the same rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub creation_id: Option<HookRuleId>,
    pub definition: HookRuleDefinition,
    /// A new rule triggers at once unless it starts disabled.
    #[serde(default = "enabled")]
    #[schemars(default, extend("default" = true))]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaceHookRuleInput {
    pub rule_id: HookRuleId,
    #[schemars(length(min = 1, max = 100))]
    pub revision: String,
    pub definition: HookRuleDefinition,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetHookRuleEnabledInput {
    pub rule_id: HookRuleId,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetHookRuleEmergencyPauseInput {
    pub rule_id: HookRuleId,
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DuplicateHookRuleInput {
    pub rule_id: HookRuleId,
    /// A random UUID: retrying returns the same duplicate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub creation_id: Option<HookRuleId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteHookRuleOutput {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListHookRuleVersionsOutput {
    pub versions: Vec<HookRuleVersion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidateHookRuleInput {
    /// The rule being edited, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub rule_id: Option<HookRuleId>,
    pub definition: HookRuleDefinition,
}

/// A problem that prevents saving, located by a JSON Pointer into the definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleProblem {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidateHookRuleOutput {
    pub problems: Vec<HookRuleProblem>,
    pub warnings: Vec<HookRuleWarning>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetHookRuleCatalogInput {
    /// Only what this execution agent holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub agent_id: Option<AgentId>,
}

/// A hook a rule can listen to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleCatalogHook {
    pub trigger: HookRuleTrigger,
    pub title: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub plugin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub instance_name: Option<String>,
    #[ts(type = "any ")]
    pub payload_schema: serde_json::Value,
    pub resource_fields: Vec<cairn_plugin_api::ResourceField>,
    /// Usable execution agents that hold this hook. Every agent receives Cairn hooks.
    pub agent_ids: Vec<AgentId>,
}

/// A plugin tool a rule can call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleCatalogTool {
    pub instance_id: PluginInstanceId,
    pub instance_name: String,
    pub plugin_id: String,
    pub tool: String,
    pub description: String,
    #[ts(type = "any ")]
    pub input_schema: serde_json::Value,
    pub resource_fields: Vec<cairn_plugin_api::ResourceField>,
    /// Usable execution agents that hold this tool.
    pub agent_ids: Vec<AgentId>,
}

/// An action kind and the JSON Schema of its fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleCatalogAction {
    pub kind: String,
    pub title: String,
    pub description: String,
    #[ts(type = "any ")]
    pub input_schema: serde_json::Value,
    /// Whether the action starts or steers a conversation, which counts against the
    /// hourly conversation cap.
    pub conversation: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleCatalogEntry {
    pub kind: String,
    pub title: String,
    pub description: String,
}

/// A Cairn function available in formulas, besides the CEL standard library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleCatalogFunction {
    pub name: String,
    /// For example `truncate(text: string, length: int) -> string`.
    pub signature: String,
    pub description: String,
    pub example: String,
}

/// A variable formulas can read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleCatalogVariable {
    pub name: String,
    pub description: String,
}

/// An agent the caller may designate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleCatalogAgent {
    pub agent_id: AgentId,
    pub name: String,
    /// Whether the agent may run natively, which needs native authority.
    pub native: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleCatalogLimits {
    pub default_limits: HookRuleLimits,
    pub max_triggerings_per_hour: u32,
    pub max_conversations_per_hour: u32,
    pub max_wait_seconds: u32,
    /// Blocks and iterations one triggering may run, actions excluded.
    pub max_steps: u32,
    pub max_tool_calls: u32,
    pub max_nodes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub max_recorded_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub max_cursor_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub max_rule_values_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub max_pending: Option<u32>,
    /// Event bytes retained by unresolved admissions and active triggerings, per rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub max_pending_event_bytes: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleCatalog {
    pub hooks: Vec<RuleCatalogHook>,
    pub tools: Vec<RuleCatalogTool>,
    pub actions: Vec<RuleCatalogAction>,
    pub conditions: Vec<RuleCatalogEntry>,
    pub operators: Vec<RuleCatalogEntry>,
    pub functions: Vec<RuleCatalogFunction>,
    pub variables: Vec<RuleCatalogVariable>,
    pub agents: Vec<RuleCatalogAgent>,
    pub limits: RuleCatalogLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum HookRuleTriggeringStatus {
    Running,
    /// Waiting for a wait step, or to verify its creator's membership.
    Waiting,
    /// Retrying an action the service confirmed it did not perform.
    Retrying,
    Completed,
    Failed,
    /// Stopped because the rule was disabled, paused or lost its rights.
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum HookRuleStepStatus {
    Running,
    Completed,
    Failed,
    Retrying,
    Waiting,
}

/// The event a triggering started from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleTriggeringEvent {
    pub hook: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub instance_id: Option<PluginInstanceId>,
    #[ts(type = "any ")]
    pub payload: serde_json::Value,
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub received_at: i64,
}

/// One action a triggering ran.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleStep {
    pub node_id: String,
    pub label: String,
    /// The action kind, such as `callTool`.
    pub kind: String,
    #[schemars(
        with = "String",
        pattern("^(running|completed|failed|retrying|waiting)$")
    )]
    pub status: HookRuleStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable, type = "any")]
    pub input: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable, type = "any")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub error: Option<String>,
    pub attempts: u32,
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(optional = nullable, type = "number")]
    pub finished_at: Option<i64>,
}

/// A triggering of a rule. Every member sees its state; the event, the actions and
/// the error are shown only to the creator, the owner and members with access to the
/// execution agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleTriggering {
    pub id: Uuid,
    pub rule_id: HookRuleId,
    /// The rule version this triggering keeps.
    pub version: u32,
    #[schemars(
        with = "String",
        pattern("^(running|waiting|retrying|completed|failed|stopped)$")
    )]
    pub status: HookRuleTriggeringStatus,
    pub actions: u32,
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub created_at: i64,
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(optional = nullable, type = "number")]
    pub finished_at: Option<i64>,
    /// When a waiting or retrying triggering resumes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(optional = nullable, type = "number")]
    pub resumes_at: Option<i64>,
    pub details_visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub event: Option<HookRuleTriggeringEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub steps: Option<Vec<HookRuleStep>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListHookRuleTriggeringsInput {
    pub rule_id: HookRuleId,
    /// The `nextCursor` of the previous page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 200))]
    #[ts(optional = nullable)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListHookRuleTriggeringsOutput {
    /// Newest first, without their details.
    pub triggerings: Vec<HookRuleTriggering>,
    /// Triggerings that ended without any action in the last 24 hours. They are not
    /// kept and do not count against the hourly caps.
    pub without_effect: u32,
    /// Events refused for this rule's capacity or admission deadline in the last 24 hours. No payload is retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub not_admitted: Option<u32>,
    /// Present when older triggerings remain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleTriggeringInput {
    pub triggering_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListHookRuleTemplatesInput {
    /// Only the templates of this instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub instance_id: Option<PluginInstanceId>,
}

/// A rule template a plugin instance offers. Its rules use that instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRuleTemplate {
    pub instance_id: PluginInstanceId,
    pub instance_name: String,
    pub plugin_id: String,
    pub plugin_version: String,
    pub template: cairn_plugin_api::RuleTemplate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListHookRuleTemplatesOutput {
    pub templates: Vec<HookRuleTemplate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallHookRuleTemplateInput {
    /// A random UUID: retrying returns the same rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub creation_id: Option<HookRuleId>,
    pub instance_id: PluginInstanceId,
    #[schemars(length(min = 1, max = 40))]
    pub template: String,
    /// Values by parameter name: an agent identifier, `{resourceType, id}`, a list of
    /// them, a text, a number or a boolean. Omitted optional parameters take their default.
    #[ts(type = "Record<string, any>")]
    #[schemars(extend("x-cairn-json-object" = true))]
    pub parameters: BTreeMap<String, serde_json::Value>,
    /// The new rules trigger at once unless they start disabled.
    #[serde(default = "enabled")]
    #[schemars(default, extend("default" = true))]
    pub enabled: bool,
}

/// Ordinary rules owned by whoever installed the template, with no link to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallHookRuleTemplateOutput {
    pub rules: Vec<HookRule>,
}
