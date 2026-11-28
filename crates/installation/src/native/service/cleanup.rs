//! Bridge the isolated physical worker to the authenticated attempt result.
use super::super::cleanup::read_attempt_evidence;
use super::*;

// The manager can miss the underlying failure if this optional probe stalls.
const WAIT_RETIREMENT_PROBE: Duration = Duration::from_millis(250);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Completed {
    pub completion: block::Completion,
}

pub(super) struct CloseWork {
    pub protection: Option<tokio::task::JoinHandle<()>>,
    pub mounted: Option<Arc<block::Handle>>,
    pub mounts: Arc<Mutex<HashMap<String, Arc<block::Handle>>>>,
    pub storage_close: StorageClose,
    pub completion_lock: Arc<Mutex<()>>,
    pub execution_guard: Arc<Mutex<()>>,
    pub _owner: Option<Arc<crate::file_lock::Guard>>,
    pub data: PathBuf,
    pub exported: bool,
    pub suspension: Arc<super::super::suspension::Suspension>,
    pub state: PathBuf,
    pub run: String,
    pub attempt: String,
    pub completion: Option<block::Completion>,
}

#[async_trait::async_trait]
impl super::super::cleanup::Work for CloseWork {
    async fn run(&mut self) -> Result<()> {
        // A late freeze/thaw must settle before releasing disk ownership, but
        // its runtime can itself be blocked by the execution's filesystem I/O.
        if let Some(protection) = self.protection.take() {
            let _ = protection.await;
        }
        let _execution = self.execution_guard.lock().await;
        if self.mounted.is_none() {
            self.mounted = self.mounts.lock().await.get(&self.run).cloned();
        }
        self.storage_close.drain().await?;
        super::super::identity::Mapped::recover(&self.state, &self.attempt, &self.run)?;
        if !self.exported {
            let root = workspace::directory(&self.state, &self.run)?;
            let output = block::view(&self.state, &self.run)
                .await
                .map_or_else(|_| root.join("data/output"), |view| view.output);
            let result = output.join("result.md");
            if let Ok(file) = crate::artifacts::file::open_export(&result, &output).await {
                let file = tokio::fs::File::from_std(file);
                let mut bytes = Vec::new();
                if file.take(1_000_001).read_to_end(&mut bytes).await.is_ok()
                    && bytes.len() <= 1_000_000
                {
                    let target = self
                        .data
                        .join("runs")
                        .join(&self.run)
                        .join("output/result.md");
                    let _ = atomic_write(&target, &bytes).await;
                }
            }
            self.exported = true;
        }
        if self.completion.is_none() {
            self.completion = Some(if let Some(mounted) = &self.mounted {
                self.storage_close
                    .finish(async {
                        let _ingress = mounted.ingress.lock().await;
                        mounted.mount.lock().await.close().await
                    })
                    .await?
            } else {
                block::recover_completed(&self.state, &self.run, &self.attempt).await?;
                block::Completion::Aborted
            });
        }
        let mut mounts = self.mounts.lock().await;
        if self.mounted.as_ref().is_some_and(|owned| {
            mounts
                .get(&self.run)
                .is_some_and(|current| Arc::ptr_eq(owned, current))
        }) {
            mounts.remove(&self.run);
        }
        drop(mounts);
        let _completion = self.completion_lock.lock().await;
        atomic_write(
            &self.state.join(format!("{}.cleanup-result", self.attempt)),
            &serde_json::to_vec(&Completed {
                completion: self.completion.unwrap(),
            })?,
        )
        .await?;
        // Late completion must retire this exact attempt before its quarantine
        // can disappear. Otherwise restart may select an obsolete active file.
        self.suspension.finish().await?;
        if self
            .state
            .join(format!("{}.exit", self.attempt))
            .try_exists()?
        {
            match tokio::fs::remove_file(self.state.join(format!("{}.active", self.attempt))).await
            {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    async fn pending(&mut self, message: &str) -> Result<()> {
        let _completion = self.completion_lock.lock().await;
        let path = self.state.join(format!("{}.exit", self.attempt));
        let Ok(bytes) = tokio::fs::read(path).await else {
            return Ok(());
        };
        let previous: Value = serde_json::from_slice(&bytes)?;
        if crate::runner::client_cleanup_result(&previous, &self.attempt).is_some() {
            record_pending(&self.state, &self.attempt, message).await?;
        }
        Ok(())
    }
}

async fn record_pending(state: &Path, id: &str, message: &str) -> Result<()> {
    let diagnostic: String = message
        .chars()
        .filter(|c| !c.is_control())
        .take(2048)
        .collect();
    let result = json!({
        "StatusCode": crate::runner::CONTROLLER_FAILED,
        "FinalizationFailed": true,
        "NativeCleanup": { "attemptId": id, "workloadFenced": true, "message": diagnostic },
    });
    let path = state.join(format!("{id}.exit"));
    let bytes = serde_json::to_vec(&result).unwrap();
    tokio::time::timeout(Duration::from_secs(2), atomic_write(&path, &bytes))
        .await
        .map_err(|_| Error::unavailable("Persisting native cleanup result timed out."))??;
    Ok(())
}

impl Controller {
    pub(super) async fn record_cleanup_pending(&self, id: &str, message: &str) -> Result<()> {
        record_pending(&self.state, id, message).await
    }

    pub(super) async fn stopped_result(&self, id: &str) -> Value {
        let Ok(bytes) = tokio::fs::read(self.file(id, "exit")).await else {
            return json!({});
        };
        let Ok(mut result) = serde_json::from_slice::<Value>(&bytes) else {
            return json!({});
        };
        if crate::runner::client_cleanup_result(&result, id).is_some()
            && self.cleanup_receipt_superseded(id, None).await
        {
            // A new fence must not report that the verified close is still pending.
            tracing::debug!(attempt = id, "Ignoring superseded native cleanup receipt");
            return json!({ "NativeCleanupRetired": { "attemptId": id } });
        }
        if result["PhysicalFinalized"] == true
            && result["FinalizationFailed"] != true
            && self
                .cleanup_status(id)
                .await
                .is_ok_and(|status| status["flushed"] == true)
        {
            // Physical finalization also covers an aborted disk. Keep the
            // legacy exit fields and attest native flush separately.
            result["NativeCleanupRetired"] = json!({ "attemptId": id });
        }
        result
    }

    pub(super) async fn reconcile_wait_result(&self, id: &str, mut result: Value) -> Value {
        if crate::runner::client_cleanup_result(&result, id).is_some() {
            result
                .as_object_mut()
                .unwrap()
                .remove("NativeCleanupReusable");
            let (retired, reusable_without_flush) =
                tokio::time::timeout(WAIT_RETIREMENT_PROBE, async {
                    if self.cleanup_flushed(id).await {
                        (self.cleanup_reusable(id, Some(id)).await, false)
                    } else {
                        (false, self.cleanup_reusable(id, None).await)
                    }
                })
                .await
                .unwrap_or_default();
            let cleanup = result["NativeCleanup"].as_object_mut().unwrap();
            cleanup.remove("retired");
            if retired {
                // Older managers still need the original failure fields to
                // suppress replay. The attempt-bound extension attests closure.
                cleanup.insert("retired".into(), true.into());
            }
            if reusable_without_flush {
                result["NativeCleanupReusable"] = json!({ "attemptId": id });
            }
        }
        result
    }

    async fn cleanup_receipt_superseded(&self, id: &str, attempt: Option<&str>) -> bool {
        self.cleanup_flushed(id).await && self.cleanup_reusable(id, attempt).await
    }

    pub(super) async fn cleanup_status(&self, id: &str) -> Result<Value> {
        uuid(id)?;
        let check = async {
            let flushed = self.cleanup_flushed(id).await;
            let reusable = self.cleanup_reusable(id, None).await;
            (flushed && reusable, reusable)
        };
        let (flushed, reusable) = tokio::time::timeout(WAIT_RETIREMENT_PROBE, check)
            .await
            .unwrap_or_default();
        Ok(json!({ "attemptId": id, "flushed": flushed, "reusable": reusable }))
    }

    async fn cleanup_flushed(&self, id: &str) -> bool {
        let Ok(Some(bytes)) =
            read_attempt_evidence(&self.file(id, "cleanup-result"), 1024, false).await
        else {
            return false;
        };
        bytes.trim_ascii_start().starts_with(b"{")
            && matches!(
                serde_json::from_slice::<Completed>(&bytes),
                Ok(Completed {
                    completion: block::Completion::Flushed,
                })
            )
    }

    async fn cleanup_reusable(&self, id: &str, attempt: Option<&str>) -> bool {
        let Ok(Some(owner)) = read_attempt_evidence(&self.file(id, "run"), 36, false).await else {
            return false;
        };
        let Ok(run) = std::str::from_utf8(&owner) else {
            return false;
        };
        // The completion file is written before final suspension accounting
        // and quarantine retirement. Require both durable markers to be gone.
        uuid(run).is_ok()
            && matches!(
                tokio::fs::symlink_metadata(self.file(id, "active")).await,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            )
            && self.cleanup.retired(&self.state, run, attempt).await
    }
}

pub(super) struct RecoveryWork {
    pub state: PathBuf,
    pub run: String,
    pub attempt: String,
    pub unmounted: bool,
    pub retired: bool,
    pub owner: Option<crate::file_lock::Guard>,
    pub disk: Option<storage::environment::Lease>,
    pub fenced: Option<Vec<(String, PathBuf)>>,
    pub progress: Arc<Mutex<RecoveryProgress>>,
}

#[derive(Default)]
pub(super) struct RecoveryProgress {
    attempts: Option<Vec<(String, PathBuf)>>,
    complete: bool,
    report: bool,
}

pub(super) fn observe_recovery(
    state: PathBuf,
    attempt: String,
    progress: Arc<Mutex<RecoveryProgress>>,
    mut ticket: super::super::cleanup::Ticket,
) -> Result<()> {
    super::super::cleanup::spawn(async move {
        if matches!(
            ticket.wait(super::super::cleanup::WAIT).await,
            super::super::cleanup::Outcome::Complete
        ) {
            return;
        }
        loop {
            let mut current = progress.lock().await;
            if current.complete {
                return;
            }
            current.report = true;
            if let Some(attempts) = &current.attempts {
                let message = match ticket.current() {
                    super::super::cleanup::Outcome::Complete => return,
                    super::super::cleanup::Outcome::Pending(message) => message,
                };
                if let Err(error) =
                    record_recovery_pending(&state, &attempt, attempts, &message).await
                {
                    tracing::error!(%error, "Could not persist pending native recovery result");
                }
            }
            drop(current);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })?;
    Ok(())
}

async fn record_recovery_pending(
    state: &Path,
    attempt: &str,
    attempts: &[(String, PathBuf)],
    message: &str,
) -> Result<()> {
    let mut ids = vec![attempt];
    ids.extend(attempts.iter().map(|(id, _)| id.as_str()));
    ids.sort_unstable();
    ids.dedup();
    for id in ids {
        let path = state.join(format!("{id}.exit"));
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let previous = serde_json::from_slice::<Value>(&bytes)?;
                if crate::runner::client_cleanup_result(&previous, id).is_none() {
                    continue;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        record_pending(state, id, message).await?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl super::super::cleanup::Work for RecoveryWork {
    async fn run(&mut self) -> Result<()> {
        if self.fenced.is_none() {
            let mut attempts = Vec::new();
            let mut entries = tokio::fs::read_dir(&self.state).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path
                    .extension()
                    .is_none_or(|extension| extension != "active")
                {
                    continue;
                }
                if tokio::fs::read_to_string(&path).await? != self.run {
                    continue;
                }
                let attempt = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| Error::conflict("Invalid native attempt marker."))?
                    .to_owned();
                uuid(&attempt)?;
                attempts.push((attempt, path));
            }
            let mut stopped = systemd::stop(&self.attempt).await.is_ok();
            for (attempt, _) in &attempts {
                stopped &= systemd::stop(attempt).await.is_ok();
            }
            if !stopped {
                // A dead backend may leave consumers in D-state. Its owned
                // recovery path aborts I/O before requiring their final fence.
                super::super::block::recover_host_mounts(&self.state, &self.run).await?;
                self.unmounted = true;
                systemd::stop(&self.attempt).await?;
                for (attempt, _) in &attempts {
                    systemd::stop(attempt).await?;
                }
            }
            super::super::identity::Mapped::recover(&self.state, &self.attempt, &self.run)?;
            for (attempt, _) in &attempts {
                super::super::identity::Mapped::recover(&self.state, attempt, &self.run)?;
            }
            self.fenced = Some(attempts);
        }
        // The independent observer publishes a failure only once this recovery
        // exceeds its observation budget and the workload fence is confirmed.
        self.progress.lock().await.attempts = self.fenced.clone();
        if !self.unmounted {
            super::super::block::recover_host_mounts(&self.state, &self.run).await?;
            systemd::stop(&self.attempt).await?;
            self.unmounted = true;
        }
        if !self.retired {
            if self.owner.is_none() {
                let root = workspace::directory(&self.state, &self.run)?;
                self.owner = Some(crate::file_lock::exclusive(
                    &root.join("owner.lock"),
                    "Native recovery is still owned.",
                )?);
            }
            if self.disk.is_none() {
                self.disk = Some(
                    storage::environment::lock(
                        &self.state,
                        &self.run,
                        "Native recovery is still owned.",
                    )
                    .await?,
                );
            }
            let state = self.state.clone();
            let run = self.run.clone();
            tokio::task::spawn_blocking(move || storage::ublk::cleanup_native_run(&state, &run))
                .await
                .map_err(Error::internal)??;
            self.retired = true;
        }
        // Release physical locks only after verified kernel retirement, before
        // recovering the logical journal through its normal ownership API.
        drop(self.disk.take());
        drop(self.owner.take());
        block::recover_completed(&self.state, &self.run, &self.attempt).await?;
        super::super::suspension::recover(&self.state, &self.attempt).await?;
        match tokio::fs::remove_file(self.state.join("native-preparations").join(&self.attempt))
            .await
        {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        // Settle receipts only after journal recovery succeeds. The observer
        // cannot publish a delayed failure once completion wins this lock.
        let mut progress = self.progress.lock().await;
        for (attempt, path) in self.fenced.as_ref().unwrap() {
            super::super::suspension::recover(&self.state, attempt).await?;
            preserve_interrupted_exit(path).await?;
            match tokio::fs::remove_file(self.state.join("native-preparations").join(attempt)).await
            {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            match tokio::fs::remove_file(path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        progress.complete = true;
        Ok(())
    }

    async fn pending(&mut self, message: &str) -> Result<()> {
        let current = self.progress.lock().await;
        if current.report
            && let Some(attempts) = &current.attempts
        {
            record_recovery_pending(&self.state, &self.attempt, attempts, message).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::cleanup::Work;
    use super::*;

    #[tokio::test]
    async fn quick_recovery_observer_keeps_the_normal_interrupted_result() {
        struct Complete;
        #[async_trait::async_trait]
        impl Work for Complete {
            async fn run(&mut self) -> Result<()> {
                Ok(())
            }
        }
        let root = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let attempt = crate::config::id();
        let active = root.path().join(format!("{attempt}.active"));
        std::fs::write(&active, &run).unwrap();
        preserve_interrupted_exit(&active).await.unwrap();
        let progress = Arc::new(Mutex::new(RecoveryProgress {
            attempts: Some(vec![(attempt.clone(), active)]),
            complete: false,
            report: false,
        }));
        let mut ticket = super::super::super::cleanup::Registry::default()
            .start(root.path(), &run, &attempt, Complete)
            .unwrap();
        observe_recovery(
            root.path().into(),
            attempt.clone(),
            progress.clone(),
            ticket.clone(),
        )
        .unwrap();
        assert!(matches!(
            ticket.wait(Duration::from_secs(2)).await,
            super::super::super::cleanup::Outcome::Complete
        ));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!progress.lock().await.report);
        assert_eq!(
            std::fs::read_to_string(root.path().join(format!("{attempt}.exit"))).unwrap(),
            crate::runner::CONTROLLER_INTERRUPTED.to_string()
        );
    }

    #[tokio::test]
    async fn completed_recovery_keeps_interrupted_exit_without_reporting_failure() {
        let root = tempfile::tempdir().unwrap();
        let attempt = crate::config::id();
        let marker = root.path().join(format!("{attempt}.active"));
        std::fs::write(&marker, b"run").unwrap();
        let progress = Arc::new(Mutex::new(RecoveryProgress::default()));
        let mut work = RecoveryWork {
            state: root.path().into(),
            run: crate::config::id(),
            attempt: attempt.clone(),
            unmounted: false,
            retired: false,
            owner: None,
            disk: None,
            fenced: Some(vec![(attempt.clone(), marker.clone())]),
            progress: progress.clone(),
        };
        work.progress.lock().await.attempts = work.fenced.clone();
        work.pending("ordinary recovery").await.unwrap();
        assert!(!root.path().join(format!("{attempt}.exit")).exists());
        preserve_interrupted_exit(&marker).await.unwrap();
        progress.lock().await.complete = true;
        assert_eq!(
            std::fs::read_to_string(root.path().join(format!("{attempt}.exit"))).unwrap(),
            crate::runner::CONTROLLER_INTERRUPTED.to_string()
        );
    }

    #[tokio::test]
    async fn recovered_cleanup_result_requires_workload_fence_and_keeps_its_cause() {
        let root = tempfile::tempdir().unwrap();
        let attempt = crate::config::id();
        let mut work = RecoveryWork {
            state: root.path().into(),
            run: crate::config::id(),
            attempt: attempt.clone(),
            unmounted: false,
            retired: false,
            owner: None,
            disk: None,
            fenced: None,
            progress: Arc::default(),
        };
        work.pending("unsafe before fence").await.unwrap();
        let path = root.path().join(format!("{attempt}.exit"));
        assert!(!path.exists());
        work.fenced = Some(Vec::new());
        work.progress.lock().await.attempts = Some(Vec::new());
        work.pending("still within observation budget")
            .await
            .unwrap();
        assert!(!path.exists());
        work.progress.lock().await.report = true;
        work.pending("Held by valkey pid 42").await.unwrap();
        let result: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let result = crate::runner::client_cleanup_result(&result, &attempt).unwrap();
        assert_eq!(
            result.native_cleanup.as_deref(),
            Some("Held by valkey pid 42")
        );
        assert!(!result.physical_finalized);
        work.pending("Updated holder diagnosis").await.unwrap();
        let result: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            crate::runner::client_cleanup_result(&result, &attempt)
                .unwrap()
                .native_cleanup
                .as_deref(),
            Some("Updated holder diagnosis")
        );
        std::fs::write(&path, b"0").unwrap();
        work.pending("Do not replace successful exit")
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"0");
    }

    #[tokio::test]
    async fn blocked_protection_remains_owned_without_blocking_the_cleanup_observer() {
        let (_root, controller) = super::super::tests::fixture().await;
        let run = crate::config::id();
        let attempt = crate::config::id();
        private_dir(&workspace::directory(&controller.state, &run).unwrap())
            .await
            .unwrap();
        let (release, blocked) = tokio::sync::oneshot::channel::<()>();
        let protection = tokio::spawn(async move {
            let _ = blocked.await;
        });
        let work = CloseWork {
            protection: Some(protection),
            mounted: None,
            mounts: controller.mounted.clone(),
            storage_close: StorageClose::default(),
            completion_lock: Arc::default(),
            execution_guard: Arc::default(),
            _owner: None,
            data: controller.data.clone(),
            exported: true,
            suspension: Arc::new(super::super::super::suspension::Suspension::new(
                &controller.state,
                &attempt,
            )),
            state: controller.state.clone(),
            run: run.clone(),
            attempt,
            completion: Some(block::Completion::Aborted),
        };
        let mut ticket = controller
            .cleanup
            .start(&controller.state, &run, &work.attempt.clone(), work)
            .unwrap();
        assert!(matches!(
            ticket.wait(Duration::from_millis(20)).await,
            super::super::super::cleanup::Outcome::Pending(_)
        ));
        assert!(super::super::super::cleanup::check(&controller.state, &run).is_err());
        assert_eq!(controller.health().await["backend"], "native");
        release.send(()).unwrap();
        assert!(matches!(
            ticket.wait(Duration::from_secs(2)).await,
            super::super::super::cleanup::Outcome::Complete
        ));
    }

    #[tokio::test]
    async fn late_execution_fence_keeps_disk_quarantine_before_completion() {
        let (_root, controller) = super::super::tests::fixture().await;
        let run = crate::config::id();
        let attempt = crate::config::id();
        private_dir(&workspace::directory(&controller.state, &run).unwrap())
            .await
            .unwrap();
        atomic_write(&controller.file(&attempt, "active"), run.as_bytes())
            .await
            .unwrap();
        atomic_write(&controller.file(&attempt, "run"), run.as_bytes())
            .await
            .unwrap();
        controller
            .record_cleanup_pending(&attempt, "pending disk operation")
            .await
            .unwrap();
        let execution_guard = Arc::new(Mutex::new(()));
        let running = execution_guard.clone().lock_owned().await;
        let mut work = CloseWork {
            protection: None,
            mounted: None,
            mounts: controller.mounted.clone(),
            storage_close: StorageClose::default(),
            completion_lock: Arc::default(),
            execution_guard,
            _owner: None,
            data: controller.data.clone(),
            exported: true,
            suspension: Arc::new(super::super::super::suspension::Suspension::new(
                &controller.state,
                &attempt,
            )),
            state: controller.state.clone(),
            run: run.clone(),
            attempt: attempt.clone(),
            completion: Some(block::Completion::Flushed),
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(20), work.run())
                .await
                .is_err()
        );
        assert!(controller.file(&attempt, "active").exists());
        work.pending("disk held by valkey pid 42").await.unwrap();
        let value = controller.stopped_result(&attempt).await;
        assert_eq!(
            crate::runner::client_cleanup_result(&value, &attempt)
                .unwrap()
                .native_cleanup
                .as_deref(),
            Some("disk held by valkey pid 42")
        );
        super::super::super::cleanup::quarantine(&controller.state, &run, &attempt).unwrap();
        let mut ticket = controller
            .cleanup
            .start(&controller.state, &run, &attempt, work)
            .unwrap();
        drop(running);
        assert!(matches!(
            ticket.wait(Duration::from_secs(2)).await,
            super::super::super::cleanup::Outcome::Complete
        ));
        assert!(!controller.file(&attempt, "active").exists());
        assert!(controller.file(&attempt, "cleanup-result").exists());
        assert_eq!(
            controller.stopped_result(&attempt).await,
            json!({ "NativeCleanupRetired": { "attemptId": attempt } })
        );
        let response = controller.wait(&attempt, true);
        let history = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let history = serde_json::from_slice::<Value>(&history).unwrap();
        let mut retired = value.clone();
        retired["NativeCleanup"]["retired"] = true.into();
        assert_eq!(history, retired);
        // Legacy managers keep treating this as an execution failure.
        let legacy = crate::runner::client_cleanup_result(&history, &attempt).unwrap();
        assert!(legacy.finalization_failed);
        assert!(!legacy.physical_finalized);
        assert_eq!(
            serde_json::from_slice::<Value>(
                &std::fs::read(controller.file(&attempt, "exit")).unwrap()
            )
            .unwrap(),
            value
        );

        let later_attempt = crate::config::id();
        super::super::super::cleanup::quarantine(&controller.state, &run, &later_attempt).unwrap();
        let historical_result =
            axum::body::to_bytes(controller.wait(&attempt, true).into_body(), 4096)
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&historical_result).unwrap(),
            history
        );
        // Stop still fences the whole conversation.
        assert_eq!(controller.stopped_result(&attempt).await, value);
    }

    #[test]
    fn wait_preserves_a_fenced_failure_when_retirement_io_stalls() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (_root, controller) = super::super::tests::fixture().await;
            let attempt = crate::config::id();
            let pending = json!({
                "StatusCode": crate::runner::CONTROLLER_FAILED,
                "FinalizationFailed": true,
                "NativeCleanup": {
                    "attemptId": attempt,
                    "workloadFenced": true,
                    "message": "Native disk cleanup is pending."
                }
            });
            let (ready, started) = tokio::sync::oneshot::channel();
            let (release, blocked) = tokio::sync::oneshot::channel::<()>();
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = ready.send(());
                let _ = blocked.blocking_recv();
            });
            started.await.unwrap();

            let result = tokio::time::timeout(
                Duration::from_secs(2),
                controller.reconcile_wait_result(&attempt, pending.clone()),
            )
            .await;
            let _ = release.send(());
            blocker.await.unwrap();

            assert_eq!(result.unwrap(), pending);
        });
    }

    #[tokio::test]
    async fn a_plain_native_exit_attests_flush_only_when_the_disk_was_flushed() {
        let (_root, controller) = super::super::tests::fixture().await;
        private_dir(&controller.state.join("native-cleanup"))
            .await
            .unwrap();
        for completion in [block::Completion::Aborted, block::Completion::Flushed] {
            let run = crate::config::id();
            let attempt = crate::config::id();
            std::fs::write(controller.file(&attempt, "run"), run).unwrap();
            let exit = json!({ "StatusCode": 0, "PhysicalFinalized": true });
            std::fs::write(controller.file(&attempt, "exit"), exit.to_string()).unwrap();
            std::fs::write(
                controller.file(&attempt, "cleanup-result"),
                serde_json::to_vec(&Completed { completion }).unwrap(),
            )
            .unwrap();
            let stopped = controller.stopped_result(&attempt).await;
            let mut expected = exit;
            if completion == block::Completion::Flushed {
                expected["NativeCleanupRetired"] = json!({ "attemptId": attempt });
            }
            assert_eq!(stopped, expected);
        }
    }

    #[tokio::test]
    async fn cleanup_status_distinguishes_reusable_workspace_from_clean_flush() {
        let (_root, controller) = super::super::tests::fixture().await;
        private_dir(&controller.state.join("native-cleanup"))
            .await
            .unwrap();
        let run = crate::config::id();
        let attempt = crate::config::id();
        atomic_write(&controller.file(&attempt, "run"), run.as_bytes())
            .await
            .unwrap();
        atomic_write(&controller.file(&attempt, "active"), run.as_bytes())
            .await
            .unwrap();
        controller
            .record_cleanup_pending(&attempt, "cleanup pending")
            .await
            .unwrap();
        let exit = controller.stopped_result(&attempt).await;
        super::super::super::cleanup::quarantine(&controller.state, &run, &attempt).unwrap();

        let status = |flushed, reusable| json!({ "attemptId": attempt, "flushed": flushed, "reusable": reusable });
        assert_eq!(
            controller.cleanup_status(&attempt).await.unwrap(),
            status(false, false)
        );
        assert_eq!(
            controller
                .reconcile_wait_result(&attempt, exit.clone())
                .await,
            exit
        );
        tokio::fs::remove_file(controller.file(&attempt, "active"))
            .await
            .unwrap();
        assert_eq!(
            controller.cleanup_status(&attempt).await.unwrap(),
            status(false, false)
        );
        tokio::fs::remove_file(
            controller
                .state
                .join("native-cleanup")
                .join(format!("{run}.json")),
        )
        .await
        .unwrap();

        // On restart, orphan recovery can release the workspace without a
        // receipt from the controller that attempted the original flush.
        assert_eq!(
            controller.cleanup_status(&attempt).await.unwrap(),
            status(false, true)
        );
        for (completion, flushed) in [
            (block::Completion::FailedFlush, false),
            (block::Completion::Aborted, false),
            (block::Completion::Flushed, true),
        ] {
            atomic_write(
                &controller.file(&attempt, "cleanup-result"),
                &serde_json::to_vec(&Completed { completion }).unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(
                controller.cleanup_status(&attempt).await.unwrap(),
                status(flushed, true)
            );
            let observation = controller
                .reconcile_wait_result(&attempt, exit.clone())
                .await;
            assert_eq!(observation["NativeCleanup"]["retired"] == true, flushed);
            assert_eq!(
                observation["NativeCleanupReusable"]["attemptId"].as_str(),
                (!flushed).then_some(attempt.as_str()),
            );
        }
        let other = crate::config::id();
        super::super::super::cleanup::quarantine(&controller.state, &run, &other).unwrap();
        assert_eq!(
            controller.cleanup_status(&attempt).await.unwrap(),
            status(false, false)
        );
    }

    #[tokio::test]
    async fn stop_reconciles_only_retired_cleanup_for_the_same_attempt() {
        let (_root, controller) = super::super::tests::fixture().await;
        private_dir(&controller.state.join("native-cleanup"))
            .await
            .unwrap();
        let run = crate::config::id();
        let attempt = crate::config::id();
        let other_attempt = crate::config::id();
        tokio::fs::write(controller.file(&attempt, "run"), &run)
            .await
            .unwrap();
        controller
            .record_cleanup_pending(&attempt, "cleanup is still pending")
            .await
            .unwrap();
        let pending = controller.stopped_result(&attempt).await;
        assert!(crate::runner::client_cleanup_result(&pending, &attempt).is_some());
        let mut reusable = pending.clone();
        reusable["NativeCleanupReusable"] = json!({ "attemptId": attempt });
        let waiting = async || {
            let body = axum::body::to_bytes(controller.wait(&attempt, true).into_body(), 4096)
                .await
                .unwrap();
            serde_json::from_slice::<Value>(&body).unwrap()
        };
        assert_eq!(waiting().await, reusable);

        let completion = controller.file(&attempt, "cleanup-result");
        let flushed = br#"{"completion":"Flushed"}"#;
        let other_completion = controller.file(&other_attempt, "cleanup-result");
        tokio::fs::write(&other_completion, flushed).await.unwrap();
        assert_eq!(controller.stopped_result(&attempt).await, pending);
        assert_eq!(waiting().await, reusable);
        tokio::fs::hard_link(&other_completion, &completion)
            .await
            .unwrap();
        assert_eq!(controller.stopped_result(&attempt).await, pending);
        assert_eq!(waiting().await, reusable);
        tokio::fs::remove_file(&completion).await.unwrap();
        tokio::fs::symlink(&other_completion, &completion)
            .await
            .unwrap();
        assert_eq!(controller.stopped_result(&attempt).await, pending);
        assert_eq!(waiting().await, reusable);
        tokio::fs::remove_file(&completion).await.unwrap();
        for bytes in [
            b"malformed".as_slice(),
            br#"{"completion":"FailedFlush"}"#,
            br#"{"completion":"Aborted"}"#,
            br#"["Flushed"]"#,
            br#"{"completion":"Flushed","attemptId":"another-attempt"}"#,
            br#"{"completion":"FailedFlush","completion":"Flushed"}"#,
        ] {
            tokio::fs::write(&completion, bytes).await.unwrap();
            assert_eq!(controller.stopped_result(&attempt).await, pending);
            assert_eq!(waiting().await, reusable);
        }

        tokio::fs::write(&completion, flushed).await.unwrap();
        let active = controller.file(&attempt, "active");
        tokio::fs::write(&active, &run).await.unwrap();
        assert_eq!(controller.stopped_result(&attempt).await, pending);
        assert_eq!(waiting().await, pending);
        tokio::fs::remove_file(active).await.unwrap();
        super::super::super::cleanup::quarantine(&controller.state, &run, &attempt).unwrap();
        assert_eq!(controller.stopped_result(&attempt).await, pending);
        assert_eq!(waiting().await, pending);
        tokio::fs::remove_file(
            controller
                .state
                .join("native-cleanup")
                .join(format!("{run}.json")),
        )
        .await
        .unwrap();
        assert_eq!(
            controller.stopped_result(&attempt).await,
            json!({ "NativeCleanupRetired": { "attemptId": attempt } })
        );

        let failure = json!({ "StatusCode": 70, "FinalizationFailed": true });
        tokio::fs::write(controller.file(&attempt, "exit"), failure.to_string())
            .await
            .unwrap();
        assert_eq!(controller.stopped_result(&attempt).await, failure);
        assert_eq!(waiting().await, failure);
    }
}
