//! Explicit change of an existing chat's backend after its agent's backend changes.
//! VM and native execution never share a node: with an existing environment this is
//! a confirmed node change, whose environment expires (ADR-0083, local conversation
//! disks). An expired environment only selects the backend and node of the next one.
use crate::{
    conversation_lifecycle::require_active,
    environment_expiration::{Expiration, WARNING},
    error::{Error, Result, required},
    native::policy::{self, Origin},
    nodes,
    service::{Service, policy as access},
    store::Db,
    validation::{parse, text},
};
use cairn_contracts::{EnvironmentExpirationCause, EventKind};
use serde_json::{Value, json};

// Only the explicit migration operation uses the verified public MCP owner.
// Other management operations keep their existing provenance rules.
tokio::task_local! { pub(crate) static OWNER: Option<String>; }

fn origin() -> Origin {
    OWNER
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .map_or_else(policy::origin, Origin::Human)
}

fn backend(agent: &Value) -> &'static str {
    if policy::native(agent) {
        "native"
    } else {
        "firecracker"
    }
}

/// Capacity/materialization and disk reads recheck the run's native ancestry.
/// This is only a structural check; principal, node and revocation decisions
/// remain with each caller's existing transaction-local authorization.
pub(crate) fn require_lineage(db: &Db<'_>, run: &Value) -> Result<()> {
    policy::require_acyclic_parent(db, text(run, "id"), run)
}

fn subject(db: &Db<'_>, key: &str) -> Result<Value> {
    if let Some(chat) = db.conversation_boundary(key)? {
        return Ok(chat);
    }
    if let Some(chat) = crate::chats::chat_for_run(db, key)? {
        return Ok(chat);
    }
    let run = required(db.run(key)?, "Conversation not found")?;
    if run["trigger"] == "chat" {
        return Err(Error::not_found(
            "The conversation for this chat run is no longer available.",
        ));
    }
    Ok(
        json!({ "id": key, "runId": key, "agentId": nodes::run_agent(&run), "mission": true, "paused": false }),
    )
}

async fn load_subject(s: &Service, key: &str) -> Result<Value> {
    let key = key.to_owned();
    s.store.read(move |db| subject(db, &key)).await
}

fn save_subject(db: &Db<'_>, chat: &Value) -> Result<()> {
    if chat["mission"] != true {
        db.save_conversation_boundary(chat)?;
    }
    Ok(())
}

fn chat_run(db: &Db<'_>, chat_id: &str) -> Result<(Value, Value, Value, bool)> {
    let chat = subject(db, chat_id)?;
    if chat["mission"] != true {
        require_active(&chat)?;
    }
    let run = required(db.run(text(&chat, "runId"))?, "Run not found")?;
    let mut agent = required(
        db.agent_snapshot(text(&chat, "agentId"))?,
        "Agent not found",
    )?;
    // Automatic profiles retain explicit conversion: the preview offers the
    // other environment, never an implicit move of existing files.
    let automatic = access(&agent)["backend"] == "auto";
    if automatic {
        agent["access"]["backend"] = if policy::native(&run["snapshot"]["agent"]) {
            "firecracker"
        } else {
            "native"
        }
        .into();
    }
    Ok((chat, run, agent, automatic))
}

pub(crate) fn prospective(
    db: &Db<'_>,
    run: &Value,
    agent: &Value,
    origin: &Origin,
) -> Result<Value> {
    check_access(run, agent)?;
    let mut proposed = run.clone();
    proposed["snapshot"]["agent"] = agent.clone();
    for key in ["provider", "model", "reasoning"] {
        proposed["snapshot"]["agent"][key] = run["snapshot"]["agent"][key].clone();
    }
    // A run without a speed keeps none, rather than the profile's or a null one.
    match run["snapshot"]["agent"].get("speed") {
        Some(speed) => proposed["snapshot"]["agent"]["speed"] = speed.clone(),
        None => {
            if let Some(agent) = proposed["snapshot"]["agent"].as_object_mut() {
                agent.remove("speed");
            }
        }
    }
    // An explicit authenticated owner adopts the existing work. Installation
    // agents keep their parent's bounded, revocable native authority.
    let grant = migration_grant(db, run, agent, origin)?;
    policy::apply(&mut proposed, &grant);
    Ok(proposed)
}

fn check_access(run: &Value, agent: &Value) -> Result<()> {
    // A backend switch necessarily replaces placement grants. Other reductions
    // retain the ordinary chat rule: old files cannot bypass reduced access.
    let mut previous = run["snapshot"]["agent"].clone();
    previous["access"]["backend"] = access(agent)["backend"].clone();
    previous["access"]["nodes"] = access(agent)["nodes"].clone();
    if !crate::service::covers(agent, &previous) {
        return Err(Error::conflict(
            "Other agent access was reduced. Restore that access before migrating this environment.",
        ));
    }
    Ok(())
}

pub(crate) fn migration_grant(
    db: &Db<'_>,
    run: &Value,
    agent: &Value,
    source: &Origin,
) -> Result<Value> {
    let retained;
    let source = if matches!(source, Origin::Run(id) if run["id"] == *id) {
        retained = Origin::Stored(run.clone());
        &retained
    } else {
        source
    };
    let mut grant = policy::authorize_origin(db, agent, source)?;
    // Initial/expired migration and the final conversion commit all construct
    // this grant inside their writer transaction. Owner adoption is explicit;
    // delegated callers must not make the target depend on its own descendant.
    policy::require_acyclic_parent(db, text(run, "id"), &grant)?;
    if matches!(source, Origin::Human(_)) {
        return Ok(grant);
    }
    // A delegated manager cannot replace the target's provenance with its own.
    // Only an explicit owner may adopt historical or unrelated work.
    if run["nativeProvenanceUnknown"] != false
        || policy::principals(run).as_array().is_none_or(Vec::is_empty)
        || grant["nativeProvenanceUnknown"] != false
        || (run["nativeParentRunId"].is_string()
            && run["nativeParentRunId"] != grant["nativeParentRunId"])
    {
        return Err(Error::forbidden(
            "An installation owner must explicitly adopt this conversation before a delegated agent can migrate it.",
        ));
    }
    let mut people = policy::principals(run)
        .as_array()
        .cloned()
        .unwrap_or_default();
    for person in policy::principals(&grant).as_array().into_iter().flatten() {
        if !people.contains(person) {
            people.push(person.clone());
        }
    }
    grant["nativePrincipals"] = people.into();
    policy::merge_plugin_grants(&mut grant, run);
    grant["nativeAuthorityNodes"] = grant["nativeAuthorityNodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| {
            run["nativeAuthorityNodes"]
                .as_array()
                .is_some_and(|old| old.contains(node))
        })
        .cloned()
        .collect::<Vec<_>>()
        .into();
    Ok(grant)
}

fn allowed(db: &Db<'_>, proposed: &Value, node: &Value) -> Result<bool> {
    // Pending migration grants are read again after asynchronous conversion.
    // An ancestor may have changed since preparation; neither backend may use
    // a saved prospective edge that now closes a cycle.
    policy::require_acyclic_parent(db, text(proposed, "id"), proposed)?;
    let agent = required(
        db.agent_snapshot(nodes::run_agent(proposed))?,
        "Agent not found",
    )?;
    Ok(node["revoked"] != true
        && node["capabilities"]["backend"] == backend(&proposed["snapshot"]["agent"])
        && nodes::agent_allows(&agent, text(node, "id"))
        && (!policy::node_native(node) || policy::allows(db, proposed, node)?))
}

/// Explicitly selects the new backend after cleanup has already retired every disk.
/// Keep the expired marker so the next message still receives the fresh-session handoff.
fn change_expired_backend(
    db: &Db<'_>,
    mut chat: Value,
    proposed: &Value,
    node: &Value,
    required_tags: &Value,
) -> Result<()> {
    let run_id = text(&chat, "runId");
    let mut boundary = json!({
        "requiredRuntime": node["runtimeId"],
        "environmentAdmissionRevision": crate::config::id(),
        // No disk exists yet: constrain fresh placement without recording a
        // materialized checkpoint. The owner can still change this preference.
        "isolated": false,
        "backup": null,
        "storage": null,
    });
    policy::apply(&mut boundary, proposed);

    db.change_run(
        run_id,
        crate::runs::RunChanges {
            snapshot: Some(serde_json::from_value(proposed["snapshot"].clone())?),
            session_id: Some(None),
            resume_available: Some(false),
            workspace: Some(None),
            recovery_pending: Some(false),
            placement: Some(Box::new(nodes::run_placement::PlacementChanges {
                required_tags: Some(serde_json::from_value(required_tags.clone())?),
                node_id: Some(None),
                pinned_node_id: Some(None),
                target_node_id: Some(serde_json::from_value(node["id"].clone())?),
                execution_backend: Some(
                    proposed["snapshot"]["agent"]["access"]["backend"]
                        .as_str()
                        .map(str::to_owned),
                ),
                preferred_node_id: Some(None),
                ..Default::default()
            })),
            boundary: Some(boundary),
            ..Default::default()
        },
    )?;

    db.event(
        run_id,
        &EventKind::Status,
        "Execution backend changed after environment expiration. The next message creates an empty environment; conversation history, uploads and published artifacts are retained.",
        None,
    )?;

    policy::apply(&mut chat, proposed);
    save_subject(db, &chat)?;

    for action in [
        "chat.backend-migration.requested",
        "chat.backend-migration.completed",
    ] {
        db.audit(
            action,
            &json!({
                "chatId": chat["id"],
                "backend": proposed["snapshot"]["agent"]["access"]["backend"],
                "nodeId": node["id"],
                "environmentExpired": true,
            }),
        )?;
    }
    Ok(())
}

pub(crate) async fn chat_id(s: &Service, run: &str) -> Result<String> {
    let run = run.to_owned();
    s.store
        .read(move |db| {
            Ok(crate::chats::chat_for_run(db, &run)?
                .map_or(run, |chat| text(&chat, "id").to_owned()))
        })
        .await
}

async fn refresh_authority(s: &Service, chat: &Value) -> Result<()> {
    let run = s.store.run(text(chat, "runId")).await?;
    let agent = s.agent_snapshot(text(chat, "agentId")).await?;
    if policy::native(&run["snapshot"]["agent"]) || policy::supports(&agent, "native") {
        policy::refresh(s).await?;
    }
    Ok(())
}

pub(crate) async fn preview(
    s: &Service,
    chat_id: &str,
    authorize: impl Fn(&Db<'_>) -> Result<()> + Clone + Send + 'static,
) -> Result<Value> {
    preview_live(
        s,
        chat_id,
        crate::authorization::Authorization::local(authorize),
    )
    .await
}

pub(crate) async fn preview_live(
    s: &Service,
    chat_id: &str,
    authorize: crate::authorization::Authorization,
) -> Result<Value> {
    authorize.recheck().await?;
    let check = authorize.clone();
    s.store.read(move |db| check.authorize(db)).await?;
    // Both native sources and destinations need fresh human authority.
    let chat = load_subject(s, chat_id).await?;
    refresh_authority(s, &chat).await?;
    authorize.recheck().await?;
    let (chat_id, origin) = (chat_id.to_owned(), origin());
    s.store.read(move |db| {
        authorize.authorize(db)?;
        let (chat, run, agent, automatic) = chat_run(db, &chat_id)?;
        let from = backend(&run["snapshot"]["agent"]);
        let to = backend(&agent);
        let mut nodes = Vec::new();
        let mut reason = Value::Null;
        if from != to {
            match prospective(db, &run, &agent, &origin) {
                Ok(proposed) => for node in db.nodes_boundary()? {
                    if allowed(db, &proposed, &node)? {
                        nodes.push(json!({ "id": node["id"], "name": node["name"] }));
                    }
                },
                Err(error) => reason = error.message.into(),
            }
        }
        let expired = run["environmentState"] == "expired";
        Ok(json!({
            "chatId": chat["id"],
            "mission": chat["mission"] == true,
            "fromBackend": from,
            "toBackend": to,
            "required": !automatic && from != to,
            "optional": automatic && from != to,
            "environmentExpired": expired,
            // Present while the environment exists: a change discards its working files.
            "warning": if expired { Value::Null } else { WARNING.into() },
            "nodes": nodes,
            "reason": reason,
            "pending": crate::environment_expiration::pending(db, text(&run, "id"))?,
            "error": if run["backendMigrationError"].is_string() { run["backendMigrationError"].clone() } else { run["movementError"].clone() },
        }))
    }).await
}

impl Service {
    pub(crate) async fn migrate_chat_backend<F>(&self, input: Value, authorize: F) -> Result<Value>
    where
        F: Fn(&Db<'_>) -> Result<()> + Clone + Send + Sync + 'static,
    {
        self.migrate_chat_backend_live(input, crate::authorization::Authorization::local(authorize))
            .await
    }

    pub(crate) async fn migrate_chat_backend_live(
        &self,
        input: Value,
        authorize: crate::authorization::Authorization,
    ) -> Result<Value> {
        authorize.recheck().await?;
        let mut input = input;
        if let Some(run_id) = input
            .as_object_mut()
            .and_then(|input| input.remove("runId"))
        {
            if input.get("chatId").is_some() {
                return Err(Error::bad("Choose chatId or runId, not both."));
            }
            input["chatId"] = run_id;
        }
        let input = parse("mcp:migrate_chat_backend", input)?;
        let chat_id = text(&input, "chatId").to_owned();
        // Check management authority before contacting Beacon or a controller.
        let check = authorize.clone();
        self.store.read(move |db| check.authorize(db)).await?;
        let chat = load_subject(self, &chat_id).await?;
        refresh_authority(self, &chat).await?;
        authorize.recheck().await?;
        let retained = self.store.run(text(&chat, "runId")).await?;
        let source = origin();
        let chosen = self.node_boundary(text(&input, "nodeId")).await?;
        if chosen["capabilities"]["backend"] != input["backend"] {
            return Err(Error::conflict(
                "Destination backend does not match the requested backend.",
            ));
        }
        let expired = retained["environmentState"] == "expired";
        if !expired {
            crate::environment_expiration::require_discard(
                input["discardWorkingFiles"] == true,
                WARNING,
            )?;
        }
        let (args, who) = (input.clone(), source.clone());
        let prepare = move |db: &Db<'_>,
                            expected_expired: bool|
              -> Result<(Value, Value, Value, Value)> {
            authorize.authorize(db)?;
            let (chat, run, agent, _) = chat_run(db, text(&args, "chatId"))?;
            if backend(&agent) != text(&args, "backend")
                || backend(&agent) == backend(&run["snapshot"]["agent"])
            {
                return Err(Error::conflict(
                    "Choose the backend currently configured on this conversation's agent.",
                ));
            }
            if (run["environmentState"] == "expired") != expected_expired {
                return Err(Error::conflict(
                    "The working environment changed. Review the migration again.",
                ));
            }
            let node = required(
                db.node_boundary(text(&args, "nodeId"))?,
                "Destination node not found",
            )?;
            let required_tags = args
                .get("requiredTags")
                .filter(|tags| !tags.is_null())
                .unwrap_or(&run["requiredTags"])
                .clone();
            if required_tags
                .as_array()
                .into_iter()
                .flatten()
                .any(|tag| !nodes::node_tags(&node).any(|present| present == tag))
            {
                return Err(Error::conflict("Destination lacks required tags."));
            }
            let proposed = prospective(db, &run, &agent, &who)?;
            if !allowed(db, &proposed, &node)? || !nodes::valid_runtime(text(&node, "runtimeId")) {
                return Err(Error::forbidden(
                    "Destination is not authorized for this backend.",
                ));
            }
            Ok((chat, proposed, node, required_tags))
        };
        if expired {
            self.store
                .transaction(move |db| {
                    let (chat, proposed, node, tags) = prepare(db, true)?;
                    change_expired_backend(db, chat, &proposed, &node, &tags)
                })
                .await?;
            self.worker.notify();
            return Ok(json!({ "status": "migrated", "chatId": chat_id }));
        }
        let run_id = text(&retained, "id").to_owned();
        let node_id = text(&input, "nodeId").to_owned();
        let expiration = Expiration {
            cause: EnvironmentExpirationCause::NodeChange,
            destination: Some(node_id.clone()),
            continue_turn: matches!(&source, Origin::Run(id) if *id == run_id),
        };
        self.expire_environment(&run_id, expiration, move |db| {
            let (chat, proposed, node, tags) = prepare(db, false)?;
            change_backend(db, chat, &proposed, &node, &tags)
        })
        .await?;
        Ok(json!({ "status": "migrated", "chatId": chat_id, "nodeId": node_id }))
    }
}

/// Conversions of an earlier version cannot finish after an update: the original
/// environment stays where it is, and the conversation leaves its migration pause.
pub(crate) fn abandon_legacy_conversions(db: &Db<'_>) -> Result<()> {
    let runs = db.json_rows(
        "SELECT data FROM run_views AS runs WHERE json_type(boundary,'$.backendMigration')='object'",
        [],
    )?;
    for run in runs {
        let id = text(&run, "id");
        let migration = &run["backendMigration"];
        if let Some(reservation) = migration["reservationId"].as_str() {
            nodes::placement::release_in(db, reservation)?;
        }
        db.update_run_boundary(
            id,
            &json!({
                "backendMigration": null,
                "resumeMissionAfterConversion": null,
                "backendMigrationError": "This backend migration was stopped by an update. Changing backend now discards the working files; confirm a new migration to continue.",
            }),
        )?;
        if let Some(mut chat) = crate::chats::chat_for_run(db, id)?
            && migration["paused"].is_boolean()
        {
            chat["paused"] = migration["paused"].clone();
            db.save_conversation_boundary(&chat)?;
        }
    }
    Ok(())
}

/// Records the destination backend and authority of an existing environment, whose
/// expiration follows in the same transaction.
fn change_backend(
    db: &Db<'_>,
    mut chat: Value,
    proposed: &Value,
    node: &Value,
    required_tags: &Value,
) -> Result<()> {
    let run_id = text(&chat, "runId").to_owned();
    let to = backend(&proposed["snapshot"]["agent"]);
    let mut boundary = json!({ "backendMigrationError": null });
    policy::apply(&mut boundary, proposed);
    db.change_run(
        &run_id,
        crate::runs::RunChanges {
            snapshot: Some(serde_json::from_value(proposed["snapshot"].clone())?),
            placement: Some(Box::new(nodes::run_placement::PlacementChanges {
                execution_backend: Some(Some(to.to_owned())),
                required_tags: Some(serde_json::from_value(required_tags.clone())?),
                ..Default::default()
            })),
            boundary: Some(boundary),
            ..Default::default()
        },
    )?;
    policy::apply(&mut chat, proposed);
    save_subject(db, &chat)?;
    let name = node["name"].as_str().unwrap_or("another node");
    let label = if to == "native" {
        "native execution"
    } else {
        "a Firecracker VM"
    };
    db.event(
        &run_id,
        &EventKind::Status,
        &format!("Changing this conversation to {label} on {name}."),
        None,
    )?;
    for action in [
        "chat.backend-migration.requested",
        "chat.backend-migration.completed",
    ] {
        db.audit(
            action,
            &json!({
                "chatId": chat["id"],
                "backend": to,
                "nodeId": node["id"],
                "environmentExpired": false,
            }),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{id, now},
        node_fixture::*,
        run_fixture::DbRunFixture,
        run_status::RunStatus,
    };

    #[tokio::test]
    async fn delegated_migration_keeps_historical_principals_and_parent_bounds() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store.transaction(|db| {
            let node = id(); let parent = id(); let coordinator = id(); let target = id();
            db.set("native-authority", &json!({ "observedAt": now(), "accounts": { "owner": {}, "editor": {} } }), None)?;
            let parent_agent = serde_json::to_value(crate::agent_fixture::profile(json!({ "id": coordinator, "access": { "installation": true } }))?)?;
            db.save_agent(&serde_json::from_value(parent_agent.clone())?)?;
            let parent_run = json!({
                "id": parent,
                "taskId": "fixture",
                "status": RunStatus::Running,
                "createdAt": now(),
                "snapshot": { "agent": parent_agent },
                "nativePrincipals": ["owner"],
                "nativeAuthorityNodes": [node],
                "nativeProvenanceUnknown": false,
            });
            db.add_run_fixture(&parent_run, None)?;
            let agent = serde_json::to_value(crate::agent_fixture::profile(json!({ "id": target, "access": { "backend": "native", "nodes": [node] } }))?)?;
            db.save_agent(&serde_json::from_value(agent.clone())?)?;
            db.put_node_fixture(&json!({
                "id": node,
                "capabilities": { "backend": "native", "nativeModeProtocol": 1 },
                "nativeModeObserved": { "mode": "protected", "generation": null, "status": "applied" },
                "nativeEnabled": true,
                "nativeAccounts": ["owner", "editor"],
            }))?;
            let origin = Origin::Run(parent.clone());
            let mut historical = json!({
                "snapshot": { "agent": { "id": target, "access": { "backend": "firecracker", "nodes": [node] } } },
            });
            assert!(prospective(db, &historical, &agent, &origin).is_err());
            historical["nativePrincipals"] = json!(["editor"]);
            historical["nativeAuthorityNodes"] = json!([node]);
            historical["nativeProvenanceUnknown"] = false.into();
            historical["nativeParentRunId"] = id().into();
            assert!(prospective(db, &historical, &agent, &origin).is_err());
            historical["nativeParentRunId"] = parent.clone().into();
            let candidate = prospective(db, &historical, &agent, &origin)?;
            // The run's speed carries over; a run without one keeps none, whatever the profile's.
            assert!(candidate["snapshot"]["agent"].get("speed").is_none());
            let mut faster = agent.clone();
            faster["speed"] = "priority".into();
            let standard = prospective(db, &historical, &faster, &origin)?;
            assert!(standard["snapshot"]["agent"].get("speed").is_none());
            let mut fast = historical.clone();
            fast["snapshot"]["agent"]["speed"] = "priority".into();
            assert_eq!(prospective(db, &fast, &agent, &origin)?["snapshot"]["agent"]["speed"], "priority");
            assert_eq!(candidate["nativePrincipals"], json!(["editor", "owner"]));
            assert_eq!(candidate["nativeParentRunId"], parent);
            assert!(allowed(db, &candidate, &db.node_boundary(&node)?.unwrap())?);
            let mut plugin_bound = historical.clone();
            plugin_bound["nativeParentRunId"] = Value::Null;
            plugin_bound["nativePluginGrants"] = json!([{
                "connectionId": id(), "revision": id(), "authorId": "external-person"
            }]);
            let delegated = prospective(db, &plugin_bound, &agent, &origin)?;
            assert!(!allowed(db, &delegated, &db.node_boundary(&node)?.unwrap())?,
                "An agent cannot erase a removed bot's revocation lineage by migrating its conversation");
            let adopted = prospective(db, &plugin_bound, &agent, &Origin::Human("owner".into()))?;
            assert!(allowed(db, &adopted, &db.node_boundary(&node)?.unwrap())?,
                "The explicit owner can still adopt existing work");
            let mut revoked = db.node_boundary(&node)?.unwrap();
            revoked["nativeAccounts"] = json!(["owner"]); db.put_node_fixture(&revoked)?;
            assert!(!allowed(db, &candidate, &revoked)?);
            let mut unknown = historical.clone(); unknown["nativeProvenanceUnknown"] = true.into();
            assert!(prospective(db, &unknown, &agent, &origin).is_err());
            // The explicitly authenticated owner can adopt even unknown history.
            assert!(prospective(db, &unknown, &agent, &Origin::Human("owner".into())).is_ok());
            Ok(())
        }).await.unwrap();
    }
}
