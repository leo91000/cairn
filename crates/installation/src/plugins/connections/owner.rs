//! Presentation reads select one package's instances before loading configuration.
//! Summaries describe saved grants and live state; they confer no execution access.
use super::KIND;
use crate::{
    config::now,
    error::{Error, Result},
    plugins::{paging, registry},
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use rusqlite::{Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use std::collections::HashMap;

/// One statement projects metadata rather than materializing every instance's
/// settings. Root and resource rules contribute distinct configured agent IDs,
/// including paused rules. The snapshot does not claim those grants are usable.
pub(crate) async fn summaries(s: &Service, packages: Value) -> Result<HashMap<String, Value>> {
    s.store
        .read(move |db| {
            let observed_at = now();
            let rows = db.json_rows(
                "WITH packages AS MATERIALIZED (
                    SELECT json_extract(value,'$.id') AS id,
                           json_extract(value,'$.needsWorker') AS worker FROM json_each(?1)
                 ), instances AS MATERIALIZED (
                    SELECT r.id, p.id AS plugin, json_extract(r.data,'$.agentIds') AS agent_ids,
                           CASE WHEN json_extract(r.data,'$.enabled') IS NOT 1 THEN 'paused'
                                WHEN p.worker=0 THEN 'ready'
                                ELSE coalesce(json_extract(k.data,'$.state'),'disconnected') END AS state
                    FROM records r JOIN packages p ON json_extract(r.data,'$.pluginId')=p.id
                    LEFT JOIN kv k ON k.key='plugin-status:'||r.id
                        AND (k.expires IS NULL OR k.expires>?2)
                    WHERE r.kind='plugin-connections'
                 ), agents AS (
                    SELECT i.plugin, a.value AS agent FROM instances i,
                        json_each(i.agent_ids) a WHERE a.type='text'
                    UNION
                    SELECT i.plugin, json_extract(g.data,'$.agentId') FROM records g
                        JOIN instances i ON json_extract(g.data,'$.connectionId')=i.id
                        WHERE g.kind='plugin-resource-grants'
                 )
                 SELECT json_object('id',p.id,'summary',json_object(
                    'instances',(SELECT count(*) FROM instances i WHERE i.plugin=p.id),
                    'states',json((SELECT json_group_object(state,n) FROM (
                        SELECT state,count(*) AS n FROM instances i WHERE i.plugin=p.id GROUP BY state))),
                    'pendingRequests',(SELECT count(*) FROM records r JOIN instances i
                        ON json_extract(r.data,'$.connectionId')=i.id
                        WHERE r.kind='plugin-requests' AND json_extract(r.data,'$.status')='pending'
                        AND i.plugin=p.id),
                    'authorizedAgents',(SELECT count(*) FROM agents a
                        WHERE a.plugin=p.id AND a.agent IS NOT NULL AND a.agent!=''),
                    'observedAt',?2)) FROM packages p",
                params![packages.to_string(), observed_at],
            )?;
            Ok(rows.into_iter().map(|row| (text(&row, "id").to_owned(), row["summary"].clone())).collect())
        })
        .await
}

/// Saved access for owner eligibility and labels, independent of package availability.
/// Project only metadata in SQL, without materializing settings in Rust or loading
/// credential and history records. SQLite still reads the underlying instance JSON.
/// Root grants and resource rules remain separate; neither implies an effective grant.
pub async fn access(s: &Service, cursor: Option<&str>) -> Result<Value> {
    if let Some(cursor) = cursor {
        uuid(cursor)?;
    }
    let cursor = cursor.unwrap_or("").to_owned();
    s.store
        .read(move |db| {
            let mut rows = db.json_rows(
                "SELECT json_object(
                    'id',r.id,'pluginId',json_extract(r.data,'$.pluginId'),
                    'name',coalesce(json_extract(r.data,'$.name'),''),
                    'enabled',json(CASE WHEN json_extract(r.data,'$.enabled')=1 THEN 'true' ELSE 'false' END),
                    'agentId',coalesce(json_extract(r.data,'$.agentId'),''),
                    'status',json(coalesce(k.data,'{\"state\":\"disconnected\"}')),
                    'agentIds',json(coalesce(json_extract(r.data,'$.agentIds'),'[]')),
                    'resourceAgentIds',json((SELECT json_group_array(agent) FROM (
                        SELECT DISTINCT json_extract(g.data,'$.agentId') AS agent
                        FROM records g WHERE g.kind='plugin-resource-grants'
                        AND json_extract(g.data,'$.connectionId')=r.id
                        AND json_type(g.data,'$.agentId')='text' ORDER BY agent))))
                 FROM records r LEFT JOIN kv k ON k.key='plugin-status:'||r.id
                    AND (k.expires IS NULL OR k.expires>?2)
                 WHERE r.kind='plugin-connections' AND r.id>?1
                 ORDER BY r.id LIMIT ?3",
                params![cursor, now(), (paging::MAX_ITEMS + 1) as i64],
            )?;
            for row in &mut rows {
                // Bundled names require no registry scan. An unavailable/custom package
                // is identified honestly by its stable ID, never by a fabricated manifest.
                let plugin = text(row, "pluginId");
                let name = registry::builtin(plugin)
                    .map_or_else(|| plugin.to_owned(), |package| package.manifest.name);
                row["pluginName"] = json!(name);
            }
            paging::ordered_page(rows, "id", paging::MAX_ITEMS, paging::MAX_BYTES)
        })
        .await
}

/// Totals count saved rules even when their instance, resource or package is
/// unavailable. They are presentation metadata, never effective authorization.
pub async fn agent_grant_counts(s: &Service, cursor: Option<&str>) -> Result<Value> {
    if let Some(cursor) = cursor {
        uuid(cursor)?;
    }
    let cursor = cursor.unwrap_or("").to_owned();
    s.store
        .read(move |db| {
            let rows = db.json_rows(
                "SELECT json_object('id',json_extract(data,'$.agentId'),'count',count(*))
                 FROM records WHERE kind='plugin-resource-grants'
                 AND json_type(data,'$.agentId')='text' AND json_extract(data,'$.agentId')>?1
                 GROUP BY json_extract(data,'$.agentId') ORDER BY json_extract(data,'$.agentId')
                 LIMIT ?2",
                params![cursor, (paging::MAX_ITEMS + 1) as i64],
            )?;
            paging::ordered_page(rows, "id", paging::MAX_ITEMS, paging::MAX_BYTES)
        })
        .await
}

/// No approval is required to inspect or delete an already saved instance. A
/// missing package has no ready override; failed filesystem reads remain errors.
async fn needs_worker(s: &Service, plugin: &str) -> Result<Option<bool>> {
    match registry::owner::detail(s, plugin).await {
        Ok(detail) => {
            let manifest: Option<cairn_plugin_api::Manifest> =
                serde_json::from_value(detail["manifest"].clone())?;
            Ok(manifest.map(|manifest| manifest.needs_worker()))
        }
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error),
    }
}

fn enrich(
    db: &Db<'_>,
    mut instance: Value,
    worker: Option<bool>,
    observed_at: i64,
) -> Result<Value> {
    let id = text(&instance, "id").to_owned();
    instance["status"] = if worker == Some(false) && instance["enabled"] == true {
        json!({ "state": "ready" })
    } else {
        db.kv(&format!("plugin-status:{id}"))?
            .unwrap_or(json!({ "state": "disconnected" }))
    };
    instance["usage"] = json!({
        "hour": db.kv(&format!("plugin-rate:{id}:{}", observed_at / 3_600_000))?
            .and_then(|value| value.as_u64()).unwrap_or(0),
    });
    // The admission limit is 50 pending requests per instance, including routes.
    // Do not silently truncate requests used by owner approval controls.
    instance["requests"] = json!(db.json_rows(
        "SELECT data FROM records WHERE kind='plugin-requests'
         AND json_extract(data,'$.connectionId')=? AND json_extract(data,'$.status')='pending'
         ORDER BY json_extract(data,'$.updatedAt') DESC,id",
        [&id],
    )?);
    instance["activity"] = json!(db.json_rows(
        "SELECT data FROM records WHERE kind='plugin-activity' AND json_extract(data,'$.connectionId')=?
         ORDER BY json_extract(data,'$.createdAt') DESC,id LIMIT 8",
        [&id],
    )?);
    Ok(instance)
}

/// Settings and live adornments are limited by record count and encoded bytes.
/// SQL selects the package and cursor before decoding/enriching the lookahead.
pub async fn list(s: &Service, plugin: &str, cursor: Option<&str>) -> Result<Value> {
    if let Some(cursor) = cursor {
        uuid(cursor)?;
    }
    let worker = needs_worker(s, plugin).await?;
    let plugin = plugin.to_owned();
    let cursor = cursor.unwrap_or("").to_owned();
    s.store
        .read(move |db| {
            let transaction = Transaction::new_unchecked(db.0, TransactionBehavior::Deferred)?;
            let db = &Db(&transaction);
            let observed_at = now();
            let records = db.json_rows(
                "SELECT data FROM records WHERE kind=? AND json_extract(data,'$.pluginId')=?
             AND id>? ORDER BY id LIMIT ?",
                params![KIND, plugin, cursor, (paging::MAX_ITEMS + 1) as i64],
            )?;
            let values = records
                .into_iter()
                .map(|instance| enrich(db, instance, worker, observed_at))
                .collect::<Result<Vec<_>>>()?;
            let mut page =
                paging::ordered_page(values, "id", paging::MAX_ITEMS, paging::MAX_BYTES)?;
            page["instanceReads"] = json!(true);
            Ok(page)
        })
        .await
}

/// Full current IDs are needed when merging concurrent agent grants on save.
pub async fn get(s: &Service, plugin: &str, instance: &str) -> Result<Value> {
    uuid(instance)?;
    let worker = needs_worker(s, plugin).await?;
    let (plugin, instance) = (plugin.to_owned(), instance.to_owned());
    s.store
        .read(move |db| {
            let transaction = Transaction::new_unchecked(db.0, TransactionBehavior::Deferred)?;
            let db = &Db(&transaction);
            let value = db
                .get(KIND, &instance)?
                .filter(|value| value["pluginId"] == plugin)
                .ok_or_else(|| Error::not_found("Plugin connection not found."))?;
            let value = enrich(db, value, worker, now())?;
            if serde_json::to_vec(&value)?.len() > paging::MAX_BYTES {
                return Err(Error::too_large("The saved instance is too large to read."));
            }
            Ok(value)
        })
        .await
}
