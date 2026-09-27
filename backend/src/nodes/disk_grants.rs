//! Durable read grants pin the mounted base until the controller acknowledges a newer one.
use crate::{
    error::{Error, Result},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
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
        || (run["nodeId"] != node && run["moveRequest"]["nodeId"] != node)
        || (run["status"] == "running" && !run["cancelRequestedAt"].is_null())
        || !crate::service::allowed(&crate::service::policy(&agent)["nodes"], node)
    {
        return Err(Error::new(403, "Disk ownership or authorization changed."));
    }
    Ok(Some(grant))
}
pub async fn extend(s: &Service, run: &str, node: &str, backup: &str) -> Result<()> {
    let (run, node, backup) = (run.to_owned(), node.to_owned(), backup.to_owned());
    s.store
        .transaction(move |db| {
            for mut grant in db.list("node-disk-grants")? {
                if grant["runId"] == run && grant["nodeId"] == node {
                    let ids = grant["backups"]
                        .as_array_mut()
                        .ok_or_else(|| Error::bad("Invalid disk grant."))?;
                    if !ids.iter().any(|v| v == &backup) {
                        ids.push(backup.clone().into());
                    }
                    db.put("node-disk-grants", &grant)?;
                }
            }
            Ok(())
        })
        .await
}
pub async fn acknowledged(s: &Service, run: &str, node: &str, backup: &str) -> Result<()> {
    let (run, node, backup) = (run.to_owned(), node.to_owned(), backup.to_owned());
    s.store
        .transaction(move |db| {
            for mut grant in db.list("node-disk-grants")? {
                if grant["runId"] == run && grant["nodeId"] == node {
                    grant["backups"] = json!([backup]);
                    db.put("node-disk-grants", &grant)?;
                }
            }
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
