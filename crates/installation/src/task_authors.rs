//! Task commitments are reconciled with the beacon authority. No member list is stored.
use crate::{
    auth::{InstallationIdentity, InstallationRole},
    error::{Error, Result},
    service::Service,
    validation::parse,
};
use cairn_protocol::{TaskAuthorGrant, TaskAuthorPolicy};
use serde_json::{Value, json};

impl Service {
    pub async fn task_as(
        &self,
        input: Value,
        existing: Option<&str>,
        identity: Option<&InstallationIdentity>,
    ) -> Result<Value> {
        let _guard = self.task_author_lock.lock().await;
        let input = parse("task", input)?;

        if let Some(id) = existing {
            let previous = self.get("tasks", id).await?;
            let previous_input = parse("task", previous.clone())?;
            let mut unchanged_content = input.clone();
            unchanged_content["enabled"] = previous_input["enabled"].clone();
            unchanged_content["archived"] = previous_input["archived"].clone();

            let only_reduces_execution = input["enabled"] == false
                && (input["archived"] == previous_input["archived"] || input["archived"] == true)
                && unchanged_content == previous_input;
            if only_reduces_execution {
                return self.save_task(input, existing, None).await;
            }

            if let Some(identity) = identity
                && identity.role == InstallationRole::Member
                && previous["authorId"] != identity.account_id
            {
                return Err(Error::forbidden(
                    "Only the task author or installation owner can change this task. Duplicate it to make your own commitment.",
                ));
            }
        }

        let policy = self.synchronize_task_authors_locked().await?;
        let author = if let Some(policy) = &policy {
            let account = identity.map_or(policy.owner.account_id.as_str(), |identity| {
                identity.account_id.as_str()
            });
            Some(policy.grant(account).cloned().ok_or_else(|| {
                Error::forbidden("This account no longer has access to the installation.")
            })?)
        } else {
            None
        };

        self.save_task(input, existing, author).await
    }

    pub async fn synchronize_task_authors(&self) -> Result<()> {
        let _guard = self.task_author_lock.lock().await;
        self.synchronize_task_authors_locked().await?;
        Ok(())
    }

    pub(crate) async fn synchronize_task_authors_locked(&self) -> Result<Option<TaskAuthorPolicy>> {
        let policy = crate::relay::task_author_policy(self).await?;
        if let Some(policy) = &policy {
            apply(self, policy.owner.clone(), policy.members.clone()).await?;
        }
        Ok(policy)
    }
}

async fn apply(
    service: &Service,
    owner: TaskAuthorGrant,
    members: Vec<TaskAuthorGrant>,
) -> Result<()> {
    let grants: std::collections::HashMap<_, _> = members
        .into_iter()
        .chain(std::iter::once(owner.clone()))
        .map(|grant| (grant.account_id, grant.access_id))
        .collect();
    service
        .store
        .transaction(move |db| {
            for mut task in db.list("tasks")? {
                let previous = task.clone();

                if !task["authorId"].is_string() {
                    task["authorId"] = owner.account_id.clone().into();
                    task["authorAccessId"] = owner.access_id.clone().into();
                }

                let current_access = grants.get(task["authorId"].as_str().unwrap());
                let eligible =
                    current_access.is_some_and(|access| task["authorAccessId"] == access.as_str());

                if !eligible {
                    task["authorRemoved"] = true.into();
                    task["scheduleWaitReason"] = Value::Null;
                    if task["cron"].is_string() {
                        task["enabled"] = false.into();
                        task["nextRun"] = Value::Null;
                    }
                }

                if task != previous {
                    db.put("tasks", &task)?;
                    db.audit("task.author_reconciled", &json!({ "taskId": task["id"] }))?;
                }
            }
            Ok(())
        })
        .await
}
