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
        .get("projects", id)?
        .ok_or_else(|| Error::not_found("Project no longer exists."))?;
    if current["origin"] != granted["origin"] {
        return Err(Error::conflict(
            "Project origin changed; register a new wait.",
        ));
    }
    Ok(json!({
        "id": current["id"],
        "name": current["name"],
        "origin": current["origin"],
        "baseBranch": current["baseBranch"],
        "source": current["source"],
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
}

pub async fn observe(s: &Service, run: &Value, condition: &Value) -> Result<Observed> {
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
    let scope = Scope::Agent {
        agent_id: agent_id.clone(),
        instances,
    };
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
    if serde_json::to_string(&value)?.len() > MAX_OBSERVATION_BYTES {
        return Err(Error::bad_gateway(
            "The plugin returned too large a result.",
        ));
    }
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
    })
}

/// One plugin condition of a wait: the whole wait (`None`), or one condition of a group.
type Slot = Option<usize>;

// Share each wait snapshot: cloning its full member list per watcher is quadratic.
type Watch = (Arc<crate::waits::Owner>, Arc<Value>, Slot);

fn condition_of(wakeup: &Value, slot: Slot) -> &Value {
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

/// One batch is capped and deduplicated; slow services never hold the admission tick.
pub async fn poll_once(s: &Service) -> Result<()> {
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
    let results = stream::iter(groups)
        .map(|group| async move {
            let (first, run, slot) = &group[0];
            let condition = condition_of(&first.record["wakeup"], *slot);
            let observed = observe(s, run, condition).await;
            s.store
                .transaction(move |db| record(db, group, &observed))
                .await
        })
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
fn unchanged(current: &Value, expected: &Value, slot: Slot) -> bool {
    match slot {
        Some(index) => {
            current["id"] == expected["id"]
                && current["condition"] == expected["condition"]
                && current["members"][index] == expected["members"][index]
        }
        None => current == expected,
    }
}

fn record(db: &Db<'_>, group: Vec<Watch>, observed: &Result<Observed>) -> Result<()> {
    for (expected, observed_run, slot) in group {
        let Some(mut owner) = expected.reload(db)? else {
            continue;
        };
        let Some(run) = db.run(owner.run_id())? else {
            continue;
        };
        // A merge can transfer a sleeping run while its plugin check is in flight.
        // Retry under the current profile rather than recording a stale denial.
        if run["id"] != observed_run["id"]
            || run["snapshot"]["agent"] != observed_run["snapshot"]["agent"]
            || !unchanged(&owner.record["wakeup"], &expected.record["wakeup"], slot)
            || !watched(&owner, &run, now()).contains(&slot)
        {
            continue;
        }
        let condition = condition_of(&owner.record["wakeup"], slot).clone();
        let authorized = crate::waits::access(db, &run, &condition);
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
        if let Ok(observed) = observed {
            if let Some(display) = &observed.display {
                progress["display"] = display.clone();
            }
            delay = observed.check_after.unwrap_or(POLL_MS);
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

    #[tokio::test]
    async fn observations_captured_before_a_merge_are_retried_under_the_destination() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let source = json!({ "id": "source" });
                let target = json!({ "id": "target" });
                db.put("agents", &target)?;
                db.put(
                    connections::KIND,
                    &json!({
                        "id": "instance", "pluginId": "fixture", "enabled": true,
                        "agentIds": ["target"],
                    }),
                )?;
                let captured_run = json!({
                    "id": "run", "taskId": "task", "status": "succeeded", "createdAt": now(),
                    "snapshot": { "agent": source }, "chatExecution": { "messageId": "message" },
                });
                db.add_run(&captured_run, None)?;
                let chat = db.put(
                    "chats",
                    &json!({
                        "id": "chat", "agentId": "target", "runId": "run",
                        "wakeup": {
                            "messageId": "message", "wakeAt": now() + 60_000,
                            "condition": { "type": "fixture_ready", "pluginId": "fixture" },
                        },
                    }),
                )?;
                let current_run =
                    db.patch_run("run", &json!({ "snapshot": { "agent": target } }))?;
                let stale_denial =
                    crate::waits::access(db, &captured_run, &chat["wakeup"]["condition"])
                        .unwrap_err();
                record(
                    db,
                    vec![(
                        crate::waits::Owner {
                            chat: true,
                            record: chat.clone(),
                        }
                        .into(),
                        captured_run.into(),
                        None,
                    )],
                    &Err(stale_denial),
                )?;
                assert_eq!(db.get("chats", "chat")?.unwrap(), chat);
                assert!(db.events("run", 0, 10)?.is_empty());

                let fresh = Ok(Observed {
                    result: Some(json!({ "state": "completed" })),
                    display: None,
                    check_after: None,
                });
                record(
                    db,
                    vec![(
                        crate::waits::Owner {
                            chat: true,
                            record: chat,
                        }
                        .into(),
                        current_run.into(),
                        None,
                    )],
                    &fresh,
                )?;
                assert_eq!(
                    db.get("chats", "chat")?.unwrap()["wakeup"]["observation"]["state"],
                    "completed"
                );
                assert_eq!(db.events("run", 0, 10)?.len(), 1);
                Ok(())
            })
            .await
            .unwrap();
    }

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
