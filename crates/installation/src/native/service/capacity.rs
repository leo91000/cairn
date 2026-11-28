use super::*;

impl Controller {
    pub(super) async fn monitor_capacity(
        &self,
        id: &str,
        plan: &Value,
        workspace: u64,
        temporary: u64,
    ) -> Result<u64> {
        let run = text(plan, "runId");
        let mounted = self.mounted.lock().await.get(run).cloned();
        let filesystem = mounted.as_ref().map_or(self.disk_budget(plan), |handle| {
            handle.filesystem.load(std::sync::atomic::Ordering::Acquire)
        });
        let (allowance, notice, negotiated) = {
            let mut capacity = self.capacity.lock().await;
            let (allowance, notice) = capacity
                .observe(id, workspace, temporary, filesystem)
                .unwrap_or((self.disk_budget(plan), None));
            (allowance, notice, capacity.negotiated)
        };
        if let Some(mounted) = &mounted {
            // Apply current grants even while geometry growth retries. Managed
            // /tmp shares the task allowance and cannot finance journal writes.
            mounted.volume.limit_backing(allowance)?;
        }
        if let Some(notice) = notice {
            let directory = self.data.join("runs").join(run).join("chat-input");
            crate::nodes::workspace::publish_native_notice(
                &directory,
                &serde_json::to_vec(&notice)?,
            )
            .await?;
            let event = json!({
                "type": "runtime.disk_notice", "text": notice["text"],
                "capacity": { "capturedAt": crate::config::now(), "workspaceBytes": workspace, "temporaryBytes": temporary,
                    "allowanceBytes": allowance, "filesystemBytes": filesystem },
            });
            self.append(
                id,
                false,
                format!("{event}\n").as_bytes(),
                LogPurpose::Execution,
            )
            .await?;
        }
        if negotiated
            && allowance > filesystem
            && let Some(mounted) = mounted
        {
            let attempt = id.to_owned();
            // The lifecycle lock pins the mount through the metadata commit and
            // resize, while supervision remains responsive to lease/cancellation.
            let _ = super::super::cleanup::spawn(async move {
                let Ok(mut mount) = mounted.mount.try_lock() else {
                    return;
                };
                if let Err(error) = mount.grow(allowance).await {
                    tracing::warn!(
                        attempt,
                        status = error.status,
                        "Native disk growth deferred; data retained"
                    );
                }
            });
        }
        Ok(allowance)
    }

    pub(super) fn disk_budget(&self, plan: &Value) -> u64 {
        plan["resources"]["diskMiB"]
            .as_u64()
            .unwrap_or(32768)
            .saturating_mul(1_048_576)
    }

    pub(super) async fn disk_limit(&self) -> Result<u64> {
        self.budget.lock().await["limits"]["diskMiB"]
            .as_u64()
            .map(|limit| limit.saturating_mul(disk::MIB))
            .ok_or_else(|| Error::unavailable("Native node disk budget is unavailable."))
    }

    pub(super) async fn disk_growth(&self) -> Result<u64> {
        Ok(disk::available(
            self.disk_limit().await?,
            disk::used(&self.data, &self.state).await?,
        ))
    }

    pub(super) async fn usage(&self) -> Result<Value> {
        let used = disk::actual_used(&self.data, &self.state).await?;
        Ok(json!({
            "memoryMiB": 0,
            "diskMiB": used.div_ceil(disk::MIB),
            "diskBytes": used,
        }))
    }

    /// Ceilings are not preallocated reservations. Charge actual retained
    /// payloads, then bound every additional copy by the remaining shared pool.
    pub(super) async fn check_preparation_space(&self, plan: &Value) -> Result<PreparationSpace> {
        let run = text(plan, "runId");
        let root = workspace::directory(&self.state, run)?;
        let has_journal = block::exists(&self.state, run).await?;
        let current = if has_journal {
            0
        } else {
            disk::bytes_if_present(&root.join("data")).await?
        };
        let assigned_base = has_journal && root.join("file-owner.json").exists();
        let mut base_key = None;
        let mut spare_key = None;
        let mut project_bytes = None;
        let seed = if assigned_base {
            0
        } else if !root.join("initialized").exists()
            && !root.join("data/.cairn-seed-complete").exists()
        {
            let source = Plan::new(plan.clone())
                .imports()
                .find(|import| import.target == text(plan, "projectRoot"))
                .map(|import| import.source.to_owned())
                .ok_or_else(|| Error::bad("Missing native workspace seed."))?;
            if let Some(archives) = plan["nativeProjectArchives"].as_array() {
                let descriptors: Vec<crate::project_archives::Archive> = archives
                    .iter()
                    .cloned()
                    .map(serde_json::from_value)
                    .collect::<std::result::Result<_, _>>()?;
                let size = plan["resources"]["diskMiB"]
                    .as_u64()
                    .ok_or_else(|| Error::bad("Missing native disk size."))?;
                if let Some(base) = self.bases().lookup(&descriptors, size).await? {
                    base_key = Some(base.key.clone());
                    if !has_journal && self.bases().claimable_spare(run, &base.key)? {
                        spare_key = Some(base.key);
                        0
                    } else {
                        project_bytes = Some(base.metadata_bytes);
                        base.allocated_for_clone(self.bases().reflink_enabled().await?)
                    }
                } else {
                    archives.iter().try_fold(0u64, |bytes, archive| {
                        bytes
                            .checked_add(
                                archive["logicalBytes"]
                                    .as_u64()
                                    .ok_or_else(|| Error::bad("Invalid project archive size."))?,
                            )
                            .ok_or_else(|| Error::too_large("Project archives exceed disk budget."))
                    })?
                }
            } else {
                workspace::bytes(&source).await?
            }
        } else {
            0
        };
        let home = disk::bytes_if_present(&self.data.join("runs").join(run).join("home")).await?;
        let incoming = seed.saturating_add(home);
        let private_bytes = current
            .saturating_add(project_bytes.unwrap_or(seed))
            .saturating_add(home);
        if private_bytes > self.disk_budget(plan) {
            return Err(Error::conflict(
                "Retained native work leaves insufficient disk allowance for this preparation.",
            ));
        }
        // Callers hold the storage gate, so no reservation can be published
        // between samples: one full scan suffices unless reclamation changed
        // the pool.
        let limit = self.disk_limit().await?;
        let observed = std::time::Instant::now();
        let mut used = disk::used(&self.data, &self.state).await?;
        let growth = disk::available(limit, used);
        let policy = storage::policy::for_node(&plan["storage"]["policy"])?;
        let headroom = || async {
            let (state, policy) = (self.state.clone(), policy.clone());
            let sample = tokio::task::spawn_blocking(move || {
                if workspace::data_root(&state).try_exists()? {
                    disk::data_pool_headroom(&state, &policy, None)
                } else {
                    Ok(u64::MAX)
                }
            });
            tokio::time::timeout(Duration::from_secs(5), sample)
                .await
                .map_err(|_| Error::unavailable("Native data pool capacity observation is stale."))?
                .map_err(Error::internal)?
        };
        let physical_need = incoming.saturating_add(disk::MIB);
        let mut physical = headroom().await?;
        if (incoming > growth || physical_need > physical)
            && super::super::spares::discard_unclaimed(&self.state, spare_key.as_deref()).await?
        {
            used = disk::used(&self.data, &self.state).await?;
            physical = headroom().await?;
        }
        if physical_need > physical {
            // Cache allocation can be shared with retained reflink clones.
            // Recheck actual free space before relying on an eviction's credit.
            let bases = self.bases();
            loop {
                let credited = bases
                    .trim(u64::MAX, physical_need - physical, base_key.as_deref())
                    .await?;
                physical = headroom().await?;
                if physical >= physical_need || credited == 0 {
                    break;
                }
            }
            if physical < physical_need {
                return Err(Error::new(
                    507,
                    "Native data pool lacks preparation headroom.",
                ));
            }
            used = disk::used(&self.data, &self.state).await?;
        }
        let growth = disk::available(limit, used);
        if incoming > growth {
            let reclaimed =
                crate::project_archives::reclaim_for_foreground(&self.data, incoming - growth, &[])
                    .await?;
            if reclaimed > 0 {
                used = disk::used(&self.data, &self.state).await?;
            }
        }
        if incoming > disk::available(limit, used) || used > limit {
            return Err(Error::conflict(
                "Retained native work leaves insufficient shared disk budget for this preparation.",
            ));
        }
        Ok(PreparationSpace {
            incoming,
            spare: spare_key.is_some(),
            usage_sample: (used, observed),
        })
    }

    /// Admission is global before it is attempt-scoped: a queued pool waiter
    /// must never retain the guard needed by an unrelated physical close.
    pub(super) async fn with_live_access<T: Send + 'static, F, Fut>(
        &self,
        id: &str,
        requested_run: Value,
        forbidden: &'static str,
        operation: F,
    ) -> Result<T>
    where
        F: FnOnce(Self, String, CancellationToken, tokio::sync::OwnedMutexGuard<()>) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        uuid(id)?;
        let pool = disk::observed_lock(&self.state, "native_storage_live_access", id).await?;
        let live_access = {
            let active = self.active.lock().await;
            let attempt = active
                .get(id)
                .ok_or_else(|| Error::conflict("Native attempt is not active."))?;
            if !attempt.started {
                return Err(Error::conflict(
                    "Native attempt is still preparing its filesystem.",
                ));
            }
            if requested_run != attempt.run {
                return Err(Error::forbidden(forbidden));
            }
            attempt.live_access.clone()
        };
        drop(pool);
        let admitted = live_access.clone().lock_owned().await;
        let (run, stop) = {
            let active = self.active.lock().await;
            let attempt = active
                .get(id)
                .ok_or_else(|| Error::conflict("Native attempt is not active."))?;
            if requested_run != attempt.run
                || !Arc::ptr_eq(&live_access, &attempt.live_access)
                || attempt.stop.is_cancelled()
            {
                return Err(Error::forbidden(forbidden));
            }
            (attempt.run.clone(), attempt.stop.clone())
        };
        operation(self.clone(), run, stop, admitted).await
    }

    pub(super) async fn reserve_copy(
        &self,
        maximum: u64,
        filesystem: &Path,
        policy: &storage::policy::Policy,
    ) -> Result<(disk::Preparation, u64)> {
        let _pool = disk::observed_lock(&self.state, "native_storage_reserve_copy", "").await?;
        let physical = disk::physical_headroom(&self.state, filesystem, policy, None)?;
        let budget = maximum
            .min(self.disk_growth().await?)
            .min(physical.saturating_sub(disk::MIB));
        let reservation =
            disk::Preparation::reserve_on(&self.state, &crate::config::id(), budget, filesystem)
                .await?;
        Ok((reservation, budget))
    }

    pub(super) async fn artifact_snapshot(
        &self,
        id: &str,
        value: Value,
    ) -> Result<(tempfile::NamedTempFile, u64)> {
        let attempt = id.to_owned();
        self.with_live_access(
            id,
            value["runId"].clone(),
            "Wrong native artifact scope.",
            move |controller, run, stop, admitted| async move {
                let plan = controller.plan(&attempt).await?;
                let view = block::view(&controller.state, &run).await?;
                let path = Path::new(text(&value, "path"));
                let project_root = Path::new(text(&plan, "projectRoot"));
                let (source, allowed) = if let Ok(relative) = path.strip_prefix(project_root) {
                    (view.workspace.join(relative), view.workspace.clone())
                } else if let Ok(relative) = path.strip_prefix(
                    guest_data(&plan, &controller.data)
                        .join("runs")
                        .join(&run)
                        .join("output"),
                ) {
                    (view.output.join(relative), view.output.clone())
                } else if let Ok(relative) = path.strip_prefix("/tmp") {
                    let temporary = block::temporary(&controller.state, &run).await?;
                    (temporary.join(relative), temporary)
                } else {
                    return Err(Error::forbidden(
                        "Native artifacts must be in this conversation's workspace, output directory or /tmp.",
                    ));
                };
                let exports = controller.state.join("exports");
                let policy = storage::policy::for_node(&plan["storage"]["policy"])?;
                let limit = controller.disk_limit().await?;
                let mut admission = super::super::ingress::Admission::new(&controller.data, &controller.state, &exports, &controller.state, limit, &policy)?;
                admission.admit(5120)?;
                private_dir(&exports).await?;
                let (reservation, budget) = controller.reserve_copy(crate::artifacts::file::MAX_FILE, &exports, &policy).await?;
                let mut admission = super::super::ingress::Admission::new(&controller.data, &controller.state, &exports, &exports, limit, &policy)?.with_reservation(&reservation, false)?;
                let copying_stop = stop.clone();
                let (snapshot, size) = super::super::ingress::write((reservation, admitted), async move {
                    crate::artifacts::file::snapshot_in(&source, &allowed, &exports, budget, |bytes| {
                        if copying_stop.is_cancelled() { return Err(Error::forbidden("Native artifact export stopped.")); }
                        admission.admit(bytes)
                    }).await
                }).await?;
                if stop.is_cancelled() {
                    return Err(Error::forbidden("Native artifact attempt stopped."));
                }

                Ok((snapshot, size))
            },
        )
        .await
    }

    pub(super) async fn import_project(
        &self,
        id: &str,
        project: &str,
        value: Value,
    ) -> Result<Value> {
        uuid(project)?;
        let attempt = id.to_owned();
        let project = project.to_owned();
        self.with_live_access(
            id,
            value["runId"].clone(),
            "Wrong native project scope.",
            move |controller, run, stop, admitted| {
                super::super::ingress::write(admitted, async move {
                    let plan = controller.plan(&attempt).await?;
                    if stop.is_cancelled() {
                        return Err(Error::conflict("Native project import stopped."));
                    }
                    let manager = guest_data(&plan, &controller.data);
                    let expected = crate::execution::project_seed_path(
                        &manager.join("runs").join(&run),
                        &plan,
                        &project,
                    )?;
                    if value["source"] != expected.to_string_lossy().as_ref()
                        || value["target"] != expected.to_string_lossy().as_ref()
                    {
                        return Err(Error::forbidden("Invalid native project path."));
                    }
                    let relative = expected.strip_prefix(&manager).map_err(Error::internal)?;
                    let source = controller.data.join(relative);
                    if tokio::fs::canonicalize(&source).await? != source {
                        return Err(Error::bad("Native project seed changed location."));
                    }
                    let view = block::view(&controller.state, &run).await?;
                    let target = view.workspace.join(&project);
                    if !target.exists() {
                        let data = view.filesystem_of(&view.workspace)?;
                        // Stage on the mounted filesystem so installation is an atomic
                        // rename, and do not walk all lazy files to estimate free space.
                        let staged = tempfile::tempdir_in(data)?;
                        let received = staged.path().join("project");
                        let remaining = block::free_bytes(data)?;
                        let pool = workspace::data_root(&controller.state);
                        let policy = storage::policy::for_node(&plan["storage"]["policy"])?;
                        let (reservation, budget) =
                            controller.reserve_copy(remaining, &pool, &policy).await?;
                        let mut admission = super::super::ingress::Admission::new(
                            &controller.data,
                            &controller.state,
                            staged.path(),
                            &workspace::data_root(&controller.state),
                            controller.disk_limit().await?,
                            &policy,
                        )?
                        .with_reservation(&reservation, true)?;
                        workspace::copy_admitted(&source, &received, budget, &mut |bytes| {
                            if stop.is_cancelled() {
                                return Err(Error::conflict("Native project import stopped."));
                            }
                            admission.admit(bytes)
                        })
                        .await?;
                        if stop.is_cancelled() {
                            return Err(Error::conflict("Native project import stopped."));
                        }
                        let mut identity =
                            super::super::identity::provision(&controller.state, &run, &stop)
                                .await?;
                        if plan["nativeMode"]["mode"] == "administrator" {
                            identity.host_engine =
                                Some((controller.identity.uid, controller.identity.gid));
                        }
                        let stored = super::super::identity::stored_owner(data)
                            .await?
                            .unwrap_or(identity);
                        workspace::own_tree(&received, stored.uid(), stored.gid()).await?;
                        if stop.is_cancelled() {
                            return Err(Error::conflict("Native project import stopped."));
                        }
                        tokio::fs::rename(received, &target).await?;
                    }
                    Ok(json!({ "ok": true }))
                })
            },
        )
        .await
    }

    pub(super) async fn abort_disk(&self, id: &str) -> Result<()> {
        let run = self
            .active
            .lock()
            .await
            .get(id)
            .map(|attempt| attempt.run.clone());
        if let Some(run) = run {
            let mounted = self.mounted.lock().await.get(&run).cloned();
            if let Some(mounted) = mounted {
                mounted.abort_io().await?;
            }
        }
        Ok(())
    }

    pub(super) async fn stop_attempt(&self, id: &str) -> Result<()> {
        uuid(id)?;
        // Publish cancellation before waiting for an admission that may itself
        // be waiting for storage. Its waiter observes this durable marker.
        let written = tokio::time::timeout(
            Duration::from_secs(1),
            atomic_write(&self.file(id, "stopped"), b"1"),
        )
        .await;
        if !matches!(written, Ok(Ok(()))) {
            if let Some(attempt) = self.active.lock().await.get(id) {
                attempt.stop.cancel();
            }
            return Err(Error::unavailable(
                "Could not persist the native stop marker.",
            ));
        }
        {
            let _admission = self.admission.lock().await;
        }
        self.stop_admitted(id).await
    }

    pub(super) async fn stop_admitted(&self, id: &str) -> Result<()> {
        if self.stop.is_cancelled() {
            self.record_stop(id, "controller_shutdown", Value::Null)
                .await;
        }
        let written = tokio::time::timeout(
            Duration::from_secs(1),
            atomic_write(&self.file(id, "stopped"), b"1"),
        )
        .await;
        if !matches!(written, Ok(Ok(()))) {
            if let Some(attempt) = self.active.lock().await.get(id) {
                attempt.stop.cancel();
            }
            return Err(Error::unavailable(
                "Could not persist the native stop marker.",
            ));
        }
        let requested = self.active.lock().await.get(id).and_then(|attempt| {
            if attempt.stop.is_cancelled() {
                return None;
            }
            attempt.stop.cancel();
            Some(attempt.expires)
        });
        if let Some(expires) = requested {
            let reason = if self.expired(id, expires).await {
                "time_limit"
            } else {
                "cancelled"
            };
            self.record_stop(id, reason, Value::Null).await;
        }
        let preparing = self
            .active
            .lock()
            .await
            .get(id)
            .is_some_and(|attempt| !attempt.started);
        if preparing {
            self.abort_disk(id).await?;
        }
        if let Err(error) = systemd::stop(id).await {
            self.abort_disk(id).await?;
            return Err(error);
        }
        self.wait_stopped(id).await?;
        if let Err(error) = super::super::bases::release_attempt(&self.data, id).await {
            tracing::debug!(
                status = error.status,
                "Stopped native preparation pin cleanup deferred"
            );
        }
        Ok(())
    }

    pub(super) async fn wait_stopped(&self, id: &str) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(300), async {
            while self.active.lock().await.contains_key(id) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| {
            Error::unavailable("Stopped native checkpoint did not finish before its deadline.")
        })
    }

    /// Match the VM wait protocol: headers and spaces keep relayed waits alive.
    pub(super) fn wait(&self, id: &str, observe_retirement: bool) -> Response {
        let stream = futures_util::stream::unfold(
            (self.clone(), id.to_owned(), false),
            move |(controller, id, done)| async move {
                if done {
                    return None;
                }
                // An exit marker alone does not release physical ownership.
                let released = controller
                    .active
                    .try_lock()
                    .is_ok_and(|active| !active.contains_key(&id));
                if released
                    && let Ok(code) = tokio::fs::read_to_string(controller.file(&id, "exit")).await
                {
                    let result = code
                        .parse::<i32>()
                        .map(|code| json!({ "StatusCode": code }))
                        .or_else(|_| serde_json::from_str::<Value>(&code));
                    let result = match result {
                        Ok(value) if observe_retirement => Ok(Bytes::from(
                            controller
                                .reconcile_wait_result(&id, value)
                                .await
                                .to_string(),
                        )),
                        Ok(value) => Ok(Bytes::from(value.to_string())),
                        Err(error) => Err(std::io::Error::other(error)),
                    };
                    return Some((result, (controller, id, true)));
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                let stopped = controller.stop.is_cancelled();
                Some((Ok(Bytes::from_static(b" ")), (controller, id, stopped)))
            },
        );
        (
            [("content-type", "application/json")],
            Body::from_stream(stream),
        )
            .into_response()
    }
}
