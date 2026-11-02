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
async fn grant(
    s: &Service,
    run: &Value,
    node: &str,
    backups: Vec<&Value>,
    authorize: crate::authorization::Authorization,
) -> Result<String> {
    authorize.recheck().await?;
    let token = crate::auth::token();
    let grant = NewGrant {
        id: crate::auth::digest(&token),
        run_id: &run["id"],
        node_id: node,
        backups,
    };
    let grant = serde_json::to_value(grant)?;
    s.store
        .transaction(move |db| {
            authorize.check_local(db)?;
            db.put("node-disk-grants", &grant).map(|_| ())
        })
        .await?;
    Ok(token)
}

pub async fn new_disk(s: &Service, run: &Value, node: &str) -> Result<String> {
    grant(
        s,
        run,
        node,
        Vec::new(),
        crate::authorization::Authorization::default(),
    )
    .await
}

pub async fn issue(s: &Service, run: &Value, node: &str, backup: &Value) -> Result<String> {
    issue_authorized(
        s,
        run,
        node,
        backup,
        crate::authorization::Authorization::default(),
    )
    .await
}

pub(crate) async fn issue_authorized(
    s: &Service,
    run: &Value,
    node: &str,
    backup: &Value,
    authorize: crate::authorization::Authorization,
) -> Result<String> {
    if !super::publication::in_s3(backup) {
        return Err(Error::conflict(
            "On-demand disks require a verified S3 recovery point.",
        ));
    }
    grant(s, run, node, vec![&backup["id"]], authorize).await
}

pub async fn authorize(s: &Service, credential: &str) -> Result<Option<Value>> {
    authorize_id(s, &crate::auth::digest(credential)).await
}

pub(crate) async fn authorize_id(s: &Service, grant_id: &str) -> Result<Option<Value>> {
    // Refresh the external native-policy observation before the transactional
    // authorization. Every actual grant/run/node/agent check uses one DB view.
    if let Some(grant) = s.store.get("node-disk-grants", grant_id).await?
        && s.store.run(text(&grant, "runId")).await?["backendMigration"]["sourceReadReady"] == true
    {
        crate::native::policy::refresh(s).await?;
    }
    let grant_id = grant_id.to_owned();
    s.store
        .read(move |db| {
            let tx = rusqlite::Transaction::new_unchecked(
                db.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let grant = authorize_id_db(&crate::store::Db(&tx), &grant_id)?;
            tx.commit()?;
            Ok(grant)
        })
        .await
}

pub(crate) fn authorize_id_db(db: &crate::store::Db<'_>, grant_id: &str) -> Result<Option<Value>> {
    let grant = db.get("node-disk-grants", grant_id)?;
    let Some(grant) = grant else { return Ok(None) };
    let mut run = crate::error::required(db.run(text(&grant, "runId"))?, "Conversation missing.")?;
    let node = text(&grant, "nodeId");
    if run["backendMigration"]["nodeId"] == node
        && run["backendMigration"]["convertedBackup"].is_string()
        && grant["backups"]
            .as_array()
            .is_some_and(|points| points.contains(&run["backendMigration"]["convertedBackup"]))
    {
        run = crate::backend_migration::candidate(&run);
    }
    authorize_owner(db, &run, node)?;
    Ok(Some(grant))
}

pub(crate) fn authorize_owner(db: &crate::store::Db<'_>, run: &Value, node: &str) -> Result<()> {
    let agent = crate::error::required(
        db.agent_snapshot(super::run_agent(run))?,
        "Agent not found.",
    )?;
    let record = crate::error::required(db.node_boundary(node)?, "Node not found.")?;
    let source_allowed = crate::backend_migration::source_read(db, text(run, "id"), &record)?;
    let native_allowed = source_allowed
        || !crate::native::policy::node_native(&record)
        || crate::native::policy::allows(db, run, &record)?;
    let revoked = record["revoked"] == true;
    let owner = super::owner_node(run) == node
        || run["moveRequest"]["nodeId"] == node
        || run["backendMigration"]["nodeId"] == node;
    let cancelling = run["status"] == RunStatus::Running && !run["cancelRequestedAt"].is_null();
    if !native_allowed
        || revoked
        || !owner
        || cancelling
        || (!source_allowed && !super::agent_allows(&agent, node))
    {
        return Err(Error::forbidden("Disk ownership or authorization changed."));
    }
    Ok(())
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
    let mut points: std::collections::HashSet<String> = s
        .store
        .list("node-disk-grants")
        .await?
        .into_iter()
        .filter(|g| g["runId"] == run)
        .flat_map(|g| g["backups"].as_array().cloned().unwrap_or_default())
        .filter_map(|id| id.as_str().map(str::to_owned))
        .collect();
    let run = run.to_owned();
    let converted = s
        .store
        .read(move |db| {
            Ok(db.run(&run)?.and_then(|run| {
                run["backendMigration"]["convertedBackup"]
                    .as_str()
                    .map(str::to_owned)
            }))
        })
        .await?;
    if let Some(point) = converted {
        points.insert(point);
    }
    Ok(points)
}
