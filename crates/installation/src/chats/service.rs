use super::*;

impl Service {
    pub async fn chat_list(&self) -> Result<Vec<Value>> {
        self.store.read(|db| list(db)).await
    }

    pub async fn chat_detail(&self, id: &str) -> Result<Value> {
        let id = id.to_owned();
        self.store.read(move |db| detail(db, &id)).await
    }

    pub async fn chat_create(&self, input: Value) -> Result<Value> {
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_agent(
                self,
                input["agentId"]
                    .as_str()
                    .unwrap_or(crate::config::MAIN_AGENT_ID),
            )
            .await?;
        }
        self.store
            .transaction(move |db| create_as(db, input, &origin))
            .await
    }

    pub async fn chat_send(&self, id: &str, input: Value) -> Result<Value> {
        let values = parse_message(input)?;
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_chat(self, id).await?;
        }
        let id = id.to_owned();
        let result = self
            .store
            .transaction(move |db| send(db, &id, values, None, &origin))
            .await?;
        self.worker.notify();
        Ok(result)
    }

    pub async fn chat_edit(&self, id: &str, message: &str, input: Option<Value>) -> Result<Value> {
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_chat(self, id).await?;
        }
        let (id, message) = (id.to_owned(), message.to_owned());
        let values = input
            .map(|mut input| {
                input["id"] = message.clone().into();
                parse_edit(input)
            })
            .transpose()?;
        let result = self
            .store
            .transaction(move |db| edit_as(db, &id, &message, values, &origin))
            .await?;
        self.worker.notify();
        Ok(result)
    }

    /// Compacts the conversation's native context while it is idle.
    pub(crate) async fn chat_compact_live(
        &self,
        id: &str,
        authorize: crate::authorization::Authorization,
    ) -> Result<Value> {
        authorize.recheck().await?;
        let origin = crate::native::policy::origin();
        if origin.known() {
            crate::native::policy::refresh_chat(self, id).await?;
        }
        let id = id.to_owned();

        // A succeeded run's worker may still be saving its checkpoint: the compaction resets
        // it, so it waits for that worker to release the run, like resuming does.
        let active = self.worker.active.lock().await;
        authorize.recheck().await?;
        let run_id = self
            .store
            .read({
                let id = id.clone();
                move |db| Ok(conversation(db, &id)?.run_id.unwrap_or_default())
            })
            .await?;
        if active.contains_key(&run_id) {
            return Err(Error::conflict(
                "Wait for the current turn to finish before compacting.",
            ));
        }
        authorize.recheck().await?;
        let result = self
            .store
            .transaction(move |db| {
                authorize.authorize(db)?;
                compact(db, &id, &origin)
            })
            .await?;
        drop(active);
        self.worker.notify();
        Ok(result)
    }

    pub async fn chat_pause(&self, id: &str, paused: bool) -> Result<Value> {
        let id = id.to_owned();
        let result = self
            .store
            .write(move |db| pause_in(db, &id, paused))
            .await?;
        self.worker.notify();
        Ok(result)
    }

    pub async fn chat_acknowledge(&self, run_id: &str, message_id: &str) -> Result<()> {
        let (run_id, message_id) = (run_id.to_owned(), message_id.to_owned());
        self.store
            .transaction(move |db| acknowledge(db, &run_id, &message_id))
            .await
    }

    pub async fn chat_tick(&self, active: &HashSet<String>) -> Result<()> {
        self.expire_attachment_uploads().await?;
        let side_open = self.side_question_chats().await?;
        let expiring = crate::environment_expiration::pending_runs(self).await?;
        for chat in self.store.conversations().await? {
            // Before anything holds the conversation (trash, cleaning, a pending agent
            // switch): a stopped turn's side questions fail now.
            if side_open.contains(&chat.id) {
                let chat_id = chat.id.clone();
                let expiring = self
                    .store
                    .read(move |db| side_questions::expiring(db, &chat_id))
                    .await?;
                if expiring {
                    let chat_id = chat.id.clone();
                    self.store
                        .transaction(move |db| side_questions::expire(db, &chat_id))
                        .await?;
                }
            }
            if chat.lifecycle != cairn_contracts::ConversationLifecycle::Active
                || chat
                    .run_id
                    .as_ref()
                    .is_some_and(|run| expiring.contains(run))
            {
                continue;
            }
            if self
                .store
                .read({
                    let id = chat.id.clone();
                    move |db| db.engine_switch(&id)
                })
                .await?
                .is_some_and(|value| value.pending)
            {
                continue;
            }
            let run = match chat.run_id.as_deref() {
                Some(run) => Some(self.store.run(run).await?),
                None => None,
            };
            if let Some(run) = run.as_ref().filter(|run| run_is_active(run)) {
                // Messages sent while compacting wait for the next turn instead of steering.
                // A queued run has no process to read steering yet: it is claimed once one
                // runs, so a turn that never ran gives it back with its message.
                if !is_compaction(run) && run["status"] != RunStatus::Queued {
                    self.publish_steering(&chat, run, side_open.contains(&chat.id))
                        .await?;
                }
                continue;
            }
            let busy = run.as_ref().is_some_and(|run| {
                active.contains(text(run, "id")) || run["recoveryPending"] == true
            });
            if busy {
                continue;
            }
            let id = chat.id.clone();
            if let Some(run) = run.as_ref().filter(|run| needs_attention(&chat, Some(run))) {
                self.chat_cancel_wait(&id).await?;
                // Read first: an idle failed conversation needs no writer on each tick.
                let (chat_id, ended) = (id.clone(), run.clone());
                let settling = self
                    .store
                    .read(move |db| Ok(db.has_pending_messages(&chat_id)? && !settled(db, &ended)?))
                    .await?;
                if settling {
                    // The next tick dispatches what remains, unless a pause holds it.
                    let (chat_id, ended) = (id.clone(), run.clone());
                    self.store
                        .transaction(move |db| settle_turn(db, &chat_id, &ended))
                        .await?;
                    // No turn runs now: steering returned to its author must not wait in the
                    // inbox for the next turn's process.
                    let directory = self
                        .config
                        .data_dir
                        .join("runs")
                        .join(text(run, "id"))
                        .join("chat-input");
                    if tokio::fs::try_exists(&directory).await.unwrap_or(false) {
                        side_questions::publish_inbox(
                            &self.store,
                            text(run, "id"),
                            "",
                            &directory,
                            Vec::new(),
                        )
                        .await?;
                    }
                    continue;
                }
                if chat.paused {
                    continue;
                }
            }
            // A scheduled wait continues the current task before queued follow-ups.
            let (waiting_chat, waiting_run) = (chat.clone(), run.clone());
            let waiting = self
                .store
                .read(move |db| continuation::holds_queue(db, &waiting_chat, waiting_run.as_ref()))
                .await?;
            let chat_id = id.clone();
            let message = if waiting {
                None
            } else {
                self.store
                    .transaction(move |db| next_queued(db, &chat_id))
                    .await?
            };
            let message = match message {
                Some(message) => message,
                None => {
                    if chat.paused {
                        continue;
                    }
                    let chat = chat.clone();
                    let run = run.clone();
                    let Some(message) = self
                        .store
                        .read(move |db| continuation::message(db, &chat, run.as_ref(), now()))
                        .await?
                    else {
                        continue;
                    };
                    message
                }
            };
            if let Err(error) = self.chat_prepare(chat.clone(), run, message).await {
                self.store
                    .transaction(move |db| preparation_failed(db, &chat, error))
                    .await?;
            }
        }
        Ok(())
    }

    /// Conversations with a side question their reply may still receive.
    pub(super) async fn side_question_chats(&self) -> Result<HashSet<String>> {
        self.store.read(|db| side_questions::open_chats(db)).await
    }

    /// Writes steering messages where the running agent turn reads them.
    /// `side_open`: the conversation has side questions its reply may still receive.
    pub(super) async fn publish_steering(
        &self,
        chat: &Conversation,
        run: &Value,
        side_open: bool,
    ) -> Result<()> {
        let chat_id = chat.id.clone();
        let paused = chat.paused;
        let claiming = run.clone();
        // Only a reply that announced side questions, with some to receive, reads them; a
        // later tick sees an announcement or a question made since this one read them.
        let announced = side_open && run["sideQuestions"] == true;
        let Some((mut steering, side_turn)) = self
            .store
            .transaction(move |db| {
                let Some(current) = steering_run(db, &claiming)? else {
                    return Ok(None);
                };
                let mut steering = claim_steering(db, &chat_id, paused, &current)?;
                // Side questions are not chat messages: the queue, pause and steering never
                // hold them, and the adapter answers them beside the turn. The run is read
                // again here: a launch since the tick read it withdrew its announcement.
                let mut side_turn = String::new();
                if announced {
                    steering.extend(side_questions::deliveries(db, &chat_id, &current)?);
                    side_turn = text(&current["chatExecution"], "messageId").to_owned();
                }
                Ok(Some((steering, side_turn)))
            })
            .await?
        else {
            return Ok(());
        };
        let skills = &run["snapshot"]["skills"];
        for message in &mut steering {
            let side =
                message["sideQuestion"].is_object() || message["sideQuestionCancelled"] == true;
            if side {
                continue;
            }
            self.prepare_chat_files(text(run, "id"), &message["attachments"])
                .await?;
            message["text"] = with_invoked_skills(text(message, "text"), skills).into();
        }
        // Publish a bounded prefix. Unacknowledged messages remain pending and
        // join a later batch once the adapter has received this one.
        let mut encoded_bytes = 2;
        for (index, message) in steering.iter().enumerate() {
            encoded_bytes += serde_json::to_vec(message)?.len() + usize::from(index != 0);
            if encoded_bytes > crate::chat_inbox::ADAPTER_LIMIT {
                steering.truncate(index);
                break;
            }
        }
        let directory = self
            .config
            .data_dir
            .join("runs")
            .join(chat.run_id.as_deref().unwrap_or(""))
            .join("chat-input");
        // Nor may this write restore side questions an attempt removed meanwhile, or hand
        // one turn's side questions to the next.
        side_questions::publish_inbox(
            &self.store,
            text(run, "id"),
            &side_turn,
            &directory,
            steering,
        )
        .await
    }

    pub(super) async fn chat_prepare(
        &self,
        chat: Conversation,
        run: Option<Value>,
        message: Value,
    ) -> Result<()> {
        let prompt = if message["questionId"].is_string() {
            // The structured response is carried by chatExecution, not by a
            // mission prompt constrained to the ordinary composer budget.
            "Continue from the questionnaire response."
        } else if text(&message, "text").is_empty() {
            "Review the attached files."
        } else {
            text(&message, "text")
        };
        let task = json!({
            "id": chat.id,
            "name": chat.title,
            "prompt": prompt,
            "agentId": chat.agent_id,
            "projectId": chat.project_id,
            "worktree": true,
        });
        let mut snapshot = self.snapshot(task, "chat").await?;
        snapshot["snapshot"]["delegation"] = crate::delegation::role(
            &serde_json::to_value(&chat)?,
            &snapshot["snapshot"]["agent"],
        );
        select_agent(&mut snapshot, run.as_ref(), &message)?;
        let provider = Provider::of_run(&snapshot)?;
        provider
            .adapter()
            .validate_agent(&snapshot["snapshot"]["agent"])?;
        if message["plan"] == true {
            require_plan_turns(provider)?;
        }
        let chat_id = chat.id.clone();
        self.store
            .transaction(move |db| launch_turn(db, &chat_id, run, message, snapshot))
            .await
    }
}
