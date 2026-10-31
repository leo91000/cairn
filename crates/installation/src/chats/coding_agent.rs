//! Explicit recovery of a quota-blocked turn on another coding agent.
use super::{ChatExecution, chat, continue_run, handoff_context, is_compaction, view};
use crate::{
    config::now,
    error::{Error, Result, required},
    native::policy,
    provider::Provider,
    service::Service,
    store::Db,
    validation::text,
    worker::checkpoint::RunCheckpoint,
};
use cairn_contracts::EventKind;
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};

const RECEIPT_TTL_MS: i64 = 7 * 86_400_000;

pub(crate) fn progress(
    db: &Db<'_>,
    chat: &str,
) -> Result<Option<cairn_contracts::CodingAgentSwitch>> {
    db.engine_switch(chat)?
        .filter(|state| state.pending)
        .map(|state| {
            Ok(cairn_contracts::CodingAgentSwitch {
                provider: serde_json::from_value(Value::from(state.provider))?,
                error: state.error,
            })
        })
        .transpose()
}

pub(crate) fn quota_blocked(db: &Db<'_>, run: &Value) -> Result<bool> {
    if run["environmentState"].is_string() {
        return Ok(false);
    }
    if is_compaction(run)
        || run["backendMigration"].is_object()
        || !run["cancelRequestedAt"].is_null()
    {
        return Ok(false);
    }
    let reason = text(run, "accountWaitReason");
    if matches!(text(run, "status"), "queued" | "running") {
        return Ok(reason.starts_with("Usage exhausted.")
            || reason.starts_with("Waiting for a ")
                && reason.contains(" account with available usage."));
    }
    if run["status"] != "failed" {
        return Ok(false);
    }
    if crate::run_output::exhausted(&json!({
        "type": "error",
        "message": text(run, "error").trim(),
    })) {
        return Ok(true);
    }
    // Inspect the same bounded recent history as the conversation card. A new
    // turn or recovery boundary prevents an old structured quota from resurfacing.
    let (events, _) = db.events_before(text(run, "id"), i64::MAX)?;
    for event in events.into_iter().rev() {
        let kind = event.kind.as_str();
        let turn_boundary = matches!(kind, "chat.user" | "turn.started" | "turn.completed");
        let execution_boundary = kind == "status"
            && matches!(
                event.text.as_str(),
                "queued" | "succeeded" | "cancelled" | "interrupted"
            );
        if turn_boundary || execution_boundary {
            return Ok(false);
        }
        if !matches!(kind, "turn.failed" | "error") {
            continue;
        }
        let mut payload: Value = event
            .payload
            .as_ref()
            .map(|raw| serde_json::from_str(raw.get()))
            .transpose()?
            .unwrap_or_else(|| json!({}));
        if !payload.is_object() {
            continue;
        }
        payload["type"] = kind.into();
        if !crate::run_output::exhausted(&payload) {
            continue;
        }
        let error = if kind == "turn.failed" {
            &payload["error"]
        } else {
            &payload
        };
        let message = text(error, "message");
        let message = if message.is_empty() {
            event.text.as_str()
        } else {
            message
        };
        let message = if message.is_empty() {
            "Usage limit reached"
        } else {
            message
        };
        return Ok(message == text(run, "error"));
    }
    Ok(false)
}

struct Choice {
    provider: Provider,
    model: String,
}

fn available(account: &cairn_contracts::CodingAgentAccount) -> bool {
    matches!(
        account.status,
        cairn_contracts::AccountStatus::Next
            | cairn_contracts::AccountStatus::Ready
            | cairn_contracts::AccountStatus::Low
            | cairn_contracts::AccountStatus::Full
    )
}

fn selection(agent: &Value, provider: Provider, model: &str) -> Value {
    let mut agent = agent.clone();
    agent["provider"] = provider.as_str().into();
    agent["model"] = model.into();
    agent["reasoning"] = "".into();
    agent["speed"] = "".into();
    agent
}

async fn choices(s: &Service, agent: &Value) -> Result<Vec<Choice>> {
    let accounts = s.accounts.list(s).await?;
    let mut choices = Vec::new();
    for provider in Provider::ALL {
        if provider.as_str() == text(agent, "provider")
            || !accounts
                .iter()
                .any(|account| account.provider == provider.into() && available(account))
        {
            continue;
        }
        let model = if provider == Provider::Opencode {
            // OpenCode cannot implement another engine's sandbox. Never broaden access.
            if crate::service::policy(agent)["sandbox"] != "yolo" {
                continue;
            }
            let Ok(Ok(catalog)) =
                tokio::time::timeout(std::time::Duration::from_secs(3), s.opencode.models(s)).await
            else {
                continue;
            };
            let models = catalog["models"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|model| {
                    model["routes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|route| {
                            route["hidden"] != true
                                && accounts.iter().any(|account| {
                                    account.provider == cairn_contracts::AgentProvider::Opencode
                                        && available(account)
                                        && account.model_provider.as_deref()
                                            == route["provider"].as_str()
                                })
                        })
                })
                .collect::<Vec<_>>();
            let Some(model) = models
                .iter()
                .find(|model| model["isDefault"] == true)
                .or_else(|| models.first())
            else {
                continue;
            };
            text(model, "model").to_owned()
        } else {
            String::new()
        };
        if provider
            .adapter()
            .validate_agent(&selection(agent, provider, &model))
            .is_ok()
        {
            choices.push(Choice { provider, model });
        }
    }
    Ok(choices)
}

pub(crate) async fn options<F>(s: &Service, id: &str, authorize: F) -> Result<Value>
where
    F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
{
    let id = id.to_owned();
    let (run, pending, eligible) = s
        .store
        .read(move |db| {
            authorize(db)?;
            let chat = chat(db, &id)?;
            super::require_active(&chat)?;
            let run = db.run(text(&chat, "runId"))?.unwrap_or(Value::Null);
            let eligible = quota_blocked(db, &run)?;
            Ok((run, progress(db, &id)?, eligible))
        })
        .await?;
    let available = if eligible {
        choices(s, &run["snapshot"]["agent"]).await?
    } else {
        Vec::new()
    };
    // A plan turn only moves to a coding agent that can keep its boundary.
    let planning = super::planning(&run);
    let available = available
        .into_iter()
        .filter(|choice| !planning || choice.provider.adapter().plan_turns())
        .map(|choice| {
            json!({
                "provider": choice.provider.as_str(),
                "label": choice.provider.label(),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "runId": run["id"],
        "turnId": run["chatExecution"]["messageId"],
        "provider": run["snapshot"]["agent"]["provider"],
        "choices": available,
        "switching": pending,
    }))
}

impl Service {
    pub(crate) async fn switch_chat_coding_agent<F>(
        &self,
        input: Value,
        authorize: F,
    ) -> Result<Value>
    where
        F: Fn(&Db<'_>) -> Result<()> + Clone + Send + Sync + 'static,
    {
        let input: cairn_contracts::SwitchChatCodingAgentInput =
            serde_json::from_value(input).map_err(|e| Error::bad(e.to_string()))?;
        if input.turn_id.is_empty() || input.turn_id.chars().count() > 200 {
            return Err(Error::bad("Invalid turn identifier."));
        }
        let input = serde_json::to_value(input)?;
        let target = Provider::parse(text(&input, "provider"))?;
        let origin = policy::origin();
        if origin.known() {
            policy::refresh_chat(self, text(&input, "chatId")).await?;
        }
        let check = authorize.clone();
        let chat_id = text(&input, "chatId").to_owned();
        let agent = self
            .store
            .read(move |db| {
                check(db)?;
                let chat = chat(db, &chat_id)?;
                let run = required(db.run(text(&chat, "runId"))?, "Run not found")?;
                Ok(run["snapshot"]["agent"].clone())
            })
            .await?;
        let available = choices(self, &agent).await?;
        let active = self.worker.active.lock().await;
        let saved = input.clone();
        let result = self
            .store
            .transaction(move |db| {
                authorize(db)?;
                let id = text(&saved, "chatId");
                let chat = chat(db, id)?;
                super::require_active(&chat)?;
                let current = db.engine_switch(id)?;
                let receipt = db.engine_switch_receipt(id, text(&saved, "requestId"))?;
                if let Some(previous) = receipt {
                    if previous.provider != text(&saved, "provider")
                        || previous.run_id != text(&saved, "runId")
                        || previous.turn_id != text(&saved, "turnId")
                    {
                        return Err(Error::conflict("This switch identity was already used."));
                    }
                    let stopping_previous = current.as_ref().is_some_and(|value| {
                        value.request_id == text(&saved, "requestId") && value.pending
                    });
                    return Ok((view(db, chat)?, stopping_previous));
                }
                if current.is_some_and(|value| value.pending) {
                    return Err(Error::conflict("A coding agent switch is already in progress."));
                }
                let Some(choice) = available.iter().find(|choice| choice.provider == target) else {
                    return Err(Error::conflict(
                        "This coding agent is unavailable for this conversation. Choose another or manage accounts.",
                    ));
                };
                let run = required(db.run(text(&chat, "runId"))?, "Run not found")?;
                if run["id"] != saved["runId"]
                    || run["chatExecution"]["messageId"] != saved["turnId"]
                    || !saved["turnId"].is_string()
                    || !quota_blocked(db, &run)?
                {
                    return Err(Error::conflict(
                        "This turn is no longer waiting for quota. Refresh the conversation.",
                    ));
                }
                if Provider::of_run(&run)? == target {
                    return Err(Error::bad("Choose a different coding agent."));
                }
                if super::planning(&run) {
                    super::require_plan_turns(target)?;
                }
                let replacement = selection(&run["snapshot"]["agent"], target, &choice.model);
                target.adapter().validate_agent(&replacement)?;
                policy::authorize_chat_in(db, id, &origin)?;
                let pending = super::engine_switch::EngineSwitch {
                    chat_id: id.to_owned(),
                    request_id: text(&saved, "requestId").to_owned(),
                    run_id: text(&saved, "runId").to_owned(),
                    turn_id: text(&saved, "turnId").to_owned(),
                    provider: target.as_str().to_owned(),
                    model: choice.model.clone(),
                    pending: true,
                    error: None,
                    retry_at: None,
                    receipt_expires_at: now() + RECEIPT_TTL_MS,
                    current_expires_at: None,
                };
                db.save_engine_switch(&pending)?;
                db.change_run(
                    text(&run, "id"),
                    crate::runs::RunChanges {
                        cancel_requested_at: Some(Some(now())),
                        ..Default::default()
                    },
                )?;
                db.event(
                    text(&run, "id"),
                    &EventKind::Status,
                    &format!("Switching coding agent to {}", target.label()),
                    None,
                )?;
                Ok((view(db, super::chat(db, id)?)?, true))
            })
            .await?;
        if result.1
            && let Some(cancel) = active.get(text(&input, "runId"))
        {
            cancel.cancel();
        }
        self.worker.notify();
        Ok(result.0)
    }
}

/// Recover accepted switches after restart. The per-run preparation lock excludes both
/// competing switch tasks and ordinary admission while the old execution is fenced.
pub(crate) async fn tick(s: &Arc<Service>, active: &HashSet<String>) -> Result<()> {
    for chat in s.store.conversations_boundary().await? {
        let id = text(&chat, "id");
        if !super::is_active(&chat) {
            continue;
        }
        let Some(pending) = s
            .store
            .read({
                let id = id.to_owned();
                move |db| db.engine_switch(&id)
            })
            .await?
            .filter(|v| v.pending)
        else {
            continue;
        };
        let run = &pending.run_id;
        if active.contains(run) || pending.retry_at.is_some_and(|at| at > now()) {
            continue;
        }
        let lock = s.worker.run_preparation_lock(run).await;
        let Ok(guard) = lock.try_lock_owned() else {
            continue;
        };
        let (s, id) = (s.clone(), id.to_owned());
        s.worker.clone().spawn_recovery(async move {
            let _guard = guard;
            if s.shutdown.is_cancelled()
                || s.worker.active.lock().await.contains_key(&pending.run_id)
            {
                return;
            }
            let result = advance(&s, &id, &pending).await;
            if let Err(error) = result {
                let message = if error.status < 500 {
                    error.message
                } else {
                    "Waiting for the previous execution to stop. Your work is preserved.".into()
                };
                let _ = s
                    .store
                    .transaction(move |db| {
                        let Some(mut current) = db.engine_switch(&id)?.filter(|value| {
                            value.request_id == pending.request_id && value.pending
                        }) else {
                            return Ok(());
                        };
                        current.error = Some(message);
                        current.retry_at = Some(now() + 2_000);
                        db.save_engine_switch(&current)
                    })
                    .await;
            } else {
                s.worker.notify();
            }
        });
    }
    Ok(())
}

async fn advance(
    s: &Service,
    id: &str,
    pending: &super::engine_switch::EngineSwitch,
) -> Result<()> {
    let run = s.store.run(&pending.run_id).await?;
    crate::recovery::fence(s, &run).await?;
    s.accounts.recover_run(s, &run).await?;
    let (id, pending) = (id.to_owned(), pending.clone());
    s.store
        .transaction(move |db| {
            let Some(mut current) = db
                .engine_switch(&id)?
                .filter(|p| p.pending && p.request_id == pending.request_id)
            else {
                return Ok(());
            };
            let chat = chat(db, &id)?;
            super::require_active(&chat)?;
            if chat["runId"].as_str() != Some(pending.run_id.as_str())
                || chat["cancelledByDeletion"] == true
            {
                return Err(Error::conflict("The conversation changed while switching."));
            }
            let run = required(db.run(&pending.run_id)?, "Run not found")?;
            if run["chatExecution"]["messageId"].as_str() != Some(pending.turn_id.as_str()) {
                return Err(Error::conflict("The pending turn changed."));
            }
            let target = Provider::parse(&pending.provider)?;
            let mut snapshot = run.clone();
            snapshot["snapshot"]["agent"] =
                selection(&run["snapshot"]["agent"], target, &pending.model);
            target
                .adapter()
                .validate_agent(&snapshot["snapshot"]["agent"])?;
            let live = required(
                db.agent_snapshot(text(&chat, "agentId"))?,
                "Agent not found",
            )?;
            if !crate::service::covers(&live, &snapshot["snapshot"]["agent"]) {
                return Err(Error::conflict(
                    "Agent access changed. Review its permissions before resuming.",
                ));
            }
            let run_id = text(&run, "id");
            let mut checkpoint = RunCheckpoint::load_in(db, run_id)?.unwrap_or_default();
            checkpoint.retry_cause = Some(None);
            checkpoint.last_error = Some(None);
            checkpoint.store_in(db, run_id)?;
            let execution = ChatExecution {
                message_id: text(&run["chatExecution"], "messageId").to_owned(),
                text: text(&run["chatExecution"], "text").to_owned(),
                attachments: serde_json::from_value(run["chatExecution"]["attachments"].clone())?,
                recovery: true,
                context: Some(handoff_context(db, run_id)?),
                // The switched turn keeps its plan boundary, or the restore after one.
                plan: super::planning(&run),
                after_plan: run["chatExecution"]["afterPlan"] == true,
                operation: None,
                previous: None,
            };
            continue_run(
                db,
                &serde_json::from_value(chat.clone())?,
                &run,
                &snapshot,
                execution,
            )?;
            db.change_run(
                run_id,
                crate::runs::RunChanges {
                    account_wait_reason: Some(None),
                    account_required: Some(None),
                    account_id: Some(None),
                    account_name: Some(None),
                    retry: Some(None),
                    ..Default::default()
                },
            )?;
            current.pending = false;
            current.error = None;
            current.current_expires_at = Some(now() + RECEIPT_TTL_MS);
            db.save_engine_switch(&current)?;
            db.event(run_id, &EventKind::Status, "queued", None)?;
            Ok(())
        })
        .await
}

pub(crate) fn pending_for_run(db: &Db<'_>, run: &str) -> Result<bool> {
    let Some(chat) = db.conversation_for_run(run)? else {
        return Ok(false);
    };
    Ok(db
        .engine_switch(&chat.id)?
        .is_some_and(|state| state.pending))
}

pub(crate) fn cancel_in(db: &Db<'_>, run: &str) -> Result<()> {
    if let Some(chat) = db.conversation_for_run(run)?
        && let Some(mut state) = db.engine_switch(&chat.id)?
    {
        state.pending = false;
        state.error = None;
        state.current_expires_at = Some(now() + RECEIPT_TTL_MS);
        db.save_engine_switch(&state)?;
    }
    Ok(())
}
