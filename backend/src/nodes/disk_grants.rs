//! Durable read grants pin the mounted base until the controller acknowledges a newer one.
use crate::{
    error::{Error, Result},
    run_status::RunStatus,
    service::Service,
    validation::text,
};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NewGrant<'a> {
    id: String,
    run_id: &'a Value,
    node_id: &'a str,
    backups: Vec<&'a Value>,
}

/// A publication the controller may read before acknowledging it.
#[derive(Serialize)]
struct Pending<'a> {
    id: &'a Value,
    generation: i64,
}

/// Stores a grant readable with the returned credential.
async fn grant(s: &Service, run: &Value, node: &str, backups: Vec<&Value>) -> Result<String> {
    let token = crate::auth::token();
    let grant = NewGrant {
        id: crate::auth::digest(&token),
        run_id: &run["id"],
        node_id: node,
        backups,
    };
    s.store
        .put("node-disk-grants", serde_json::to_value(grant)?)
        .await?;
    Ok(token)
}

pub async fn new_disk(s: &Service, run: &Value, node: &str) -> Result<String> {
    grant(s, run, node, Vec::new()).await
}

pub async fn issue(s: &Service, run: &Value, node: &str, backup: &Value) -> Result<String> {
    if !super::publication::in_s3(backup) {
        return Err(Error::conflict(
            "On-demand disks require a verified S3 recovery point.",
        ));
    }
    grant(s, run, node, vec![&backup["id"]]).await
}

pub async fn authorize(s: &Service, credential: &str) -> Result<Option<Value>> {
    let grant = s
        .store
        .read({
            let id = crate::auth::digest(credential);
            move |db| db.get("node-disk-grants", &id)
        })
        .await?;
    let Some(grant) = grant else { return Ok(None) };
    let run = s.store.run(text(&grant, "runId")).await?;
    let node = text(&grant, "nodeId");
    let agent = s.get("agents", super::run_agent(&run)).await?;
    let revoked = s.get("nodes", node).await?["revoked"] == true;
    let owner = super::owner_node(&run) == node || run["moveRequest"]["nodeId"] == node;
    let cancelling = run["status"] == RunStatus::Running && !run["cancelRequestedAt"].is_null();
    if revoked || !owner || cancelling || !super::agent_allows(&agent, node) {
        return Err(Error::forbidden("Disk ownership or authorization changed."));
    }
    Ok(Some(grant))
}

/// Before committing the local journal, authorize the candidate beside its old base.
pub async fn extend(s: &Service, grant_id: &str, backup: &Value) -> Result<()> {
    update(s, grant_id, backup, false).await
}

/// Retire only this disk instance's old base; stale disks retain their own pins.
pub async fn acknowledged(s: &Service, grant_id: &str, backup: &Value) -> Result<()> {
    update(s, grant_id, backup, true).await
}

async fn update(s: &Service, grant_id: &str, backup: &Value, acknowledged: bool) -> Result<()> {
    let (grant_id, backup) = (grant_id.to_owned(), backup.clone());
    s.store
        .transaction(move |db| {
            let mut grant = db
                .get("node-disk-grants", &grant_id)?
                .ok_or_else(|| Error::forbidden("Disk read grant missing."))?;
            if grant["runId"] != backup["runId"]
                || super::owner_node(&grant) != super::owner_node(&backup)
            {
                return Err(Error::forbidden(
                    "Publication belongs to another disk owner.",
                ));
            }
            let generation = backup["diskGeneration"]
                .as_i64()
                .filter(|g| *g > 0)
                .ok_or_else(|| Error::bad("Missing journal publication generation."))?;
            let current = grant["generation"].as_i64().unwrap_or(0);
            if generation < current {
                return Ok(());
            }
            if generation == current {
                if grant["published"] == backup["id"] {
                    return Ok(());
                }
                return Err(Error::conflict(
                    "Conflicting journal publication generation.",
                ));
            }
            let mut pending = grant["pending"].as_array().cloned().unwrap_or_default();
            if acknowledged {
                // A delayed receipt must preserve newer candidates whose blocks
                // the controller may begin reading before their own ack arrives.
                pending
                    .retain(|point| point["generation"].as_i64().is_some_and(|g| g > generation));
                let mut ids = vec![backup["id"].clone()];
                ids.extend(pending.iter().map(|point| point["id"].clone()));
                grant["generation"] = generation.into();
                grant["published"] = backup["id"].clone();
                grant["backups"] = ids.into();
            } else {
                let ids = grant["backups"]
                    .as_array_mut()
                    .ok_or_else(|| Error::bad("Invalid disk grant."))?;
                if !ids.contains(&backup["id"]) {
                    ids.push(backup["id"].clone());
                }
                if !pending.iter().any(|point| point["id"] == backup["id"]) {
                    pending.push(serde_json::to_value(Pending {
                        id: &backup["id"],
                        generation,
                    })?);
                }
            }
            grant["pending"] = pending.into();
            db.put("node-disk-grants", &grant)?;
            Ok(())
        })
        .await
}

pub async fn pinned(s: &Service, run: &str) -> Result<std::collections::HashSet<String>> {
    Ok(s.store
        .list("node-disk-grants")
        .await?
        .into_iter()
        .filter(|g| g["runId"] == run)
        .flat_map(|g| g["backups"].as_array().cloned().unwrap_or_default())
        .filter_map(|id| id.as_str().map(str::to_owned))
        .collect())
}
