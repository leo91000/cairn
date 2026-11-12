//! What a rule must satisfy to be saved: a bounded tree, formulas that compile and
//! read only what exists at their place, and references its creator may use.
use super::{
    catalog,
    formula::{self, Scope},
    storage::Use,
};
use crate::{error::Result, plugins::connections::Connection, store::Db};
use cairn_contracts::{
    HookRuleDefinition, HookRuleProblem, HookRuleTrigger, HookRuleWarning,
    PluginContributionKind as Kind, RuleAction, RuleCondition, RuleNode, RuleValue, RuleValueScope,
};
use cairn_plugin_api::Manifest;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const MAX_NODES: usize = 200;
pub(crate) const MAX_DEPTH: usize = 8;
pub(crate) const MAX_CONDITION_DEPTH: usize = 6;
pub(crate) const MAX_LITERAL: usize = 16_384;
/// Fits the apps' request limit, so a saved rule can always be saved again.
pub(crate) const MAX_DEFINITION: usize = 120_000;
pub(crate) const MAX_WAIT_SECONDS: u32 = 7 * 86_400;
pub(crate) const MAX_TRIGGERINGS_PER_HOUR: u32 = 600;
pub(crate) const MAX_CONVERSATIONS_PER_HOUR: u32 = 100;
/// Blocks and iterations of one triggering, waits excluded.
pub(crate) const MAX_STEPS: u32 = 500;
pub(crate) const MAX_TOOL_CALLS: u32 = 50;

/// Variables every formula reads, besides the payload's top-level fields.
pub(crate) const RESERVED: &[&str] = &["payload", "steps", "values", "event", "now"];

/// Who the rule acts for.
#[derive(Clone, Debug)]
pub(crate) struct Creator {
    pub account: String,
    pub owner: bool,
}

impl Creator {
    pub(crate) fn uses_agent(&self, db: &Db<'_>, agent: &str) -> Result<bool> {
        if db.agent(agent)?.is_none() {
            return Ok(false);
        }
        if self.owner {
            return Ok(true);
        }
        db.member_uses_agent(&self.account, agent)
    }
}

/// The plugin instances a definition names, with the manifest of their active package.
#[derive(Default)]
pub(crate) struct Instances(pub BTreeMap<String, (Connection, Option<Manifest>)>);

impl Instances {
    fn manifest(&self, instance: &str) -> Option<(&Connection, Option<&Manifest>)> {
        self.0
            .get(instance)
            .map(|(connection, manifest)| (connection, manifest.as_ref()))
    }
}

/// The instances whose manifest checking a definition needs.
pub(crate) fn instance_ids(definition: &HookRuleDefinition) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if let HookRuleTrigger::Plugin { instance_id, .. } = &definition.trigger {
        ids.insert(instance_id.to_string());
    }
    visit_actions(&definition.steps, &mut |action| {
        if let RuleAction::CallTool { instance_id, .. } = action {
            ids.insert(instance_id.to_string());
        }
    });
    ids
}

pub(crate) fn visit_actions<'a>(nodes: &'a [RuleNode], visit: &mut impl FnMut(&'a RuleAction)) {
    for node in nodes {
        match node {
            RuleNode::If {
                then, otherwise, ..
            } => {
                visit_actions(then, visit);
                visit_actions(otherwise, visit);
            }
            RuleNode::ForEach { body, .. } => visit_actions(body, visit),
            RuleNode::Action { action, .. } => visit(action),
        }
    }
}

pub(crate) struct Checked {
    pub problems: Vec<HookRuleProblem>,
    pub warnings: Vec<HookRuleWarning>,
    pub uses: Vec<Use>,
}

pub(crate) struct Request<'a> {
    pub definition: &'a HookRuleDefinition,
    pub creator: &'a Creator,
    pub instances: &'a Instances,
    /// Who saves this version, for native authority (ADR-0050).
    pub origin: &'a crate::native::policy::Origin,
}

pub(crate) fn check(db: &Db<'_>, request: &Request<'_>) -> Result<Checked> {
    let mut checker = Checker {
        problems: Vec::new(),
        ids: BTreeSet::new(),
        nodes: 0,
    };
    let definition = request.definition;

    let name = definition.name.trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        checker.problem("/name", "Enter a name of 1 to 100 characters.");
    }
    if serde_json::to_vec(definition).map_or(true, |bytes| bytes.len() > MAX_DEFINITION) {
        checker.problem(
            "",
            "A rule has at most 120 KB: shorten its texts or split it.",
        );
    }
    if definition.description.chars().count() > 2000 {
        checker.problem("/description", "A description has at most 2000 characters.");
    }
    let limits = definition.limits;
    if !(1..=MAX_TRIGGERINGS_PER_HOUR).contains(&limits.triggerings_per_hour) {
        checker.problem(
            "/limits/triggeringsPerHour",
            &format!("Allow 1 to {MAX_TRIGGERINGS_PER_HOUR} triggerings per hour."),
        );
    }
    if !(1..=MAX_CONVERSATIONS_PER_HOUR).contains(&limits.conversations_per_hour) {
        checker.problem(
            "/limits/conversationsPerHour",
            &format!(
                "Allow 1 to {MAX_CONVERSATIONS_PER_HOUR} conversations started or steered per hour."
            ),
        );
    }

    let payload = trigger_payload(db, request, &mut checker)?;
    let mut scope = Scope::default();
    for name in RESERVED {
        scope.names.insert((*name).to_owned());
    }
    if let Some(properties) = payload
        .as_ref()
        .and_then(|schema| schema["properties"].as_object())
    {
        for field in properties.keys() {
            if identifier(field) && !RESERVED.contains(&field.as_str()) {
                scope.names.insert(field.clone());
            }
        }
    }

    if definition.steps.is_empty() {
        checker.problem("/steps", "Add at least one step.");
    }
    checker.nodes(&definition.steps, "/steps", &mut scope, 1, false);

    let mut uses = BTreeSet::new();
    let agent = definition.execution_agent_id.to_string();
    uses.insert(Use {
        kind: "executionAgent",
        agent_id: agent.clone(),
        instance_id: String::new(),
        name: String::new(),
    });
    match db.agent(&agent)? {
        None => checker.problem("/executionAgentId", "This agent no longer exists."),
        Some(_) if !request.creator.uses_agent(db, &agent)? => checker.problem(
            "/executionAgentId",
            "You don't have access to this agent. Ask the installation owner to give you access.",
        ),
        Some(_) => {}
    }
    match &definition.trigger {
        HookRuleTrigger::Plugin { instance_id, hook } => {
            uses.insert(Use {
                kind: "hook",
                agent_id: agent.clone(),
                instance_id: instance_id.to_string(),
                name: hook.clone(),
            });
            if payload.is_some()
                && db
                    .plugin_contribution(&agent, &instance_id.to_string(), Kind::Hook, hook)?
                    .is_none()
            {
                checker.problem(
                    "/trigger",
                    "The execution agent does not hold this hook. The installation owner grants it on the agent's page.",
                );
            }
        }
        HookRuleTrigger::Cairn { hook } => {
            uses.insert(Use {
                kind: "cairnHook",
                agent_id: agent.clone(),
                instance_id: String::new(),
                name: hook.key().to_owned(),
            });
        }
    }

    let mut index = Vec::new();
    actions_with_paths(&definition.steps, "/steps", &mut index);
    for (path, action) in index {
        match action {
            RuleAction::CallTool {
                instance_id,
                tool,
                input,
            } => {
                let instance = instance_id.to_string();
                uses.insert(Use {
                    kind: "tool",
                    agent_id: agent.clone(),
                    instance_id: instance.clone(),
                    name: tool.clone(),
                });
                check_tool(
                    db,
                    request,
                    &mut checker,
                    &path,
                    &agent,
                    &instance,
                    tool,
                    input,
                )?;
            }
            RuleAction::StartAgent {
                agent_id,
                project_id,
                ..
            } => {
                let id = agent_id.to_string();
                uses.insert(Use {
                    kind: "startedAgent",
                    agent_id: id.clone(),
                    instance_id: String::new(),
                    name: String::new(),
                });
                let Some(profile) = db.agent(&id)? else {
                    checker.problem(&format!("{path}/agentId"), "This agent no longer exists.");
                    continue;
                };
                if !request.creator.uses_agent(db, &id)? {
                    checker.problem(
                        &format!("{path}/agentId"),
                        "You don't have access to this agent. Ask the installation owner to give you access.",
                    );
                }
                // An agent restricted to native nodes needs the saver's native authority
                // now; an automatic one keeps to VMs without it (ADR-0050).
                let snapshot = serde_json::to_value(&profile)?;
                if let Err(error) =
                    crate::native::policy::authorize_origin(db, &snapshot, request.origin)
                {
                    checker.problem(&format!("{path}/agentId"), &error.message);
                }
                if let Some(project) = project_id {
                    let project = project.to_string();
                    if db.project(&project)?.is_none() {
                        checker.problem(
                            &format!("{path}/projectId"),
                            "This project no longer exists.",
                        );
                    } else if profile
                        .settings
                        .access
                        .projects
                        .as_ref()
                        .is_some_and(|projects| !projects.contains(&project))
                    {
                        checker.problem(
                            &format!("{path}/projectId"),
                            "This agent does not have access to this project.",
                        );
                    }
                }
            }
            _ => {}
        }
    }

    let warnings = warnings(db, definition)?;

    Ok(Checked {
        problems: checker.problems,
        warnings,
        uses: uses.into_iter().collect(),
    })
}

/// Warns the creator when anyone who triggers the hook may direct an agent with broad
/// rights, because no condition tests the author (ADR-0075, « Conséquences »).
pub(crate) fn warnings(
    db: &Db<'_>,
    definition: &HookRuleDefinition,
) -> Result<Vec<HookRuleWarning>> {
    if tests_author(&definition.steps) {
        return Ok(Vec::new());
    }
    let mut started = BTreeSet::new();
    visit_actions(&definition.steps, &mut |action| {
        if let RuleAction::StartAgent { agent_id, .. } = action {
            started.insert(agent_id.to_string());
        }
    });
    let mut exposed = Vec::new();
    for id in &started {
        if let Some(profile) = db.agent(id)? {
            let rights = super::exposure(db, &profile)?;
            if !rights.is_empty() {
                exposed.push(format!("{} ({})", profile.settings.name, rights.join(", ")));
            }
        }
    }
    if exposed.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![HookRuleWarning {
        kind: "openToAnyone".into(),
        message: format!(
            "No condition tests the author: anyone who triggers this hook can direct {}.",
            exposed.join("; ")
        ),
    }])
}

/// Whether some condition of the tree examines the author.
pub(crate) fn tests_author(nodes: &[RuleNode]) -> bool {
    fn examines(condition: &RuleCondition) -> bool {
        match condition {
            RuleCondition::All { conditions } | RuleCondition::Any { conditions } => {
                conditions.iter().any(examines)
            }
            RuleCondition::Not { condition } => examines(condition),
            RuleCondition::Compare { left, right, .. } => {
                mentions_author(left) || right.as_ref().is_some_and(mentions_author)
            }
            RuleCondition::Formula { expression } => formula::reads(expression, "author"),
            RuleCondition::AuthorIn { .. }
            | RuleCondition::AuthorStartedConversation { .. }
            | RuleCondition::AuthorQuota { .. } => true,
            RuleCondition::ConversationExists { .. } => false,
        }
    }
    nodes.iter().any(|node| match node {
        RuleNode::If {
            condition,
            then,
            otherwise,
            ..
        } => examines(condition) || tests_author(then) || tests_author(otherwise),
        RuleNode::ForEach { body, .. } => tests_author(body),
        RuleNode::Action { .. } => false,
    })
}

/// The payload schema of the trigger, or a problem when it no longer exists.
fn trigger_payload(
    db: &Db<'_>,
    request: &Request<'_>,
    checker: &mut Checker,
) -> Result<Option<Value>> {
    match &request.definition.trigger {
        HookRuleTrigger::Cairn { hook } => Ok(Some(catalog::cairn_payload(*hook))),
        HookRuleTrigger::Plugin { instance_id, hook } => {
            let instance = instance_id.to_string();
            let Some((connection, manifest)) = request.instances.manifest(&instance) else {
                if db.plugin_instance(&instance)?.is_none() {
                    checker.problem(
                        "/trigger/instanceId",
                        "This plugin instance no longer exists.",
                    );
                } else {
                    checker.problem("/trigger/instanceId", "This plugin is not active.");
                }
                return Ok(None);
            };
            let Some(manifest) = manifest else {
                checker.problem(
                    "/trigger/instanceId",
                    &format!("The plugin of {} is not active.", connection.name),
                );
                return Ok(None);
            };
            let Some(declaration) = manifest.hook(hook) else {
                checker.problem(
                    "/trigger/hook",
                    &format!("{} does not emit the hook {hook}.", manifest.name),
                );
                return Ok(None);
            };
            Ok(Some(declaration.payload_schema.clone()))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn check_tool(
    db: &Db<'_>,
    request: &Request<'_>,
    checker: &mut Checker,
    path: &str,
    agent: &str,
    instance: &str,
    tool: &str,
    input: &BTreeMap<String, RuleValue>,
) -> Result<()> {
    let Some((connection, manifest)) = request.instances.manifest(instance) else {
        checker.problem(
            &format!("{path}/instanceId"),
            "This plugin instance no longer exists.",
        );
        return Ok(());
    };
    let Some(manifest) = manifest else {
        checker.problem(
            &format!("{path}/instanceId"),
            &format!("The plugin of {} is not active.", connection.name),
        );
        return Ok(());
    };
    let Some(declaration) = manifest.tool(tool) else {
        checker.problem(
            &format!("{path}/tool"),
            &format!("{} has no tool named {tool}.", manifest.name),
        );
        return Ok(());
    };
    if db
        .plugin_contribution(agent, instance, Kind::Tool, tool)?
        .is_none()
    {
        checker.problem(
            &format!("{path}/tool"),
            "The execution agent does not hold this tool. The installation owner grants it on the agent's page.",
        );
    }
    let schema = &declaration.input_schema;
    let properties = schema["properties"].as_object();
    let closed =
        schema["additionalProperties"] == false && schema.get("patternProperties").is_none();
    for (key, value) in input {
        let property = properties.and_then(|properties| properties.get(key));
        if property.is_none() && closed {
            checker.problem(
                &format!("{path}/input/{}", pointer(key)),
                &format!("{tool} has no input named {key}."),
            );
            continue;
        }
        // A property may refer to the rest of its schema: one that does not compile alone
        // is checked when the tool is called.
        let validator = property.and_then(|property| jsonschema::validator_for(property).ok());
        if let (Some(validator), RuleValue::Literal { value }) = (validator, value)
            && !validator.is_valid(value)
        {
            checker.problem(
                &format!("{path}/input/{}", pointer(key)),
                &format!("This value does not fit {key}."),
            );
        }
    }
    for required in schema["required"].as_array().into_iter().flatten() {
        if let Some(required) = required.as_str()
            && !input.contains_key(required)
        {
            checker.problem(
                &format!("{path}/input"),
                &format!("{tool} needs a value for {required}."),
            );
        }
    }
    Ok(())
}

fn actions_with_paths<'a>(
    nodes: &'a [RuleNode],
    prefix: &str,
    index: &mut Vec<(String, &'a RuleAction)>,
) {
    for (position, node) in nodes.iter().enumerate() {
        let path = format!("{prefix}/{position}");
        match node {
            RuleNode::If {
                then, otherwise, ..
            } => {
                actions_with_paths(then, &format!("{path}/then"), index);
                actions_with_paths(otherwise, &format!("{path}/otherwise"), index);
            }
            RuleNode::ForEach { body, .. } => {
                actions_with_paths(body, &format!("{path}/body"), index);
            }
            RuleNode::Action { action, .. } => index.push((format!("{path}/action"), action)),
        }
    }
}

/// A payload field readable as a variable of its own.
fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 40
}

/// A name the rule chooses: a value's name may start with a capital, an item's not.
fn name(name: &str, capital: bool) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || capital && first.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 40
}

fn pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

struct Checker {
    problems: Vec<HookRuleProblem>,
    ids: BTreeSet<String>,
    nodes: usize,
}

impl Checker {
    fn problem(&mut self, path: &str, message: &str) {
        self.problems.push(HookRuleProblem {
            path: path.to_owned(),
            message: message.to_owned(),
        });
    }

    fn id(&mut self, id: &str, path: &str) {
        let valid = id.len() <= 40
            && id.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            self.problem(
                &format!("{path}/id"),
                "A step name starts with a lowercase letter and has only letters, digits and _ (at most 40).",
            );
        } else if !self.ids.insert(id.to_owned()) {
            self.problem(
                &format!("{path}/id"),
                &format!("Another step is named {id}."),
            );
        }
    }

    /// Checks nodes in execution order. `scope.steps` gains the actions that may have
    /// run once these nodes complete.
    fn nodes(
        &mut self,
        nodes: &[RuleNode],
        prefix: &str,
        scope: &mut Scope,
        depth: usize,
        in_loop: bool,
    ) {
        if nodes.len() > 100 {
            self.problem(prefix, "A block holds at most 100 steps.");
        }
        if depth > MAX_DEPTH {
            self.problem(
                prefix,
                &format!("Blocks nest at most {MAX_DEPTH} levels deep."),
            );
            return;
        }
        for (position, node) in nodes.iter().enumerate() {
            let path = format!("{prefix}/{position}");
            self.nodes += 1;
            if self.nodes == MAX_NODES + 1 {
                self.problem(&path, &format!("A rule has at most {MAX_NODES} steps."));
            }
            self.id(node.id(), &path);
            match node {
                RuleNode::If {
                    condition,
                    then,
                    otherwise,
                    ..
                } => {
                    self.condition(condition, &format!("{path}/condition"), scope, 1);
                    let mut then_scope = scope.clone();
                    self.nodes(
                        then,
                        &format!("{path}/then"),
                        &mut then_scope,
                        depth + 1,
                        in_loop,
                    );
                    let mut otherwise_scope = scope.clone();
                    self.nodes(
                        otherwise,
                        &format!("{path}/otherwise"),
                        &mut otherwise_scope,
                        depth + 1,
                        in_loop,
                    );
                    scope.steps.extend(then_scope.steps);
                    scope.steps.extend(otherwise_scope.steps);
                }
                RuleNode::ForEach {
                    items,
                    item_name,
                    body,
                    ..
                } => {
                    self.value(items, &format!("{path}/items"), scope);
                    if !name(item_name, false)
                        || RESERVED.contains(&item_name.as_str())
                        || scope.names.contains(item_name)
                    {
                        self.problem(
                            &format!("{path}/itemName"),
                            "Name the item with a new word: letters, digits and _, not a variable already in use.",
                        );
                    }
                    let mut body_scope = scope.clone();
                    body_scope.names.insert(item_name.clone());
                    self.nodes(
                        body,
                        &format!("{path}/body"),
                        &mut body_scope,
                        depth + 1,
                        true,
                    );
                    scope.steps.extend(body_scope.steps);
                }
                RuleNode::Action { label, action, .. } => {
                    if label.chars().count() > 100 || label.chars().any(char::is_control) {
                        self.problem(
                            &format!("{path}/label"),
                            "A label has at most 100 characters.",
                        );
                    }
                    self.action(action, &format!("{path}/action"), scope, in_loop);
                    scope.steps.insert(node.id().to_owned());
                }
            }
        }
    }

    fn action(&mut self, action: &RuleAction, path: &str, scope: &Scope, in_loop: bool) {
        match action {
            RuleAction::CallTool { input, .. } => {
                if input.len() > 50 {
                    self.problem(
                        &format!("{path}/input"),
                        "A tool call sets at most 50 inputs.",
                    );
                }
                for (key, value) in input {
                    self.value(value, &format!("{path}/input/{}", pointer(key)), scope);
                }
            }
            RuleAction::StartAgent { prompt, key, .. } => {
                self.value(prompt, &format!("{path}/prompt"), scope);
                if let Some(key) = key {
                    self.key(key, &format!("{path}/key"), scope);
                }
            }
            RuleAction::SteerConversation { key, message } => {
                self.key(key, &format!("{path}/key"), scope);
                self.value(message, &format!("{path}/message"), scope);
            }
            RuleAction::CancelConversation { key } => self.key(key, &format!("{path}/key"), scope),
            RuleAction::AnswerQuestion { key, answer } => {
                self.key(key, &format!("{path}/key"), scope);
                self.value(answer, &format!("{path}/answer"), scope);
            }
            RuleAction::StoreValue {
                scope: target,
                key,
                name,
                value,
            } => {
                if !self::name(name, true) {
                    self.problem(
                        &format!("{path}/name"),
                        "Name the value with letters, digits and _ (at most 40).",
                    );
                }
                match (target, key) {
                    (RuleValueScope::Conversation, None) => self.problem(
                        &format!("{path}/key"),
                        "Choose the key of the conversation that keeps this value.",
                    ),
                    (RuleValueScope::Rule, Some(_)) => self.problem(
                        &format!("{path}/key"),
                        "A value kept with the rule has no conversation key.",
                    ),
                    (RuleValueScope::Conversation, Some(key)) => {
                        self.key(key, &format!("{path}/key"), scope);
                    }
                    (RuleValueScope::Rule, None) => {}
                }
                self.value(value, &format!("{path}/value"), scope);
            }
            RuleAction::Wait { seconds } => {
                if in_loop {
                    self.problem(path, "A loop cannot wait. Wait before or after it.");
                }
                self.value(seconds, &format!("{path}/seconds"), scope);
                if let RuleValue::Literal { value } = seconds
                    && !value
                        .as_u64()
                        .is_some_and(|seconds| (1..=u64::from(MAX_WAIT_SECONDS)).contains(&seconds))
                {
                    self.problem(
                        &format!("{path}/seconds"),
                        "Wait from 1 second to 7 days (604800 seconds).",
                    );
                }
            }
        }
    }

    /// The author comes from the event; a fixed value would test nobody.
    fn author(&mut self, author: &RuleValue, path: &str, scope: &Scope) {
        if matches!(author, RuleValue::Literal { .. }) {
            self.problem(path, "Choose the payload field that holds the author.");
        }
        self.value(author, path, scope);
    }

    fn key(&mut self, key: &RuleValue, path: &str, scope: &Scope) {
        if let RuleValue::Literal { value } = key
            && !value
                .as_str()
                .is_some_and(|key| !key.is_empty() && key.chars().count() <= 200)
        {
            self.problem(path, "A conversation key is text of 1 to 200 characters.");
        }
        self.value(key, path, scope);
    }

    fn value(&mut self, value: &RuleValue, path: &str, scope: &Scope) {
        match value {
            RuleValue::Literal { value } => {
                if serde_json::to_vec(value).map_or(true, |bytes| bytes.len() > MAX_LITERAL) {
                    self.problem(path, "A fixed value has at most 16 KiB.");
                }
            }
            RuleValue::Template { text } => {
                if let Err(error) = formula::check_template(text, scope) {
                    self.problem(&format!("{path}/text"), &error);
                }
            }
            RuleValue::Formula { expression } => {
                if let Err(error) = formula::check(expression, scope) {
                    self.problem(&format!("{path}/expression"), &error);
                }
            }
        }
    }

    fn condition(&mut self, condition: &RuleCondition, path: &str, scope: &Scope, depth: usize) {
        if depth > MAX_CONDITION_DEPTH {
            self.problem(
                path,
                &format!("Condition groups nest at most {MAX_CONDITION_DEPTH} levels deep."),
            );
            return;
        }
        match condition {
            RuleCondition::All { conditions } | RuleCondition::Any { conditions } => {
                if conditions.is_empty() {
                    self.problem(
                        &format!("{path}/conditions"),
                        "Add a condition to this group.",
                    );
                }
                if conditions.len() > 50 {
                    self.problem(
                        &format!("{path}/conditions"),
                        "A group holds at most 50 conditions.",
                    );
                }
                for (position, condition) in conditions.iter().enumerate() {
                    self.condition(
                        condition,
                        &format!("{path}/conditions/{position}"),
                        scope,
                        depth + 1,
                    );
                }
            }
            RuleCondition::Not { condition } => {
                self.condition(condition, &format!("{path}/condition"), scope, depth + 1);
            }
            RuleCondition::Compare {
                left,
                operator,
                right,
            } => {
                self.value(left, &format!("{path}/left"), scope);
                match (operator.binary(), right) {
                    (true, None) => {
                        self.problem(&format!("{path}/right"), "Enter the value to compare with.");
                    }
                    (false, Some(_)) => self.problem(
                        &format!("{path}/right"),
                        "This comparison takes no second value.",
                    ),
                    (true, Some(right)) => {
                        self.value(right, &format!("{path}/right"), scope);
                        if *operator == cairn_contracts::RuleOperator::Matches
                            && let RuleValue::Literal { value } = right
                            && value
                                .as_str()
                                .is_none_or(|pattern| cairn_plugin_api::pattern(pattern).is_none())
                        {
                            self.problem(
                                &format!("{path}/right"),
                                "Enter a valid regular expression of at most 200 characters.",
                            );
                        }
                    }
                    (false, None) => {}
                }
            }
            RuleCondition::Formula { expression } => {
                if let Err(error) = formula::check(expression, scope) {
                    self.problem(&format!("{path}/expression"), &error);
                }
            }
            RuleCondition::AuthorIn { author, people } => {
                self.author(author, &format!("{path}/author"), scope);
                if people.is_empty() || people.len() > 200 {
                    self.problem(&format!("{path}/people"), "Choose 1 to 200 people.");
                }
                for (position, person) in people.iter().enumerate() {
                    if person.id.is_empty()
                        || person.id.chars().count() > 200
                        || person.resource_type.is_empty()
                        || person.resource_type.len() > 40
                    {
                        self.problem(
                            &format!("{path}/people/{position}"),
                            "This person is not valid.",
                        );
                    }
                }
            }
            RuleCondition::AuthorStartedConversation { author, key } => {
                self.author(author, &format!("{path}/author"), scope);
                self.key(key, &format!("{path}/key"), scope);
            }
            RuleCondition::ConversationExists { key } => {
                self.key(key, &format!("{path}/key"), scope);
            }
            RuleCondition::AuthorQuota {
                author,
                limit,
                window_minutes,
            } => {
                self.author(author, &format!("{path}/author"), scope);
                if !(1..=10_000).contains(limit) {
                    self.problem(&format!("{path}/limit"), "Allow 1 to 10000 triggerings.");
                }
                if !(1..=10_080).contains(window_minutes) {
                    self.problem(
                        &format!("{path}/windowMinutes"),
                        "Count over 1 minute to 7 days (10080 minutes).",
                    );
                }
            }
        }
    }
}

fn mentions_author(value: &RuleValue) -> bool {
    match value {
        RuleValue::Literal { .. } => false,
        RuleValue::Template { text } => formula::template(text).is_ok_and(|parts| {
            parts.iter().any(|part| match part {
                formula::Part::Expression(source) => formula::reads(source, "author"),
                formula::Part::Text(_) => false,
            })
        }),
        RuleValue::Formula { expression } => formula::reads(expression, "author"),
    }
}
