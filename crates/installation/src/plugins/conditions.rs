//! `conditions`: what granted agents can wait for with `wait_for`, observed by the
//! package's `condition` handler. Cairn keeps the wait durable, checks authorization
//! and the project, schedules observations and resumes the conversation; the package
//! only answers whether its condition is complete.
use super::{
    connections::{self, Connection},
    registry::{self, Package},
    scope::{self, Scope},
};
use crate::{
    config::now,
    error::{Error, Result},
    service::{Service, policy},
    store::Db,
    validation::text,
};
use futures_util::{StreamExt, stream};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

/// How often a waiting condition is observed, unless its package asks for later.
const POLL_MS: i64 = 30_000;
const BATCH_SIZE: usize = 16;
const MAX_OBSERVATION_BYTES: usize = 65_536;

/// Whether a wait condition belongs to a plugin.
pub fn is_condition(condition: &Value) -> bool {
    condition["pluginId"].is_string()
}

/// Packages with conditions and the enabled instances granting `agent_id`.
async fn granted(s: &Service, agent_id: &str) -> Result<Vec<(Package, Vec<Connection>)>> {
    let mut by_plugin = BTreeMap::<String, Vec<Connection>>::new();
    for value in s.store.list(connections::KIND).await? {
        if let Ok(connection) = serde_json::from_value::<Connection>(value)
            && connection.grants(agent_id)
        {
            by_plugin
                .entry(connection.plugin_id.clone())
                .or_default()
                .push(connection);
        }
    }
    let mut output = Vec::new();
    for (plugin, mut instances) in by_plugin {
        let Ok(package) = registry::active(s, &plugin).await else {
            continue;
        };
        if package.manifest.conditions.is_empty() || package.manifest.agent_access.is_none() {
            continue;
        }
        instances.sort_by(|a, b| a.name.cmp(&b.name));
        output.push((package, instances));
    }
    Ok(output)
}

/// The `wait_for` schema of one condition: its type, the project when pinned to one,
/// then the package's own properties.
fn schema(package: &Package, condition: &cairn_plugin_api::Condition) -> Value {
    let kind = package.manifest.condition_type(condition);
    let mut properties = Map::new();
    properties.insert("type".into(), json!({ "const": kind }));
    let mut required = vec![json!("type")];
    if condition.project {
        properties.insert(
            "projectId".into(),
            json!({ "type": "string", "format": "uuid" }),
        );
        required.push(json!("projectId"));
    }
    if let Some(own) = condition.input_schema["properties"].as_object() {
        properties.extend(own.clone());
    }
    required.extend(
        condition.input_schema["required"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );
    json!({
        "type": "object",
        "description": format!("{} ({})", condition.description, package.manifest.name),
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// Condition schemas for the agent, for the `wait_for` tool.
pub async fn schemas(s: &Service, agent_id: &str) -> Result<Vec<Value>> {
    Ok(granted(s, agent_id)
        .await?
        .iter()
        .flat_map(|(package, _)| {
            package
                .manifest
                .conditions
                .iter()
                .map(|condition| schema(package, condition))
        })
        .collect())
}

/// Integral floats JSON Schema accepted as integers become integers, so identical
/// requests compare equal.
fn canonical(condition: &mut Value, schema: &Value) {
    let Some(object) = condition.as_object_mut() else {
        return;
    };
    for (key, value) in object.iter_mut() {
        let integer = schema["properties"][key]["type"] == "integer";
        if integer && let Some(number) = value.as_f64() {
            *value = json!(number as i64);
        }
    }
}

/// Recognizes a plugin condition the agent may use and records its plugin. Returns
/// `None` for Cairn's own conditions.
pub async fn normalize(s: &Service, agent_id: &str, condition: &Value) -> Result<Option<Value>> {
    let kind = text(condition, "type");
    if matches!(kind, "run_finished" | "task_finished") {
        return Ok(None);
    }
    for (package, _) in granted(s, agent_id).await? {
        if let Some(declared) = package.manifest.condition_of_type(kind) {
            let schema = schema(&package, declared);
            let mut condition = condition.clone();
            if let Some(object) = condition.as_object_mut() {
                object.remove("pluginId");
            }
            if !jsonschema::is_valid(&schema, &condition) {
                return Err(Error::bad(format!("Provide the fields {kind} requires.")));
            }
            canonical(&mut condition, &schema);
            condition["pluginId"] = package.manifest.id.clone().into();
            return Ok(Some(condition));
        }
    }
    Err(Error::bad("Unsupported wait condition."))
}

/// Rechecked with every observation: the agent still holds an enabled instance of the
/// package, and a pinned project is still the one the run was granted.
pub(crate) fn access(db: &Db<'_>, run: &Value, condition: &Value) -> Result<()> {
    let agent_id = text(&run["snapshot"]["agent"], "id");
    let plugin = text(condition, "pluginId");
    let granted = db.list(connections::KIND)?.into_iter().any(|value| {
        value["pluginId"] == plugin
            && value["enabled"] == true
            && value["agentIds"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| id == agent_id))
    });
    if !granted {
        return Err(Error::forbidden(
            "This agent is no longer granted the plugin of this wait.",
        ));
    }
    if condition.get("projectId").is_some() {
        project(db, run, condition)?;
    }
    Ok(())
}

fn project(db: &Db<'_>, run: &Value, condition: &Value) -> Result<Value> {
    let id = text(condition, "projectId");
    let granted = crate::project_workspaces::catalog(run)
        .into_iter()
        .find(|p| p["id"] == id)
        .ok_or_else(|| Error::forbidden("This project is not authorized for the wait."))?;
    let current = db
        .project(id)?
        .ok_or_else(|| Error::not_found("Project no longer exists."))?;
    if current.origin != granted["origin"].as_str().unwrap_or("") {
        return Err(Error::conflict(
            "Project origin changed; register a new wait.",
        ));
    }
    Ok(json!({
        "id": current.id,
        "name": current.settings.name,
        "origin": current.origin,
        "baseBranch": current.settings.base_branch,
        "source": current.source,
    }))
}

fn short(value: &Value, max: usize) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.chars().count() <= max)
        .map(|value| value.chars().filter(|c| !c.is_control()).collect())
}

/// What clients show while waiting: `{ title, short, details }`.
fn waiting_display(raw: &Value) -> Option<Value> {
    let title = short(&raw["title"], 200)?;
    let details = raw["details"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|detail| short(detail, 80))
        .take(4)
        .collect::<Vec<_>>();
    Some(json!({
        "title": title,
        "short": short(&raw["short"], 60).unwrap_or_else(|| title.clone()),
        "details": details,
    }))
}

/// What clients show for a result: `{ tone, title, detail }`.
fn result_display(raw: &Value) -> Option<Value> {
    let tone = raw["tone"]
        .as_str()
        .filter(|tone| matches!(*tone, "ok" | "error" | "warning" | "neutral"))
        .unwrap_or("neutral");
    Some(json!({
        "tone": tone,
        "title": short(&raw["title"], 200)?,
        "detail": short(&raw["detail"], 300).unwrap_or_default(),
    }))
}

/// One observation: the condition's result when complete, its waiting display
/// otherwise. Errors are transient; a package reports a permanent failure as
/// `{ state = "unavailable", message }`.
pub struct Observed {
    pub result: Option<Value>,
    pub display: Option<Value>,
    pub check_after: Option<i64>,
    agent: Box<scope::Agent>,
    approval: registry::Approval,
    digest: String,
}

impl Observed {
    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        self.agent.recheck(s).await?;
        self.approval.recheck(s, &self.digest).await
    }

    pub(crate) fn recheck_in(&self, db: &Db<'_>) -> Result<()> {
        self.approval.recheck_in(db)?;
        self.agent.recheck_in(db)
    }
}

pub async fn observe(
    s: &Service,
    run: &Value,
    condition: &Value,
    bearer: Option<&str>,
) -> Result<Observed> {
    let caller = run.clone();
    let source = condition.clone();
    let project = s
        .store
        .read(move |db| {
            crate::waits::access(db, &caller, &source)?;
            if source.get("projectId").is_some() {
                project(db, &caller, &source).map(Some)
            } else {
                Ok(None)
            }
        })
        .await?;
    let agent = &run["snapshot"]["agent"];
    let agent_id = text(agent, "id").to_owned();
    let plugin = text(condition, "pluginId");
    let (package, instances) = granted(s, &agent_id)
        .await?
        .into_iter()
        .find(|(package, _)| package.manifest.id == plugin)
        .ok_or_else(|| {
            Error::forbidden("This agent is no longer granted the plugin of this wait.")
        })?;
    let expected_plugin = package.manifest.id.clone();
    let expected_digest = package.digest.clone();
    let approval = s
        .store
        .read(move |db| registry::Approval::capture_in(db, &expected_plugin, &expected_digest))
        .await?;
    let kind = text(condition, "type");
    let declared = package
        .manifest
        .condition_of_type(kind)
        .ok_or_else(|| Error::conflict("The plugin no longer provides this wait condition."))?;
    let mut input = condition.clone();
    if let Some(object) = input.as_object_mut() {
        for key in ["type", "pluginId", "projectId"] {
            object.remove(key);
        }
    }
    let scope = match bearer {
        Some(bearer) => Scope::for_agent(s, run, instances, bearer).await?,
        None => Scope::for_condition(s, run, condition, instances).await?,
    };
    approval.recheck(s, &package.digest).await?;
    let payload = json!({
        "condition": declared.name,
        "input": input,
        "project": project,
        "agent": { "id": agent_id, "name": text(agent, "name") },
        "instances": scope.instances(),
    });
    let value = tokio::time::timeout(
        Duration::from_secs(40),
        scope::invoke(s, &package, "condition", payload, &scope),
    )
    .await
    .map_err(|_| Error::bad_gateway("The condition could not be checked in time."))??;
    approval.recheck(s, &package.digest).await?;
    if serde_json::to_string(&value)?.len() > MAX_OBSERVATION_BYTES {
        return Err(Error::bad_gateway(
            "The plugin returned too large a result.",
        ));
    }
    let Scope::Agent(agent) = scope else {
        return Err(Error::forbidden(
            "The condition has no agent authorization.",
        ));
    };
    let check_after = value["checkAfter"]
        .as_i64()
        .map(|seconds| seconds.clamp(10, 3600) * 1000);
    let state = value["state"].as_str().filter(|state| {
        state.len() <= 40
            && *state != "waiting"
            && state
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
    });
    let Some(state) = state.filter(|state| !state.is_empty()) else {
        return Ok(Observed {
            result: None,
            display: waiting_display(&value["display"]),
            check_after,
            agent,
            approval,
            digest: package.digest,
        });
    };
    let mut result = value.as_object().cloned().unwrap_or_default();
    result.remove("checkAfter");
    result.insert("state".into(), state.into());
    result.insert("type".into(), kind.into());
    result.insert("pluginId".into(), plugin.into());
    match result_display(&value["display"]) {
        Some(display) => result.insert("display".into(), display),
        None => result.remove("display"),
    };
    if state == "unavailable" {
        let message = short(&value["message"], 300)
            .unwrap_or_else(|| "The condition cannot be observed.".into());
        result.insert("message".into(), message.into());
    }
    Ok(Observed {
        result: Some(Value::Object(result)),
        display: None,
        check_after,
        agent,
        approval,
        digest: package.digest,
    })
}

/// One plugin condition of a wait: the whole wait (`None`), or one condition of a group.
type Slot = Option<usize>;

// Share each wait snapshot: cloning its full member list per watcher is quadratic.
type Watch = (Arc<crate::waits::Owner>, Arc<Value>, Slot);

pub(super) fn condition_of(wakeup: &Value, slot: Option<usize>) -> &Value {
    match slot {
        Some(index) => &wakeup["condition"]["conditions"][index],
        None => &wakeup["condition"],
    }
}

/// Where a condition's progress is recorded: on the wakeup, or beside its grouped condition.
fn progress_of(wakeup: &mut Value, slot: Slot) -> &mut Value {
    match slot {
        Some(index) => &mut wakeup["members"][index],
        None => wakeup,
    }
}

/// The plugin conditions of a wait to observe now.
fn watched(owner: &crate::waits::Owner, run: &Value, time: i64) -> Vec<Slot> {
    let wakeup = &owner.record["wakeup"];
    let pending = owner.resumable(run)
        && !wakeup["observation"].is_object()
        && wakeup["wakeAt"]
            .as_i64()
            .is_some_and(|deadline| deadline > time);
    if !pending {
        return Vec::new();
    }
    let slots = if crate::waits::is_group(&wakeup["condition"]) {
        (0..crate::waits::members(&wakeup["condition"]).len())
            .map(Some)
            .collect()
    } else {
        vec![None]
    };
    slots
        .into_iter()
        .filter(|slot| {
            let progress = match slot {
                Some(index) => &wakeup["members"][index],
                None => wakeup,
            };
            is_condition(condition_of(wakeup, *slot))
                && !progress["observation"].is_object()
                && progress["nextCheckAt"]
                    .as_i64()
                    .is_none_or(|next| next <= time)
        })
        .collect()
}

/// A finished probe owns its unchanged wait snapshots until publication. A queued
/// store write may outlive a source change; callers cannot rebuild this proof.
pub struct PendingObservation {
    group: Vec<Watch>,
    observed: Result<Observed>,
}

impl PendingObservation {
    pub async fn publish(self, s: &Service) -> Result<()> {
        let observed = match self.observed {
            Ok(observed) => match observed.recheck(s).await {
                Ok(()) => Ok(observed),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        s.store
            .transaction(move |db| record(db, self.group, &observed))
            .await
    }
}

/// One batch is capped and deduplicated. Completed probes retain their original
/// wait snapshots while a publication waits for the store writer.
pub async fn pending(
    s: &Service,
) -> Result<impl futures_util::Stream<Item = PendingObservation> + '_> {
    let groups = s
        .store
        .read(|db| {
            let mut groups: BTreeMap<String, Vec<Watch>> = BTreeMap::new();
            let mut owners = db
                .list("chats")?
                .into_iter()
                .map(|record| crate::waits::Owner { chat: true, record })
                .collect::<Vec<_>>();
            owners.extend(
                db.active()?
                    .into_iter()
                    .filter(|run| run["status"] == "waiting")
                    .map(|record| crate::waits::Owner {
                        chat: false,
                        record,
                    }),
            );
            for owner in owners {
                let Some(run) = db.run(owner.run_id())? else {
                    continue;
                };
                let owner = Arc::new(owner);
                let run = Arc::new(run);
                for slot in watched(&owner, &run, now()) {
                    let condition = condition_of(&owner.record["wakeup"], slot);
                    let project_id = &condition["projectId"];
                    let granted_origin = crate::project_workspaces::catalog(&run)
                        .into_iter()
                        .find(|project| &project["id"] == project_id)
                        .map(|project| project["origin"].clone());
                    let key = json!({
                        "agentId": run["snapshot"]["agent"]["id"],
                        "access": policy(&run["snapshot"]["agent"]),
                        "condition": condition,
                        "projectOrigin": granted_origin,
                        "requester": super::requesters::observation_key_in(db, &run)?,
                    })
                    .to_string();
                    groups
                        .entry(key)
                        .or_default()
                        .push((owner.clone(), run.clone(), slot));
                }
            }
            let mut groups: Vec<_> = groups.into_values().collect();
            groups.sort_by_key(|group| {
                group
                    .iter()
                    .filter_map(|(owner, _, slot)| {
                        let wakeup = &owner.record["wakeup"];
                        match slot {
                            Some(index) => wakeup["members"][index]["nextCheckAt"].as_i64(),
                            None => wakeup["nextCheckAt"].as_i64(),
                        }
                    })
                    .min()
                    .unwrap_or(0)
            });
            groups.truncate(BATCH_SIZE);
            Ok(groups)
        })
        .await?;
    Ok(stream::iter(groups)
        .map(move |group| async move {
            let (first, run, slot) = &group[0];
            let condition = condition_of(&first.record["wakeup"], *slot);
            let observed = observe(s, run, condition, None).await;
            PendingObservation { group, observed }
        })
        .buffer_unordered(4))
}

/// Slow services never hold the admission tick. Successful probes publish as they
/// finish; a slow or refused group does not postpone the healthy groups.
pub async fn poll_once(s: &Service) -> Result<()> {
    let results = pending(s)
        .await?
        .map(|observed| observed.publish(s))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    for result in results {
        result?;
    }
    s.worker.notify();
    Ok(())
}

/// Whether the condition observed is still the one waited for, in the same state. Other
/// conditions of its group may have progressed meanwhile.
pub(super) fn unchanged(current: &Value, expected: &Value, slot: Option<usize>) -> bool {
    match slot {
        Some(index) => {
            current["id"] == expected["id"]
                && current["condition"] == expected["condition"]
                && current["observation"] == expected["observation"]
                && current["members"][index] == expected["members"][index]
        }
        None => current == expected,
    }
}

/// Resolve the current member under the exact profile used for its probe.
fn current_wait(
    db: &Db<'_>,
    expected: &crate::waits::Owner,
    observed_run: &Value,
    slot: Slot,
) -> Result<Option<(crate::waits::Owner, Value)>> {
    let Some(owner) = expected.reload(db)? else {
        return Ok(None);
    };
    let Some(run) = db.run(owner.run_id())? else {
        return Ok(None);
    };
    if run["id"] != observed_run["id"]
        || run["snapshot"]["agent"] != observed_run["snapshot"]["agent"]
        || !unchanged(&owner.record["wakeup"], &expected.record["wakeup"], slot)
        || !watched(&owner, &run, now()).contains(&slot)
    {
        return Ok(None);
    }
    Ok(Some((owner, run)))
}

fn record(db: &Db<'_>, group: Vec<Watch>, observed: &Result<Observed>) -> Result<()> {
    // A shared probe carries its representative's requester proof. Retry peers
    // under a current representative if that member changed during provider I/O.
    if let Some((expected, run, slot)) = group.first()
        && current_wait(db, expected, run, *slot)?.is_none()
    {
        return Ok(());
    }
    // Check the shared probe before publishing any peer. A successful first
    // publication changes its representative's progress; it must not invalidate
    // the same authorized result for the remaining peers in this transaction.
    let source = match observed {
        Ok(observed) => observed.recheck_in(db),
        Err(_) => Ok(()),
    };
    for (expected, observed_run, slot) in group {
        let Some((mut owner, run)) = current_wait(db, &expected, &observed_run, slot)? else {
            continue;
        };
        let condition = condition_of(&owner.record["wakeup"], slot).clone();
        let authorized = crate::waits::access(db, &run, &condition).and_then(|()| source.clone());
        let result = match (&authorized, observed) {
            (Err(error), _) => Some(json!({ "state": "unavailable", "message": error.message })),
            (Ok(()), Ok(value)) => value.result.clone(),
            (Ok(()), Err(error)) if matches!(error.status, 403 | 404 | 409) => {
                Some(json!({ "state": "unavailable", "message": error.message }))
            }
            _ => None,
        };
        let progress = progress_of(&mut owner.record["wakeup"], slot);
        let mut delay = POLL_MS;
        if let Some(result) = &result {
            progress["observation"] = result.clone();
        } else if observed.is_err() {
            progress["lastError"] =
                "The service is temporarily unavailable; retrying until the wait deadline.".into();
        } else if let Some(progress) = progress.as_object_mut() {
            // A successful check means the service answers again.
            progress.remove("lastError");
        }
        if authorized.is_ok() {
            if let Ok(observed) = observed {
                if let Some(display) = &observed.display {
                    progress["display"] = display.clone();
                }
                delay = observed.check_after.unwrap_or(POLL_MS);
            }
        } else if let Some(progress) = progress.as_object_mut() {
            progress.remove("display");
        }
        progress["nextCheckAt"] = (now() + delay).into();
        if slot.is_some() {
            // The group resumes once this result completes it.
            crate::waits::record_members(db, &mut owner, &run)?;
        } else if let Some(result) = result {
            db.event(
                text(&run, "id"),
                owner.event("wait_ready"),
                "Wait condition resolved",
                Some(&result),
            )?;
        }
        owner.record["updatedAt"] = now().into();
        owner.save(db)?;
    }
    Ok(())
}

pub(crate) async fn maintain(s: Arc<Service>) {
    let mut timer = tokio::time::interval(Duration::from_millis(POLL_MS as u64));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            _ = timer.tick() => {},
        }
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            result = poll_once(&s) => {
                if let Err(error) = result {
                    tracing::warn!(message = error.message, "Wait observer failed");
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_are_bounded_and_toned() {
        let waiting = waiting_display(&json!({
            "title": "Waiting for required checks on PR #4",
            "short": "PR #4 checks",
            "details": ["abc1234", 3, "x".repeat(81), "b", "c", "d", "e"],
        }))
        .unwrap();
        assert_eq!(waiting["details"], json!(["abc1234", "b", "c", "d"]));
        assert!(waiting_display(&json!({ "short": "only" })).is_none());
        let result = result_display(&json!({ "tone": "loud", "title": "Done" })).unwrap();
        assert_eq!(result["tone"], "neutral");
        assert_eq!(result["detail"], "");
    }

    #[test]
    fn integral_floats_become_integers() {
        let mut condition = json!({ "type": "x", "prNumber": 4.0, "label": 2.5 });
        let schema = json!({ "properties": { "prNumber": { "type": "integer" }, "label": { "type": "number" } } });
        canonical(&mut condition, &schema);
        assert_eq!(condition["prNumber"], json!(4));
        assert_eq!(condition["label"], json!(2.5));
    }
}
