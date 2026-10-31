use crate::{
    capabilities::ResumeContext,
    error::{Error, Result},
    service::Service,
    store::Db,
};
use cairn_contracts::*;
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) struct RunOperation {
    pub name: &'static str,
    scope: &'static str,
    description: &'static str,
}

pub(crate) const RUN_OPERATIONS: &[RunOperation] = &[
    RunOperation {
        name: "run_task",
        scope: "run",
        description: "Queue a new mission run with the mission's current settings and agent permissions. This creates a new run, unlike resume_run.",
    },
    RunOperation {
        name: "retry_run",
        scope: "run",
        description: "Create a new run of the original run's mission with current settings. This does not resume the saved session or reuse its workspace.",
    },
    RunOperation {
        name: "cancel_run",
        scope: "run",
        description: "Cancel queued, executing or waiting work, including pending recovery. Existing external effects are preserved.",
    },
    RunOperation {
        name: "cleanup_run",
        scope: "run",
        description: "Remove a clean managed worktree of a stopped run. Refuses retained private disks and workspaces with changes; resume those runs to preserve their work.",
    },
    RunOperation {
        name: "list_runs",
        scope: "read",
        description: "List recent run summaries with pagination, without bulky snapshots or logs.",
    },
    RunOperation {
        name: "get_run",
        scope: "read",
        description: "Read byte-bounded run metadata and an event page. Follow nextAfter while hasMore is true. Pass history to read_run_history for backwards windows. Read records marked truncated with read_run_content.",
    },
    RunOperation {
        name: "read_run_history",
        scope: "read",
        description: "Read a previous history window selected with a 256 KiB gzip budget, at most 1,000 events and a decoded-size ceiling. Tool responses retain bounded previews; read truncated events with read_run_content. Pass history from get_run and use oldest as the next before cursor while hasOlder is true. Reconnect when the history revision changes.",
    },
    RunOperation {
        name: "read_run_timeline",
        scope: "read",
        description: "Read the run's timeline, newest page first. Save the first page's changeCursor and pass it as snapshot with oldest as before for older pages. Pages can include newer revisions while the run continues. Then pass the saved cursor as changes and follow changeCursor while hasMoreChanges is true to catch up. Restart if retained history changes. A newer card revision replaces an older one. For a truncated card, pass its entryId and preview.snapshot as timelineSnapshot to read_run_content.",
    },
    RunOperation {
        name: "read_run_content",
        scope: "read",
        description: "Read complete run metadata, an event, or a timeline card by entryId in UTF-8 JSON chunks. Continue with nextOffset and sha256, restarting from zero if the content changes.",
    },
];

pub(crate) fn run_operation(name: &str) -> Option<&'static RunOperation> {
    RUN_OPERATIONS.iter().find(|op| op.name == name)
}

fn decode<T: serde::de::DeserializeOwned>(input: Value) -> Result<T> {
    serde_json::from_value(input).map_err(|e| Error::bad(e.to_string()))
}

impl RunOperation {
    pub(crate) fn descriptor(&self) -> Value {
        let input = match self.name {
            "run_task" => schema_for::<RunMissionInput>(Contract::Deserialize),
            "list_runs" => schema_for::<ListRunsInput>(Contract::Deserialize),
            "get_run" => schema_for::<GetRunInput>(Contract::Deserialize),
            "read_run_history" => schema_for::<ReadRunHistoryInput>(Contract::Deserialize),
            "read_run_timeline" => schema_for::<ReadRunTimelineInput>(Contract::Deserialize),
            "read_run_content" => schema_for::<ReadRunContentInput>(Contract::Deserialize),
            _ => schema_for::<RunInput>(Contract::Deserialize),
        };
        let output = match self.name {
            "cancel_run" => schema_for::<CancelRunOutput>(Contract::Serialize),
            "cleanup_run" => schema_for::<CleanupRunOutput>(Contract::Serialize),
            "list_runs" => schema_for::<Vec<Run>>(Contract::Serialize),
            "get_run" => schema_for::<RunHistoryOutput>(Contract::Serialize),
            "read_run_history" => schema_for::<RunHistoryWindowOutput>(Contract::Serialize),
            "read_run_timeline" => schema_for::<RunTimelineOutput>(Contract::Serialize),
            "read_run_content" => schema_for::<RunContentOutput>(Contract::Serialize),
            _ => schema_for::<Run>(Contract::Serialize),
        };
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": input,
            "outputSchema": output,
            "_meta": { "securitySchemes": [{ "type": "oauth2", "scopes": [self.scope] }] },
            "annotations": {
                "readOnlyHint": self.scope == "read",
                "destructiveHint": self.scope != "read",
                "openWorldHint": self.scope != "read",
            },
        })
    }

    pub(crate) async fn invoke(
        &self,
        s: &Arc<Service>,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        let human = matches!(&context, ResumeContext::Human(_));
        let bearer = match context {
            ResumeContext::Human(_) => None,
            ResumeContext::Public(scopes, _) => {
                if !scopes.iter().any(|scope| scope == self.scope) {
                    return Err(Error::forbidden(format!(
                        "The {} scope is required.",
                        self.scope
                    )));
                }
                None
            }
            ResumeContext::Installation(bearer) => {
                crate::installation_mcp::authorize(s, &bearer).await?;
                Some(bearer)
            }
        };
        let authorize = crate::installation_mcp::management_authorization(bearer.as_deref());
        match self.name {
            "run_task" => {
                let input: RunMissionInput = decode(input)?;
                self.enqueue(
                    s,
                    &input.task_id.to_string(),
                    if human { "manual" } else { "mcp" },
                    bearer,
                )
                .await
            }
            "retry_run" => {
                let input: RunInput = decode(input)?;
                let run = s.store.run_state(&input.run_id.to_string()).await?;
                self.enqueue(s, &run.task_id, "retry", bearer).await
            }
            "cancel_run" => {
                let input: RunInput = decode(input)?;
                let id = input.run_id.to_string();
                if let Some(bearer) = bearer {
                    s.worker.cancel_from_run(s, &id, &bearer).await?;
                } else {
                    s.worker.cancel(s, &id).await?;
                }
                Ok(serde_json::to_value(CancelRunOutput { cancelled: true })?)
            }
            "cleanup_run" => {
                let input: RunInput = decode(input)?;
                s.worker
                    .cleanup_authorized(s, &input.run_id.to_string(), authorize)
                    .await
            }
            "list_runs" => {
                let input: ListRunsInput = decode(input)?;
                input.validate().map_err(Error::bad)?;
                s.store
                    .read(move |db| {
                        let tx = rusqlite::Transaction::new_unchecked(
                            db.0,
                            rusqlite::TransactionBehavior::Deferred,
                        )?;
                        let db = Db(&tx);
                        authorize(&db)?;
                        let task = input.task_id.map(|id| id.to_string());
                        let runs = db.runs(
                            input.status.map(RunStatus::as_str),
                            task.as_deref(),
                            i64::from(input.limit),
                            input.offset,
                            false,
                        )?;
                        let runs: Vec<Run> = runs
                            .into_iter()
                            .map(serde_json::from_value)
                            .collect::<serde_json::Result<_>>()?;
                        Ok(serde_json::to_value(runs)?)
                    })
                    .await
            }
            "get_run" => {
                let input: GetRunInput = decode(input)?;
                if input.after < 0 {
                    return Err(Error::bad("Invalid event cursor."));
                }
                s.store
                    .read(move |db| {
                        let tx = rusqlite::Transaction::new_unchecked(
                            db.0,
                            rusqlite::TransactionBehavior::Deferred,
                        )?;
                        let db = Db(&tx);
                        authorize(&db)?;
                        crate::mcp_server::run_history(&db, &input.run_id.to_string(), input.after)
                    })
                    .await
            }
            "read_run_history" => {
                let input: ReadRunHistoryInput = decode(input)?;
                input.validate().map_err(Error::bad)?;
                s.store
                    .read(move |db| {
                        let tx = rusqlite::Transaction::new_unchecked(
                            db.0,
                            rusqlite::TransactionBehavior::Deferred,
                        )?;
                        let db = Db(&tx);
                        authorize(&db)?;
                        let run = input.run_id.to_string();
                        db.require_run(&run)?;
                        crate::conversation_lifecycle::require_active_run(&db, &run)?;
                        let page =
                            crate::live::history_page(&db, &run, input.before, &input.history)?;
                        Ok(serde_json::to_value(
                            crate::mcp_server::bounded_history_window(page)?,
                        )?)
                    })
                    .await
            }
            "read_run_timeline" => {
                let input: ReadRunTimelineInput = decode(input)?;
                input.validate().map_err(Error::bad)?;
                s.store
                    .read(move |db| {
                        let tx = rusqlite::Transaction::new_unchecked(
                            db.0,
                            rusqlite::TransactionBehavior::Deferred,
                        )?;
                        let db = Db(&tx);
                        authorize(&db)?;
                        let run = input.run_id.to_string();
                        db.require_run(&run)?;
                        crate::conversation_lifecycle::require_active_run(&db, &run)?;
                        Ok(serde_json::to_value(crate::mcp_server::run_timeline(
                            &db, &run, &input,
                        )?)?)
                    })
                    .await
            }
            "read_run_content" => {
                let input: ReadRunContentInput = decode(input)?;
                if input.offset < 0
                    || (input.event_id.is_some() && input.entry_id.is_some())
                    || (input.timeline_snapshot.is_some() && input.entry_id.is_none())
                    || input
                        .entry_id
                        .as_ref()
                        .is_some_and(|id| id.is_empty() || id.chars().count() > 1024)
                    || input
                        .event_id
                        .is_some_and(|id| !(1..=9_007_199_254_740_991).contains(&id))
                    || input.sha256.as_ref().is_some_and(|s| {
                        s.len() != 64
                            || !s
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    })
                {
                    return Err(Error::bad("Invalid content cursor or checksum."));
                }
                s.store
                    .read(move |db| {
                        let tx = rusqlite::Transaction::new_unchecked(
                            db.0,
                            rusqlite::TransactionBehavior::Deferred,
                        )?;
                        let db = Db(&tx);
                        authorize(&db)?;
                        let run = input.run_id.to_string();
                        db.require_run(&run)?;
                        crate::conversation_lifecycle::require_active_run(&db, &run)?;
                        crate::mcp_server::read_run_content(&db, &input)
                    })
                    .await
            }
            _ => unreachable!("registered run operation"),
        }
    }

    async fn enqueue(
        &self,
        s: &Arc<Service>,
        task: &str,
        trigger: &str,
        bearer: Option<String>,
    ) -> Result<Value> {
        let run = s.prepare_run(task, trigger).await?;
        let run: Run = serde_json::from_value(s.queue_run(run, None, bearer).await?)?;
        Ok(serde_json::to_value(run)?)
    }
}
