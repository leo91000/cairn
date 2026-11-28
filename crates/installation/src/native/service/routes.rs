use super::*;

pub(super) async fn input(request: Request, limit: usize) -> Result<Value> {
    let bytes = axum::body::to_bytes(request.into_body(), limit)
        .await
        .map_err(|_| Error::bad("Native request exceeds limit."))?;
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(&bytes).map_err(Error::from)
}

pub(super) async fn handler(
    State(controller): State<Controller>,
    request: Request,
) -> Result<Response> {
    if request.method() == "GET" {
        return route(controller, request).await;
    }
    // A disconnected caller must not release physical ownership while a restore,
    // stop or filesystem mutation is still executing.
    tokio::spawn(route(controller, request))
        .await
        .map_err(Error::internal)?
}

pub(super) async fn route(controller: Controller, request: Request) -> Result<Response> {
    let path = request.uri().path().to_owned();
    let method = request.method().as_str().to_owned();
    if path == "/health" && method == "GET" {
        return Ok(Json(controller.health().await).into_response());
    }

    // This read-only endpoint is local to the root-private controller socket.
    // Its caller retains storage ownership through the bounded staging write;
    // taking that same lock here would deadlock the caller.
    if path == "/staging-budget" && method == "GET" {
        let started = std::time::Instant::now();
        let sample = async {
            let limit = controller.disk_limit().await?;
            let used = disk::used(&controller.data, &controller.state).await?;
            Ok::<_, Error>(disk::StagingSample {
                available_bytes: disk::available(limit, used),
                used_bytes: Some(used),
            })
        }
        .await;
        let elapsed = started.elapsed();
        if elapsed >= Duration::from_secs(1) || sample.is_err() {
            tracing::info!(
                target: "cairn_performance",
                operation = "native_staging_budget",
                elapsed_ms = elapsed.as_millis() as u64,
                status = sample.as_ref().map_or_else(|error| error.status, |_| 200),
                "Authoritative native staging budget observation finished"
            );
        }
        return Ok(Json(sample?).into_response());
    }

    let secret = tokio::fs::read_to_string(controller.data.join("runner-secret")).await?;
    let bearer = crate::nodes::bearer(request.headers()).unwrap_or("");
    if !crate::auth::safe_equal(bearer, secret.trim()) {
        return Err(Error::unauthorized("Invalid native controller credential."));
    }
    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    let value = match (method.as_str(), parts.as_slice()) {
        ("POST", ["disk-growth"]) => {
            let value = input(request, 1_000_000).await?;
            controller
                .capacity
                .lock()
                .await
                .apply(&value, controller.disk_limit().await?);
            json!({ "ready": true })
        }
        ("POST", ["native-bases", "lookup"]) => {
            let value = input(request, 8_000_000).await?;
            let archives: Vec<crate::project_archives::Archive> =
                serde_json::from_value(value["archives"].clone())?;
            let size = value["diskMiB"]
                .as_u64()
                .ok_or_else(|| Error::bad("Missing native base size."))?;
            json!({ "base": controller.bases().lookup(&archives, size).await? })
        }
        ("POST", ["native-bases", "warm"]) => {
            let value = input(request, 8_000_000).await?;
            controller
                .base_warmer
                .enqueue(
                    controller.bases(),
                    value,
                    controller.budget.clone(),
                    controller.stop.clone(),
                )
                .await?;
            json!({ "queued": true })
        }
        ("POST", ["native-mode"]) => controller.change_mode(input(request, 16384).await?).await?,
        ("POST", ["node-budget"]) => {
            let value = input(request, 16384).await?;
            let budget = budget_request(&value)?;
            let _admission = controller.admission.lock().await;
            let previous = controller.budget.lock().await.clone();
            if value != previous {
                // Do not hold the budget mutex while waiting for storage:
                // foreground transfers query health with their disk lease held.
                let _disk =
                    disk::observed_lock(&controller.state, "native_storage_node_budget", "")
                        .await?;
                super::super::spares::discard_unclaimed(&controller.state, None).await?;
                let limit = budget.limits.disk_mi_b.saturating_mul(disk::MIB);
                let used = disk::used(&controller.data, &controller.state).await?;
                if let Err(error) = controller
                    .bases()
                    .trim(
                        super::super::bases::cache_limit(limit),
                        used.saturating_sub(limit),
                        None,
                    )
                    .await
                {
                    tracing::debug!(
                        status = error.status,
                        "Native cache reclamation deferred during budget update"
                    );
                }
                if value["limits"] != previous["limits"] {
                    systemd::apply_budget(&budget).await?;
                }
                atomic_write(
                    &controller.state.join(crate::microvm::budget::FILE),
                    &serde_json::to_vec(&budget_record(&value))?,
                )
                .await?;
                *controller.budget.lock().await = value;
            }
            json!({ "ready": true })
        }
        ("POST", ["storage-policy", "probe"]) => {
            let policy: crate::storage::policy::Policy =
                serde_json::from_value(input(request, 16384).await?)?;
            policy.validate().map_err(Error::bad)?;
            json!({ "ready": true })
        }
        ("POST", ["storage-policy"]) => {
            let configuration: crate::storage::policy::Configuration =
                serde_json::from_value(input(request, 16384).await?)?;
            if !controller
                .storage_policy
                .apply_relief(&controller.state, &configuration)
                .await?
            {
                let _disk =
                    disk::observed_lock(&controller.state, "native_storage_policy", "").await?;
                controller
                    .storage_policy
                    .apply(&controller.state, configuration)
                    .await?;
            }
            json!({ "ready": true })
        }
        ("POST", ["runs", id, "lease"]) => {
            uuid(id)?;
            let lease = input(request, 1024).await?;
            let ms = lease["remainingMs"]
                .as_u64()
                .filter(|ms| (1..=300_000).contains(ms))
                .ok_or_else(|| Error::bad("Invalid native lease."))?;
            if controller.file(id, "exit").exists() || controller.file(id, "stopped").exists() {
                return Err(Error::conflict("Attempt stopped."));
            }
            let until = crate::nodes::boot_ms().saturating_add(ms);
            super::super::watchdog::grant(&controller.file(id, "lease"), until).await?;
            controller.leases.lock().await.insert((*id).into(), until);
            json!({ "ok": true })
        }
        ("POST", ["runs", id]) => {
            controller.start(id).await?;
            json!({})
        }
        ("GET", ["runs", id, "bootstrap"]) => {
            uuid(id)?;
            let report = controller.bootstrap.get(id).ok_or_else(|| {
                Error::not_found(
                    "Bootstrap observation is unavailable; inspect retained technical logs.",
                )
            })?;
            json!({ "source": "memory", "bootstrap": report })
        }
        ("DELETE", ["runs", id]) => {
            controller.stop_attempt(id).await?;
            controller.stopped_result(id).await
        }
        ("GET", ["runs", id, "cleanup-status"]) => controller.cleanup_status(id).await?,
        ("POST", ["runs", id, "wait"]) => {
            uuid(id)?;
            let observe_retirement =
                request.uri().query() == Some(crate::runner::NATIVE_CLEANUP_RETIREMENT_QUERY);
            return Ok(controller.wait(id, observe_retirement));
        }
        ("GET", ["exclusive"]) => controller.inspect_exclusive().await?,
        ("POST", ["exclusive", "acquire"]) => {
            controller
                .acquire_exclusive(serde_json::from_value(input(request, 16384).await?)?)
                .await?
        }
        ("POST", ["exclusive", "release"]) => {
            controller
                .release_exclusive(serde_json::from_value(input(request, 16384).await?)?)
                .await?
        }
        ("GET", ["runs", id, "administrative-pause"]) => {
            controller.administrative_pause(id).await?
        }
        ("GET", ["runs", id, "logs"]) => {
            uuid(id)?;
            if let Some(offset) = crate::runner::logs::offset(request.uri().query())? {
                let path = controller.file(id, "log");
                let (controller, id) = (controller.clone(), (*id).to_owned());
                return crate::runner::logs::response(path, offset, move || {
                    let (controller, id) = (controller.clone(), id.clone());
                    async move {
                        controller
                            .active
                            .try_lock()
                            .is_ok_and(|active| !active.contains_key(&id))
                    }
                })
                .await;
            }
            let (mut writer, reader) = tokio::io::duplex(65536);
            let (controller, id) = (controller.clone(), (*id).to_owned());
            tokio::spawn(async move {
                let Ok(mut file) = tokio::fs::File::open(controller.file(&id, "log")).await else {
                    return;
                };
                let mut offset = 0;
                let mut buffer = vec![0u8; 65536];
                let mut at_line_boundary = true;
                let mut attempt_finished = false;
                let mut last_progress = tokio::time::Instant::now();
                loop {
                    if file.seek(std::io::SeekFrom::Start(offset)).await.is_err() {
                        break;
                    }
                    match file.read(&mut buffer).await {
                        Ok(0) => {
                            if attempt_finished {
                                break;
                            }
                            if controller
                                .active
                                .try_lock()
                                .is_ok_and(|active| !active.contains_key(&id))
                            {
                                // The final append can land between the EOF read and
                                // retirement observation. Drain again after retirement.
                                attempt_finished = true;
                                continue;
                            }
                            // Idle transport frames must not split a log record
                            // whose remaining bytes have not been appended yet.
                            if at_line_boundary && last_progress.elapsed() >= Duration::from_secs(1)
                            {
                                if writer
                                    .write_all(b"{\"type\":\"heartbeat\"}\n")
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                last_progress = tokio::time::Instant::now();
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        Ok(count) => {
                            if writer.write_all(&buffer[..count]).await.is_err() {
                                break;
                            }
                            at_line_boundary = buffer[count - 1] == b'\n';
                            last_progress = tokio::time::Instant::now();
                            offset += count as u64;
                        }
                        Err(_) => break,
                    }
                }
            });
            return Ok((
                [("content-type", "application/x-ndjson")],
                Body::from_stream(tokio_util::io::ReaderStream::new(reader)),
            )
                .into_response());
        }
        ("POST", ["disks", run, "storage-status"]) => {
            uuid(run)?;
            let mut status = block::status(&controller.state, run).await?;
            if let Some(mounted) = controller.mounted.lock().await.get(*run) {
                status["paused"] = mounted.volume.paused().into();
                status["waitingFor"] = mounted.volume.cpu_pause_reason()?.into();
            }
            status
        }
        ("POST", ["disks", run, action @ ("delete" | "prune")]) => {
            uuid(run)?;
            let operation = if *action == "delete" {
                "native_storage_delete"
            } else {
                "native_storage_prune"
            };
            let _disk = disk::observed_lock(&controller.state, operation, run).await?;
            let _staging = if *action == "delete" {
                Some(disk::lock_staging(&controller.state, run).await?)
            } else {
                None
            };
            super::super::cleanup::check(&controller.state, run)?;
            let value = input(request, 4096).await?;
            let retirement = if *action == "delete" {
                crate::storage::retirement::Retirement::begin(&controller.state, run, &value)
                    .await?
            } else {
                None
            };
            if let Some(retirement) = &retirement
                && retirement.completed().await?
            {
                erase_native_attempts(&controller, run, retirement.attempts(), true).await?;
                return Ok(
                    Json(json!({ "deleted": true, "retirementId": retirement.id() }))
                        .into_response(),
                );
            }
            let root = workspace::directory(&controller.state, run)?;
            private_dir(&root).await?;
            let _owner = crate::file_lock::exclusive(
                &root.join("owner.lock"),
                "Native workspace is active.",
            )?;
            let retired_grants = block::action(&controller.state, run, *action == "delete").await?;
            let mut entries = tokio::fs::read_dir(&root).await?;
            while let Some(entry) = entries.next_entry().await? {
                if entry.file_name().to_string_lossy().starts_with("stale-")
                    && entry.file_type().await?.is_dir()
                {
                    tokio::fs::remove_dir_all(entry.path()).await?;
                }
            }
            if *action == "delete" {
                for entry in [
                    "data",
                    "workload",
                    "docker-runtime",
                    "initialized",
                    "resources.json",
                    "attempt",
                    "runtime.json",
                    "block-initialized",
                    "file-owner.json",
                    "block-seeding",
                    "block-restore.pending.json",
                    "tmp",
                    "directory.json",
                    "directory-conversion",
                    "directory-conversion-verified",
                    "base.partial",
                    "view.json",
                ] {
                    let path = root.join(entry);
                    if let Ok(meta) = tokio::fs::symlink_metadata(&path).await {
                        if meta.is_dir() && !meta.file_type().is_symlink() {
                            tokio::fs::remove_dir_all(path).await?;
                        } else {
                            tokio::fs::remove_file(path).await?;
                        }
                    }
                }
            }
            if *action == "delete" {
                controller.bootstrap.forget(run);
                let stage = controller.data.join("runs").join(run);
                if stage.exists()
                    && !stage
                        .join(crate::conversation_deletion::retired::MANAGER_NAMESPACE)
                        .exists()
                {
                    tokio::fs::remove_dir_all(&stage).await?;
                    tokio::fs::File::open(stage.parent().unwrap())
                        .await?
                        .sync_all()
                        .await?;
                }
                erase_native_attempts(
                    &controller,
                    run,
                    retirement.as_ref().map_or(&[], |value| value.attempts()),
                    false,
                )
                .await?;
            }
            for directory in [
                controller.state.join("roots"),
                controller.data.join("runner-plans"),
            ] {
                if directory.is_dir() {
                    tokio::fs::File::open(directory).await?.sync_all().await?;
                }
            }
            if let Some(retirement) = &retirement {
                retirement.commit(&root).await?;
            }
            json!({
                "deleted": *action == "delete",
                "pruned": *action == "prune",
                "retiredGrants": retired_grants,
                "retirementId": retirement.as_ref().map(crate::storage::retirement::Retirement::id),
            })
        }
        ("POST", ["runs", id, "artifact"]) => {
            let value = input(request, 16384).await?;
            let (snapshot, size) = controller.artifact_snapshot(id, value).await?;
            let file = tokio::fs::File::from_std(snapshot.reopen()?);
            let stream = futures_util::stream::try_unfold(
                (file, snapshot),
                |(mut file, snapshot)| async move {
                    let mut bytes = vec![0; 65536];
                    let count = file.read(&mut bytes).await?;
                    bytes.truncate(count);
                    Ok::<_, std::io::Error>((count > 0).then_some((bytes, (file, snapshot))))
                },
            );
            return Ok((
                [("content-length", size.to_string())],
                Body::from_stream(stream),
            )
                .into_response());
        }
        ("POST", ["runs", id, "environment"]) => {
            let value = input(request, 2_000_000).await?;
            let operation = serde_json::from_value(value)?;
            let control = controller
                .environments
                .lock()
                .await
                .get(*id)
                .cloned()
                .ok_or_else(|| Error::unavailable("Execution environment is not active."))?;
            serde_json::to_value(control.request(operation).await?)?
        }
        ("GET", ["runs", id, "turn", turn]) | ("GET", ["runs", id, "turn", turn, "start"]) => {
            crate::validation::uuid(turn)?;
            let control = controller
                .environments
                .lock()
                .await
                .get(*id)
                .cloned()
                .ok_or_else(|| Error::unavailable("Execution environment is not active."))?;
            if parts.last() == Some(&"start") {
                match tokio::time::timeout(Duration::from_secs(20), control.start_offset(turn))
                    .await
                {
                    Ok(offset) => json!({ "offset": offset? }),
                    Err(_) => {
                        return Ok((
                            axum::http::StatusCode::ACCEPTED,
                            Json(json!({"pending":true})),
                        )
                            .into_response());
                    }
                }
            } else {
                match tokio::time::timeout(Duration::from_secs(20), control.wait(turn)).await {
                    Ok(receipt) => serde_json::to_value(receipt?)?,
                    Err(_) => {
                        return Ok((
                            axum::http::StatusCode::ACCEPTED,
                            Json(json!({"pending":true})),
                        )
                            .into_response());
                    }
                }
            }
        }
        ("POST", ["runs", id, "projects", project]) => {
            let value = input(request, 16384).await?;
            controller.import_project(id, project, value).await?
        }
        _ => {
            return Err(Error::not_found(
                "Native controller operation is not supported.",
            ));
        }
    };
    Ok(Json(value).into_response())
}

// Called only after old workloads and block devices are fenced. Temporary
// anonymous bases/clones have no authority; conventional committed disks remain.
// Base cache cleanup follows pin recovery so link counts show only durable intents.
