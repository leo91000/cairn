//! Explicit migration of an existing chat after its agent's backend changes.
//! Conversion and restoration finish before the source checkpoint is replaced.
use crate::{
    config::{id, now},
    conversation_lifecycle::require_active,
    error::{Error, Result, required},
    native::policy::{self, Origin},
    nodes,
    run_status::RunStatus,
    service::{Service, policy as access},
    store::Db,
    validation::{parse, text},
};
use cairn_contracts::EventKind;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

// Only the explicit migration operation uses the verified public MCP owner.
// Other management operations keep their existing provenance rules.
tokio::task_local! { pub(crate) static OWNER: Option<String>; }

static PROCESS: LazyLock<String> = LazyLock::new(id);

pub(crate) fn pending(run: &Value) -> bool {
    run["backendMigration"]["process"] == PROCESS.as_str()
}

/// Call only while holding the installation and in-process storage locks.
fn migration_staging(data_dir: &Path) -> Result<PathBuf> {
    let root = data_dir.join("backend-migration-staging");
    match std::fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.is_dir() => {
            // The root may be a separately mounted staging filesystem. Remove
            // abandoned conversions without trying to unlink its mountpoint.
            for entry in std::fs::read_dir(&root)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    std::fs::remove_dir_all(entry.path())?;
                } else {
                    std::fs::remove_file(entry.path())?;
                }
            }
        }
        Ok(_) => {
            return Err(Error::conflict(
                "Migration staging root must be a directory.",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&root)?,
        Err(error) => return Err(error.into()),
    }
    Ok(root)
}

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

pub(crate) fn candidate(run: &Value) -> Value {
    let migration = &run["backendMigration"];
    let mut candidate = run.clone();
    candidate["snapshot"] = migration["snapshot"].clone();
    policy::apply(&mut candidate, &migration["grant"]);
    candidate["nodeId"] = migration["nodeId"].clone();
    candidate["targetNodeId"] = migration["nodeId"].clone();
    candidate["pinnedNodeId"] = Value::Null;
    candidate["preferredNodeId"] = Value::Null;
    candidate["requiredRuntime"] = migration["runtimeId"].clone();
    candidate["placementTransition"] = true.into();
    if migration.get("requiredTags").is_some() {
        candidate["requiredTags"] = migration["requiredTags"].clone();
    }
    candidate
}

/// Capacity/materialization and disk reads use the pending destination grant
/// during a migration, including an explicit owner's adoption of old authority.
/// This is only a structural check; principal, node and revocation decisions
/// remain with each caller's existing transaction-local authorization.
pub(crate) fn require_lineage(db: &Db<'_>, run: &Value) -> Result<()> {
    policy::require_acyclic_parent(db, text(run, "id"), lineage(run))
}

/// The authority that preparation will actually use, without cloning a frozen
/// execution snapshot merely to inspect its parent edge.
pub(crate) fn lineage(run: &Value) -> &Value {
    let pending = &run["backendMigration"]["grant"];
    if pending.is_object() { pending } else { run }
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
        crate::conversation_lifecycle::require_environment_ready(&chat)?;
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

fn prospective(db: &Db<'_>, run: &Value, agent: &Value, origin: &Origin) -> Result<Value> {
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

/// A stopped source can fetch its own base while being copied. This is never a
/// workload lease: ordinary execution continues to use the current backend grants.
pub(crate) fn source_read(db: &Db<'_>, run_id: &str, node: &Value) -> Result<bool> {
    let Some(mut run) = db.run(run_id)? else {
        return Ok(false);
    };
    let direct_move =
        run["moveRequest"]["backend"].is_string() && run["moveRequest"]["sourceReadReady"] == true;
    if direct_move {
        let agent = required(db.agent_snapshot(nodes::run_agent(&run))?, "Agent removed")?;
        let mut snapshot = run["snapshot"].clone();
        snapshot["agent"] = agent;
        snapshot["agent"]["access"]["backend"] = run["moveRequest"]["backend"].clone();
        run["backendMigration"] = json!({ "process": PROCESS.as_str(), "sourceReadReady": true, "nodeId": run["moveRequest"]["nodeId"], "snapshot": snapshot, "grant": run.clone(), "runtimeId": nodes::component_runtime() });
    }
    if run["backendMigration"]["process"] != PROCESS.as_str()
        || run["backendMigration"]["sourceReadReady"] != true
        || !direct_move && RunStatus::of(&run).is_some_and(RunStatus::is_active)
        || nodes::owner_node(&run) != text(node, "id")
    {
        return Ok(false);
    }
    let agent = required(db.agent_snapshot(nodes::run_agent(&run))?, "Agent removed")?;
    check_access(&run, &agent)?;
    let destination = required(
        db.node_boundary(text(&run["backendMigration"], "nodeId"))?,
        "Destination removed",
    )?;
    if !allowed(db, &candidate(&run), &destination)? {
        return Ok(false);
    }
    if policy::node_native(node) {
        return policy::allows_migration_source(db, &run, node);
    }
    Ok(true)
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
        && (!policy::node_native(node)
            || !nodes::vm_disk(&nodes::db_checkpoint(db, text(proposed, "id"))?)
            || node["capabilities"]["vmDisks"] == true)
        && (!policy::node_native(node) || policy::allows(db, proposed, node)?))
}

/// Explicitly selects the new backend after cleanup has already retired every disk.
/// Keep the expired marker so the next message still receives the fresh-session handoff.
fn change_expired_backend(
    db: &Db<'_>,
    mut chat: Value,
    proposed: &Value,
    node: &Value,
) -> Result<()> {
    let run_id = text(&chat, "runId");
    let mut boundary = json!({
        "requiredRuntime": node["runtimeId"],
        // No disk exists yet: constrain fresh placement without recording a
        // materialized checkpoint. The owner can still change this preference.
        "backendMigration": null,
        "backendMigrationError": null,
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
                node_id: Some(serde_json::from_value(node["id"].clone())?),
                pinned_node_id: Some(serde_json::from_value(node["id"].clone())?),
                target_node_id: Some(None),
                preferred_node_id: Some(None),
                ..Default::default()
            })),
            boundary: Some(boundary),
            ..Default::default()
        },
    )?;

    crate::worker::checkpoint::RunCheckpoint {
        completed: Some(true),
        ..Default::default()
    }
    .store_in(db, run_id)?;

    db.event(
        run_id,
        &EventKind::Status,
        "Execution backend changed after environment expiration. The next message creates an empty environment; conversation history, uploads and published artifacts are retained.",
        None,
    )?;

    policy::apply(&mut chat, proposed);
    chat["sessionRestartRequested"] = true.into();
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
        Ok(json!({
            "chatId": chat["id"],
            "mission": chat["mission"] == true,
            "diskFormat": if nodes::vm_disk(&nodes::db_checkpoint(db, text(&run, "id"))?) { "vm" } else { "native" },
            "fromBackend": from,
            "toBackend": to,
            "required": !automatic && from != to,
            "optional": automatic && from != to,
            "environmentExpired": chat["environmentState"] == "expired",
            "nodes": nodes,
            "reason": reason,
            "conversion": !nodes::vm_disk(&nodes::db_checkpoint(db, text(&run, "id"))?) && to == "firecracker",
            "pending": run["backendMigration"]["process"] == PROCESS.as_str() || run["moveRequest"].is_object(),
            "error": if run["backendMigration"].is_object() && run["backendMigration"]["process"] != PROCESS.as_str() {
                run["backendMigrationError"].as_str().map_or_else(|| json!("Migration interrupted by a manager restart. The original environment is retained; retry migration."), |message| json!(message))
            } else if run["backendMigrationError"].is_string() { run["backendMigrationError"].clone() } else { run["movementError"].clone() },
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
        self.migrate_backend(input, authorize, origin(), false)
            .await
    }

    pub(crate) async fn convert_for_vm(
        &self,
        run: &Value,
        args: &Value,
        authorize: nodes::moves::Authorization,
        by_agent: bool,
    ) -> Result<Value> {
        authorize.recheck(self).await?;
        let guard = authorize.clone();
        let auth = if let Some(auth) = authorize.management_authorization() {
            auth
        } else if let Some(bearer) = authorize.workspace_bearer() {
            crate::authorization::Authorization::workspace_move(self, &bearer, text(run, "id"))
                .await?
        } else {
            crate::authorization::Authorization::new(self, move |db| guard.check(db))
        };
        let key = chat_id(self, text(run, "id")).await?;
        let source = if by_agent {
            Origin::Stored(run.clone())
        } else {
            origin()
        };
        let node_id = args["nodeId"].clone();
        self.migrate_backend(json!({ "chatId": key, "backend": "firecracker", "nodeId": node_id, "confirm": true, "requiredTags": args["requiredTags"], "waitSeconds": args["waitSeconds"].as_u64().unwrap_or(0) }), auth, source, by_agent).await?;
        Ok(json!({ "status": "migrating", "nodeId": node_id }))
    }

    fn migrate_backend(
        &self,
        input: Value,
        authorize: crate::authorization::Authorization,
        source: Origin,
        own_move: bool,
    ) -> futures_util::future::BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
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
            if input["waitSeconds"].as_u64().unwrap_or(0)
                > nodes::publication::settings(self).await?["maxCapacityWaitSeconds"]
                    .as_u64()
                    .unwrap_or(3600)
            {
                return Err(Error::bad("Capacity wait exceeds the configured maximum."));
            }
            let chat_id = text(&input, "chatId").to_owned();
            // Check management authority before contacting Beacon or a controller.
            let check = authorize.clone();
            self.store.read(move |db| check.authorize(db)).await?;
            let chat = load_subject(self, &chat_id).await?;
            refresh_authority(self, &chat).await?;
            authorize.recheck().await?;
            let retained = self.store.run(text(&chat, "runId")).await?;
            let own_move = own_move || matches!(&source, Origin::Run(id) if retained["id"] == *id);
            let checkpoint = nodes::checkpoint(self, text(&retained, "id")).await?;
            let chosen = self.node_boundary(text(&input, "nodeId")).await?;
            if chosen["capabilities"]["backend"] != input["backend"] {
                return Err(Error::conflict(
                    "Destination backend does not match the requested backend.",
                ));
            }
            if chat["environmentState"] != "expired" && nodes::vm_disk(&checkpoint) {
                let movement = nodes::moves::Authorization::new(|_| Ok(()), None)
                    .with_management_authorization(authorize)
                    .with_origin(source.clone());
                return Box::pin(nodes::moves::request_authorized(
                    self,
                    &retained,
                    &json!({ "nodeId": input["nodeId"], "requiredTags": input["requiredTags"], "waitSeconds": input["waitSeconds"].as_u64().unwrap_or(0) }),
                    movement,
                ))
                .await;
            }
            let required_tags = input
                .get("requiredTags")
                .filter(|tags| !tags.is_null())
                .unwrap_or(&retained["requiredTags"])
                .clone();
            if required_tags
                .as_array()
                .into_iter()
                .flatten()
                .any(|tag| !nodes::node_tags(&chosen).any(|present| present == tag))
            {
                return Err(Error::conflict("Destination lacks required tags."));
            }
            if retained["trigger"] != "chat" && retained["wakeup"].is_object() {
                return Err(Error::conflict(
                    "Finish the mission wait before changing backend.",
                ));
            }
            let operation = id();
            let (args, who, migration) = (input.clone(), source.clone(), operation.clone());
            let job_authorization = authorize.clone().accepted().await?;
            if own_move {
                job_authorization.bind_self_move(text(&retained, "id"), &operation)?;
            }
            let request_authorization = authorize.clone();
            let expired = self.store.transaction(move |db| {
            request_authorization.authorize(db)?;
            let (mut chat, run, agent, _) = chat_run(db, text(&args, "chatId"))?;
            if backend(&agent) != text(&args, "backend") || backend(&agent) == backend(&run["snapshot"]["agent"]) {
                return Err(Error::conflict("Choose the backend currently configured on this conversation's agent."));
            }
            if run["backendMigration"]["process"] == PROCESS.as_str() {
                return Err(Error::conflict("A backend migration is already in progress."));
            }
            if run["moveRequest"].is_object() {
                return Err(Error::conflict("Finish the pending node move first."));
            }
            let checkpoint = nodes::db_checkpoint(db, text(&run, "id"))?;
            let expired = chat["environmentState"] == "expired";
            if !expired && !checkpoint["prepared"]["projectRoot"].is_string() {
                return Err(Error::conflict("This conversation has no saved isolated environment to migrate."));
            }
            let node = required(db.node_boundary(text(&args, "nodeId"))?, "Destination node not found")?;
            let proposed = prospective(db, &run, &agent, &who)?;
            if !allowed(db, &proposed, &node)? || !nodes::valid_runtime(text(&node, "runtimeId")) {
                return Err(Error::forbidden("Destination is not authorized for this backend."));
            }
            if expired {
                change_expired_backend(db, chat, &proposed, &node)?;
                return Ok(true);
            }
            let previous = &run["backendMigration"];
            let reusable = previous["convertedBackup"].is_string()
                && previous["nodeId"] == node["id"]
                && previous["runtimeId"] == node["runtimeId"]
                && backend(&previous["snapshot"]["agent"]) == backend(&agent);
            let reservation = previous["reservationId"].as_str().map_or_else(|| migration.clone(), str::to_owned);
            let migration = json!({
                "convertedBackup": if reusable { previous["convertedBackup"].clone() } else { Value::Null },
                "id": migration,
                "process": PROCESS.as_str(),
                "sourceReadReady": false,
                "selfMove": own_move,
                "requiredTags": required_tags,
                "resumeMission": chat["mission"] == true && (RunStatus::of(&run).is_some_and(RunStatus::is_active) || previous["resumeMission"] == true || run["resumeMissionAfterConversion"] == true),
                "nodeId": node["id"],
                "runtimeId": node["runtimeId"],
                "snapshot": proposed["snapshot"],
                "grant": migration_grant(db, &run, &agent, &who)?,
                "reservationId": reservation,
                "agentRevision": agent["revision"],
                "paused": if previous.is_object() { previous["paused"].clone() } else { chat["paused"].clone() },
                "createdAt": now(),
            });
            for mut message in db.messages(text(&chat, "id"))? {
                if message["status"] == crate::chats::MessageStatus::Sending {
                    message["status"] = crate::chats::MessageStatus::Cancelled.into();
                    db.put_message(&message)?;
                }
            }
            chat["paused"] = true.into();
            save_subject(db, &chat)?;
            db.update_run_boundary(text(&run, "id"), &json!({ "resumeMissionAfterConversion": migration["resumeMission"], "backendMigration": migration, "backendMigrationError": null }))?;
            db.audit("chat.backend-migration.requested", &json!({
                "chatId": chat["id"],
                "backend": args["backend"],
                "nodeId": node["id"],
            }))?;
            Ok(false)
        }).await?;
            if expired {
                self.worker.notify();
                return Ok(json!({ "status": "migrated", "chatId": chat_id }));
            }
            let s = self.clone();
            let result = json!({
                "status": "migrating",
                "chatId": chat_id,
                "migrationId": operation,
            });
            tokio::spawn(async move {
                let outcome = s
                    .convert_chat_live(&chat_id, &operation, input, source, job_authorization)
                    .await;
                if let Err(error) = outcome {
                    let message = error.message.clone();
                    let _ = s
                        .store
                        .transaction(move |db| {
                            let Ok(chat) = subject(db, &chat_id) else {
                                return Ok(());
                            };
                            let Some(run) = db.run(text(&chat, "runId"))? else {
                                return Ok(());
                            };
                            if run["backendMigration"]["id"] == operation {
                                let mut migration = run["backendMigration"].clone();
                                migration["process"] = Value::Null;
                                // Before a converted point exists no destination has been
                                // reserved. Clear the guard so prerequisites (including a
                                // legacy native upgrade) can be repaired on the source.
                                if !migration["convertedBackup"].is_string() {
                                    migration = Value::Null;
                                }
                                db.update_run_boundary(
                                    text(&run, "id"),
                                    &json!({
                                        "backendMigration": migration,
                                                "backendMigrationError": message,
                                    }),
                                )?;
                            }
                            Ok(())
                        })
                        .await;
                    tracing::warn!(error = %error, "Backend migration retained its original environment");
                }
            });
            Ok(result)
        })
    }

    async fn convert_chat_live(
        &self,
        chat_id: &str,
        operation: &str,
        args: Value,
        source: Origin,
        authorize: crate::authorization::Authorization,
    ) -> Result<()> {
        authorize.recheck().await?;
        let chat = load_subject(self, chat_id).await?;
        let run_id = text(&chat, "runId").to_owned();
        let _storage_lock = crate::conversation_lifecycle::storage_lock(&self.config.data_dir)?;
        let _storage = self.conversation_storage_lock.lock().await;
        authorize.recheck().await?;
        // Both storage locks exclude every live converter. Anything retained in
        // this dedicated directory therefore belongs to a terminated process.
        // Reclaim it before capacity checks, including retries using a saved point.
        let staging_root = migration_staging(&self.config.data_dir)?;
        let initial = self.store.run(&run_id).await?;
        if RunStatus::of(&initial).is_some_and(RunStatus::is_active)
            && initial["backendMigration"]["selfMove"] == true
        {
            if let Some(stop) = self.worker.active.lock().await.get(&run_id) {
                stop.cancel();
            }
        } else if RunStatus::of(&initial).is_some_and(RunStatus::is_active) {
            match self
                .worker
                .cancel_live(self, &run_id, authorize.clone())
                .await
            {
                Ok(()) => {}
                Err(error) if error.is_conflict() => {}
                Err(error) => return Err(error),
            }
        }
        for _ in 0..600 {
            if !self.worker.active.lock().await.contains_key(&run_id) {
                break;
            }
            tokio::select! {
                () = authorize.cancelled() => {
                    return Err(Error::forbidden("The accepted migration was cancelled."));
                }
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
        if self.worker.active.lock().await.contains_key(&run_id) {
            return Err(Error::unavailable(
                "Waiting for the previous execution to stop; retry migration after it finishes.",
            ));
        }
        let preparation = self.worker.run_preparation_lock(&run_id).await;
        let _preparation = preparation.lock_owned().await;
        let run = self.store.run(&run_id).await?;
        authorize.recheck().await?;
        crate::recovery::fence_authorized(self, &run, authorize.clone()).await?;
        authorize.recheck().await?;
        if RunStatus::of(&run).is_some_and(RunStatus::is_active) {
            let id = run_id.clone();
            let check = authorize.clone();
            self.store
                .transaction(move |db| {
                    check.check_local(db)?;
                    db.change_run(
                        &id,
                        crate::runs::RunChanges {
                            status: Some(RunStatus::Cancelled),
                            recovery_pending: Some(false),
                            ..Default::default()
                        },
                    )
                    .map(|_| ())
                })
                .await?;
        }
        let (key, operation_id) = (run_id.clone(), operation.to_owned());
        authorize.recheck().await?;
        let check = authorize.clone();
        let run = self
            .store
            .transaction(move |db| {
                check.check_local(db)?;
                let mut current = required(db.run(&key)?, "Conversation removed")?;
                if current["backendMigration"]["id"] != operation_id {
                    return Err(Error::conflict(
                        "Migration changed while fencing its source.",
                    ));
                }
                current["backendMigration"]["sourceReadReady"] = true.into();
                db.update_run_boundary(
                    &key,
                    &json!({ "backendMigration": current["backendMigration"] }),
                )?;
                required(db.run(&key)?, "Conversation removed")
            })
            .await?;
        let source_node = self.node_boundary(nodes::owner_node(&run)).await?;
        let (source_run_id, read_node) = (run_id.clone(), source_node.clone());
        let readable = self
            .store
            .read(move |db| source_read(db, &source_run_id, &read_node))
            .await?;
        if !readable {
            return Err(Error::forbidden(
                "The source environment is no longer authorized for migration reads.",
            ));
        }
        let reservation = text(&run["backendMigration"], "reservationId").to_owned();
        // The reservation ID survives process death. No workload starts during
        // migration; release an interrupted admission while retaining any
        // materialized disk allocation, then reserve again with live limits.
        nodes::placement::release(self, &reservation).await?;
        authorize.recheck().await?;
        let node = self.node_boundary(text(&args, "nodeId")).await?;
        let point = if let Some(saved) = run["backendMigration"]["convertedBackup"].as_str() {
            // A controller may have installed this exact point before losing its
            // reply. Retrying it also completes the native restore transaction.
            self.get("node-backups", saved).await?
        } else {
            let point =
                nodes::publication::capture_authorized(self, &run, authorize.clone()).await?;
            let backup = self.get("node-backups", text(&point, "id")).await?;
            let manifest = nodes::publication::manifest(self, &backup).await?;
            if manifest["kind"] == "native-files-v1" {
                return Err(Error::conflict(
                    "Resume this legacy native environment once on its original node to upgrade its storage before migrating.",
                ));
            }
            let staging = tempfile::Builder::new()
                .prefix("conversion-")
                .tempdir_in(&staging_root)?;
            let raw = staging.path().join("source.ext4");
            let reserve = materialize(self, &backup, &manifest, &raw, &node, &authorize).await?;
            let checkpoint = nodes::checkpoint(self, &run_id).await?;
            authorize.recheck().await?;
            let disk = crate::storage::backend_conversion::convert(
                &raw,
                &manifest,
                &checkpoint["prepared"],
                text(&args, "backend"),
                staging.path(),
                reserve,
            )
            .await?;
            authorize.recheck().await?;
            let mut converted = nodes::snapshots::index(&disk).await?;
            converted["runtime"] = json!({
                "runtimeId": node["runtimeId"],
                "backend": args["backend"],
                "os": "linux",
                "arch": "x86_64",
                "storage": "ublk",
            });
            converted["onDemand"] = true.into();
            converted["generation"] = 1.into();
            converted["capturedAt"] = now().into();
            if args["backend"] == "native" {
                converted["kind"] = "native-ext4-v1".into();
            }
            nodes::publication::converted_environment_authorized(
                self,
                &run,
                &node,
                &disk,
                converted,
                authorize.clone(),
            )
            .await?
        };
        policy::refresh(self).await?;
        let proposed = candidate(&self.store.run(&run_id).await?);
        let placement_authorization = nodes::moves::Authorization::new(|_| Ok(()), None)
            .with_management_authorization(authorize.clone());
        let deadline = now() + args["waitSeconds"].as_u64().unwrap_or(0) as i64 * 1000;
        let placement = loop {
            match nodes::placement::reserve_authorized(
                self,
                &proposed,
                &reservation,
                placement_authorization.clone(),
            )
            .await
            {
                Ok(placement) => break placement,
                Err(error) if error.is_unavailable() && now() < deadline => {
                    tokio::select! { () = authorize.cancelled() => return Err(Error::forbidden("Accepted migration cancelled.")), () = tokio::time::sleep(Duration::from_secs(1)) => {} }
                }
                Err(error) => return Err(error),
            }
        };
        let restored = async {
            nodes::placement::materialize_authorized(self, &reservation, authorize.clone()).await?;
            nodes::restore::start_authorized(
                self,
                &proposed,
                text(&node, "id"),
                &point,
                authorize.clone(),
            )
            .await
        }
        .await;
        let release = nodes::placement::release(self, &reservation).await;
        restored?;
        release?;
        policy::refresh(self).await?;
        authorize.recheck().await?;
        let (chat_id, operation) = (chat_id.to_owned(), operation.to_owned());
        self.store.transaction(move |db| {
            authorize.authorize(db)?;
            let (mut chat, current, agent, _) = chat_run(db, &chat_id)?;
            let migration = &current["backendMigration"];
            if migration["id"] != operation || migration["agentRevision"] != agent["revision"]
                || backend(&agent) != text(&args, "backend") {
                return Err(Error::conflict("Conversation or agent changed during backend migration."));
            }
            let proposed = prospective(db, &current, &agent, &source)?;
            let source_node = required(db.node_boundary(text(&source_node, "id"))?, "Source node removed")?;
            if !source_read(db, &run_id, &source_node)? {
                return Err(Error::forbidden("Source read permission was revoked during migration."));
            }
            let destination = required(db.node_boundary(text(&node, "id"))?, "Destination node removed")?;
            if !allowed(db, &proposed, &destination)? {
                return Err(Error::forbidden("Destination permission was revoked during migration."));
            }
            let mut checkpoint = nodes::db_checkpoint(db, &run_id)?;
            checkpoint["prepared"]["backend"] = args["backend"].clone();
            checkpoint["prepared"]["diskFormat"] = if args["backend"] == "firecracker" { "vm" } else { "native" }.into();
            checkpoint["nodeId"] = node["id"].clone();
            checkpoint["runtimeId"] = node["runtimeId"].clone();
            checkpoint["resources"] = placement["resources"].clone();
            checkpoint["runnerId"] = run_id.clone().into();
            checkpoint["launched"] = false.into();
            checkpoint["freshSession"] = true.into();
            let resume_mission = chat["mission"] == true && migration["resumeMission"] == true;
            checkpoint["workspaceInitialized"] = true.into();
            checkpoint["completed"] = (!resume_mission).into();
            for key in ["process", "settled", "controllerRecoveries"] { checkpoint[key] = Value::Null; }
            db.save_checkpoint_boundary(&run_id, &checkpoint)?;
            // This boundary replaces the backup pointer. Preserve only the
            // historical diagnosis; the migration owns the new pointer/status.
            let mut migrated_backup = json!({
                "id": point["id"],
                "snapshotId": point["snapshotId"],
                "capturedAt": point["capturedAt"],
                "status": "ready"
            });
            if let Some(context) = current["backup"].get("lastFailureContext") {
                migrated_backup["lastFailureContext"] = context.clone();
            }
            let mut patch = crate::runs::RunChanges {
                snapshot: Some(serde_json::from_value(proposed["snapshot"].clone())?),
                session_id : Some(None),
                resume_available : Some(false),
                status : Some(if resume_mission { RunStatus::Queued } else { RunStatus::Succeeded }),
                error : Some(None),
                cancel_requested_at : Some(None),
                recovery_pending : Some(resume_mission),
                placement: Some(Box::new(crate::nodes::run_placement::PlacementChanges {
                    required_tags: Some(serde_json::from_value(migration.get("requiredTags").unwrap_or(&current["requiredTags"]).clone())?),
                    execution_backend: Some(Some(text(&args, "backend").to_owned())),
                    node_id: Some(serde_json::from_value(serde_json::json!(node["id"]))?),
                    resources: Some(serde_json::from_value(serde_json::json!(placement["resources"]))?),
                    pinned_node_id: Some(None),
                    preferred_node_id: Some(None),
                    node_state: Some(None),
                    ..Default::default()
                })),
                boundary: Some(serde_json::json!({
                    "requiredRuntime": node["runtimeId"],
                    "backendMigration": null,
                    "backendMigrationError": null,
                    "resumeMissionAfterConversion": null,
                    "storage": { "mode": nodes::ON_DEMAND },
                    "backup": migrated_backup
                })),
                ..Default::default()
            };
            let grant = migration_grant(db, &current, &agent, &source)?;
            if chat["mission"] == true { patch.boundary.as_mut().unwrap()["sessionHandoff"] = crate::chats::handoff_context(db, &run_id)?.into(); }
            policy::apply(patch.boundary.as_mut().unwrap(), &grant);
            db.change_run(&run_id, patch)?;
            policy::apply(&mut chat, &grant);
            chat["paused"] = migration["paused"].clone();
            chat["sessionRestartRequested"] = true.into();
            chat["wakeup"] = Value::Null;
            save_subject(db, &chat)?;
            db.event(&run_id, &EventKind::Status, "Execution backend migrated. Conversation history and files preserved; the next message starts a fresh engine session.", None)?;
            db.audit("chat.backend-migration.completed", &json!({
                "chatId": chat_id,
                "backend": args["backend"],
                "nodeId": node["id"],
            }))?;
            Ok(())
        }).await?;
        self.worker.notify();
        Ok(())
    }
}

async fn materialize(
    s: &Service,
    backup: &Value,
    manifest: &Value,
    path: &Path,
    node: &Value,
    authorize: &crate::authorization::Authorization,
) -> Result<u64> {
    let size = manifest["size"]
        .as_u64()
        .ok_or_else(|| Error::bad("Missing saved disk size."))?;
    if size
        > node["limits"]["diskMiB"]
            .as_u64()
            .unwrap_or(0)
            .saturating_mul(1024 * 1024)
    {
        return Err(Error::new(
            507,
            "The destination disk limit is smaller than the saved environment.",
        ));
    }
    // Staging lives on the manager. A remote destination's reserve cannot
    // lower the protection of the manager's database filesystem.
    let local = s
        .store
        .node_boundary(nodes::LOCAL_NODE_ID)
        .await?
        .unwrap_or_default();
    let policy = crate::storage::policy::for_node(&local["storage"])?;
    let (total, free) = crate::storage::policy::space(path.parent().unwrap())?;
    let payload: u64 = manifest["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["hash"].is_string())
        .filter_map(|block| block["size"].as_u64())
        .sum();
    if free
        < policy
            .reserve(total)
            .saturating_add(size)
            .saturating_add(payload.saturating_mul(4))
            .saturating_add(64 * 1024 * 1024)
    {
        return Err(Error::new(
            507,
            "The installation needs additional temporary disk space to migrate this environment; the original disk is retained.",
        ));
    }
    authorize.recheck().await?;
    let mut output = tokio::fs::File::create(path).await?;
    output.set_len(size).await?;
    for block in manifest["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["hash"].is_string())
    {
        authorize.recheck().await?;
        let bytes =
            nodes::publication::read_manifest_block_authorized(s, backup, block, authorize).await?;
        authorize.recheck().await?;
        output
            .seek(std::io::SeekFrom::Start(block["offset"].as_u64().unwrap()))
            .await?;
        output.write_all(&bytes).await?;
    }
    output.sync_all().await?;
    Ok(policy.reserve(total))
}

#[cfg(test)]
use crate::node_fixture::*;

#[cfg(test)]
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn self_conversion_retains_stop_and_plugin_revocation_after_worker_exit() {
        use crate::agent_fixture::StoreAgentFixture;
        for change in ["stop", "plugin"] {
            let f = crate::plugin_grant_fixture::Fixture::new().await;
            f.set(json!([crate::plugin_grant_fixture::contribution(
                "tool",
                "grant_probe_post",
                crate::plugin_grant_fixture::support()
            )]))
            .await;
            let run_id = f.run.clone();
            let node = id();
            let mut profile =
                f.s.agent_snapshot(crate::config::MAIN_AGENT_ID)
                    .await
                    .unwrap();
            profile["access"]["backend"] = "auto".into();
            profile["access"]["installation"] = false.into();
            profile["access"]["nodes"] = json!([node]);
            f.s.store.put_agent_fixture(profile.clone()).await.unwrap();
            let chat =
                f.s.chat_create(json!({"agentId": profile["id"]}))
                    .await
                    .unwrap();
            let chat_id = text(&chat, "id").to_owned();
            let mut snapshot = f.s.store.run(&run_id).await.unwrap()["snapshot"].clone();
            snapshot["agent"] = profile;
            snapshot["agent"]["access"]["backend"] = "native".into();
            f.s.store.transaction({
                let run_id = run_id.clone();
                let mut chat = chat.clone();
                let node = node.clone();
                move |db| {
                    chat["runId"] = run_id.clone().into();
                    db.save_conversation_boundary(&chat)?;
                    let resolved = subject(db, &run_id)?;
                    assert_eq!(resolved["id"], chat["id"], "runId must retain chat lifecycle and message identity");
                    assert_ne!(resolved["mission"], true);
                    db.change_run(&run_id, crate::runs::RunChanges {
                        snapshot: Some(serde_json::from_value(snapshot)?),
                        trigger: Some("chat".into()),
                        boundary: Some(json!({"nativePrincipals": ["owner"], "nativeAuthorityNodes": [node], "nativeProvenanceUnknown": false})),
                        ..Default::default()
                    })?;
                    db.save_checkpoint_boundary(&run_id, &json!({"prepared": {"backend": "native", "projectRoot": "/data/runs/test/workspace"}}))?;
                    db.put_node_fixture(&json!({"id": node, "runtimeId": "development", "capabilities": {"backend": "firecracker"}}))?;
                    db.set("native-authority", &json!({"observedAt": now(), "accounts": {}}), None)?;
                    Ok(())
                }
            }).await.unwrap();
            let current = f.s.store.run(&run_id).await.unwrap();
            let token = f.s.mcps.run_configuration(&f.s, &current).await.unwrap();
            let bearer = text(&token["env"], "CAIRN_MCP_RUN_TOKEN");
            let proof = nodes::moves::Authorization::workspace(bearer, &current);
            let storage = f.s.conversation_storage_lock.clone().lock_owned().await;
            let marker =
                f.s.config
                    .data_dir
                    .join("backend-migration-staging/existing-source");
            tokio::fs::create_dir_all(marker.parent().unwrap())
                .await
                .unwrap();
            tokio::fs::write(&marker, b"retained").await.unwrap();
            // Exercise the actual workspace move admission, with no installation grant.
            f.s.convert_for_vm(&current, &json!({"nodeId": node}), proof, true)
                .await
                .unwrap();
            let admitted = f.s.store.run(&run_id).await.unwrap();
            assert!(pending(&admitted));
            assert_eq!(admitted["backendMigration"]["selfMove"], true);
            f.s.store
                .change_run(
                    &run_id,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Cancelled),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            f.s.mcps.revoke_run(&f.s, &run_id).await.unwrap();
            if change == "stop" {
                f.s.worker
                    .stop_chat_live(
                        &f.s,
                        &chat_id,
                        crate::authorization::Authorization::default(),
                    )
                    .await
                    .unwrap();
            } else {
                f.marker("revoked-during-self-conversion").await;
            }
            drop(storage);
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let current = f.s.store.run(&run_id).await.unwrap();
                    if !pending(&current) {
                        assert!(current["backendMigrationError"].is_string(), "{current}");
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(tokio::fs::read(&marker).await.unwrap(), b"retained");
            assert_eq!(
                f.s.store.run(&run_id).await.unwrap()["snapshot"]["agent"]["access"]["backend"],
                "native"
            );
            if change == "stop" {
                assert_eq!(
                    f.s.store
                        .conversation_boundary(&chat_id)
                        .await
                        .unwrap()
                        .unwrap()["paused"],
                    true
                );
            }
            f.s.shutdown.cancel();
        }
    }

    #[tokio::test]
    async fn merge_preserves_native_reviews_and_active_conversion_ownership() {
        use crate::agent_fixture::profile;
        let directory = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(directory.path()).unwrap();
        store.transaction(|db| {
            let (source, target, node, run) = (id(), id(), id(), id());
            for agent in [&source, &target] {
                db.save_agent(&profile(json!({"id": agent, "access": {"backend": "auto", "nodes": [node]}}))?)?;
            }
            let mut host = db.put_node_fixture(&json!({
                "id": node, "capabilities": {"backend": "native", "nativeModeProtocol": 1},
                "nativeModeRequest": {"mode": "administrator", "generation": "review"},
                "nativeModeObserved": {"mode": "administrator", "generation": "review", "status": "applied"},
                "nativeReviewedAgents": [source],
            }))?;
            let snapshot = db.agent_snapshot(&source)?.unwrap();
            db.add_run_fixture(&json!({"id": run, "taskId": id(), "status": RunStatus::Succeeded, "createdAt": now(), "snapshot": {"agent": snapshot}, "backendMigration": {"process": PROCESS.as_str()}}), None)?;
            let input = serde_json::from_value(json!({"sourceId": source, "targetId": target, "confirm": true}))?;
            assert!(crate::agent_merge::merge(db, &input, &[]).unwrap_err().message.contains("administrator approval"));
            assert!(db.agent(&source)?.is_some());
            assert_eq!(nodes::run_agent(&db.run(&run)?.unwrap()), source);
            host["nativeReviewedAgents"] = json!([source, target]);
            db.put_node_fixture(&host)?;
            assert!(crate::agent_merge::merge(db, &input, &[]).unwrap_err().message.contains("active work"));
            assert!(db.agent(&source)?.is_some());
            // A process from a prior boot is interrupted, never a live converter.
            db.update_run_boundary(&run, &json!({"backendMigration": {"process": "previous-process"}}))?;
            crate::agent_merge::merge(db, &input, &[])?;
            assert!(db.agent(&source)?.is_none());
            assert_eq!(nodes::run_agent(&db.run(&run)?.unwrap()), target);
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn pending_migration_rechecks_ancestor_edges_before_reads_and_final_grant() {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(directory.path()).unwrap();
        store.transaction(|db| {
            let (vm, native, profile, target, caller) = (id(), id(), id(), id(), id());
            db.set("native-authority", &json!({
                "observedAt": now(),
                "accounts": { "owner": {} },
            }), None)?;
            db.put_node_fixture(&json!({
                "id": vm,
                "capabilities": { "backend": "firecracker" },
            }))?;
            let native_node = db.put_node_fixture(&json!({
                "id": native,
                "capabilities": { "backend": "native", "nativeModeProtocol": 1 },
                "nativeModeObserved": { "mode": "protected", "generation": null, "status": "applied" },
                "nativeEnabled": true,
                "nativeAccounts": ["owner"],
            }))?;
            let current = serde_json::to_value(crate::agent_fixture::profile(json!({
                "id": profile,
                "access": { "backend": "auto", "nodes": [vm, native], "installation": true },
            }))?)?;
            db.save_agent(&serde_json::from_value(current.clone())?)?;
            let mut vm_agent = current.clone();
            vm_agent["access"]["backend"] = "firecracker".into();
            for run in [&target, &caller] {
                db.add_run_fixture(&json!({
                    "id": run,
                    "taskId": id(),
                    "status": RunStatus::Running,
                    "createdAt": now(),
                    "snapshot": { "agent": vm_agent },
                    "nodeId": vm,
                    "nativePrincipals": ["owner"],
                    "nativeAuthorityNodes": [native],
                    "nativeProvenanceUnknown": false,
                }), None)?;
            }
            let original = db.run(&target)?.unwrap();
            let source = Origin::Run(caller.clone());
            let mut destination = current;
            destination["access"]["backend"] = "native".into();
            let initial = prospective(db, &original, &destination, &source)?;
            assert!(allowed(db, &initial, &native_node)?);
            let grant = migration_grant(db, &original, &destination, &source)?;
            db.update_run_boundary(&target, &json!({
                "backendMigration": {
                    "process": PROCESS.as_str(),
                    "sourceReadReady": true,
                    "nodeId": native,
                    "snapshot": initial["snapshot"],
                    "grant": grant,
                },
            }))?;

            // A normal directing effect can change a still-independent ancestor
            // after preparation. It does not see an uncommitted migration edge.
            policy::authorize_run_in(db, &caller, &Origin::Run(target.clone()))?;
            assert_eq!(db.run(&caller)?.unwrap()["nativeParentRunId"], target);
            db.change_run(&target, crate::runs::RunChanges {
                status: Some(RunStatus::Succeeded),
                ..Default::default()
            })?;
            let before = db.run(&target)?.unwrap();
            assert!(before["nativeParentRunId"].is_null());
            let pending = candidate(&before);
            assert_eq!(pending["nativeParentRunId"], caller);
            assert!(!policy::allows(db, &pending, &native_node)?);
            for error in [
                allowed(db, &pending, &native_node).unwrap_err(),
                source_read(db, &target, &db.node_boundary(&vm)?.unwrap()).unwrap_err(),
                prospective(db, &before, &destination, &source).unwrap_err(),
            ] {
                assert!(error.message.contains("cyclic native authority lineage"));
            }
            assert_eq!(db.run(&target)?.unwrap(), before);
            Ok(())
        }).await.unwrap();
    }

    #[test]
    #[ignore = "requires CAP_SYS_ADMIN; uses a disposable private mount namespace"]
    fn migration_staging_preserves_a_mounted_storage_root() {
        std::thread::spawn(|| {
            crate::storage::ublk::private_mount_namespace(&[]).unwrap();
            let root = tempfile::tempdir().unwrap();
            let staging = migration_staging(root.path()).unwrap();
            let target = std::ffi::CString::new(staging.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(
                unsafe {
                    libc::mount(
                        c"tmpfs".as_ptr(),
                        target.as_ptr(),
                        c"tmpfs".as_ptr(),
                        0,
                        std::ptr::null(),
                    )
                },
                0
            );
            std::fs::write(staging.join("abandoned"), b"old conversion").unwrap();
            let result = migration_staging(root.path());
            assert_eq!(unsafe { libc::umount2(target.as_ptr(), 0) }, 0);
            assert_eq!(result.unwrap(), staging);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn retry_reclaims_staging_abandoned_by_a_terminated_process() {
        let root = tempfile::tempdir().unwrap();
        let unrelated = root.path().join("other-storage");
        std::fs::write(&unrelated, b"retained").unwrap();
        let staging = migration_staging(root.path()).unwrap();
        std::os::unix::fs::symlink(&unrelated, staging.join("abandoned-link")).unwrap();
        let abandoned = tempfile::tempdir_in(&staging).unwrap().keep();
        std::fs::write(abandoned.join("source.ext4"), b"source blocks").unwrap();
        std::fs::create_dir(abandoned.join("tree")).unwrap();
        std::fs::write(abandoned.join("tree/file"), b"extracted data").unwrap();
        std::fs::write(abandoned.join("converted.ext4"), b"destination blocks").unwrap();

        let next = migration_staging(root.path()).unwrap();
        assert_eq!(next, staging);
        assert_eq!(std::fs::read_dir(next).unwrap().count(), 0);
        assert!(!abandoned.exists());
        assert_eq!(std::fs::read(unrelated).unwrap(), b"retained");
    }

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
            let mut historical_cycle = plugin_bound.clone();
            historical_cycle["id"] = id().into();
            historical_cycle["nativeParentRunId"] = historical_cycle["id"].clone();
            assert!(policy::require_acyclic_parent(db, text(&historical_cycle, "id"), &historical_cycle).is_err());
            let adopted_grant = migration_grant(db, &historical_cycle, &agent, &Origin::Human("owner".into()))?;
            historical_cycle["backendMigration"] = json!({ "grant": adopted_grant });
            require_lineage(db, &historical_cycle)?;
            assert_eq!(historical_cycle["nativeParentRunId"], historical_cycle["id"],
                "Checking the explicit pending adoption never rewrites retained old provenance");
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

    #[tokio::test]
    async fn migration_rechecks_retained_authority_after_storage_wait_before_staging_cleanup() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        for change in ["complete", "ancestry", "stop"] {
            let (f, bearer) = crate::plugin_grant_fixture::Fixture::delegated_manager().await;
            let caller = crate::installation_mcp::authorize(&f.s, &bearer)
                .await
                .unwrap();
            let source = text(&caller, "id").to_owned();
            let chat =
                f.s.store
                    .read({
                        let source = source.clone();
                        move |db| db.conversation_for_run(&source)
                    })
                    .await
                    .unwrap()
                    .unwrap();
            let job = crate::installation_mcp::management_authorization(&f.s, Some(&bearer))
                .await
                .unwrap()
                .accepted()
                .await
                .unwrap();
            f.s.store
                .change_run(
                    &source,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Succeeded),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            f.s.mcps.revoke_run(&f.s, &source).await.unwrap();
            let staging = f.s.config.data_dir.join("backend-migration-staging");
            std::fs::create_dir(&staging).unwrap();
            let marker = staging.join("existing-source");
            std::fs::write(&marker, b"source must survive revocation").unwrap();
            let storage = f.s.conversation_storage_lock.clone().lock_owned().await;
            let entered = Arc::new(tokio::sync::Notify::new());
            let signal = entered.clone();
            let calls = AtomicUsize::new(0);
            let job = job.with_guard(move |_| {
                // The final local fence of the initial recheck has completed;
                // the next authorization fence follows the held storage lock.
                if calls.fetch_add(1, Ordering::SeqCst) == 1 {
                    signal.notify_one();
                }
                Ok(())
            });
            let service = f.s.clone();
            let target = chat.id.clone();
            let migration = tokio::spawn(async move {
                service
                    .convert_chat_live(&target, "not-admitted", json!({}), Origin::Run(source), job)
                    .await
            });
            tokio::time::timeout(Duration::from_secs(10), entered.notified())
                .await
                .unwrap();
            match change {
                "ancestry" => f.moved_parent.store(true, Ordering::SeqCst),
                "stop" => {
                    f.s.worker
                        .stop_chat_live(
                            &f.s,
                            &chat.id,
                            crate::authorization::Authorization::default(),
                        )
                        .await
                        .unwrap();
                }
                _ => {}
            }
            drop(storage);
            let error = tokio::time::timeout(Duration::from_secs(10), migration)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            if change == "complete" {
                assert_eq!(error.status, 409, "{error:?}");
                assert!(
                    !marker.exists(),
                    "normal completion must permit the accepted staging operation"
                );
            } else {
                assert_eq!(error.status, 403, "{change}: {error:?}");
                assert_eq!(
                    std::fs::read(&marker).unwrap(),
                    b"source must survive revocation"
                );
            }
            f.s.shutdown.cancel();
        }
    }
}
