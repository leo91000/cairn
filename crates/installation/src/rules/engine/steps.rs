use super::*;

/// This path is only for preparation failures, before invocation can have effects.
pub(super) fn pre_call_failure(error: Error) -> Result<Json> {
    if matches!(error.status, 408 | 429) || (error.status >= 500 && error.status != 502) {
        Ok(json!({ "retryAfter": 0, "error": error.message }))
    } else {
        Err(error)
    }
}

pub(super) async fn call_tool(
    s: &Service,
    triggering: &str,
    agent: &str,
    instance: &str,
    tool: &str,
    input: &Json,
) -> Result<Json> {
    let prepared: Result<_> = async {
        let connection = required(
            s.store.plugin_instance(instance).await?,
            "This plugin instance no longer exists.",
        )?;
        if !connection.enabled {
            return Err(Error::conflict("This plugin instance is disabled."));
        }
        let package = crate::plugins::registry::active(s, &connection.plugin_id).await?;
        let declaration = package.manifest.tool(tool).ok_or_else(|| {
            Error::bad(format!(
                "{} has no tool named {tool}.",
                package.manifest.name
            ))
        })?;
        let valid = jsonschema::validator_for(&declaration.input_schema)
            .is_ok_and(|validator| validator.is_valid(input));
        if !valid {
            return Err(Error::bad(format!("The input does not fit {tool}.")));
        }
        let profile = required(
            s.store.agent_snapshot(agent).await?,
            "The execution agent no longer exists.",
        )?;
        let snapshot =
            crate::plugins::grant_authorization::Snapshot::capture(s, agent, None, &package)
                .await?;
        let scope = crate::plugins::grant_authorization::contribution_scope(
            s,
            snapshot,
            vec![connection],
            Kind::Tool,
            tool,
            input,
        )
        .await?;
        let payload = json!({
            "tool": tool,
            "input": input,
            "agent": { "id": agent, "name": crate::validation::text(&profile, "name") },
            "instances": scope.instances(),
        });
        let triggering = triggering.to_owned();
        let allowed = s
            .store
            .transaction(move |db| {
                let Some(current) = db.hook_triggering(&triggering)? else {
                    return Ok(false);
                };
                if !current.in_flight || guard(db, &current)? {
                    return Ok(false);
                }
                Ok(db
                    .0
                    .query_row(
                        "SELECT lease_until>?2 FROM hook_rule_triggerings WHERE id=?1",
                        params![triggering, now()],
                        |row| row.get::<_, Option<bool>>(0),
                    )?
                    .unwrap_or(false))
            })
            .await?;
        if !allowed {
            return Err(Error::conflict(
                "This triggering stopped before the tool could be called.",
            ));
        }
        Ok((package, scope, payload))
    }
    .await;
    let (package, scope, payload) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return pre_call_failure(error),
    };
    tokio::time::timeout(
        TOOL_TIMEOUT,
        crate::plugins::scope::invoke(s, &package, "tool", payload, &scope),
    )
    .await
    .map_err(|_| Error::unavailable("The tool did not answer in time; it may have taken effect."))?
}

#[derive(Debug, PartialEq)]
pub(super) enum ToolOutcome<'a> {
    Success,
    Retry(f64),
    Failed(&'a str),
}

pub(super) fn retry_delay(value: &Json) -> Option<f64> {
    let seconds = value.as_f64().or_else(|| {
        let text = value.as_str()?.trim();
        text.parse::<f64>().ok().or_else(|| {
            chrono::DateTime::parse_from_rfc2822(text)
                .ok()
                .map(|date| ((date.timestamp_millis() - now()) as f64 / 1000.0).max(0.0))
        })
    })?;
    (seconds.is_finite() && seconds >= 0.0).then_some(seconds)
}

/// The legacy explicit no-effect reply and the bundled plugins' `ok` convention.
/// A status code alone never proves a mutation was not performed.
pub(super) fn tool_outcome(result: &Json) -> ToolOutcome<'_> {
    let Some(fields) = result.as_object() else {
        return ToolOutcome::Success;
    };
    let legacy = fields.contains_key("retryAfter")
        && fields.len() <= 2
        && fields
            .keys()
            .all(|key| key == "retryAfter" || key == "error");
    if legacy {
        return retry_delay(&result["retryAfter"]).map_or(
            ToolOutcome::Failed("The plugin returned an invalid retry delay."),
            ToolOutcome::Retry,
        );
    }
    if result["ok"] != false {
        return ToolOutcome::Success;
    }
    if result["retryable"] == true {
        return match fields.get("retryAfter").filter(|delay| !delay.is_null()) {
            Some(delay) => retry_delay(delay).map_or(
                ToolOutcome::Failed("The plugin returned an invalid retry delay."),
                ToolOutcome::Retry,
            ),
            None => ToolOutcome::Retry(0.0),
        };
    }
    ToolOutcome::Failed(
        result["message"]
            .as_str()
            .or_else(|| result["error"].as_str())
            .unwrap_or("The plugin reported a failure."),
    )
}

pub(super) fn retry_after(result: &Json) -> Option<f64> {
    match tool_outcome(result) {
        ToolOutcome::Retry(delay) => Some(delay),
        _ => None,
    }
}

pub(super) fn node<'a>(steps: &'a [RuleNode], id: &str) -> Option<&'a RuleNode> {
    for node in steps {
        if node.id() == id {
            return Some(node);
        }
        let found = match node {
            RuleNode::If {
                then, otherwise, ..
            } => self::node(then, id).or_else(|| self::node(otherwise, id)),
            RuleNode::ForEach { body, .. } => self::node(body, id),
            RuleNode::Action { .. } => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

pub(super) enum Step {
    Continue,
    Done,
}

/// Runs one action, then returns the slot so another rule can progress. Only a
/// successful nonterminal action releases its lease; ambiguous effects never replay.
pub(super) async fn run(s: &Arc<Service>, id: &str) -> Result<()> {
    if let Step::Continue = step(s, id).await? {
        let id = id.to_owned();
        s.store.transaction(move |db| {
            db.0.execute(
                "UPDATE hook_rule_triggerings SET lease_until=NULL WHERE id=? AND status='running' AND in_flight=0",
                [id],
            )?;
            Ok(())
        }).await?;
    }
    Ok(())
}

/// What the interpreter decided before an effect, computed off the async runtime.
pub(super) enum Plan {
    Done,
    Fail(String),
    Act(Box<Acting>),
}

/// An action and its evaluated input, with the cursor standing after it.
pub(super) struct Acting {
    pub(super) node: RuleNode,
    pub(super) input: Json,
    pub(super) cursor: Cursor,
}

/// How long the pure part of one step may take: walking blocks and evaluating values.
pub(super) const EVALUATION: std::time::Duration = std::time::Duration::from_secs(10);

pub(super) fn actions(nodes: &[RuleNode], ids: &mut Vec<String>) {
    for node in nodes {
        match node {
            RuleNode::If {
                then, otherwise, ..
            } => {
                actions(then, ids);
                actions(otherwise, ids);
            }
            RuleNode::ForEach { body, .. } => actions(body, ids),
            RuleNode::Action { id, .. } => ids.push(id.clone()),
        }
    }
}

/// Walks to the next action and evaluates its input. A pending action keeps the input
/// it was evaluated with.
pub(super) fn plan(
    definition: &cairn_contracts::HookRuleDefinition,
    mut cursor: Cursor,
    pending: Option<Pending>,
    mut base: Variables,
    facts: &dyn Facts,
) -> Plan {
    if let Err(error) = evaluate::check_deadline() {
        return Plan::Fail(error);
    }
    let steps = &definition.steps;
    if let Some(pending) = pending {
        return match node(steps, &pending.node_id) {
            Some(node) => Plan::Act(Box::new(Acting {
                node: node.clone(),
                input: pending.input,
                cursor,
            })),
            None => Plan::Fail("A pending step is missing from its rule version.".into()),
        };
    }
    let mut ids = Vec::new();
    actions(steps, &mut ids);
    base.insert("steps", cursor.steps(&ids));
    let evaluator = match evaluate::Evaluator::new(&base) {
        Ok(evaluator) => evaluator,
        Err(error) => return Plan::Fail(error),
    };
    let node = match cursor.advance(steps, &evaluator, facts) {
        Ok(Some(node)) => node,
        Ok(None) => return Plan::Done,
        Err(error) => return Plan::Fail(error),
    };
    let RuleNode::Action { action, .. } = node else {
        return Plan::Fail("The interpreter stopped on a block.".into());
    };
    let items = cursor.items();
    let input = match action {
        RuleAction::CallTool { input, .. } => input
            .iter()
            .map(|(key, value)| Ok((key.clone(), evaluator.value(value, &items)?)))
            .collect::<std::result::Result<Map<_, _>, String>>()
            .map(Json::Object),
        RuleAction::StoreValue {
            scope: RuleValueScope::Rule,
            value,
            ..
        } => evaluator.value(value, &items),
        RuleAction::Wait { seconds } => evaluator.value(seconds, &items),
        _ => conversation_input(action, &evaluator, &items),
    };
    let starts_agent = matches!(action, RuleAction::StartAgent { .. });
    let input_limit = if starts_agent {
        validation::MAX_START_AGENT_INPUT
    } else {
        evaluate::MAX_BYTES
    };
    match input {
        Ok(input) if serde_json::to_vec(&input).map_or(true, |bytes| bytes.len() > input_limit) => {
            let limit = if starts_agent {
                "the encoded brief limit"
            } else {
                "256 KiB"
            };
            Plan::Fail(format!("{}: the action input exceeds {limit}.", node.id()))
        }
        Ok(input) => Plan::Act(Box::new(Acting {
            node: node.clone(),
            input,
            cursor,
        })),
        Err(error) => Plan::Fail(format!("{}: {error}", node.id())),
    }
}

/// The evaluated input of a conversation action: its key, texts and author.
pub(super) fn conversation_input(
    action: &RuleAction,
    evaluator: &evaluate::Evaluator,
    items: &Map<String, Json>,
) -> std::result::Result<Json, String> {
    let key = |value: &RuleValue| conversations::key(&evaluator.value(value, items)?);
    let text = |value: &RuleValue, what: &str| match evaluator.value(value, items)? {
        Json::String(text) => Ok(Json::String(text)),
        _ => Err(format!("the {what} is not text.")),
    };
    Ok(match action {
        RuleAction::StartAgent {
            prompt,
            key: bound,
            author,
            ..
        } => {
            let mut input = json!({ "prompt": text(prompt, "prompt")? });
            if let Some(bound) = bound {
                input["key"] = key(bound)?.into();
            }
            if let Some(author) = author {
                let value = super::super::interpreter::lenient(author, evaluator, items)?;
                input["author"] = conversations::author(&value)?.into();
            }
            input
        }
        RuleAction::SteerConversation {
            key: bound,
            message,
        } => {
            json!({ "key": key(bound)?, "message": text(message, "message")? })
        }
        RuleAction::CancelConversation { key: bound } => json!({ "key": key(bound)? }),
        RuleAction::AnswerQuestion { key: bound, answer } => {
            json!({ "key": key(bound)?, "answer": evaluator.value(answer, items)? })
        }
        RuleAction::StoreValue {
            key: Some(bound),
            value,
            ..
        } => json!({ "key": key(bound)?, "value": evaluator.value(value, items)? }),
        _ => return Err("this action has no conversation key.".into()),
    })
}

/// What Cairn predicates read, from the planner's blocking thread.
pub(super) struct EngineFacts {
    pub(super) store: crate::store::Store,
    pub(super) runtime: tokio::runtime::Handle,
    pub(super) rule: String,
    pub(super) creator: String,
    pub(super) triggering: String,
}

impl Facts for EngineFacts {
    fn conversation(&self, key: &str) -> std::result::Result<Option<Option<String>>, String> {
        let (creator, key) = (self.creator.clone(), key.to_owned());
        self.runtime
            .block_on(
                self.store
                    .read(move |db| conversations::bound_author(db, &creator, &key)),
            )
            .map_err(|error| error.message)
    }

    fn author_triggerings(
        &self,
        author: &str,
        window_minutes: u32,
    ) -> std::result::Result<u32, String> {
        let (rule, author, triggering) = (
            self.rule.clone(),
            author.to_owned(),
            self.triggering.clone(),
        );
        let since = now() - i64::from(window_minutes) * 60_000;
        self.runtime
            .block_on(self.store.read(move |db| {
                conversations::author_triggerings(db, &rule, &author, since, &triggering)
            }))
            .map_err(|error| error.message)
    }

    fn rule_conversation_working(&self) -> std::result::Result<bool, String> {
        let rule = self.rule.clone();
        self.runtime
            .block_on(
                self.store
                    .read(move |db| conversations::rule_has_working_conversation(db, &rule)),
            )
            .map_err(|error| error.message)
    }
}

/// Stops a triggering whose rule must not act anymore, in the transaction that would
/// record its next effect. Returns whether it stopped.
pub(super) fn guard(db: &Db<'_>, t: &Triggering) -> Result<bool> {
    let Some(current) = db.hook_triggering(&t.id)? else {
        return Ok(true);
    };
    if current.status != "running" {
        return Ok(true);
    }
    let Some(row) = db.hook_rule(&t.rule_id)? else {
        return Ok(true);
    };
    for number in [row.version, t.version] {
        let version = required(
            db.hook_rule_version(&t.rule_id, number)?,
            "Rule version not found",
        )?;
        if let Some(reason) = blocked(db, &row, &version)? {
            db.save_hook_triggering(&current, "stopped", None, Some(&reason))?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// A late answer can complete the step's history, but never revive a triggering
/// stopped by its creator, the owner, lost rights or an expired executor lease.
pub(super) fn record_late_tool_result(
    db: &Db<'_>,
    t: &Triggering,
    node: &RuleNode,
    position: u32,
    attempts: u32,
    answer: &Result<Json>,
) -> Result<()> {
    let record = match answer {
        Ok(result)
            if serde_json::to_vec(result)
                .map_or(true, |bytes| bytes.len() > evaluate::MAX_BYTES) =>
        {
            StepRecord {
                status: "failed",
                input: None,
                result: None,
                error: Some("The tool's result is larger than 256 KiB."),
            }
        }
        Ok(result) if retry_after(result).is_some() => StepRecord {
            status: "failed",
            input: None,
            result: Some(result),
            error: Some("The triggering stopped before this action could be retried."),
        },
        Ok(result) if matches!(tool_outcome(result), ToolOutcome::Failed(_)) => {
            let ToolOutcome::Failed(error) = tool_outcome(result) else {
                unreachable!()
            };
            StepRecord {
                status: "failed",
                input: None,
                result: Some(result),
                error: Some(error),
            }
        }
        Ok(result) => StepRecord {
            status: "completed",
            input: None,
            result: Some(result),
            error: None,
        },
        Err(error) => StepRecord {
            status: "failed",
            input: None,
            result: None,
            error: Some(&error.message),
        },
    };
    db.record_step(&t.id, position, node, attempts, record)?;
    db.0.execute(
        "UPDATE hook_rule_triggerings SET in_flight=0,pending=NULL,updated_at=?2 WHERE id=?1",
        params![t.id, now()],
    )?;
    Ok(())
}

/// Fails a triggering before an effect, without recording one.
pub(super) fn refuse(db: &Db<'_>, t: &Triggering, error: &str) -> Result<Step> {
    db.save_hook_triggering(t, "failed", None, Some(error))?;
    Ok(Step::Done)
}

pub(super) async fn step(s: &Arc<Service>, id: &str) -> Result<Step> {
    let resumed = Instant::now();
    let Some(Loaded {
        mut triggering,
        row,
        version,
        values,
    }) = load(s, id).await?
    else {
        return Ok(Step::Done);
    };
    if triggering.status != "running" {
        return Ok(Step::Done);
    }
    let reason = {
        let (row, version) = (row.clone(), version.clone());
        s.store.read(move |db| blocked(db, &row, &version)).await?
    };
    if let Some(reason) = reason {
        finish(s, triggering, "stopped", Some(reason)).await?;
        return Ok(Step::Done);
    }

    let base = base(&triggering.event, values, triggering.created_at);
    let (definition, cursor, pending) = (
        version.definition.clone(),
        triggering.cursor.clone(),
        triggering.pending.clone(),
    );
    let deadline = std::time::Instant::now() + EVALUATION;
    let facts = EngineFacts {
        store: s.store.clone(),
        runtime: tokio::runtime::Handle::current(),
        rule: row.id.clone(),
        creator: row.creator_account_id.clone(),
        triggering: triggering.id.clone(),
    };
    let planned = tokio::time::timeout(
        EVALUATION,
        tokio::task::spawn_blocking(move || {
            evaluate::with_deadline(deadline, || {
                plan(&definition, cursor, pending, base, &facts)
            })
        }),
    )
    .await;
    let (node, input, cursor) = match planned {
        Err(_) => {
            let error = "Evaluating this step took more than 10 seconds.";
            finish(s, triggering, "failed", Some(error.into())).await?;
            return Ok(Step::Done);
        }
        Ok(Err(error)) => return Err(Error::internal(error)),
        Ok(Ok(Plan::Done)) => {
            finish(s, triggering, "completed", None).await?;
            return Ok(Step::Done);
        }
        Ok(Ok(Plan::Fail(error))) => {
            finish(s, triggering, "failed", Some(error)).await?;
            return Ok(Step::Done);
        }
        Ok(Ok(Plan::Act(acting))) => {
            let Acting {
                node,
                input,
                cursor,
            } = *acting;
            (node, input, cursor)
        }
    };
    // A member's triggering acts only on a recently verified membership, and checks it
    // again after each wait. The cursor stays where it is until it does.
    if !row.creator_owner {
        let verified = if triggering.verify {
            s.memberships.verify_membership_since(s, resumed).await
        } else {
            s.memberships
                .verify_membership(s, std::time::Duration::from_secs(60))
                .await
        };
        if verified.is_err() {
            triggering.verify = true;
            let since = *triggering.verify_since.get_or_insert_with(now);
            if now() - since > MEMBERSHIP_PATIENCE {
                let error = "Its creator's membership could not be verified for seven days.";
                finish(s, triggering, "failed", Some(error.into())).await?;
            } else {
                let at = now() + 60_000;
                let note = "Waiting to verify its creator's membership.";
                s.store
                    .transaction(move |db| {
                        if guard(db, &triggering)? {
                            return Ok(());
                        }
                        db.save_hook_triggering(&triggering, "waiting", Some(at), Some(note))
                    })
                    .await?;
            }
            return Ok(Step::Done);
        }
        triggering.verify = false;
        triggering.verify_since = None;
    }

    let RuleNode::Action { action, .. } = &node else {
        return Err(Error::internal("The interpreter stopped on a block."));
    };
    let position = triggering
        .pending
        .as_ref()
        .map_or(triggering.actions, |pending| pending.position);
    let attempts = triggering
        .pending
        .as_ref()
        .map_or(1, |pending| pending.attempts + 1);
    let agent = version.definition.execution_agent_id.to_string();

    match action {
        RuleAction::StartAgent { .. }
        | RuleAction::SteerConversation { .. }
        | RuleAction::CancelConversation { .. }
        | RuleAction::AnswerQuestion { .. }
        | RuleAction::StoreValue {
            scope: RuleValueScope::Conversation,
            ..
        } => {
            let acting = Act {
                triggering,
                row,
                version,
                node: node.clone(),
                position,
                cursor,
                input,
            };
            act(s, acting).await
        }
        RuleAction::CallTool {
            instance_id, tool, ..
        } => {
            // Persist the action as in flight before it can take effect.
            let mut prepared = triggering.clone();
            prepared.cursor = cursor;
            prepared.pending = Some(Pending {
                node_id: node.id().to_owned(),
                position,
                attempts,
                input: input.clone(),
            });
            prepared.in_flight = true;
            if attempts == 1 {
                prepared.actions += 1;
            }
            prepared.tool_calls += 1;
            let (record, recorded_input) = (node.clone(), input.clone());
            let go = s
                .store
                .transaction(move |db| {
                    if db.hook_triggering(&triggering.id)?.is_none() || guard(db, &triggering)? {
                        return Ok(None);
                    }
                    if prepared.tool_calls > validation::MAX_TOOL_CALLS {
                        let error = format!(
                            "This triggering called more than {} tools.",
                            validation::MAX_TOOL_CALLS
                        );
                        refuse(db, &triggering, &error)?;
                        return Ok(None);
                    }
                    if !count(db, &mut prepared, &row, &version)? {
                        refuse(
                            db,
                            &triggering,
                            "The rule reached its hourly limit of triggerings.",
                        )?;
                        return Ok(None);
                    }
                    conversations::record_authors(db, &prepared)?;
                    let running = StepRecord {
                        status: "running",
                        input: Some(&recorded_input),
                        result: None,
                        error: None,
                    };
                    db.record_step(&prepared.id, position, &record, attempts, running)?;
                    db.save_hook_triggering(&prepared, "running", None, None)?;
                    Ok(Some(prepared))
                })
                .await?;
            let Some(mut prepared) = go else {
                return Ok(Step::Done);
            };
            let result = call_tool(
                s,
                &prepared.id,
                &agent,
                &instance_id.to_string(),
                tool,
                &input,
            )
            .await;
            s.store
                .transaction(move |db| {
                    let Some(current) = db.hook_triggering(&prepared.id)? else {
                        return Ok(Step::Done);
                    };
                    if current.status != "running" || !current.in_flight {
                        record_late_tool_result(db, &current, &node, position, attempts, &result)?;
                        return Ok(Step::Done);
                    }
                    prepared.in_flight = false;
                    let result = match result {
                        Ok(result) => result,
                        Err(error) => {
                            let failed = StepRecord {
                                status: "failed",
                                input: None,
                                result: None,
                                error: Some(&error.message),
                            };
                            db.record_step(&prepared.id, position, &node, attempts, failed)?;
                            db.save_hook_triggering(
                                &prepared,
                                "failed",
                                None,
                                Some(&error.message),
                            )?;
                            return Ok(Step::Done);
                        }
                    };
                    if serde_json::to_vec(&result)
                        .map_or(true, |bytes| bytes.len() > evaluate::MAX_BYTES)
                    {
                        let error = "The tool's result is larger than 256 KiB.";
                        let failed = StepRecord {
                            status: "failed",
                            input: None,
                            result: None,
                            error: Some(error),
                        };
                        db.record_step(&prepared.id, position, &node, attempts, failed)?;
                        db.save_hook_triggering(&prepared, "failed", None, Some(error))?;
                        return Ok(Step::Done);
                    }
                    if let ToolOutcome::Failed(error) = tool_outcome(&result) {
                        let failed = StepRecord {
                            status: "failed",
                            input: None,
                            result: Some(&result),
                            error: Some(error),
                        };
                        db.record_step(&prepared.id, position, &node, attempts, failed)?;
                        db.save_hook_triggering(&prepared, "failed", None, Some(error))?;
                        return Ok(Step::Done);
                    }
                    if let Some(seconds) = retry_after(&result) {
                        if attempts >= MAX_ATTEMPTS {
                            let error =
                                format!("The service refused this action {attempts} times.");
                            let failed = StepRecord {
                                status: "failed",
                                input: None,
                                result: Some(&result),
                                error: Some(&error),
                            };
                            db.record_step(&prepared.id, position, &node, attempts, failed)?;
                            db.save_hook_triggering(&prepared, "failed", None, Some(&error))?;
                            return Ok(Step::Done);
                        }
                        let backoff = 2f64.powi(attempts as i32).min(3600.0);
                        let at = now();
                        let delay = (seconds.max(backoff) * 1000.0).ceil();
                        if !delay.is_finite()
                            || delay > f64::from(validation::MAX_WAIT_SECONDS) * 1000.0
                        {
                            let error = "The service requested a retry more than seven days away.";
                            let failed = StepRecord {
                                status: "failed",
                                input: None,
                                result: Some(&result),
                                error: Some(error),
                            };
                            db.record_step(&prepared.id, position, &node, attempts, failed)?;
                            db.save_hook_triggering(&prepared, "failed", None, Some(error))?;
                            return Ok(Step::Done);
                        }
                        if waiting_count(db, &prepared.rule_id, &prepared.id)? >= MAX_WAITING {
                            let error = "This rule already has 20 triggerings waiting.";
                            let failed = StepRecord {
                                status: "failed",
                                input: None,
                                result: Some(&result),
                                error: Some(error),
                            };
                            db.record_step(&prepared.id, position, &node, attempts, failed)?;
                            db.save_hook_triggering(&prepared, "failed", None, Some(error))?;
                            return Ok(Step::Done);
                        }
                        prepared.verify = true;
                        let resumes_at = at + delay as i64;
                        let retrying = StepRecord {
                            status: "retrying",
                            input: None,
                            result: Some(&result),
                            error: None,
                        };
                        db.record_step(&prepared.id, position, &node, attempts, retrying)?;
                        db.save_hook_triggering(&prepared, "retrying", Some(resumes_at), None)?;
                        return Ok(Step::Done);
                    }
                    let completed = StepRecord {
                        status: "completed",
                        input: None,
                        result: Some(&result),
                        error: None,
                    };
                    db.record_step(&prepared.id, position, &node, attempts, completed)?;
                    prepared.cursor.steps.insert(node.id().to_owned(), result);
                    prepared.pending = None;
                    db.save_hook_triggering(&prepared, "running", None, None)?;
                    Ok(Step::Continue)
                })
                .await
        }
        RuleAction::StoreValue { name, .. } => {
            if serde_json::to_vec(&input).map_or(true, |bytes| bytes.len() > 65_536) {
                let error = format!("{}: a remembered value has at most 64 KiB.", node.id());
                finish(s, triggering, "failed", Some(error)).await?;
                return Ok(Step::Done);
            }
            let name = name.clone();
            s.store
                .transaction(move |db| {
                    if db.hook_triggering(&triggering.id)?.is_none() || guard(db, &triggering)? {
                        return Ok(Step::Done);
                    }
                    let state_bytes: i64 = db.0.query_row(
                        "SELECT coalesce(sum(length(CAST(value AS BLOB))),0) FROM hook_rule_values WHERE rule_id=?1 AND name<>?2",
                        params![row.id, name], |row| row.get(0),
                    )?;
                    if state_bytes as usize + input.to_string().len() > MAX_RULE_VALUES_BYTES {
                        return refuse(db, &triggering, "This rule's remembered values exceed 1 MiB.");
                    }
                    let mut stored = triggering.clone();
                    stored.cursor = cursor;
                    stored.actions += 1;
                    if !count(db, &mut stored, &row, &version)? {
                        return refuse(db, &triggering, "The rule reached its hourly limit of triggerings.");
                    }
                    conversations::record_authors(db, &stored)?;
                    db.0.prepare_cached(
                        "INSERT INTO hook_rule_values VALUES(?1,?2,?3,?4) \
                         ON CONFLICT(rule_id,name) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
                    )?
                    .execute(params![row.id, name, input.to_string(), now()])?;
                    let completed = StepRecord {
                        status: "completed",
                        input: Some(&input),
                        result: Some(&input),
                        error: None,
                    };
                    db.record_step(&stored.id, position, &node, 1, completed)?;
                    stored.cursor.steps.insert(node.id().to_owned(), input.clone());
                    db.save_hook_triggering(&stored, "running", None, None)?;
                    Ok(Step::Continue)
                })
                .await
        }
        RuleAction::Wait { .. } => {
            let seconds = input.as_f64().filter(|seconds| {
                (1.0..=f64::from(validation::MAX_WAIT_SECONDS)).contains(seconds)
            });
            let Some(seconds) = seconds else {
                let error = format!("{}: wait from 1 second to 7 days.", node.id());
                finish(s, triggering, "failed", Some(error)).await?;
                return Ok(Step::Done);
            };
            s.store
                .transaction(move |db| {
                    if db.hook_triggering(&triggering.id)?.is_none() || guard(db, &triggering)? {
                        return Ok(Step::Done);
                    }
                    let waiting = waiting_count(db, &row.id, &triggering.id)?;
                    if waiting >= MAX_WAITING {
                        let error =
                            format!("This rule already has {MAX_WAITING} triggerings waiting.");
                        return refuse(db, &triggering, &error);
                    }
                    let mut waiting = triggering.clone();
                    waiting.cursor = cursor;
                    waiting.actions += 1;
                    if !count(db, &mut waiting, &row, &version)? {
                        return refuse(
                            db,
                            &triggering,
                            "The rule reached its hourly limit of triggerings.",
                        );
                    }
                    conversations::record_authors(db, &waiting)?;
                    let resumes = now() + (seconds * 1000.0) as i64;
                    let result = json!({ "resumesAt": resumes });
                    let completed = StepRecord {
                        status: "completed",
                        input: None,
                        result: Some(&result),
                        error: None,
                    };
                    db.record_step(&waiting.id, position, &node, 1, completed)?;
                    waiting.cursor.steps.insert(node.id().to_owned(), result);
                    waiting.verify = true;
                    db.save_hook_triggering(&waiting, "waiting", Some(resumes), None)?;
                    Ok(Step::Done)
                })
                .await
        }
    }
}

/// A conversation action and the triggering it belongs to.
pub(super) struct Act {
    pub(super) triggering: Triggering,
    pub(super) row: RuleRow,
    pub(super) version: VersionRow,
    pub(super) node: RuleNode,
    pub(super) position: u32,
    pub(super) cursor: Cursor,
    pub(super) input: Json,
}

/// Runs a conversation action. Its effect and its record share one transaction, so it
/// is never ambiguous: a failure leaves nothing done and is recorded as such.
pub(super) async fn act(s: &Arc<Service>, acting: Act) -> Result<Step> {
    let Act {
        triggering,
        row,
        version,
        node,
        position,
        cursor,
        input,
    } = acting;
    let RuleNode::Action { action, .. } = &node else {
        return Err(Error::internal("The interpreter stopped on a block."));
    };
    let action = action.clone();
    if let Err(error) = conversations::prepare(s, &row, &action, &input).await {
        let error = format!("{}: {}", node.id(), error.message);
        finish(s, triggering, "failed", Some(error)).await?;
        return Ok(Step::Done);
    }
    // Cancelling stops a turn: it follows the worker's lock, like Stop from a person.
    let active = if matches!(action, RuleAction::CancelConversation { .. }) {
        Some(s.worker.active.lock().await)
    } else {
        None
    };
    let executing: HashSet<String> = active
        .as_ref()
        .map(|active| active.keys().cloned().collect())
        .unwrap_or_default();
    let depth = conversations::chain_depth(&triggering.event).saturating_add(1);
    let meters = conversations::meters(&action);
    let effect = action.clone();
    let (step, result) = s
        .store
        .transaction(move |db| {
            if db.hook_triggering(&triggering.id)?.is_none() || guard(db, &triggering)? {
                return Ok((Step::Done, None));
            }
            let mut acting = triggering.clone();
            acting.cursor = cursor;
            acting.actions += 1;
            if !count(db, &mut acting, &row, &version)? {
                let error = "The rule reached its hourly limit of triggerings.";
                return Ok((refuse(db, &triggering, error)?, None));
            }
            if meters && !conversations::count_conversation(db, &row, &version)? {
                let error = "The rule reached its hourly limit of conversations.";
                return Ok((refuse(db, &triggering, error)?, None));
            }
            conversations::record_authors(db, &acting)?;
            let context = conversations::Acting {
                row: &row,
                version: &version,
                event: &triggering.event,
                depth,
                executing: &executing,
            };
            db.0.execute_batch("SAVEPOINT rule_action")?;
            match conversations::effect(db, &context, &effect, &input) {
                Ok(result) => {
                    db.0.execute_batch("RELEASE rule_action")?;
                    if meters {
                        conversations::record_conversation(db, &acting, position)?;
                    }
                    let completed = StepRecord {
                        status: "completed",
                        input: Some(&input),
                        result: Some(&result),
                        error: None,
                    };
                    db.record_step(&acting.id, position, &node, 1, completed)?;
                    acting
                        .cursor
                        .steps
                        .insert(node.id().to_owned(), result.clone());
                    db.save_hook_triggering(&acting, "running", None, None)?;
                    Ok((Step::Continue, Some(result)))
                }
                Err(error) => {
                    db.0.execute_batch("ROLLBACK TO rule_action; RELEASE rule_action")?;
                    let error = format!("{}: {}", node.id(), error.message);
                    let failed = StepRecord {
                        status: "failed",
                        input: Some(&input),
                        result: None,
                        error: Some(&error),
                    };
                    db.record_step(&acting.id, position, &node, 1, failed)?;
                    db.save_hook_triggering(&acting, "failed", None, Some(&error))?;
                    Ok((Step::Done, None))
                }
            }
        })
        .await?;
    if let (RuleAction::CancelConversation { .. }, Some(result)) = (&action, &result) {
        if let Some(chat) = result["conversationId"].as_str() {
            s.accepted_jobs.revoke_chat(chat);
        }
        if let (Some(active), Some(run)) = (&active, result["runId"].as_str())
            && let Some(cancel) = active.get(run)
        {
            cancel.cancel();
        }
    }
    drop(active);
    if result.is_some() {
        s.worker.notify();
    }
    Ok(step)
}

/// Whether a viewer sees a triggering's event, actions and errors.
pub(super) fn details_visible(
    db: &Db<'_>,
    row: &RuleRow,
    definition: &cairn_contracts::HookRuleDefinition,
    viewer: &super::super::Viewer,
) -> Result<bool> {
    details_visible_for_agent(db, row, &definition.execution_agent_id.to_string(), viewer)
}

fn details_visible_for_agent(
    db: &Db<'_>,
    row: &RuleRow,
    agent: &str,
    viewer: &super::super::Viewer,
) -> Result<bool> {
    let Some(account) = &viewer.account else {
        return Ok(true);
    };
    if viewer.owner || *account == row.creator_account_id {
        return Ok(true);
    }
    super::super::validation::Creator {
        account: account.clone(),
        owner: false,
    }
    .uses_agent(db, agent)
}

pub(super) fn view(
    db: &Db<'_>,
    id: &str,
    visible: bool,
    detail: bool,
) -> Result<cairn_contracts::HookRuleTriggering> {
    let (t, status, updated_at, finished_at, next_at, error): (Triggering, String, i64, Option<i64>, Option<i64>, Option<String>) =
        db.0.prepare_cached(&format!("SELECT {COLUMNS},status,updated_at,finished_at,next_at,error FROM hook_rule_triggerings WHERE id=?"))?
            .query_row([id], |row| Ok((triggering(row)?, row.get(14)?, row.get(15)?, row.get(16)?, row.get(17)?, row.get(18)?)))?;
    let status = serde_json::from_value(Json::String(status))?;
    let event = (visible && detail).then(|| cairn_contracts::HookRuleTriggeringEvent {
        hook: t.event["hook"].as_str().unwrap_or_default().to_owned(),
        instance_id: t.event["instanceId"]
            .as_str()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .map(cairn_contracts::PluginInstanceId),
        payload: t.event["payload"].clone(),
        received_at: t.event["receivedAt"].as_i64().unwrap_or_default(),
    });
    let steps = if visible && detail {
        let mut statement = db.0.prepare_cached("SELECT node_id,label,kind,status,input,result,error,attempts,started_at,finished_at FROM hook_rule_steps WHERE triggering_id=? ORDER BY position")?;
        let rows = statement.query_map([id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, u32>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<i64>>(9)?,
            ))
        })?;
        let mut steps = Vec::new();
        for row in rows {
            let (
                node_id,
                label,
                kind,
                status,
                input,
                result,
                error,
                attempts,
                started_at,
                finished_at,
            ) = row?;
            steps.push(cairn_contracts::HookRuleStep {
                node_id,
                label,
                kind,
                status: serde_json::from_value(Json::String(status))?,
                input: input
                    .map(|input| serde_json::from_str(&input))
                    .transpose()?,
                result: result
                    .map(|result| serde_json::from_str(&result))
                    .transpose()?,
                error,
                attempts,
                started_at,
                finished_at,
            });
        }
        Some(steps)
    } else {
        None
    };
    Ok(cairn_contracts::HookRuleTriggering {
        id: uuid::Uuid::parse_str(&t.id)
            .map_err(|_| Error::bad("Invalid triggering identifier."))?,
        rule_id: cairn_contracts::HookRuleId(
            uuid::Uuid::parse_str(&t.rule_id)
                .map_err(|_| Error::bad("Invalid rule identifier."))?,
        ),
        version: t.version,
        status,
        actions: t.actions,
        created_at: t.created_at,
        updated_at,
        finished_at,
        resumes_at: next_at,
        details_visible: visible,
        error: if visible { error } else { None },
        event,
        steps,
    })
}

pub(super) const PAGE: usize = 50;

/// The old run remains in its conversation. Project it into rule history without
/// giving the rule executor a new triggering or changing the rule's budget.
fn migrated_view(
    db: &Db<'_>,
    chat_id: &str,
    rule: &RuleRow,
    viewer: &super::super::Viewer,
    detail: bool,
) -> Result<cairn_contracts::HookRuleTriggering> {
    use cairn_contracts::{HookRuleTriggering, HookRuleTriggeringEvent, HookRuleTriggeringStatus};
    let (created_at, finished_at, status, title, slug, agent): (
        i64,
        Option<i64>,
        String,
        String,
        String,
        String,
    ) = db.0.query_row(
        "SELECT h.created_at,
               CASE WHEN json_extract(h.origin,'$.legacyRun.status') IS NOT NULL
                    THEN json_extract(h.origin,'$.legacyRun.finishedAt') ELSE r.finished_at END,
               coalesce(json_extract(h.origin,'$.legacyRun.status'),r.status,'interrupted'),
               c.title,c.slug,h.agent_id
             FROM hook_rule_conversations h JOIN conversations c ON c.id=h.chat_id
             LEFT JOIN runs r ON r.id=h.run_id WHERE h.chat_id=?1",
        [chat_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    let visible = details_visible_for_agent(db, rule, &agent, viewer)?;
    let triggering_status = match status.as_str() {
        "queued" | "running" => HookRuleTriggeringStatus::Running,
        "waiting" => HookRuleTriggeringStatus::Waiting,
        "succeeded" => HookRuleTriggeringStatus::Completed,
        "cancelled" => HookRuleTriggeringStatus::Stopped,
        _ => HookRuleTriggeringStatus::Failed,
    };
    Ok(HookRuleTriggering {
        id: uuid::Uuid::parse_str(chat_id).map_err(Error::internal)?,
        rule_id: cairn_contracts::HookRuleId(
            uuid::Uuid::parse_str(&rule.id).map_err(Error::internal)?,
        ),
        version: 1,
        status: triggering_status,
        actions: 1,
        created_at,
        updated_at: finished_at.unwrap_or(created_at),
        finished_at,
        resumes_at: None,
        details_visible: visible,
        error: None,
        event: (visible && detail).then(|| HookRuleTriggeringEvent {
            hook: "clock".into(),
            instance_id: None,
            received_at: created_at,
            payload: json!({
                "migratedConversation": {
                    "id": chat_id,
                    "slug": slug,
                    "title": title,
                    "runStatus": status,
                },
            }),
        }),
        steps: None,
    })
}

/// Details follow the execution agent of the version a triggering ran with.
pub(super) fn visible_for(
    db: &Db<'_>,
    row: &RuleRow,
    version: u32,
    viewer: &super::super::Viewer,
) -> Result<bool> {
    let version = required(
        db.hook_rule_version(&row.id, version)?,
        "Rule version not found",
    )?;
    details_visible(db, row, &version.definition, viewer)
}

pub(crate) fn list(
    db: &Db<'_>,
    input: &cairn_contracts::ListHookRuleTriggeringsInput,
    viewer: &super::super::Viewer,
) -> Result<cairn_contracts::ListHookRuleTriggeringsOutput> {
    let rule = input.rule_id.to_string();
    let row = required(db.hook_rule(&rule)?, "Rule not found")?;
    // The cursor is the creation time and identity of the last triggering shown.
    let (before, after_id) = match input.cursor.as_deref() {
        Some(cursor) => {
            let (at, id) = cursor
                .split_once(':')
                .ok_or_else(|| Error::bad("Invalid triggering cursor."))?;
            let at = at
                .parse::<i64>()
                .map_err(|_| Error::bad("Invalid triggering cursor."))?;
            (at, id.to_owned())
        }
        None => (i64::MAX, String::new()),
    };
    let ids =
        db.0.prepare_cached(
            "SELECT id,created_at,version,0 FROM hook_rule_triggerings WHERE rule_id=?1 \
             AND (created_at,id)<(?2,?3) \
             UNION ALL \
             SELECT chat_id AS id,created_at,1,1 FROM hook_rule_conversations WHERE rule_id=?1 \
             AND (created_at,chat_id)<(?2,?3) AND json_type(origin,'$.legacyRun')='object' \
             ORDER BY created_at DESC,id DESC LIMIT ?4",
        )?
        .query_map(params![rule, before, after_id, PAGE as i64 + 1], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (ids.len() > PAGE).then(|| {
        let (id, at, _, _) = &ids[PAGE - 1];
        format!("{at}:{id}")
    });
    let mut visibility = std::collections::BTreeMap::new();
    let mut triggerings = Vec::new();
    for (id, _, version, migrated) in ids.into_iter().take(PAGE) {
        if migrated {
            triggerings.push(migrated_view(db, &id, &row, viewer, false)?);
            continue;
        }
        let visible = match visibility.get(&version) {
            Some(visible) => *visible,
            None => {
                let visible = visible_for(db, &row, version, viewer)?;
                visibility.insert(version, visible);
                visible
            }
        };
        triggerings.push(view(db, &id, visible, false)?);
    }
    let without_effect: i64 =
        db.0.prepare_cached(
            "SELECT coalesce(sum(count),0) FROM hook_rule_quiet_hours WHERE rule_id=? AND hour>=?",
        )?
        .query_row(params![rule, now() / 3_600_000 - 23], |row| row.get(0))?;
    Ok(cairn_contracts::ListHookRuleTriggeringsOutput {
        triggerings,
        without_effect: u32::try_from(without_effect).unwrap_or(u32::MAX),
        next_cursor,
        not_admitted: Some(db.0.query_row(
            "SELECT coalesce(sum(not_admitted),0) FROM hook_rule_quiet_hours WHERE rule_id=? AND hour>=?",
            params![rule, now()/3_600_000 - 23], |row| row.get::<_, u32>(0),
        )?),
    })
}

pub(crate) fn get(
    db: &Db<'_>,
    id: &str,
    viewer: &super::super::Viewer,
) -> Result<cairn_contracts::HookRuleTriggering> {
    if let Some(t) = db.hook_triggering(id)? {
        let row = required(db.hook_rule(&t.rule_id)?, "Rule not found")?;
        let visible = visible_for(db, &row, t.version, viewer)?;
        return view(db, id, visible, true);
    }
    let rule: Option<String> =
        db.0.query_row(
            "SELECT rule_id FROM hook_rule_conversations
         WHERE chat_id=?1 AND json_type(origin,'$.legacyRun')='object'",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let rule = required(rule, "Triggering not found")?;
    let row = required(db.hook_rule(&rule)?, "Rule not found")?;
    migrated_view(db, id, &row, viewer, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn member_work_leaves_executor_capacity_for_owner_rules() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::common::config(root.path()))
            .await
            .unwrap();
        service.store.transaction(|db| {
            for (rule, owner) in [("member", false), ("owner", true)] {
                db.0.execute(
                    "INSERT INTO hook_rules(id,creator_account_id,creator_owner,revision,version,created_at,updated_at) \
                     VALUES(?1,?1,?2,'revision',1,1,1)", params![rule, owner],
                )?;
            }
            for index in 0..50 {
                db.0.execute(
                    "INSERT INTO hook_rule_triggerings(id,rule_id,version,event_id,event,status,cursor,in_flight,actions,tool_calls,verify,created_at,updated_at) \
                     VALUES(?1,'member',1,?1,'{}','running','{}',0,0,0,1,1,1)",
                    [format!("member-{index}")],
                )?;
            }
            Ok(())
        }).await.unwrap();
        let first = claim(&service, PARALLEL, &HashSet::new()).await.unwrap();
        assert_eq!(first.len(), 1);
        service.store.transaction(|db| {
            db.0.execute(
                "INSERT INTO hook_rule_triggerings(id,rule_id,version,event_id,event,status,cursor,in_flight,actions,tool_calls,verify,created_at,updated_at) \
                 VALUES('owner-triggering','owner',1,'owner-event','{}','running','{}',0,0,0,0,2,2)", [],
            )?;
            Ok(())
        }).await.unwrap();
        assert_eq!(
            claim(&service, PARALLEL - first.len(), &HashSet::new())
                .await
                .unwrap(),
            [("owner-triggering".to_owned(), "owner".to_owned())]
        );
        // More old member work cannot take the reserved slots on a later pass.
        assert!(
            claim(&service, PARALLEL, &HashSet::new())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rules_not_yet_served_precede_an_older_rules_next_action() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::common::config(root.path()))
            .await
            .unwrap();
        service.store.transaction(|db| {
            for index in 0..9 {
                let rule = format!("rule-{index}");
                db.0.execute(
                    "INSERT INTO hook_rules(id,creator_account_id,creator_owner,revision,version,created_at,updated_at) \
                     VALUES(?1,'owner',1,'revision',1,1,1)", [&rule],
                )?;
                db.0.execute(
                    "INSERT INTO hook_rule_triggerings(id,rule_id,version,event_id,event,status,cursor,in_flight,actions,tool_calls,verify,created_at,updated_at) \
                     VALUES(?1,?2,1,?1,'{}','running','{}',0,0,0,0,1,1)",
                    params![format!("triggering-{index}"), rule],
                )?;
            }
            Ok(())
        }).await.unwrap();
        let first = claim(&service, PARALLEL, &HashSet::new()).await.unwrap();
        assert_eq!(first.len(), 8);
        service
            .store
            .transaction(|db| {
                db.0.execute("UPDATE hook_rule_triggerings SET lease_until=NULL", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let second = claim(&service, PARALLEL, &HashSet::new()).await.unwrap();
        assert_eq!(second[0].1, "rule-8", "Older work took every slot again.");
    }

    #[test]
    fn only_explicit_safe_replies_or_known_pre_call_refusals_are_retryable() {
        for status in [408, 429, 500, 503, 504] {
            let reply = pre_call_failure(Error::new(status, "not invoked")).unwrap();
            assert_eq!(tool_outcome(&reply), ToolOutcome::Retry(0.0));
        }
        for status in [400, 401, 403, 404, 409, 502] {
            assert!(pre_call_failure(Error::new(status, "permanent refusal")).is_err());
        }
        assert_eq!(
            tool_outcome(&json!({"ok": false, "status": 429, "retryable": false})),
            ToolOutcome::Failed("The plugin reported a failure.")
        );
        assert_eq!(
            tool_outcome(&json!({"ok": true, "error": "domain data"})),
            ToolOutcome::Success
        );
        assert_eq!(
            tool_outcome(&json!({"retryAfter": -1})),
            ToolOutcome::Failed("The plugin returned an invalid retry delay.")
        );
    }

    #[test]
    fn retry_after_accepts_numeric_headers_and_http_dates() {
        assert_eq!(retry_delay(&json!("3600")), Some(3600.0));
        assert_eq!(
            retry_delay(&json!("Sun, 06 Nov 1994 08:49:37 GMT")),
            Some(0.0)
        );
        assert_eq!(retry_delay(&json!("not a date")), None);
        assert_eq!(retry_delay(&json!("NaN")), None);
        assert_eq!(retry_delay(&json!("inf")), None);
    }
}
