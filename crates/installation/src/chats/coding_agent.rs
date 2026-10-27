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
use std::{
    collections::HashSet,
    sync::{Arc, OnceLock},
};

const RECEIPT_TTL_MS: i64 = 7 * 86_400_000;

fn request_key(chat: &str, request: &str) -> String {
    format!("chat-coding-agent-request:{chat}:{request}")
}

pub(crate) fn key(chat: &str) -> String {
    format!("chat-coding-agent:{chat}")
}

pub(crate) fn progress(db: &Db<'_>, chat: &str) -> Result<Value> {
    Ok(db
        .kv(&key(chat))?
        .filter(|value| value["pending"] == true)
        .map_or(
            Value::Null,
            |value| json!({ "provider": value["provider"], "error": value["error"] }),
        ))
}

pub(crate) fn quota_blocked(run: &Value) -> bool {
    static QUOTA: OnceLock<regex::Regex> = OnceLock::new();
    if is_compaction(run)
        || run["backendMigration"].is_object()
        || !run["cancelRequestedAt"].is_null()
    {
        return false;
    }
    let reason = text(run, "accountWaitReason");
    if matches!(text(run, "status"), "queued" | "running") {
        return reason.starts_with("Usage exhausted.")
            || reason.starts_with("Waiting for a ")
                && reason.contains(" account with available usage.");
    }
    run["status"] == "failed" && QUOTA.get_or_init(|| regex::Regex::new(
        r"(?i)^(?:you['’]ve hit your (?:(?:usage|session|weekly|spend|individual spend) )?limit|you have hit your (?:usage )?limit|usage limit (?:has been )?(?:reached|exceeded)|claude ai usage limit reached|(?:5-hour|session|weekly|opus) limit reached)\b"
    ).unwrap()).is_match(text(run,"error").trim())
}

struct Choice {
    provider: Provider,
    model: String,
}

fn available(account: &Value) -> bool {
    matches!(text(account, "status"), "next" | "ready" | "low" | "full")
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
                .any(|account| account["provider"] == provider.as_str() && available(account))
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
                                    account["provider"] == "opencode"
                                        && available(account)
                                        && account["modelProvider"] == route["provider"]
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
    let (run, pending) = s
        .store
        .read(move |db| {
            authorize(db)?;
            let chat = chat(db, &id)?;
            super::require_active(&chat)?;
            Ok((
                db.run(text(&chat, "runId"))?.unwrap_or(Value::Null),
                progress(db, &id)?,
            ))
        })
        .await?;
    let available = if quota_blocked(&run) {
        choices(s, &run["snapshot"]["agent"]).await?
    } else {
        Vec::new()
    };
    let available = available
        .into_iter()
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
        let input = crate::validation::parse("mcp:switch_chat_coding_agent", input)?;
        let target = Provider::parse(text(&input, "provider"))?;
        let origin = policy::origin();
        if origin.known() {
            policy::refresh_chat(self, text(&input, "chatId")).await?;
        }
        let check = authorize.clone();
        let chat_id = text(&input, "chatId").to_owned();
        let agent = self
            .store
            .transaction(move |db| {
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
                policy::authorize_chat_in(db, id, &origin)?;
                let chat = chat(db, id)?;
                super::require_active(&chat)?;
                let current = db.kv(&key(id))?;
                let receipt = db.kv(&request_key(id, text(&saved, "requestId")))?.or_else(|| {
                    current.as_ref().filter(|value| value["requestId"] == saved["requestId"]).cloned()
                });
                if let Some(previous) = receipt {
                    if previous["provider"] != saved["provider"]
                        || previous["runId"] != saved["runId"]
                        || previous["turnId"] != saved["turnId"]
                    {
                        return Err(Error::conflict("This switch identity was already used."));
                    }
                    let stopping_previous = current.as_ref().is_some_and(|value| {
                        value["requestId"] == saved["requestId"] && value["pending"] == true
                    });
                    return Ok((view(db, chat)?, stopping_previous));
                }
                if current.is_some_and(|value| value["pending"] == true) {
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
                    || !quota_blocked(&run)
                {
                    return Err(Error::conflict(
                        "This turn is no longer waiting for quota. Refresh the conversation.",
                    ));
                }
                if Provider::of_run(&run)? == target {
                    return Err(Error::bad("Choose a different coding agent."));
                }
                let replacement = selection(&run["snapshot"]["agent"], target, &choice.model);
                target.adapter().validate_agent(&replacement)?;
                let mut pending = saved.clone();
                pending["model"] = choice.model.clone().into();
                pending["pending"] = true.into();
                pending["error"] = Value::Null;
                db.set(&key(id), &pending, None)?;
                db.set(&request_key(id, text(&saved, "requestId")), &saved, Some(now() + RECEIPT_TTL_MS))?;
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
                Ok((view(db, chat)?, true))
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
    for chat in s.store.list("chats").await? {
        let id = text(&chat, "id");
        if !super::is_active(&chat) {
            continue;
        }
        let Some(pending) = s.store.kv(&key(id)).await?.filter(|v| v["pending"] == true) else {
            continue;
        };
        let run = text(&pending, "runId");
        if active.contains(run) || pending["retryAt"].as_i64().is_some_and(|at| at > now()) {
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
                || s.worker
                    .active
                    .lock()
                    .await
                    .contains_key(text(&pending, "runId"))
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
                        let Some(mut current) = db.kv(&key(&id))?.filter(|value| {
                            value["requestId"] == pending["requestId"] && value["pending"] == true
                        }) else {
                            return Ok(());
                        };
                        current["error"] = message.into();
                        current["retryAt"] = (now() + 2_000).into();
                        db.set(&key(&id), &current, None)
                    })
                    .await;
            } else {
                s.worker.notify();
            }
        });
    }
    Ok(())
}

async fn advance(s: &Service, id: &str, pending: &Value) -> Result<()> {
    let run = s.store.run(text(pending, "runId")).await?;
    crate::recovery::fence(s, &run).await?;
    s.accounts.recover_run(s, &run).await?;
    let (id, pending) = (id.to_owned(), pending.clone());
    s.store
        .transaction(move |db| {
            let Some(mut current) = db
                .kv(&key(&id))?
                .filter(|p| p["pending"] == true && p["requestId"] == pending["requestId"])
            else {
                return Ok(());
            };
            let mut chat = chat(db, &id)?;
            super::require_active(&chat)?;
            if chat["runId"] != pending["runId"] || chat["cancelledByDeletion"] == true {
                return Err(Error::conflict("The conversation changed while switching."));
            }
            let run = required(db.run(text(&pending, "runId"))?, "Run not found")?;
            if run["chatExecution"]["messageId"] != pending["turnId"] {
                return Err(Error::conflict("The pending turn changed."));
            }
            let target = Provider::parse(text(&pending, "provider"))?;
            let mut snapshot = run.clone();
            snapshot["snapshot"]["agent"] =
                selection(&run["snapshot"]["agent"], target, text(&pending, "model"));
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
                message_id: run["chatExecution"]["messageId"].clone(),
                text: text(&run["chatExecution"], "text").to_owned(),
                attachments: run["chatExecution"]["attachments"].clone(),
                recovery: true,
                context: Some(handoff_context(db, run_id)?),
                operation: None,
                previous: None,
            };
            continue_run(db, &chat, &run, &snapshot, execution)?;
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
            chat["paused"] = false.into();
            db.put("chats", &chat)?;
            current["pending"] = false.into();
            current["error"] = Value::Null;
            db.set(&key(&id), &current, Some(now() + RECEIPT_TTL_MS))?;
            db.event(run_id, &EventKind::Status, "queued", None)?;
            Ok(())
        })
        .await
}

pub(crate) fn pending_for_run(db: &Db<'_>, run: &str) -> Result<bool> {
    let Some(chat) = super::chat_for_run(db, run)? else {
        return Ok(false);
    };
    Ok(db
        .kv(&key(text(&chat, "id")))?
        .is_some_and(|value| value["pending"] == true))
}

pub(crate) fn cancel_in(db: &Db<'_>, run: &str) -> Result<()> {
    if let Some(chat) = super::chat_for_run(db, run)? {
        let key = key(text(&chat, "id"));
        if let Some(mut receipt) = db.kv(&key)? {
            receipt["pending"] = false.into();
            receipt["error"] = Value::Null;
            db.set(&key, &receipt, Some(now() + RECEIPT_TTL_MS))?;
        }
    }
    Ok(())
}
