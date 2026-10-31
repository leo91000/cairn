//! `conditions`: what granted agents can wait for with `wait_for`, observed by the
//! package's `condition` handler. Cairn keeps the wait durable, checks authorization
//! and the project, schedules observations and resumes the conversation; the package
//! only answers whether its condition is complete.
use super::{
    connections::Connection,
    registry::{self, Package},
    scope::{self},
};
use crate::{
    config::now,
    error::{Error, Result},
    service::{Service, policy},
    store::Db,
    validation::text,
};
use cairn_contracts::{EventKind, PluginContributionKind as Kind};
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
    for connection in s.store.plugin_instances().await? {
        let (agent, id) = (agent_id.to_owned(), connection.id.clone());
        if connection.enabled
            && s.store
                .read(move |db| db.has_plugin_grant(&agent, &id))
                .await?
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

async fn allowed_instances(
    s: &Service,
    agent: &str,
    instances: &[Connection],
    name: &str,
) -> Result<Vec<Connection>> {
    let (agent, name, instances) = (agent.to_owned(), name.to_owned(), instances.to_vec());
    s.store
        .read(move |db| {
            let mut allowed = Vec::new();
            for instance in instances {
                if db
                    .plugin_contribution(&agent, &instance.id, Kind::Condition, &name)?
                    .is_some()
                {
                    allowed.push(instance);
                }
            }
            Ok(allowed)
        })
        .await
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
    let mut output = Vec::new();
    for (package, instances) in granted(s, agent_id).await? {
        for condition in &package.manifest.conditions {
            if !allowed_instances(s, agent_id, &instances, &condition.name)
                .await?
                .is_empty()
            {
                output.push(schema(&package, condition));
            }
        }
    }
    Ok(output)
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
    for (package, instances) in granted(s, agent_id).await? {
        if let Some(declared) = package.manifest.condition_of_type(kind) {
            if allowed_instances(s, agent_id, &instances, &declared.name)
                .await?
                .is_empty()
            {
                continue;
            }
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
    let name = text(condition, "type")
        .strip_prefix(&format!("{plugin}_"))
        .unwrap_or("");
    let mut granted = false;
    for instance in db.plugin_instances()? {
        if instance.enabled
            && instance.plugin_id == plugin
            && db
                .plugin_contribution(agent_id, &instance.id, Kind::Condition, name)?
                .is_some()
        {
            granted = true;
            break;
        }
    }
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
}

pub async fn observe(s: &Service, run: &Value, condition: &Value) -> Result<Observed> {
    observe_with(s, run, condition, None)
        .await
        .map(|(observed, _)| observed)
}

async fn observe_with(
    s: &Service,
    run: &Value,
    condition: &Value,
    owner: Option<Arc<crate::waits::Owner>>,
) -> Result<(Observed, crate::capabilities::plugin_packages::Authority)> {
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
    let snapshot = if let Some(owner) = owner {
        super::grant_authorization::Snapshot::for_wait(s, run, &package, owner).await?
    } else {
        super::grant_authorization::Snapshot::for_run(s, run, &package).await?
    };
    let scope = super::grant_authorization::contribution_scope(
        s,
        snapshot,
        instances,
        Kind::Condition,
        &declared.name,
        &input,
    )
    .await?;
    let scope = Arc::new(scope);
    let local = scope.clone();
    let asynchronous = scope.clone();
    let publication = crate::capabilities::plugin_packages::Authority::default()
        .with_guard(move |db| scope::authorize_in(db, &local))
        .with_recheck_guard(move |s| {
            let scope = asynchronous.clone();
            Box::pin(async move { scope::authorize(s, &scope).await })
        });
    let publication = crate::delegation::authorize_async_with(s, run, publication).await?;
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
    publication.recheck(s).await?;
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
        return Ok((
            Observed {
                result: None,
                display: waiting_display(&value["display"]),
                check_after,
            },
            publication,
        ));
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
    Ok((
        Observed {
            result: Some(Value::Object(result)),
            display: None,
            check_after,
        },
        publication,
    ))
}

/// One plugin condition of a wait: the whole wait (`None`), or one condition of a group.
type Slot = Option<usize>;

// Share each wait snapshot: cloning its full member list per watcher is quadratic.
type Watch = (Arc<crate::waits::Owner>, Arc<Value>, Slot);

fn condition_of(
    wakeup: &cairn_contracts::ChatWakeup,
    slot: Slot,
) -> Option<&cairn_contracts::WaitCondition> {
    match (slot, wakeup.condition.as_ref()?) {
        (Some(index), cairn_contracts::WaitCondition::Group { conditions, .. }) => {
            conditions.get(index)
        }
        (None, condition) => Some(condition),
        _ => None,
    }
}

fn progress_of(
    wakeup: &mut cairn_contracts::ChatWakeup,
    slot: Slot,
) -> Option<&mut cairn_contracts::WaitMemberProgress> {
    match slot {
        Some(index) => wakeup.members.as_mut()?.get_mut(index),
        None => Some(&mut wakeup.progress),
    }
}

fn watched(owner: &crate::waits::Owner, run: &Value, time: i64) -> Vec<Slot> {
    let Some(wakeup) = owner
        .wakeup
        .as_ref()
        .filter(|w| owner.resumable(run) && w.progress.observation.is_none() && w.wake_at > time)
    else {
        return Vec::new();
    };
    let slots = match &wakeup.condition {
        Some(cairn_contracts::WaitCondition::Group { conditions, .. }) => {
            (0..conditions.len()).map(Some).collect::<Vec<_>>()
        }
        _ => vec![None],
    };
    slots
        .into_iter()
        .filter(|slot| {
            let progress = match slot {
                Some(index) => wakeup.members.as_ref().and_then(|m| m.get(*index)),
                None => Some(&wakeup.progress),
            };
            matches!(
                condition_of(wakeup, *slot),
                Some(cairn_contracts::WaitCondition::Plugin { .. })
            ) && progress.is_some_and(|p| {
                p.observation.is_none() && p.next_check_at.is_none_or(|at| at <= time)
            })
        })
        .collect()
}

pub async fn poll_once(s: &Service) -> Result<()> {
    let groups = s
        .store
        .read(|db| {
            let mut groups = BTreeMap::<String, Vec<Watch>>::new();
            let mut owners = Vec::new();
            for record in db.conversations()? {
                owners.push(crate::waits::Owner::conversation(record));
            }
            for record in db.waiting_run_records()? {
                owners.push(crate::waits::Owner::mission(record));
            }
            for owner in owners {
                let Some(run) = db.run(owner.run_id())? else {
                    continue;
                };
                let owner = Arc::new(owner);
                let run = Arc::new(run);
                for slot in watched(&owner, &run, now()) {
                    let condition = condition_of(owner.wakeup.as_ref().unwrap(), slot).unwrap();
                    let wire = crate::waits::condition_wire(condition);
                    let project_id = &wire["projectId"];
                    let granted_origin = crate::project_workspaces::catalog(&run)
                        .into_iter()
                        .find(|p| &p["id"] == project_id)
                        .map(|p| p["origin"].clone());
                    let requester = db
                        .conversation_for_run(text(&run, "id"))?
                        .and_then(|chat| chat.requested_by);
                    let key = json!({
                        "agentId": run["snapshot"]["agent"]["id"],
                        "access": policy(&run["snapshot"]["agent"]),
                        "condition": condition,
                        "projectOrigin": granted_origin,
                        "requester": requester,
                    })
                    .to_string();
                    groups
                        .entry(key)
                        .or_default()
                        .push((owner.clone(), run.clone(), slot));
                }
            }
            let mut groups = groups.into_values().collect::<Vec<_>>();
            groups.sort_by_key(|group| {
                group
                    .iter()
                    .filter_map(|(owner, _, slot)| {
                        let w = owner.wakeup.as_ref()?;
                        match slot {
                            Some(index) => w.members.as_ref()?.get(*index)?.next_check_at,
                            None => w.progress.next_check_at,
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
            let condition = condition_of(first.wakeup.as_ref().unwrap(), *slot).unwrap();
            let observer = text(run, "id").to_owned();
            let observed = observe_with(
                s,
                run,
                &crate::waits::condition_wire(condition),
                Some(first.clone()),
            )
            .await;
            s.store
                .transaction(move |db| record_authorized(db, group, observed, &observer))
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

fn unchanged(
    current: Option<&cairn_contracts::ChatWakeup>,
    expected: Option<&cairn_contracts::ChatWakeup>,
    slot: Slot,
) -> bool {
    let (Some(current), Some(expected)) = (current, expected) else {
        return false;
    };
    match slot {
        Some(index) => {
            current.id == expected.id
                && current.condition == expected.condition
                && current.members.as_ref().and_then(|m| m.get(index))
                    == expected.members.as_ref().and_then(|m| m.get(index))
        }
        None => current == expected,
    }
}

fn record_authorized(
    db: &Db<'_>,
    group: Vec<Watch>,
    observed: Result<(Observed, crate::capabilities::plugin_packages::Authority)>,
    observer: &str,
) -> Result<()> {
    let observed = observed.and_then(|(observed, publication)| {
        publication.authorize(db)?;
        Ok(observed)
    });
    record(db, group, &observed, observer)
}

fn record(
    db: &Db<'_>,
    group: Vec<Watch>,
    observed: &Result<Observed>,
    observer: &str,
) -> Result<()> {
    for (expected, observed_run, slot) in group {
        let Some(mut owner) = expected.reload(db)? else {
            continue;
        };
        let Some(run) = db.run(owner.run_id())? else {
            continue;
        };
        if run["id"] != observed_run["id"]
            || run["snapshot"]["agent"] != observed_run["snapshot"]["agent"]
            || !unchanged(owner.wakeup.as_ref(), expected.wakeup.as_ref(), slot)
            || !watched(&owner, &run, now()).contains(&slot)
        {
            continue;
        }
        let condition = condition_of(owner.wakeup.as_ref().unwrap(), slot)
            .unwrap()
            .clone();
        let authorized = crate::waits::access(db, &run, &crate::waits::condition_wire(&condition));
        let result = match (&authorized, observed) {
            (Err(error), _) => Some(json!({"state":"unavailable","message":error.message})),
            (Ok(()), Ok(value)) => value.result.clone(),
            (Ok(()), Err(error))
                if matches!(error.status, 403 | 404 | 409) && owner.run_id() == observer =>
            {
                Some(json!({"state":"unavailable","message":error.message}))
            }
            _ => None,
        };
        let result = result
            .as_ref()
            .map(crate::waits::observed_from_wire)
            .transpose()?;
        let progress = progress_of(owner.wakeup.as_mut().unwrap(), slot).unwrap();
        let mut delay = POLL_MS;
        if let Some(result) = &result {
            progress.observation = Some(result.clone());
        } else if observed.is_err() {
            progress.last_error = Some(
                "The service is temporarily unavailable; retrying until the wait deadline.".into(),
            );
        } else {
            progress.last_error = None;
        }
        if let Ok(observed) = observed {
            if let Some(display) = &observed.display {
                progress.display = Some(serde_json::from_value(display.clone())?);
            }
            delay = observed.check_after.unwrap_or(POLL_MS);
        }
        progress.next_check_at = Some(now() + delay);
        if slot.is_some() {
            crate::waits::record_members(db, &mut owner, &run)?;
        } else if let Some(result) = result {
            db.event(
                text(&run, "id"),
                &EventKind::from_adapter(owner.event("wait_ready")),
                "Wait condition resolved",
                Some(&serde_json::to_value(result)?),
            )?;
        }
        owner.touch(now());
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
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn observations_captured_before_a_merge_are_retried_under_the_destination() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let source = json!({ "id": "00000000-0000-4000-8000-000000000010" });
                let target = json!({ "id": "00000000-0000-4000-8000-000000000011" });
                let mut profile = crate::agent_fixture::profile(target.clone())?;
                profile.settings.plugins = vec!["00000000-0000-4000-8000-000000000012".into()];
                let mut instance = Connection::draft("00000000-0000-4000-8000-000000000012", "fixture", json!({}), Vec::new());
                instance.name = "Fixture".into();
                instance.revision = "r1".into();
                instance.enabled = true;
                db.save_plugin_version("fixture", "1", &"0".repeat(64))?;
                db.save_plugin_instance(&instance)?;
                db.save_agent(&profile)?;
                let captured_run = json!({
                    "id": "run", "taskId": "task", "status": "succeeded", "createdAt": now(),
                    "snapshot": { "agent": source }, "chatExecution": { "messageId": "message" },
                });
                db.add_run_fixture(&captured_run, None)?;
                let chat = db.save_conversation_boundary(&json!({
                        "id": "chat", "agentId": "00000000-0000-4000-8000-000000000011", "runId": "run",
                        "wakeup": {
                            "id":"fixture-wait", "messageId": "message", "wakeAt": now() + 60_000,
                            "reason":"Fixture", "seconds":60,"createdAt":now(),
                            "condition": { "type": "plugin", "conditionType":"fixture_ready", "pluginId": "fixture", "input":{} },
                        },
                    }),
                )?;
                let current_run = { db.0.execute("UPDATE runs SET snapshot=json_set(snapshot, '$.agent', json(?)) WHERE id='run'", [target.to_string()])?; db.run("run")?.unwrap() };
                let stale_denial =
                    crate::waits::access(db, &captured_run, &crate::waits::condition_wire(db.wakeup(true,"chat")?.unwrap().condition.as_ref().unwrap()))
                        .unwrap_err();
                record(
                    db,
                    vec![(
                        crate::waits::Owner::conversation(db.conversation("chat")?.unwrap())
                        .into(),
                        captured_run.into(),
                        None,
                    )],
                    &Err(stale_denial),
                    "run",
                )?;
                assert_eq!(db.conversation_boundary("chat")?.unwrap(), chat);
                assert!(db.events_fixture("run", 0, 10)?.is_empty());

                let fresh = Ok(Observed {
                    result: Some(json!({ "state": "completed" })),
                    display: None,
                    check_after: None,
                });
                record(
                    db,
                    vec![(
                        crate::waits::Owner::conversation(db.conversation("chat")?.unwrap())
                        .into(),
                        current_run.into(),
                        None,
                    )],
                    &fresh,
                    "run",
                )?;
                assert_eq!(
                    db.conversation_boundary("chat")?.unwrap()["wakeup"]["observation"]["state"],
                    "completed"
                );
                assert_eq!(db.events_fixture("run", 0, 10)?.len(), 1);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_second_granted_instance_cannot_publish_the_revoked_observers_result() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store.transaction(|db| {
            let agent = "00000000-0000-4000-8000-000000000011";
            let first = "00000000-0000-4000-8000-000000000012";
            let second = "00000000-0000-4000-8000-000000000013";
            let mut profile = crate::agent_fixture::profile(json!({ "id": agent }))?;
            profile.settings.plugins = vec![first.into(), second.into()];
            db.save_plugin_version("fixture", "1", &"0".repeat(64))?;
            let mut observed_instance = None;
            for id in [first, second] {
                let mut instance = Connection::draft(id, "fixture", json!({}), vec![]);
                instance.revision = "r1".into();
                instance.enabled = true;
                db.save_plugin_instance(&instance)?;
                if id == first { observed_instance = Some(instance); }
            }
            db.save_agent(&profile)?;
            let run = json!({ "id": "run", "taskId": "task", "status": "succeeded", "createdAt": now(),
                "snapshot": { "agent": { "id": agent } }, "chatExecution": { "messageId": "message" } });
            db.add_run_fixture(&run, None)?;
            let run = db.run("run")?.unwrap();
            db.save_conversation_boundary(&json!({ "id": "chat", "agentId": agent, "runId": "run",
                "wakeup": { "id": "fixture-wait", "messageId": "message", "wakeAt": now()+60_000,
                    "reason": "Fixture", "seconds": 60, "createdAt": now(),
                    "condition": { "type": "plugin", "conditionType": "fixture_ready", "pluginId": "fixture", "input": {} } }
            }))?;
            let instance = observed_instance.unwrap();
            let scope = scope::Scope::Agent { agent_id: agent.into(), instances: vec![instance.clone()] };
            let publication = crate::capabilities::plugin_packages::Authority::default().with_guard(move |db| scope::authorize_in(db, &scope));
            let mut revoked = instance;
            revoked.enabled = false;
            revoked.revision = "r2".into();
            db.save_plugin_instance(&revoked)?;
            let owner = crate::waits::Owner::conversation(db.conversation("chat")?.unwrap());
            let condition = crate::waits::condition_wire(owner.wakeup.as_ref().unwrap().condition.as_ref().unwrap());
            assert!(crate::waits::access(db, &run, &condition).is_ok(), "the other instance still grants the condition");
            record_authorized(db, vec![(owner.into(), run.into(), None)], Ok((Observed {
                result: Some(json!({ "state": "completed" })), display: None, check_after: None,
            }, publication)), "run")?;
            assert_eq!(db.conversation_boundary("chat")?.unwrap()["wakeup"]["observation"]["state"], "unavailable");
            Ok(())
        }).await.unwrap();
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
