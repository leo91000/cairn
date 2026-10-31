//! Merge an idle profile into an existing one without losing resumable work.
use crate::{
    config::MAIN_AGENT_ID,
    error::{Error, Result, required},
    service::{Service, covers, policy},
    store::Db,
    validation::text,
};
use serde_json::{Value, json};

impl Service {
    pub async fn merge_agents(
        &self,
        input: cairn_contracts::MergeAgentsInput,
    ) -> Result<cairn_contracts::MergeAgentsOutput> {
        self.merge_agents_authorized(input, |_| Ok(()), None).await
    }

    pub(crate) async fn merge_agents_authorized<F>(
        &self,
        input: cairn_contracts::MergeAgentsInput,
        authorize: F,
        resource_authority: Option<crate::capabilities::plugin_packages::Authority>,
    ) -> Result<cairn_contracts::MergeAgentsOutput>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let (target, source) = (input.target_id.to_string(), input.source_id.to_string());
        let coverage = crate::plugins::grants::check_coverage_authorized(
            self,
            &target,
            &source,
            resource_authority.clone(),
        )
        .await;
        let revisions = match coverage {
            Ok(revisions) => revisions,
            Err(error) if error.status == 403 => {
                if let Some(authority) = &resource_authority {
                    authority.recheck(self).await?;
                }
                return Err(Error::conflict(error.message));
            }
            Err(error) => return Err(error),
        };
        self.missions
            .merge_agents(self, input, move |db| {
                authorize(db)?;
                if let Some(authority) = &resource_authority {
                    authority.authorize(db)?;
                }
                if required(db.agent(&target)?, "Destination agent not found")?.revision
                    != revisions.0
                    || required(db.agent(&source)?, "Source agent not found")?.revision
                        != revisions.1
                {
                    return Err(Error::conflict(
                        "Plugin grants changed while checking the merge. Retry.",
                    ));
                }
                Ok(())
            })
            .await
    }
}

pub(crate) fn merge(
    db: &Db<'_>,
    input: &cairn_contracts::MergeAgentsInput,
    executing: &[String],
) -> Result<cairn_contracts::MergeAgentsOutput> {
    let source_string = input.source_id.to_string();
    let target_string = input.target_id.to_string();
    let source_id = source_string.as_str();
    let target_id = target_string.as_str();
    if source_id == target_id {
        return Err(Error::bad("Choose two different agents."));
    }
    if source_id == MAIN_AGENT_ID {
        return Err(Error::conflict(
            "The main agent must be the merge destination.",
        ));
    }
    let source = required(db.agent(source_id)?, "Source agent not found")?;
    let target = required(db.agent(target_id)?, "Destination agent not found")?;
    let source_snapshot = serde_json::to_value(&source)?;
    let target_snapshot = serde_json::to_value(&target)?;
    if crate::native::policy::supports(&source_snapshot, "native")
        || crate::native::policy::supports(&target_snapshot, "native")
    {
        return Err(Error::conflict(
            "Agents permitting native execution cannot be merged. Their execution approvals are tied to the agent.",
        ));
    }
    if !covers(&target_snapshot, &source_snapshot)
        || !nodes_covered(&target_snapshot, &source_snapshot)
    {
        return Err(Error::conflict(
            "The destination must already cover all source access, including nodes.",
        ));
    }
    for grants in [
        crate::agent_grants::plugins,
        crate::agent_grants::exclusive_nodes,
    ] {
        let target_grants = grants(&target_snapshot);
        if grants(&source_snapshot)
            .iter()
            .any(|grant| !target_grants.contains(grant))
        {
            return Err(Error::conflict(
                "Grant the source's plugin and exclusive node access to the destination before merging.",
            ));
        }
    }

    if !crate::plugins::grants::contribution_ceiling_in(db, target_id, source_id)? {
        return Err(Error::conflict(
            "The destination must cover all source plugin contributions and resource restrictions.",
        ));
    }
    for instance in db.plugin_instances()? {
        if instance.agent_id == source_id {
            return Err(Error::conflict(
                "Reassign the source's plugin delegations before merging.",
            ));
        }
    }
    let runs = db.json_rows(
        "SELECT data FROM run_views AS runs WHERE json_extract(snapshot,'$.agent.id')=?",
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
    for mut mission in db.missions()? {
        if mission.settings.agent_id == source.id {
            mission.settings.agent_id = target.id;
            mission.revision = crate::config::id();
            db.save_mission(&mission)?;
            tasks += 1;
        }
    }
    for mut record in db.conversations_boundary()? {
        if record["agentId"] != source_id {
            continue;
        }
        record["agentId"] = target_id.into();
        record["agentName"] = target.settings.name.clone().into();
        db.save_conversation_boundary(&record)?;
        chats += 1;
    }
    // Keep each run's provider, model, instructions and access snapshot. Only its
    // profile identity changes so recovery and live authorization find the target.
    for run in &runs {
        let mut snapshot = required(db.run_snapshot(text(run, "id"))?, "Run snapshot not found")?;
        snapshot.agent.id = target.id;
        if snapshot.task.settings.agent_id == source.id {
            snapshot.task.settings.agent_id = target.id;
        }
        db.change_run(
            text(run, "id"),
            crate::runs::RunChanges {
                snapshot: Some(snapshot),
                ..Default::default()
            },
        )?;
    }
    crate::service::forget_agent(db, source_id)?;
    db.remove_agent(source_id)?;
    let result = json!({
        "sourceId": source_id,
        "targetId": target_id,
        "transferred": { "tasks": tasks, "chats": chats, "runs": runs.len() },
        "deleted": true,
    });
    db.audit("agent.merged", &result)?;
    Ok(serde_json::from_value(result)?)
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
    use crate::agent_fixture::AgentFixture;

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
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let source = s
            .agent_fixture(json!({ "name": "Source" }), None)
            .await
            .unwrap();
        let source_id = text(&source, "id").to_owned();
        let removed = source_id.clone();
        // The authorization hook runs after draft preparation and inside the
        // publication transaction: reproduce deletion at that exact boundary.
        let current = s.store.agent(&source_id).await.unwrap().unwrap();
        let result = s
            .save_profile(
                crate::agents::SaveProfile::Replace(cairn_contracts::ReplaceAgentInput {
                    agent_id: current.id,
                    revision: current.revision,
                    settings: current.settings,
                    review: cairn_contracts::AgentReview::default(),
                }),
                crate::service::AgentEditor::Owner,
                move |db| {
                    db.remove_agent(&removed)?;
                    Ok(())
                },
            )
            .await;
        assert!(result.is_err());
        // The failed transaction rolls back the injected deletion as well.
        assert_eq!(s.agent_snapshot(&source_id).await.unwrap(), source);
    }
}
