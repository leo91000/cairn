//! Moves between Linux backends. Native nodes mount a VM disk directly, so most
//! moves are ordinary node moves. A disk created natively is converted once
//! before a VM boots it; conversion and restoration finish before the source
//! checkpoint is replaced.
use crate::{
    config::{id, now},
    conversation_lifecycle::require_active,
    error::{Error, Result, required},
    native::policy::{self, Origin},
    nodes::{
        self,
        backend::{self, DiskFormat},
    },
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::{parse, text},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

// Only the explicit public MCP operations use the verified owner. Other
// management operations keep their existing provenance rules.
tokio::task_local! { pub(crate) static OWNER: Option<String>; }

static PROCESS: LazyLock<String> = LazyLock::new(id);

/// Conversion temporarily fences a run without completing its mission.
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

/// The explicit owner of a public MCP request, else the authenticated caller.
pub(crate) fn origin() -> Origin {
    OWNER
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .map_or_else(policy::origin, Origin::Human)
}

/// How placement sees a conversation whose converted disk awaits its VM.
pub(crate) fn candidate(run: &Value) -> Value {
    let migration = &run["backendMigration"];
    let mut candidate = run.clone();
    candidate["nodeId"] = migration["nodeId"].clone();
    candidate["targetNodeId"] = migration["nodeId"].clone();
    candidate["targetBackend"] = backend::FIRECRACKER.into();
    candidate["pinnedNodeId"] = Value::Null;
    candidate["preferredNodeId"] = Value::Null;
    candidate["requiredRuntime"] = migration["runtimeId"].clone();
    candidate["requiredTags"] = migration
        .get("requiredTags")
        .unwrap_or(&run["requiredTags"])
        .clone();
    candidate["placementTransition"] = true.into();
    candidate
}

/// A stopped source can fetch its own base while being copied. This is never a
/// workload lease: ordinary execution continues to use the current node grants.
pub(crate) fn source_read(db: &Db<'_>, run_id: &str, node: &Value) -> Result<bool> {
    let Some(run) = db.run(run_id)? else {
        return Ok(false);
    };
    if run["backendMigration"]["process"] != PROCESS.as_str()
        || run["backendMigration"]["sourceReadReady"] != true
        || RunStatus::of(&run).is_some_and(RunStatus::is_active)
        || nodes::owner_node(&run) != text(node, "id")
    {
        return Ok(false);
    }
    let destination = required(
        db.get("nodes", text(&run["backendMigration"], "nodeId"))?,
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

fn migration_grant(db: &Db<'_>, run: &Value, agent: &Value, source: &Origin) -> Result<Value> {
    let mut grant = policy::authorize_origin(db, agent, source)?;
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

/// Converted disks always boot in a VM the agent may still use.
fn allowed(db: &Db<'_>, run: &Value, node: &Value) -> Result<bool> {
    let agent = required(db.get("agents", nodes::run_agent(run))?, "Agent not found")?;
    Ok(node["revoked"] != true
        && !policy::node_native(node)
        && nodes::agent_allows(&agent, text(node, "id")))
}

/// The conversation's backend, where it may move and what each move costs.
/// Native destinations still need the conversation's native authority when the
/// move is requested; a conversion is listed but never started here.
pub(crate) async fn preview(
    s: &Service,
    chat_id: &str,
    authorize: impl Fn(&Db<'_>) -> Result<()> + Clone + Send + 'static,
) -> Result<Value> {
    let check = authorize.clone();
    s.store.read(move |db| check(db)).await?;
    let chat = s.get("chats", chat_id).await?;
    require_active(&chat)?;
    preview_run(s, text(&chat, "runId"), authorize).await
}

/// Run-based preview also covers missions that have no chat record.
pub(crate) async fn preview_run(
    s: &Service,
    run_id: &str,
    authorize: impl Fn(&Db<'_>) -> Result<()> + Clone + Send + 'static,
) -> Result<Value> {
    let check = authorize.clone();
    s.store.read(move |db| check(db)).await?;
    let run = s.store.run(run_id).await?;
    policy::refresh_agent(s, nodes::run_agent(&run)).await?;
    let run_id = run_id.to_owned();
    s.store.read(move |db| {
        authorize(db)?;
        crate::conversation_lifecycle::require_active_run(db, &run_id)?;
        let run = required(db.run(&run_id)?, "Run not found")?;
        let agent = required(db.get("agents", nodes::run_agent(&run))?, "Agent not found")?;
        let chat = crate::chats::chat_for_run(db, &run_id)?;
        let checkpoint = nodes::db_checkpoint(db, text(&run, "id"))?;
        let current = backend::of_run_in(db, &run)?;
        let format = DiskFormat::of(&checkpoint);
        let mut destinations = Vec::new();
        for node in db.list("nodes")? {
            let target = backend::of_node(&node);
            if node["revoked"] == true || target == current || !nodes::agent_allows(&agent, text(&node, "id")) {
                continue;
            }
            let authorized = !policy::node_native(&node) || policy::allows(db, &run, &node)?;
            destinations.push(json!({
                "id": node["id"],
                "name": node["name"],
                "backend": target,
                "conversion": !format.runs_on(target),
                "nativeAuthorityConfirmed": authorized,
            }));
        }
        let interrupted = run["backendMigration"].is_object() && run["backendMigration"]["process"] != PROCESS.as_str();
        Ok(json!({
            "chatId": chat.as_ref().map(|chat| &chat["id"]),
            "runId": run_id,
            "mission": chat.is_none(),
            "backend": current,
            "diskFormat": format.as_str(),
            "nodes": destinations,
            "pending": run["backendMigration"]["process"] == PROCESS.as_str(),
            "error": if interrupted {
                run["backendMigrationError"].as_str().map_or_else(|| json!("Conversion interrupted by a manager restart. The original environment is retained; move the conversation again to retry."), |message| json!(message))
            } else { run["backendMigrationError"].clone() },
        }))
    }).await
}

impl Service {
    /// Public MCP and installation tool: move a conversation to a node of the
    /// other backend, exactly like the owner's move from its execution details.
    pub(crate) async fn migrate_chat_backend<F>(&self, input: Value, authorize: F) -> Result<Value>
    where
        F: Fn(&Db<'_>) -> Result<()> + Clone + Send + Sync + 'static,
    {
        let input = parse("mcp:migrate_chat_backend", input)?;
        let check = authorize.clone();
        self.store.read(move |db| check(db)).await?;
        let run = if let Some(run) = input["runId"].as_str() {
            self.store.run(run).await?
        } else {
            let chat = self.get("chats", text(&input, "chatId")).await?;
            require_active(&chat)?;
            self.store.run(text(&chat, "runId")).await?
        };
        let node = self.get("nodes", text(&input, "nodeId")).await?;
        let checkpoint = nodes::checkpoint(self, text(&run, "id")).await?;
        let current = backend::recorded(&run, &checkpoint);
        let target = backend::of_node(&node);
        if input["backend"]
            .as_str()
            .is_some_and(|requested| requested != target)
        {
            return Err(Error::conflict(
                "Choose the backend of the destination node.",
            ));
        }
        if current.is_none_or(|current| current == target) {
            return Err(Error::conflict(
                "Choose a node of the other execution backend for this conversation.",
            ));
        }
        nodes::moves::request_authorized(
            self,
            &run,
            &json!({ "nodeId": node["id"] }),
            origin(),
            authorize,
        )
        .await
    }

    /// Starts the one-time conversion of a natively created disk for a VM. The
    /// chat is paused and its active turn stopped until the converted point is
    /// restored; the next message then starts a fresh engine session.
    pub(crate) async fn convert_for_vm<F>(
        &self,
        run_id: &str,
        args: &Value,
        source: Option<Origin>,
        authorize: F,
    ) -> Result<Value>
    where
        F: Fn(&Db<'_>) -> Result<()> + Clone + Send + Sync + 'static,
    {
        let node_id = text(args, "nodeId");
        let requested_tags = args.get("requiredTags").cloned();
        let wait_seconds = args["waitSeconds"].as_u64().unwrap_or(0);
        let operation = id();
        let (key, destination, migration) =
            (run_id.to_owned(), node_id.to_owned(), operation.clone());
        let request_authorization = authorize.clone();
        let chat_id = self.store.transaction(move |db| {
            request_authorization(db)?;
            crate::conversation_lifecycle::require_active_run(db, &key)?;
            let run = required(db.run(&key)?, "Run not found")?;
            let mut chat = crate::chats::chat_for_run(db, &key)?;
            if chat.is_none() && run["wakeup"].is_object() {
                return Err(Error::conflict("Finish the mission wait before requesting a node move."));
            }
            if run["backendMigration"]["process"] == PROCESS.as_str() {
                return Err(Error::conflict("A conversion is already in progress."));
            }
            if run["moveRequest"].is_object() {
                return Err(Error::conflict("Finish the pending node move first."));
            }
            let checkpoint = nodes::db_checkpoint(db, text(&run, "id"))?;
            if !checkpoint["prepared"]["projectRoot"].is_string() {
                return Err(Error::conflict("This conversation has no saved isolated environment to move."));
            }
            if DiskFormat::of(&checkpoint) != DiskFormat::Native {
                return Err(Error::conflict("This environment runs in a VM without conversion."));
            }
            let node = required(db.get("nodes", &destination)?, "Destination node not found")?;
            if !allowed(db, &run, &node)? || !nodes::valid_runtime(text(&node, "runtimeId")) {
                return Err(Error::forbidden("Destination is not authorized for this conversation."));
            }
            let required_tags = requested_tags.as_ref().unwrap_or(&run["requiredTags"]);
            let tagged = required_tags.as_array().into_iter().flatten().all(|tag| nodes::node_tags(&node).any(|actual| actual == tag));
            if !tagged {
                return Err(Error::unavailable("Destination does not have the required tags."));
            }
            let mut grant = json!({});
            if let Some(source) = &source {
                let agent = required(db.get("agents", nodes::run_agent(&run))?, "Agent not found")?;
                grant = migration_grant(db, &run, &agent, source)?;
            } else {
                policy::apply(&mut grant, &run);
            }
            let source_node = required(db.get("nodes", nodes::owner_node(&run))?, "Source node removed")?;
            let mut readable = run.clone();
            readable["backendMigration"] = json!({ "sourceReadReady": true, "grant": grant });
            if !policy::allows_migration_source(db, &readable, &source_node)? {
                return Err(Error::forbidden("The source environment is not authorized for conversion reads."));
            }
            let previous = &run["backendMigration"];
            let reusable = previous["convertedBackup"].is_string()
                && previous["nodeId"] == node["id"]
                && previous["runtimeId"] == node["runtimeId"];
            let reservation = previous["reservationId"].as_str().map_or_else(|| migration.clone(), str::to_owned);
            let paused = if previous.is_object() {
                previous["paused"].clone()
            } else {
                chat.as_ref().map_or(Value::Null, |chat| chat["paused"].clone())
            };
            let resume_mission = if previous.is_object() {
                previous["resumeMission"] == true
            } else {
                chat.is_none()
                    && (run["resumeMissionAfterConversion"] == true
                        || RunStatus::of(&run).is_some_and(RunStatus::is_active))
            };
            let migration = json!({
                "convertedBackup": if reusable { previous["convertedBackup"].clone() } else { Value::Null },
                "id": migration,
                "process": PROCESS.as_str(),
                "sourceReadReady": false,
                "nodeId": node["id"],
                "runtimeId": node["runtimeId"],
                "requiredTags": required_tags,
                "waitSeconds": wait_seconds,
                "reservationId": reservation,
                "grant": grant,
                "paused": paused,
                "resumeMission": resume_mission,
                "createdAt": now(),
            });
            if let Some(chat) = &mut chat {
                for mut message in db.messages(text(chat, "id"))? {
                    if message["status"] == crate::chats::MessageStatus::Sending {
                        message["status"] = crate::chats::MessageStatus::Cancelled.into();
                        db.put_message(&message)?;
                    }
                }
                chat["paused"] = true.into();
                db.put("chats", chat)?;
            }
            db.patch_run(text(&run, "id"), &json!({
                "backendMigration": migration,
                "backendMigrationError": null,
                // A failed early conversion clears its guard to allow source
                // repair. Keep a running mission's continuation intent for retry.
                "resumeMissionAfterConversion": resume_mission,
            }))?;
            db.audit("chat.backend-migration.requested", &json!({
                "runId": run["id"],
                "chatId": chat.as_ref().map(|chat| &chat["id"]),
                "backend": backend::FIRECRACKER,
                "nodeId": node["id"],
            }))?;
            Ok(chat.as_ref().map(|chat| chat["id"].clone()))
        }).await?;
        let s = self.clone();
        let result = json!({
            "status": "migrating",
            "runId": run_id,
            "chatId": chat_id,
            "migrationId": operation,
        });
        let run_id = run_id.to_owned();
        let node_id = node_id.to_owned();
        tokio::spawn(async move {
            let outcome = s
                .convert_run(&run_id, &operation, &node_id, authorize)
                .await;
            if let Err(error) = outcome {
                let message = error.message.clone();
                let _ = s
                    .store
                    .transaction(move |db| {
                        let Some(run) = db.run(&run_id)? else {
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
                            db.patch_run(
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
                tracing::warn!(error = %error, "Backend conversion retained its original environment");
            }
        });
        Ok(result)
    }

    async fn convert_run<F>(
        &self,
        run_id: &str,
        operation: &str,
        node_id: &str,
        authorize: F,
    ) -> Result<()>
    where
        F: Fn(&Db<'_>) -> Result<()> + Clone + Send + Sync + 'static,
    {
        let run_id = run_id.to_owned();
        let _storage_lock = crate::conversation_lifecycle::storage_lock(&self.config.data_dir)?;
        let _storage = self.conversation_storage_lock.lock().await;
        // Both storage locks exclude every live converter. Anything retained in
        // this dedicated directory therefore belongs to a terminated process.
        // Reclaim it before capacity checks, including retries using a saved point.
        let staging_root = migration_staging(&self.config.data_dir)?;
        let initial = self.store.run(&run_id).await?;
        if RunStatus::of(&initial).is_some_and(RunStatus::is_active) {
            match self.worker.cancel(self, &run_id).await {
                Ok(()) => {}
                Err(error) if error.is_conflict() => {}
                Err(error) => return Err(error),
            }
        }
        for _ in 0..600 {
            if !self.worker.active.lock().await.contains_key(&run_id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if self.worker.active.lock().await.contains_key(&run_id) {
            return Err(Error::unavailable(
                "Waiting for the previous execution to stop; retry the move after it finishes.",
            ));
        }
        let preparation = self.worker.run_preparation_lock(&run_id).await;
        let _preparation = preparation.lock_owned().await;
        let run = self.store.run(&run_id).await?;
        crate::recovery::fence(self, &run).await?;
        if RunStatus::of(&run).is_some_and(RunStatus::is_active) {
            self.store
                .patch_run(
                    &run_id,
                    json!({ "status": RunStatus::Cancelled, "recoveryPending": false }),
                )
                .await?;
        }
        let (key, operation_id) = (run_id.clone(), operation.to_owned());
        let run = self
            .store
            .transaction(move |db| {
                let mut current = required(db.run(&key)?, "Conversation removed")?;
                if current["backendMigration"]["id"] != operation_id {
                    return Err(Error::conflict(
                        "Conversion changed while fencing its source.",
                    ));
                }
                current["backendMigration"]["sourceReadReady"] = true.into();
                db.patch_run(
                    &key,
                    &json!({ "backendMigration": current["backendMigration"] }),
                )
            })
            .await?;
        // The source is native: reading it still needs fresh native authority.
        policy::refresh(self).await?;
        let source_node = self.get("nodes", nodes::owner_node(&run)).await?;
        let (source_run_id, read_node) = (run_id.clone(), source_node.clone());
        let readable = self
            .store
            .read(move |db| source_read(db, &source_run_id, &read_node))
            .await?;
        if !readable {
            return Err(Error::forbidden(
                "The source environment is no longer authorized for conversion reads.",
            ));
        }
        let reservation = text(&run["backendMigration"], "reservationId").to_owned();
        // The reservation ID survives process death. No workload starts during
        // conversion; release an interrupted admission while retaining any
        // materialized disk allocation, then reserve again with live limits.
        nodes::placement::release(self, &reservation).await?;
        let node = self.get("nodes", node_id).await?;
        let point = if let Some(saved) = run["backendMigration"]["convertedBackup"].as_str() {
            // A controller may have installed this exact point before losing its
            // reply. Retrying it also completes the restore transaction.
            self.get("node-backups", saved).await?
        } else {
            let point = nodes::publication::capture(self, &run).await?;
            let backup = self.get("node-backups", text(&point, "id")).await?;
            let manifest = nodes::publication::manifest(self, &backup).await?;
            if manifest["kind"] == "native-files-v1" {
                return Err(Error::conflict(
                    "Resume this legacy native environment once on its original node to upgrade its storage before moving it to a VM.",
                ));
            }
            let staging = tempfile::Builder::new()
                .prefix("conversion-")
                .tempdir_in(&staging_root)?;
            let raw = staging.path().join("source.ext4");
            let reserve = materialize(self, &backup, &manifest, &raw, &node).await?;
            let checkpoint = nodes::checkpoint(self, &run_id).await?;
            let disk = crate::storage::backend_conversion::convert(
                &raw,
                &manifest,
                &checkpoint["prepared"],
                backend::FIRECRACKER,
                staging.path(),
                reserve,
            )
            .await?;
            let mut converted = nodes::snapshots::index(&disk).await?;
            converted["runtime"] = json!({
                "runtimeId": node["runtimeId"],
                "backend": backend::FIRECRACKER,
                "os": "linux",
                "arch": "x86_64",
                "storage": "ublk",
            });
            converted["onDemand"] = true.into();
            converted["generation"] = 1.into();
            converted["capturedAt"] = now().into();
            nodes::publication::converted_environment(self, &run, &node, &disk, converted).await?
        };
        let proposed = candidate(&self.store.run(&run_id).await?);
        let wait_until =
            now() + run["backendMigration"]["waitSeconds"].as_u64().unwrap_or(0) as i64 * 1000;
        let placement = loop {
            match nodes::placement::reserve(self, &proposed, &reservation).await {
                Ok(placement) => break placement,
                Err(error) if nodes::placement::is_no_capacity(&error) && now() < wait_until => {
                    tokio::select! {
                        () = self.shutdown.cancelled() => return Err(Error::unavailable("Master is stopping.")),
                        () = tokio::time::sleep(Duration::from_secs(1)) => {},
                    }
                }
                Err(error) => return Err(error),
            }
        };
        let restored = async {
            policy::refresh(self).await?;
            nodes::placement::materialize(self, &reservation).await?;
            nodes::restore::start(self, &proposed, text(&node, "id"), &point).await
        }
        .await;
        let release = nodes::placement::release(self, &reservation).await;
        restored?;
        release?;
        let operation = operation.to_owned();
        self.store.transaction(move |db| {
            authorize(db)?;
            crate::conversation_lifecycle::require_active_run(db, &run_id)?;
            let current = required(db.run(&run_id)?, "Run not found")?;
            let chat = crate::chats::chat_for_run(db, &run_id)?;
            let migration = &current["backendMigration"];
            if migration["id"] != operation {
                return Err(Error::conflict("Conversation changed during conversion."));
            }
            let source_node = required(db.get("nodes", text(&source_node, "id"))?, "Source node removed")?;
            if !source_read(db, &run_id, &source_node)? {
                return Err(Error::forbidden("Source read permission was revoked during conversion."));
            }
            let destination = required(db.get("nodes", text(&node, "id"))?, "Destination node removed")?;
            if !allowed(db, &current, &destination)? {
                return Err(Error::forbidden("Destination permission was revoked during conversion."));
            }
            let key = nodes::checkpoint_key(&run_id);
            let mut checkpoint = nodes::db_checkpoint(db, &run_id)?;
            checkpoint["prepared"]["backend"] = backend::FIRECRACKER.into();
            checkpoint["diskFormat"] = DiskFormat::Vm { runtime: None }.as_str().into();
            checkpoint["diskRuntimeId"] = node["runtimeId"].clone();
            checkpoint["nodeId"] = node["id"].clone();
            checkpoint["runtimeId"] = node["runtimeId"].clone();
            checkpoint["resources"] = placement["resources"].clone();
            checkpoint["runnerId"] = run_id.clone().into();
            checkpoint["launched"] = false.into();
            checkpoint["workspaceInitialized"] = true.into();
            checkpoint["freshSession"] = true.into();
            let resume_mission = chat.is_none() && migration["resumeMission"] == true;
            checkpoint["completed"] = (!resume_mission).into();
            for key in ["process", "settled", "controllerRecoveries"] { checkpoint[key] = Value::Null; }
            db.set(&key, &checkpoint, None)?;
            let mut patch = json!({
                "executionBackend": backend::FIRECRACKER,
                "nodeId": node["id"],
                "resources": placement["resources"],
                "requiredRuntime": node["runtimeId"],
                "requiredTags": proposed["requiredTags"],
                "pinnedNodeId": null,
                "preferredNodeId": null,
                "backendMigration": null,
                "backendMigrationError": null,
                "resumeMissionAfterConversion": null,
                "sessionId": null,
                "resumeAvailable": false,
                "status": if resume_mission { RunStatus::Queued } else { RunStatus::Succeeded },
                "error": null,
                "cancelRequestedAt": null,
                "recoveryPending": resume_mission,
                "nodeState": null,
                "storage": { "mode": nodes::ON_DEMAND },
                "backup": { "id": point["id"], "snapshotId": point["snapshotId"], "capturedAt": point["capturedAt"], "status": "ready" },
            });
            policy::apply(&mut patch, &migration["grant"]);
            if chat.is_none() {
                patch["sessionHandoff"] = crate::chats::handoff_context(db, &run_id)?.into();
            }
            db.patch_run(&run_id, &patch)?;
            let chat_id = chat.as_ref().map(|chat| chat["id"].clone());
            if let Some(mut chat) = chat {
                policy::apply(&mut chat, &migration["grant"]);
                chat["paused"] = migration["paused"].clone();
                chat["sessionRestartRequested"] = true.into();
                chat["wakeup"] = Value::Null;
                db.put("chats", &chat)?;
            }
            let message = if resume_mission {
                "Environment converted to run in a VM. Resuming the mission with its history and files in a fresh engine session. Verify external effects before repeating actions."
            } else {
                "Environment converted to run in a VM. Conversation history and files preserved; the next message starts a fresh engine session."
            };
            db.event(&run_id, "status", message, None)?;
            db.audit("chat.backend-migration.completed", &json!({
                "runId": run_id,
                "chatId": chat_id,
                "backend": backend::FIRECRACKER,
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
        .get("nodes", nodes::LOCAL_NODE_ID)
        .await?
        .unwrap_or_default();
    let policy = crate::storage::policy::Policy::for_node(&local["storage"])?;
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
    let mut output = tokio::fs::File::create(path).await?;
    output.set_len(size).await?;
    for block in manifest["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["hash"].is_string())
    {
        let bytes = nodes::publication::read_manifest_block(s, backup, block).await?;
        output
            .seek(std::io::SeekFrom::Start(block["offset"].as_u64().unwrap()))
            .await?;
        output.write_all(&bytes).await?;
    }
    output.sync_all().await?;
    Ok(policy.reserve(total))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn conversions_only_target_vm_nodes_the_agent_still_allows() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let (vm, native, other, agent) = (id(), id(), id(), id());
                db.put(
                    "agents",
                    &json!({ "id": agent, "access": { "nodes": [vm, native] } }),
                )?;
                db.put(
                    "nodes",
                    &json!({ "id": vm, "capabilities": { "backend": "firecracker" } }),
                )?;
                db.put(
                    "nodes",
                    &json!({ "id": other, "capabilities": { "backend": "firecracker" } }),
                )?;
                db.put(
                    "nodes",
                    &json!({
                        "id": native,
                        "capabilities": { "backend": "native" },
                        "nativeEnabled": true,
                        "nativeAccounts": ["owner"],
                    }),
                )?;
                let run = json!({ "snapshot": { "agent": { "id": agent } } });
                assert!(allowed(db, &run, &db.get("nodes", &vm)?.unwrap())?);
                assert!(!allowed(db, &run, &db.get("nodes", &native)?.unwrap())?);
                assert!(!allowed(db, &run, &db.get("nodes", &other)?.unwrap())?);
                let mut revoked = db.get("nodes", &vm)?.unwrap();
                revoked["revoked"] = true.into();
                assert!(!allowed(db, &run, &revoked)?);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn conversion_grants_adopt_only_for_humans_and_bound_delegated_retries() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store.transaction(|db| {
            let agent = json!({ "id": "agent", "access": { "nodes": [nodes::LOCAL_NODE_ID] } });
            db.put("agents", &agent)?;
            let run = json!({ "nativePrincipals": ["original"], "nativeAuthorityNodes": ["native"], "nativeProvenanceUnknown": false, "nativeParentRunId": "parent" });
            let adopted = migration_grant(db, &run, &agent, &Origin::Human("owner".into()))?;
            assert_eq!(adopted["nativePrincipals"], json!(["owner"]));
            assert!(adopted["nativeParentRunId"].is_null());
            let origin = json!({ "nativePrincipals": ["delegate"], "nativeAuthorityNodes": ["native", "extra"], "nativeProvenanceUnknown": false, "nativeParentRunId": "parent" });
            let retained = migration_grant(db, &run, &agent, &Origin::Stored(origin.clone()))?;
            assert_eq!(retained["nativePrincipals"], json!(["original", "delegate"]));
            assert_eq!(retained["nativeAuthorityNodes"], json!(["native"]));
            let mut unrelated = origin;
            unrelated["nativeParentRunId"] = "other-parent".into();
            assert!(migration_grant(db, &run, &agent, &Origin::Stored(unrelated)).is_err());
            let mut unknown = run;
            unknown["nativeProvenanceUnknown"] = true.into();
            assert!(migration_grant(db, &unknown, &agent, &Origin::Unknown).is_err());
            Ok(())
        }).await.unwrap();
    }
}
