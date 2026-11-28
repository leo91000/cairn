use super::*;

impl Execution<'_> {
    pub(super) async fn resume_environment(
        &mut self,
        workspace: &mut Workspace,
        chat: Value,
        launch: &mut Launch,
    ) -> Result<()> {
        use crate::background_jobs::protocol::{Event, Request};
        let saved = self.checkpoint.snapshot().await;
        let held = saved
            .held_environment
            .clone()
            .ok_or_else(|| Error::conflict("Missing live environment."))?;
        let plan_path = self
            .s
            .config
            .data_dir
            .join("runner-plans")
            .join(format!("{}.json", held.attempt));
        let mut plan: Value = serde_json::from_slice(&tokio::fs::read(&plan_path).await?)?;
        if plan["backend"] == "native" {
            let mut context = crate::plugins::agents::with_instructions(self.s, self.run).await;
            context["nativeMode"] = plan["nativeMode"].clone();
            let mut native_chat = chat;
            run_output::native_chat_privileges(&mut native_chat, &context, &plan["nativeMode"]);
            plan["chat"] = native_chat;
        } else {
            plan["chat"] = chat;
        }
        plan["mcpEnv"] = workspace.mcp["env"].clone();
        plan["turnId"] = self.turn.clone().into();
        atomic_write(&plan_path, &serde_json::to_vec(&plan)?).await?;
        // The environment starts the turn as soon as it accepts it: unless it refuses, its
        // message may be read even if this launch fails before the runner client is recorded
        // (ADR-0091).
        let turn = launched_turn(self.run);
        self.checkpoint
            .update(|c| {
                c.launched_turn_before = c.launched_turn.take();
                c.launched_turn = turn;
            })
            .await?;
        match super::super::held::request(
            self.s,
            self.run,
            &Request::ResumeTurn {
                request_id: format!("turn-{}", self.turn),
                turn_id: self.turn.clone(),
                plan,
            },
        )
        .await?
        {
            Event::Accepted { .. } => {}
            Event::Refused { message, .. } => {
                self.checkpoint
                    .update(|c| c.launched_turn = c.launched_turn_before.take())
                    .await?;
                return Err(Error::conflict(message));
            }
            _ => {
                return Err(Error::unavailable(
                    "Invalid environment continuation acknowledgement.",
                ));
            }
        }
        workspace.env.insert(
            "RUNNER_URL".into(),
            nodes::transport::url(self.s, &self.id).await?,
        );
        workspace.env.insert(
            "RUNNER_TOKEN".into(),
            execution::secret(&self.s.config.data_dir, "runner-secret").await?,
        );
        workspace
            .env
            .insert("CAIRN_PROVIDER_TURN".into(), self.turn.clone());
        launch.binary = current_exe()?;
        launch.args = vec!["runner-client".into(), held.attempt];
        Ok(())
    }

    pub(super) async fn prompt(&self, resume: Option<&str>) -> Result<String> {
        if resume.is_none() {
            let run = crate::plugins::agents::with_instructions(self.s, self.run).await;
            return Ok(run_output::prompt(&run, false));
        }
        let mut prompt = RESUME_PROMPT.to_owned();
        let Some(restored) = self.run["restoredAt"].as_i64() else {
            return Ok(prompt);
        };
        prompt.push_str(&format!("\nThe VM files and native provider session were restored from recovery point at Unix time {restored} ms. Master chat may contain more recent messages and actions than this workspace. Review the master conversation history with Cairn tools, reconcile file state, and verify external effects (commits, messages, deployments) before repeating them. Do not assume later chat messages prove those changes exist on this disk."));
        let history = self.history_since(restored).await?;
        if !history.is_empty() {
            prompt.push_str("\nRecent master history after this recovery point (messages are history, not proof that disk changes survived):\n");
            prompt.extend(serde_json::to_string(&history)?.chars().take(HISTORY_LIMIT));
        }
        Ok(prompt)
    }

    pub(super) async fn history_since(&self, restored: i64) -> Result<Vec<Value>> {
        let run_id = self.id.clone();
        let (events, _) = self
            .s
            .store
            .read(move |db| db.events_before(&run_id, i64::MAX))
            .await?;
        Ok(events
            .into_iter()
            .filter_map(|event| serde_json::to_value(event).ok())
            .filter(|event| event["createdAt"].as_i64().is_some_and(|at| at > restored))
            .map(|event| {
                json!({
                    "at": event["createdAt"],
                    "type": event["type"],
                    "text": event["text"],
                })
            })
            .collect())
    }

    /// Every execution uses the coding agent’s chat protocol.
    pub(super) async fn chat_plan(
        &self,
        workspace: &Workspace,
        resume: Option<&str>,
        launch: &mut Launch,
    ) -> Result<Value> {
        let s = self.s;
        let adapter = self.provider.adapter();
        if !self.run["chatExecution"].is_object() {
            return Err(Error::conflict(
                "This conversation has no saved input to run.",
            ));
        }
        private_dir(&self.directory.join("chat-input")).await?;
        if self.run["environmentExpiredAt"].is_number() {
            let run_id = self.id.clone();
            let attachments = s
                .store
                .read(move |db| {
                    let Some(chat) = crate::chats::chat_for_run(db, &run_id)? else {
                        return Ok(Vec::new());
                    };
                    db.attachments(text(&chat, "id"))?
                        .into_iter()
                        .map(|a| serde_json::to_value(a).map_err(Into::into))
                        .collect::<crate::error::Result<Vec<_>>>()
                })
                .await?;
            s.prepare_chat_files(&self.id, &attachments.into()).await?;
        }
        s.prepare_chat_files(&self.id, &self.run["chatExecution"]["attachments"])
            .await?;
        let run = crate::plugins::agents::with_instructions(s, self.run).await;
        let mut chat = run_output::chat_plan(
            &run,
            &workspace.prepared,
            &self.directory,
            &workspace.mcp,
            resume,
        )?;
        chat["runId"] = self.id.clone().into();
        chat["attemptId"] = self.attempt_id.clone().into();
        adapter
            .configure_plan(s, &mut chat, self.account.as_ref())
            .await?;
        launch.binary = current_exe()?;
        launch.args = vec!["chat".into(), adapter.binary(&s.config).to_owned()];
        launch.prompt = chat.to_string();
        Ok(chat)
    }

    /// Places the run on a node and routes the launch through its isolated runner.
    pub(super) async fn prepare_runner(
        &mut self,
        workspace: &mut Workspace,
        resume: Option<&str>,
        chat: Value,
        launch: &mut Launch,
    ) -> Result<()> {
        let s = self.s;
        let mut timing = Operation::new("runner_prepare", &self.id, "placement");
        let (runner, placement) = match self.reservation.take() {
            Some(reservation) => reservation,
            None => {
                let runner = crate::config::id();
                let mut admission = s.store.run(&self.id).await?;
                admission["environmentAdmissionRevision"] = self.admission_revision.clone();
                let placement = placement::reserve(s, &admission, &runner).await?;
                (runner, placement)
            }
        };
        self.reservation = Some((runner.clone(), placement.clone()));
        timing.next("materialize");
        if !self.run["environmentExpiredAt"].is_null() {
            crate::conversation_deletion::require_retirement_protocol(
                s,
                text(&placement, "nodeId"),
            )
            .await
            .map_err(|error| {
                Error::unavailable(format!("{} {}", placement::NO_CAPACITY, error.message))
            })?;
        }
        workspace.authorization.recheck().await?;
        placement::materialize(s, &runner).await?;
        let node_id = placement["nodeId"].as_str().map(str::to_owned);
        let runtime_id = placement["runtimeId"].clone();
        let state = s.store.run_state(&self.id).await?;
        let publication = workspace.authorization.clone();
        let admitted_run = self.id.clone();
        let admitted_attempt = runner.clone();
        let admitted_revision = self.admission_revision.clone();
        self.checkpoint
            .update_and_publish(
                |c| {
                    c.bind_paid_admission(&runner, &state);
                    c.turn_pending = None;
                    c.recovery_deadline_ms = None;
                    c.terminal_code = None;
                    c.provider_failed = None;
                    c.node_id = Some(node_id);
                    c.runtime_id = Some(Some(runtime_id));
                },
                move |db| {
                    publication.authorize_in(db)?;
                    placement::publish_admission(
                        db,
                        &admitted_run,
                        &admitted_attempt,
                        &admitted_revision,
                    )
                },
            )
            .await?;
        // The persisted checkpoint now owns fencing/release on subsequent errors.
        self.reservation = None;
        drop(self.preparation.take());
        s.worker.notify();
        timing.next("plan");
        self.write_runner_plan(workspace, &runner, &placement, resume, chat, launch)
            .await?;
        timing.next("remote_prepare");
        let runner_url = nodes::transport::url(s, &self.id).await?;
        if placement["nodeId"] != nodes::LOCAL_NODE_ID {
            self.prepare_remote(
                &runner_url,
                &runner,
                text(&placement, "nodeId"),
                &workspace.authorization,
            )
            .await?;
        }
        workspace.authorization.recheck().await?;
        timing.next("placement_commit");
        let publication = workspace.authorization.clone();
        let run_id = self.id.clone();
        let revision = self.admission_revision.clone();
        let changes = crate::nodes::run_placement::PlacementChanges {
            node_id: Some(serde_json::from_value(placement["nodeId"].clone())?),
            resources: Some(serde_json::from_value(placement["resources"].clone())?),
            node_state: Some(None),
            movement_error: Some(None),
            ..Default::default()
        };
        s.store
            .transaction(move |db| {
                let current = db
                    .run(&run_id)?
                    .ok_or_else(|| Error::not_found("Run not found"))?;
                crate::environment_expiration::require_admission_revision(&current, &revision)?;
                publication.authorize_in(db)?;
                db.update_run_placement(&run_id, changes)
            })
            .await?;
        let runner_token = execution::secret(&s.config.data_dir, "runner-secret").await?;
        workspace.env.insert("RUNNER_URL".into(), runner_url);
        workspace.env.insert("RUNNER_TOKEN".into(), runner_token);
        let plan: Value = serde_json::from_slice(
            &tokio::fs::read(
                s.config
                    .data_dir
                    .join("runner-plans")
                    .join(format!("{runner}.json")),
            )
            .await?,
        )?;
        if plan["environmentProtocol"] == 1 {
            workspace
                .env
                .insert("CAIRN_PROVIDER_TURN".into(), self.turn.clone());
        } else {
            workspace.env.remove("CAIRN_PROVIDER_TURN");
        }
        launch.binary = current_exe()?;
        launch.args = vec!["runner-client".into(), runner];
        timing.finish();
        Ok(())
    }

    pub(super) async fn write_runner_plan(
        &self,
        workspace: &Workspace,
        runner: &str,
        placement: &Value,
        resume: Option<&str>,
        mut chat: Value,
        launch: &Launch,
    ) -> Result<()> {
        let s = self.s;
        let prepared = &workspace.prepared;
        let plans = s.config.data_dir.join("runner-plans");
        private_dir(&plans).await?;
        let mut imports = prepared["mounts"].as_array().unwrap().clone();
        imports.push(json!({
            "source": self.directory.join("chat-input"),
            "target": "/run/cairn-chat",
            "readOnly": true,
        }));
        let mut context = crate::plugins::agents::with_instructions(s, self.run).await;
        context["snapshot"]["skills"] = prepared["skills"].clone();
        context["nativeMode"] = placement["nativeMode"].clone();
        if prepared["backend"] == "native" {
            run_output::native_chat_privileges(&mut chat, &context, &placement["nativeMode"]);
        }
        let prompt = if resume.is_some() {
            launch.prompt.clone()
        } else {
            run_output::prompt(&context, false)
        };
        let prompt = if prepared["backend"] == "native" {
            format!(
                "{}\n\n{prompt}",
                run_output::native_privileges(&placement["nativeMode"])
            )
        } else {
            prompt
        };
        let storage_node = text(placement, "nodeId");
        let node_storage = if prepared["backend"] == "native" {
            let (run, attempt, placement) = (self.id.clone(), runner.to_owned(), placement.clone());
            s.store
                .transaction(move |db| {
                    nodes::native_mode::acknowledge_plan(db, &run, &attempt, &placement)
                })
                .await?
        } else {
            s.store
                .node_boundary(storage_node)
                .await?
                .unwrap_or_default()["storage"]
                .clone()
        };
        let storage_policy = crate::storage::policy::for_node(&node_storage)?;
        s.store
            .update_run_boundary(&self.id, json!({ "storage": { "mode": "on-demand" } }))
            .await?;
        placement::renew_local(s, &self.id).await?;
        let resume_workspace = resume.is_some()
            || self
                .checkpoint
                .read(|saved| saved.workspace_initialized == Some(true))
                .await;
        let supports_environment = super::super::held::supported(s, self.run).await?;
        // Existing fixtures and older controllers keep the single-turn path.
        // New controllers explicitly advertise their environment protocol.
        let plan = RunnerPlan {
            id: runner,
            run_id: &self.id,
            resume_workspace,
            args: &launch.args,
            cwd: &prepared["cwd"],
            project_root: &prepared["projectRoot"],
            prompt,
            imports,
            expires: crate::microvm::plan::execution_deadline(
                self.checkpoint.deadline(),
                supports_environment,
                now(),
            ),
            sandbox: policy(&self.run["snapshot"]["agent"])["sandbox"].clone(),
            mcp_env: &workspace.mcp["env"],
            resources: &placement["resources"],
            node_lease_required: true,
            native_mode: placement["nativeMode"].clone(),
            backend: &prepared["backend"],
            runtime_id: &placement["runtimeId"],
            storage: RunnerStorage {
                owner: &self.id,
                policy: storage_policy,
            },
            chat,
            environment_protocol: supports_environment.then_some(1),
            turn_id: supports_environment.then_some(self.turn.as_str()),
        };
        workspace.authorization.recheck().await?;
        atomic_write(
            &plans.join(format!("{runner}.json")),
            &serde_json::to_vec(&plan)?,
        )
        .await
    }

    pub(super) async fn prepare_remote(
        &self,
        runner_url: &str,
        runner: &str,
        node_id: &str,
        authorization: &crate::plugins::run_credentials::Authorization,
    ) -> Result<()> {
        let s = self.s;
        let secret = execution::secret(&s.config.data_dir, "runner-secret").await?;
        authorization.recheck().await?;
        let response = s
            .http
            .post(format!("{runner_url}/prepare/{runner}"))
            .bearer_auth(secret)
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .map_err(|_| Error::unavailable("Remote workspace preparation interrupted."))?;
        authorization.recheck().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            if status == 504 {
                return Err(Error::unavailable(
                    "Remote workspace preparation timed out after 280 seconds; source files are preserved.",
                ));
            }
            return Err(Error::new(
                status,
                format!(
                    "Remote workspace preparation failed (HTTP {status}); source files are preserved.",
                ),
            ));
        }
        let message = format!("Executing on node {node_id}");
        s.store
            .event(&self.id, &EventKind::Status, &message, None)
            .await
    }

    /// Launches the supervised process and records its output until it exits.
    pub(super) async fn supervise(
        &mut self,
        workspace: &Workspace,
        launch: Launch,
        log_total: &mut usize,
    ) -> Result<Exit> {
        let attempt = output::Attempt {
            previously_launched: self.checkpoint.read(RunCheckpoint::launched).await,
            managed_antigravity: self.provider == Provider::Antigravity
                && workspace.env.contains_key("CAIRN_PROVIDER_TURN"),
        };
        workspace.authorization.recheck().await?;
        let mut child = Supervised::spawn(
            &launch.binary,
            &launch.args,
            &workspace.env,
            workspace.cwd(),
        )
        .await?;
        let identity = recovery::process_identity(child.child.id().unwrap()).await?;
        let launched_turn = launched_turn(self.run);
        let execution_patch = self.run["chatExecution"].is_object().then(|| {
            self.run["chatExecution"]["recovery"] = true.into();
            json!({ "chatExecution": self.run["chatExecution"] })
        });
        let run_id = self.id.clone();
        self.checkpoint
            .update_and_publish(
                |c| {
                    c.progress = Some(super::super::progress::Progress::launched(
                        self.attempt_id.clone(),
                        c.runner_id().map(str::to_owned),
                        identity.clone(),
                        crate::waits::execution_id(self.run)
                            .as_str()
                            .map(str::to_owned),
                        now(),
                    ));
                    c.process = Some(identity);
                    c.retained_completion = None;
                    c.turn_pending = None;
                    c.recovery_deadline_ms = None;
                    c.record_launch(launched_turn.clone());
                    c.start_rejected = Some(false);
                    c.output_interrupted = None;
                    c.provider_failure_attempt = None;
                    c.reset_finalization_observation();
                    c.retry_cause = Some(None);
                    c.completed = Some(false);
                    c.terminal_code = None;
                    c.last_error = Some(None);
                },
                move |db| {
                    if let Some(patch) = execution_patch {
                        db.update_run_boundary(&run_id, &patch)?;
                    }
                    Ok(())
                },
            )
            .await?;
        if self.stopping() {
            child.stop().await;
            return Err(Error::conflict("Execution stopped before launch."));
        }
        if let Err(error) = workspace.authorization.recheck().await {
            child.stop().await;
            return Err(error);
        }
        let stdout = child.child.stdout.take().unwrap();
        let stderr = child.child.stderr.take().unwrap();
        let (telemetry, reports) = output::Publisher::channel(&self.id, &self.attempt_id);
        let (tx, mut events) = mpsc::channel(32);
        let out = AbortOnDropHandle::new(tokio::spawn(output::read_output(
            stdout,
            false,
            tx.clone(),
            self.sensitive.snapshot().as_ref().clone(),
        )));
        let observed = self.observation.as_ref().is_some_and(Observation::active);
        let err = AbortOnDropHandle::new(tokio::spawn(async move {
            if observed {
                output::read_agent_diagnostics(stderr, tx, telemetry).await;
            } else {
                output::read_output(stderr, true, tx, Vec::new()).await;
            }
        }));
        if let Err(error) = child.start(launch.prompt).await {
            self.revoke_output_scope();
            child.stop().await;
            close_output_readers(out, err, true).await;
            return Err(error);
        }
        let span = tracing::info_span!(target: "cairn_performance", "agent_attempt", run_id = self.id, attempt_id = self.attempt_id);
        let result = Box::pin(self.wait(
            &mut child,
            &mut events,
            log_total,
            attempt,
            observed.then_some(reports),
        ))
        .instrument(span)
        .await;
        if result.is_err() {
            self.revoke_output_scope();
            child.stop().await;
        }
        close_output_readers(out, err, result.is_err()).await;
        result
    }

    pub(super) async fn close_account_output(&mut self, broker: Option<broker::Broker>) {
        self.revoke_output_scope();
        if let Some(broker) = broker {
            broker.close().await;
        }
        self.observation.take();
    }

    pub(super) fn revoke_output_scope(&self) {
        if let Some(observation) = &self.observation {
            observation.revoke();
        }
    }

    pub(super) async fn wait(
        &mut self,
        child: &mut Supervised,
        events: &mut mpsc::Receiver<output::Output>,
        log_total: &mut usize,
        attempt: output::Attempt,
        mut reports: Option<output::Reports>,
    ) -> Result<Exit> {
        let s = self.s;
        let cancel = self.cancel;
        let mut exit = Exit::default();
        let mut open = true;
        let mut activity = Activity::new(&self.id, &self.attempt_id, "worker", self.provider);
        let mut heartbeat = crate::performance::heartbeat();
        let mut timeout = tokio::time::interval(Duration::from_millis(250));
        timeout.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while exit.code.is_none() || open || reports.is_some() {
            let running = exit.code.is_none();
            tokio::select! {
                () = cancel.cancelled(), if running => {
                    self.revoke_output_scope();
                    child.stop().await;
                    exit.code = Some(STOPPED);
                }
                () = s.shutdown.cancelled(), if running => {
                    self.revoke_output_scope();
                    child.stop().await;
                    exit.code = Some(STOPPED);
                }
                _ = timeout.tick(), if running => {
                    // The controller owns confirmed suspension timing. An unavailable
                    // observation must not turn an administratively paused VM into a timeout.
                    if self.checkpoint.expired()
                        && self.checkpoint.refresh_pause(s).await.is_ok()
                        && self.checkpoint.expired()
                    {
                        exit.timed_out = true;
                        self.revoke_output_scope();
                        child.stop().await;
                        exit.code = Some(STOPPED);
                    }
                }
                code = child.child.wait(), if running => {
                    exit.code = Some(code?.code().unwrap_or(STOPPED));
                }
                _ = heartbeat.tick() => activity.heartbeat("receive_output"),
                changed = async { reports.as_mut().unwrap().changed().await }, if reports.is_some() => {
                    if changed {
                        // Credential generations already join the shared history
                        // synchronously before persistence/delivery. There is no
                        // per-metric Store lookup, output slot or detached reporter.
                        reports.as_mut().unwrap().publish_latest();
                    } else { reports = None; }
                }
                event = events.recv(), if open => match event {
                    Some(output) => {
                        let queue_ms = output.received.elapsed().as_millis() as u64;
                        if queue_ms >= 100 {
                            tracing::info!(target: "cairn_performance", operation = "agent_output", run_id = self.id, attempt_id = self.attempt_id, phase = "queue", elapsed_ms = queue_ms);
                        }
                        if output.kind == output::OutputKind::Stdout {
                            activity.output();
                            #[cfg(test)]
                            if let Some(probe) = &self.output_probe {
                                probe.canonical();
                            }
                        }
                        crate::performance::wait("refresh_redactions", self.refresh_redactions()).await?;
                        #[cfg(test)]
                        if let Some(probe) = &self.output_probe {
                            probe.before_record(&output)?;
                        }
                        exit.exhausted |= crate::performance::wait("persist_output", output::record(
                            s,
                            &output,
                            self.checkpoint,
                            attempt,
                            &self.sensitive.snapshot(),
                            log_total,
                            &mut activity,
                        )).await?;
                        #[cfg(test)]
                        if let Some(probe) = &self.output_probe {
                            probe.recorded(&output);
                        }
                    }
                    None => open = false,
                },
            }
        }
        activity.finish();
        Ok(exit)
    }

    /// Account credentials may rotate while the process runs.
    pub(super) async fn refresh_redactions(&mut self) -> Result<()> {
        #[cfg(test)]
        if let Some(probe) = &self.output_probe {
            probe.refresh().await;
            if probe.supplies_flood_marker() {
                self.sensitive.extend(["rotated-test-secret".into()]);
            }
        }
        let Some(account) = self.account.as_ref() else {
            return Ok(());
        };
        self.sensitive
            .extend(self.s.accounts.redactions(self.s, account).await?);
        Ok(())
    }

    pub(super) fn may_switch_account(&self, exit: &Exit) -> bool {
        exit.exhausted
            && exit.code != Some(0)
            && self.account.is_some()
            && !self.stopping()
            && !exit.timed_out
    }

    /// Parks the exhausted account and waits for another one to continue the session.
    pub(super) async fn switch_account(&mut self, workspace: &Workspace) -> Result<()> {
        let s = self.s;
        let label = self.provider.label();
        let previous = self.account.take().unwrap();
        let model = text(&self.run["snapshot"]["agent"], "model").to_owned();
        s.accounts
            .exhausted(s, &previous.account_id, &model)
            .await?;
        s.accounts.release(s, &previous).await?;
        let patch = crate::runs::RunChanges {
            account_wait_reason: Some(Some(
                (format!("Usage exhausted. Waiting for an available {label} account to resume."))
                    .to_owned(),
            )),
            account_id: Some(None),
            account_name: Some(None),
            ..Default::default()
        };
        let id = self.id.clone();
        s.store.transaction(move |db| {
            db.change_run(&id, patch)?;
            db.event(&id, &EventKind::Status, "Usage exhausted. Saving this session and restoring capacity or switching accounts.", None)
        }).await?;
        s.accounts.refresh(s, &previous.account_id).await?;
        self.wait_for_account(&model).await?;
        let Some(account) = self.account.as_mut() else {
            return Ok(());
        };
        self.provider
            .adapter()
            .rotate_account(s, account, &previous.home, workspace.isolated())
            .await?;
        self.sensitive
            .extend(s.accounts.redactions(s, account).await?);
        let name = serde_json::to_value(s.accounts.account(s, &account.account_id).await?.name)?;
        let patch = crate::runs::RunChanges {
            account_wait_reason: Some(None),
            account_id: Some(Some((account.account_id).to_owned())),
            account_name: Some(name.as_str().map(str::to_owned)),
            ..Default::default()
        };
        let message = format!(
            "Resuming saved session with {label} account: {}",
            name.as_str().unwrap_or("")
        );
        let id = self.id.clone();
        s.store
            .transaction(move |db| {
                db.change_run(&id, patch)?;
                db.event(&id, &EventKind::Status, &message, None)
            })
            .await
    }

    pub(super) async fn wait_for_account(&mut self, model: &str) -> Result<()> {
        let s = self.s;
        while self.account.is_none() && !self.stopping() && !self.checkpoint.expired() {
            if self
                .checkpoint
                .snapshot()
                .await
                .held_environment
                .is_some_and(|held| now() >= held.expires_at)
            {
                return Err(Error::coded(
                    409,
                    HELD_DEADLINE_CODE,
                    super::super::held::DEADLINE_MESSAGE,
                ));
            }
            let speed = text(&self.run["snapshot"]["agent"], "speed");
            match s
                .accounts
                .acquire_for(s, &self.id, self.provider, model, speed)
                .await
            {
                Ok(account) => {
                    *self.account = account;
                    return Ok(());
                }
                Err(error) if error.is_conflict() => {
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(1)) => {}
                        () = self.cancel.cancelled() => {}
                        () = s.shutdown.cancelled() => {}
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub(super) fn retained_completion<'saved>(
        &self,
        saved: &'saved RunCheckpoint,
        exit: &Exit,
    ) -> Option<&'saved RetainedCompletion> {
        saved
            .held_environment
            .as_ref()
            .and_then(|held| {
                saved.confirmed_retained_completion(
                    &held.attempt,
                    self.run["chatExecution"]["messageId"].as_str(),
                )
            })
            .filter(|_| exit.code == Some(0) && !exit.timed_out)
    }

    pub(super) async fn output_summary(&self, workspace: &Workspace) -> Option<String> {
        // A result is read before the turn receipt is saved. A pipe in the
        // output directory must not block completion while awaiting a writer.
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(workspace.output())
            .await
            .ok()?;
        if !file.metadata().await.ok()?.is_file() {
            return None;
        }
        let bytes = crate::process::read_bounded(file, SUMMARY_LIMIT)
            .await
            .ok()?;
        let summary = String::from_utf8(bytes).ok()?;
        (!summary.is_empty()).then(|| self.redact_summary(&summary))
    }

    pub(super) fn redact_summary(&self, summary: &str) -> String {
        run_output::redact(summary, &self.sensitive.snapshot())
            .chars()
            .take(SUMMARY_LIMIT)
            .collect()
    }

    pub(super) async fn finish(
        &self,
        workspace: &Workspace,
        exit: &Exit,
        saved: &RunCheckpoint,
        has_session: bool,
    ) -> Result<()> {
        let s = self.s;
        let retained_completion = self.retained_completion(saved, exit);
        let status = if self.cancel.is_cancelled() {
            RunStatus::Cancelled
        } else if exit.timed_out
            || (retained_completion.is_none() && self.checkpoint.expired())
            || saved.output_error().is_some()
            || saved.provider_failed()
        {
            RunStatus::Failed
        } else if exit.code == Some(0)
            || (s.shutdown.is_cancelled() && saved.completed() && !saved.managed())
        {
            RunStatus::Succeeded
        } else {
            RunStatus::Failed
        };
        if status == RunStatus::Failed
            && !exit.timed_out
            && !self.checkpoint.expired()
            && saved.output_error().is_none()
            && saved.finalization_error().is_none()
            && !self.stopping()
            && !saved.completed()
            && let Some(cause) = saved.retry_cause.as_ref().and_then(Option::as_ref)
            && (has_session || cause.startup)
        {
            // The provider confirmed this turn failed. Recovery is scheduled by
            // the worker after the previous execution is fenced, not replayed here.
            return Err(Error::conflict(cause.message.clone()));
        }
        let error = self.failure_message(status, exit, saved);
        let summary = if status != RunStatus::Succeeded {
            self.redact_summary(&error)
        } else if let Some(summary) =
            retained_completion.and_then(|receipt| receipt.output_summary.as_deref())
        {
            summary.to_owned()
        } else {
            self.output_summary(workspace)
                .await
                .unwrap_or_else(|| self.redact_summary(saved.last_message()))
        };
        let resume_available = (status != RunStatus::Succeeded
            || self.run["chatExecution"].is_object())
            && has_session;
        let error =
            (!error.is_empty()).then(|| run_output::redact(&error, &self.sensitive.snapshot()));
        // A failed run's final status says why it ended when no earlier event did.
        let details = error
            .as_ref()
            .map(|message| json!({ "error": { "message": message } }));
        let mut patch = crate::runs::RunChanges {
            status: Some(status),
            finished_at: Some(Some(if status == RunStatus::Succeeded {
                retained_completion.map_or_else(now, |receipt| receipt.finished_at)
            } else {
                now()
            })),
            resume_available: Some(resume_available),
            summary: Some((summary).to_owned()),
            error: Some(error.clone()),
            boundary: Some(serde_json::json!({})),
            ..Default::default()
        };
        if status == RunStatus::Succeeded {
            patch.retry = Some(None);
        }
        let id = self.id.clone();
        let deadline = self.checkpoint.deadline();
        // The fenced workload already stopped. Time spent confirming optional
        // cleanup retirement must not replace its recorded failure.
        let report_expiration =
            exit.timed_out || (retained_completion.is_none() && !saved.native_cleanup_failure());
        s.store
            .transaction(move |db| {
                let current = db
                    .run(&id)?
                    .ok_or_else(|| Error::not_found("Run not found"))?;
                let expired =
                    report_expiration && deadline.is_some_and(|deadline| now() >= deadline);
                let status = if !current["cancelRequestedAt"].is_null() {
                    RunStatus::Cancelled
                } else if expired {
                    let message = "Run exceeded its time limit.";
                    patch.error = Some(Some(message.into()));
                    patch.summary = Some(message.into());
                    patch.wakeup = Some(None);
                    RunStatus::Failed
                } else {
                    status
                };
                patch.status = Some(status);
                if status == RunStatus::Succeeded {
                    crate::chats::keep_reply_after_compaction(&current, &mut patch)?;
                }
                db.change_run(&id, patch)?;
                let details = if status == RunStatus::Failed && expired {
                    Some(json!({ "error": { "message": "Run exceeded its time limit." } }))
                } else {
                    details.filter(|_| status == RunStatus::Failed)
                };
                db.event(&id, &EventKind::Status, status.as_str(), details.as_ref())
            })
            .await
    }

    pub(super) fn failure_message(
        &self,
        status: RunStatus,
        exit: &Exit,
        saved: &RunCheckpoint,
    ) -> String {
        if status == RunStatus::Succeeded {
            return String::new();
        }
        if self.cancel.is_cancelled() {
            return "Run stopped. You can resume this conversation.".into();
        }
        if exit.timed_out || (self.checkpoint.expired() && !saved.native_cleanup_failure()) {
            return "Run exceeded its time limit.".into();
        }
        if let Some(error) = saved.output_error() {
            return error.to_owned();
        }
        if let Some(error) = saved.finalization_error() {
            return error.to_owned();
        }
        if !saved.last_error().is_empty() {
            return saved.last_error().to_owned();
        }
        // A conversation shows what continues it: its returned message or its held queue (ADR-0091).
        let next = "";
        match exit.code {
            Some(crate::runner::CONTROLLER_FAILED) => {
                format!(
                    "{} failed while running this attempt.{next}",
                    controller_name(self.run)
                )
            }
            Some(code) => format!("Process exited with status {code}.{next}"),
            None => format!("Process stopped before finishing.{next}"),
        }
    }
}

/// Join on ordinary EOF; abort AND join on failure. Abort-on-drop ownership also
/// covers cancellation of this cleanup future itself, unlike bare JoinHandles.
pub(super) async fn close_output_readers(
    out: AbortOnDropHandle<()>,
    err: AbortOnDropHandle<()>,
    abort: bool,
) {
    if abort {
        out.abort();
        err.abort();
    }
    let _ = out.await;
    let _ = err.await;
}

/// The turn a launch serves, recorded so a failure can tell whether the agent may have its
/// message (ADR-0091). A launch without a chat message, such as a mission's, records an
/// empty turn: a later chat message was not delivered by it.
pub(super) fn launched_turn(run: &Value) -> Option<String> {
    Some(
        run["chatExecution"]["messageId"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}

pub(super) fn has_live_environment(
    saved: &RunCheckpoint,
    retained: bool,
    env: &Environment,
) -> bool {
    saved.held_environment.is_some() || retained || env.contains_key("CAIRN_PROVIDER_TURN")
}

pub(super) fn can_reuse_environment(provider: Provider, run: &Value) -> Result<bool> {
    Ok(provider == Provider::of_run(run)? && run["sessionId"].is_string())
}

pub(super) fn controller_name(run: &Value) -> &'static str {
    if crate::native::policy::native(&run["snapshot"]["agent"]) {
        "Native controller"
    } else {
        "VM controller"
    }
}

pub(super) fn current_exe() -> Result<String> {
    Ok(std::env::current_exe()?.to_string_lossy().into_owned())
}
