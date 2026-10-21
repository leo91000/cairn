//! Merge an idle profile into an existing one without losing resumable work.
use crate::{
    config::MAIN_AGENT_ID,
    error::{Error, Result, required},
    service::{Service, covers, policy},
    store::Db,
    validation::{parse, text},
};
use serde_json::{Value, json};

impl Service {
    pub async fn merge_agents(&self, input: Value) -> Result<Value> {
        self.merge_agents_authorized(input, |_| Ok(())).await
    }

    pub(crate) async fn merge_agents_authorized<F>(
        &self,
        input: Value,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let input = parse("mcp:merge_agents", input)?;
        // A scheduled occurrence must finish admission before its profile can be
        // removed. Otherwise a prepared snapshot could fail and skip that occurrence.
        let _authors = self.task_author_lock.lock().await;
        // A terminal DB status can precede the worker's final writes. Serialize with
        // admission, cancellation and resume until the transfer is committed.
        let active = self.worker.active.lock().await;
        let executing = active.keys().cloned().collect::<Vec<_>>();
        let result = self
            .store
            .transaction(move |db| {
                authorize(db)?;
                merge(db, &input, &executing)
            })
            .await?;
        self.worker.notify();
        Ok(result)
    }
}

fn merge(db: &Db<'_>, input: &Value, executing: &[String]) -> Result<Value> {
    let source_id = text(input, "sourceId");
    let target_id = text(input, "targetId");
    if source_id == target_id {
        return Err(Error::bad("Choose two different agents."));
    }
    if source_id == MAIN_AGENT_ID {
        return Err(Error::conflict(
            "The main agent must be the merge destination.",
        ));
    }
    let source = required(db.get("agents", source_id)?, "Source agent not found")?;
    let target = required(db.get("agents", target_id)?, "Destination agent not found")?;
    if crate::native::policy::native(&source) || crate::native::policy::native(&target) {
        return Err(Error::conflict(
            "Native agents cannot be merged. Their execution approvals are tied to the agent.",
        ));
    }
    if !covers(&target, &source) || !nodes_covered(&target, &source) {
        return Err(Error::conflict(
            "The destination must already cover all source access, including nodes.",
        ));
    }
    for grants in [
        crate::agent_grants::plugins,
        crate::agent_grants::exclusive_nodes,
    ] {
        let target_grants = grants(&target);
        if grants(&source)
            .iter()
            .any(|grant| !target_grants.contains(grant))
        {
            return Err(Error::conflict(
                "Grant the source's plugin and exclusive node access to the destination before merging.",
            ));
        }
    }
    if db.kv(&format!("agent-github:{source_id}"))?.is_some() {
        return Err(Error::conflict(
            "Remove the source's personal GitHub token before merging; credentials are not transferred.",
        ));
    }
    for instance in db.list(crate::plugins::connections::KIND)? {
        if instance["agentId"] == source_id {
            return Err(Error::conflict(
                "Reassign the source's plugin delegations before merging.",
            ));
        }
        let granted = instance["agentIds"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id == source_id));
        let target_granted = instance["agentIds"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id == target_id));
        if granted && !target_granted {
            return Err(Error::conflict(
                "Grant the source's plugin access to the destination before merging.",
            ));
        }
    }
    let runs = db.json_rows(
        "SELECT data FROM runs WHERE json_extract(data,'$.snapshot.agent.id')=?",
        [source_id],
    )?;
    if runs.iter().any(|run| {
        crate::run_status::RunStatus::of(run).is_some_and(crate::run_status::RunStatus::is_active)
            || run["recoveryPending"] == true
            || executing.iter().any(|id| run["id"] == *id)
    }) {
        return Err(Error::conflict(
            "The source agent still has active work. Wait for it to finish before merging.",
        ));
    }

    let mut tasks = 0;
    let mut chats = 0;
    for (kind, count) in [("tasks", &mut tasks), ("chats", &mut chats)] {
        for mut record in db.list(kind)? {
            if record["agentId"] != source_id {
                continue;
            }
            record["agentId"] = target_id.into();
            if kind == "chats" {
                record["agentName"] = target["name"].clone();
            }
            db.put(kind, &record)?;
            *count += 1;
        }
    }
    // Keep each run's provider, model, instructions and access snapshot. Only its
    // profile identity changes so recovery and live authorization find the target.
    for run in &runs {
        let mut snapshot = run["snapshot"].clone();
        snapshot["agent"]["id"] = target_id.into();
        if snapshot["task"]["agentId"] == source_id {
            snapshot["task"]["agentId"] = target_id.into();
        }
        db.patch_run(text(run, "id"), &json!({ "snapshot": snapshot }))?;
    }
    crate::service::forget_agent(db, source_id)?;
    db.remove("agents", source_id)?;
    let result = json!({
        "sourceId": source_id,
        "targetId": target_id,
        "transferred": { "tasks": tasks, "chats": chats, "runs": runs.len() },
        "deleted": true,
    });
    db.audit("agent.merged", &result)?;
    Ok(result)
}

fn nodes_covered(target: &Value, source: &Value) -> bool {
    let target = policy(target);
    let source = policy(source);
    target["nodes"].is_null()
        || source["nodes"].as_array().is_some_and(|nodes| {
            nodes
                .iter()
                .all(|node| crate::service::allowed(&target["nodes"], node.as_str().unwrap_or("")))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_prepared_edit_cannot_recreate_an_agent_removed_before_its_commit() {
        let root = tempfile::tempdir().unwrap();
        let s = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().to_owned()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            gh_bin: "gh".into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let source = s.agent(json!({ "name": "Source" }), None).await.unwrap();
        let source_id = text(&source, "id").to_owned();
        let removed = source_id.clone();
        // The authorization hook runs after draft preparation and inside the
        // publication transaction: reproduce deletion at that exact boundary.
        let result = s
            .agent_authorized(
                json!({ "name": "Stale edit" }),
                Some(&source_id),
                crate::service::AgentEditor::Owner,
                move |db| {
                    db.remove("agents", &removed)?;
                    Ok(())
                },
            )
            .await;
        assert!(result.is_err());
        // The failed transaction rolls back the injected deletion as well.
        assert_eq!(s.get("agents", &source_id).await.unwrap(), source);
    }
}
