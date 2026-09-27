//! Durable read grants pin the mounted base until the controller acknowledges a newer one.
use crate::{
    error::{Error, Result},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
pub async fn new_disk(s: &Service, run: &Value, node: &str) -> Result<String> {
    let token = crate::auth::token();
    s.store
        .put(
            "node-disk-grants",
            json!({"id":crate::auth::digest(&token),"runId":run["id"],"nodeId":node,"backups":[]}),
        )
        .await?;
    Ok(token)
}
pub async fn issue(s: &Service, run: &Value, node: &str, backup: &Value) -> Result<String> {
    if backup["destination"] != "s3" {
        return Err(Error::new(
            409,
            "On-demand disks require a verified S3 recovery point.",
        ));
    }
    let token = crate::auth::token();
    s.store.put("node-disk-grants",json!({"id":crate::auth::digest(&token),"runId":run["id"],"nodeId":node,"backups":[backup["id"]]})).await?;
    Ok(token)
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
    let agent = s
        .get("agents", text(&run["snapshot"]["agent"], "id"))
        .await?;
    if s.get("nodes", node).await?["revoked"] == true
        || (run["nodeId"].as_str().unwrap_or(super::LOCAL_NODE_ID) != node
            && run["moveRequest"]["nodeId"] != node)
        || (run["status"] == "running" && !run["cancelRequestedAt"].is_null())
        || !crate::service::allowed(&crate::service::policy(&agent)["nodes"], node)
    {
        return Err(Error::new(403, "Disk ownership or authorization changed."));
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
                .ok_or_else(|| Error::new(403, "Disk read grant missing."))?;
            if grant["runId"] != backup["runId"]
                || grant["nodeId"].as_str().unwrap_or(super::LOCAL_NODE_ID)
                    != backup["nodeId"].as_str().unwrap_or(super::LOCAL_NODE_ID)
            {
                return Err(Error::new(
                    403,
                    "Publication belongs to another disk owner.",
                ));
            }
            if acknowledged {
                grant["backups"] = json!([backup["id"]]);
            } else {
                let ids = grant["backups"]
                    .as_array_mut()
                    .ok_or_else(|| Error::bad("Invalid disk grant."))?;
                if !ids.contains(&backup["id"]) {
                    ids.push(backup["id"].clone());
                }
            }
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
