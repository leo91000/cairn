//! Read-only, bounded GitHub observation through the agent's existing credentials.
mod pr_checks;

use super::access;
use crate::{
    config::now,
    conversation_lifecycle::is_active,
    error::{Error, Result},
    process::{Environment, bounded_output, codex_environment, command},
    run_status::RunStatus,
    service::{Service, policy},
    store::Db,
    validation::text,
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

const POLL_MS: i64 = 30_000;
const BATCH_SIZE: usize = 16;

pub(super) fn is_condition(condition: &Value) -> bool {
    matches!(
        text(condition, "type"),
        "github_workflow" | "github_pr_checks"
    )
}

pub(super) fn project(db: &Db<'_>, run: &Value, condition: &Value) -> Result<String> {
    let id = text(condition, "projectId");
    let granted = crate::project_workspaces::catalog(run)
        .into_iter()
        .find(|p| p["id"] == id)
        .ok_or_else(|| Error::forbidden("This project is not authorized for the wait."))?;
    let current = db
        .get("projects", id)?
        .ok_or_else(|| Error::not_found("Project no longer exists."))?;
    if current["origin"] != granted["origin"] {
        return Err(Error::conflict(
            "Project origin changed; register a new wait.",
        ));
    }
    let agent = &run["snapshot"]["agent"];
    let token = db.kv(&format!("agent-github:{}", text(agent, "id")))?;
    if policy(agent)["github"] != true
        && !token
            .as_ref()
            .is_some_and(|token| token.as_str().is_some_and(|t| !t.is_empty()))
    {
        return Err(Error::forbidden(
            "This agent has no authorized GitHub connection.",
        ));
    }
    let origin = text(&current, "origin");
    let repo = origin
        .strip_prefix("https://github.com/")
        .or_else(|| origin.strip_prefix("ssh://git@github.com/"))
        .or_else(|| origin.strip_prefix("git@github.com:"))
        .ok_or_else(|| Error::bad("This condition requires a github.com project."))?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    crate::github_projects::repository(repo)?;
    Ok(repo.to_owned())
}

pub(super) async fn observe(s: &Service, run: &Value, condition: &Value) -> Result<Option<Value>> {
    let caller = run.clone();
    let source = condition.clone();
    let (repository, token) = s
        .store
        .read(move |db| {
            access(db, &caller, &source)?;
            let repo = project(db, &caller, &source)?;
            let token = db.kv(&format!(
                "agent-github:{}",
                text(&caller["snapshot"]["agent"], "id")
            ))?;
            Ok((repo, token))
        })
        .await?;
    let mut environment = codex_environment(&s.config, &s.config.home.join(".codex"));
    if let Some(token) = token.as_ref().and_then(Value::as_str) {
        environment.remove("GITHUB_TOKEN");
        environment.insert("GH_TOKEN".into(), token.into());
    }
    if condition["type"] == "github_pr_checks" {
        return tokio::time::timeout(
            Duration::from_secs(20),
            pr_checks::observe(s, &repository, &environment, condition),
        )
        .await
        .map_err(|_| Error::bad_gateway("GitHub PR checks could not be checked in time."))?;
    }
    let endpoint = format!("repos/{repository}/actions/runs/{}", condition["runId"]);
    let args = [
        "api",
        "--hostname",
        "github.com",
        &endpoint,
        "--jq",
        "{id,head_sha,status,conclusion,run_attempt,html_url}",
    ]
    .map(str::to_owned);
    let value = request(s, &environment, &args).await?;
    if value["id"] != condition["runId"]
        || text(&value, "head_sha").to_ascii_lowercase() != condition["headSha"]
    {
        return Err(Error::conflict(
            "The workflow run does not match the requested run ID and commit SHA.",
        ));
    }
    let status = text(&value, "status");
    if status != "completed" {
        if !["queued", "in_progress", "requested", "waiting", "pending"].contains(&status) {
            return Err(Error::bad_gateway("Unknown GitHub workflow state."));
        }
        return Ok(None);
    }
    let conclusion = text(&value, "conclusion");
    if ![
        "success",
        "failure",
        "neutral",
        "cancelled",
        "skipped",
        "timed_out",
        "action_required",
        "stale",
        "startup_failure",
    ]
    .contains(&conclusion)
    {
        return Err(Error::bad_gateway("Unknown GitHub workflow conclusion."));
    }
    Ok(Some(json!({
        "state": "completed",
        "type": "github_workflow",
        "repository": repository,
        "runId": value["id"],
        "headSha": condition["headSha"],
        "status": status,
        "conclusion": conclusion,
        "runAttempt": value["run_attempt"],
    })))
}

async fn request(s: &Service, environment: &Environment, args: &[String]) -> Result<Value> {
    let output = bounded_output(
        command(&s.config.gh_bin, args, environment, None),
        Duration::from_secs(20),
        1_000_000,
    )
    .await
    .map_err(|_| {
        Error::bad_gateway("GitHub could not be checked. Verify the connection and retry.")
    })?;
    if !output.success {
        return Err(Error::bad_gateway(
            "GitHub could not be checked. Verify repository access and retry.",
        ));
    }
    let value: Value = serde_json::from_str(&output.stdout)
        .map_err(|_| Error::bad_gateway("Invalid GitHub response."))?;
    if value["errors"]
        .as_array()
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(Error::bad_gateway(
            "GitHub returned an incomplete response.",
        ));
    }
    Ok(value)
}

fn watchable(chat: &Value, run: &Value, time: i64) -> bool {
    let wakeup = &chat["wakeup"];
    is_active(chat)
        && chat["paused"] != true
        && run["status"] == RunStatus::Succeeded
        && run["recoveryPending"] != true
        && run["cancelRequestedAt"].is_null()
        && run["workspaceCleanedAt"].is_null()
        && wakeup["messageId"] == run["chatExecution"]["messageId"]
        && is_condition(&wakeup["condition"])
        && !wakeup["observation"].is_object()
        && wakeup["wakeAt"]
            .as_i64()
            .is_some_and(|deadline| deadline > time)
        && wakeup["nextCheckAt"]
            .as_i64()
            .is_none_or(|next| next <= time)
}

/// One batch is capped and deduplicated; CLI/network waits never hold the admission tick.
pub async fn poll_once(s: &Service) -> Result<()> {
    let groups = s
        .store
        .read(|db| {
            let mut groups: BTreeMap<String, Vec<(Value, Value)>> = BTreeMap::new();
            for chat in db.list("chats")? {
                let Some(run) = db.run(text(&chat, "runId"))? else {
                    continue;
                };
                if !watchable(&chat, &run, now()) {
                    continue;
                }
                let project_id = &chat["wakeup"]["condition"]["projectId"];
                let granted_origin = crate::project_workspaces::catalog(&run)
                    .into_iter()
                    .find(|project| &project["id"] == project_id)
                    .map(|project| project["origin"].clone());
                let key = json!({
                    "agentId": run["snapshot"]["agent"]["id"],
                    "access": policy(&run["snapshot"]["agent"]),
                    "condition": chat["wakeup"]["condition"],
                    "projectOrigin": granted_origin,
                })
                .to_string();
                groups.entry(key).or_default().push((chat, run));
            }
            let mut groups: Vec<_> = groups.into_values().collect();
            groups.sort_by_key(|group| {
                group
                    .iter()
                    .filter_map(|(c, _)| c["wakeup"]["nextCheckAt"].as_i64())
                    .min()
                    .unwrap_or(0)
            });
            groups.truncate(BATCH_SIZE);
            Ok(groups)
        })
        .await?;
    let results = stream::iter(groups).map(|group| async move {
        let (first, run) = &group[0];
        let observed = observe(s, run, &first["wakeup"]["condition"]).await;
        s.store.transaction(move |db| {
            for (expected, _) in group {
                let Some(mut chat) = db.get("chats", text(&expected, "id"))? else { continue; };
                let Some(run) = db.run(text(&chat, "runId"))? else { continue; };
                if chat["wakeup"] != expected["wakeup"] || !watchable(&chat, &run, now()) { continue; }
                let authorized = access(db, &run, &chat["wakeup"]["condition"]);
                let result = match (&authorized, &observed) {
                    (Err(error), _) => Some(json!({ "state": "unavailable", "message": error.message })),
                    (Ok(()), Ok(value)) => value.clone(),
                    (Ok(()), Err(error)) if error.status == 409 => Some(json!({ "state": "unavailable", "message": error.message })),
                    _ => None,
                };
                if let Some(result) = result {
                    chat["wakeup"]["observation"] = result.clone();
                    db.event(text(&run, "id"), "chat.wait_ready", "Wait condition resolved", Some(&result))?;
                } else if observed.is_err() {
                    chat["wakeup"]["lastError"] = "GitHub is temporarily unavailable; retrying until the wait deadline.".into();
                }
                chat["wakeup"]["nextCheckAt"] = (now() + POLL_MS).into();
                chat["updatedAt"] = now().into();
                db.put("chats", &chat)?;
            }
            Ok(())
        }).await
    }).buffer_unordered(4).collect::<Vec<_>>().await;
    for result in results {
        result?;
    }
    s.worker.notify();
    Ok(())
}

pub(crate) async fn maintain(s: Arc<Service>) {
    let mut timer = tokio::time::interval(Duration::from_millis(POLL_MS as u64));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            _ = timer.tick() => {},
        }
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            result = poll_once(&s) => {
                if let Err(error) = result {
                    tracing::warn!(message = error.message, "Wait observer failed");
                }
            },
        }
    }
}
