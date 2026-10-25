//! Plugin management metadata uses the same owner reads as the UI. Cursor pages
//! keep headroom for MCP's text and structured copies of each result.
use crate::{
    error::{Error, Result},
    installation_mcp,
    plugins::{connections, registry, requesters, resources},
    service::Service,
    store::Db,
    validation::text,
};
use serde_json::Value;
use std::sync::Arc;

const PAGE_BYTES: usize = 256 * 1024;
// One instance may retain 256 × 64 resource-agent UUIDs and 100 root IDs.
// Its indivisible row exceeds the ordinary page target, but fits below the code
// worker's 1 MiB result ceiling and the relay's duplicated-result envelope limit.
const MAX_VALUE_BYTES: usize = 768 * 1024;

fn bounded(mut value: Value) -> Result<Value> {
    if value["items"].is_array() {
        while serde_json::to_vec(&value)?.len() > PAGE_BYTES {
            let items = value["items"].as_array_mut().unwrap();
            if items.len() <= 1 {
                break;
            }
            items.pop();
            value["nextCursor"] = items.last().unwrap()["id"].clone();
        }
    }
    if serde_json::to_vec(&value)?.len() > MAX_VALUE_BYTES {
        return Err(Error::too_large(
            "The selected plugin metadata is too large to read.",
        ));
    }
    Ok(value)
}

pub(super) async fn invoke(
    s: &Arc<Service>,
    name: &str,
    args: Value,
    bearer: Option<&str>,
) -> Result<Value> {
    // Keep this turn's requester: a later turn or fresh approval cannot replace it.
    let requester = match bearer {
        Some(bearer) => {
            let run = installation_mcp::authorize(s, bearer).await?;
            requesters::for_run(s, &run).await?
        }
        None => None,
    };
    let cursor = args["cursor"].as_str();
    let result = match name {
        "list_plugin_packages" => registry::owner::list(s, cursor).await?,
        "get_plugin_package" => registry::owner::detail(s, text(&args, "pluginId")).await?,
        "list_plugin_instance_access" => connections::owner::access(s, cursor).await?,
        "list_plugin_agent_grant_counts" => {
            connections::owner::agent_grant_counts(s, cursor).await?
        }
        "list_plugin_resource_choices" => {
            resources::choices(
                s,
                text(&args, "pluginId"),
                text(&args, "instanceId"),
                text(&args, "collection"),
                cursor,
            )
            .await?
        }
        _ => return Err(Error::not_found("Unknown plugin metadata operation.")),
    };
    if let Some(requester) = &requester {
        requesters::recheck(s, requester).await?;
    }
    if let Some(bearer) = bearer {
        let bearer = bearer.to_owned();
        s.store
            .read(move |db| {
                // Execution and source authority must still agree in one snapshot.
                let transaction = rusqlite::Transaction::new_unchecked(
                    db.0,
                    rusqlite::TransactionBehavior::Deferred,
                )?;
                let db = &Db(&transaction);
                installation_mcp::authorize_in(db, &bearer)?;
                if let Some(requester) = &requester {
                    requesters::recheck_in(db, requester)?;
                }
                transaction.commit()?;
                Ok(())
            })
            .await?;
    }
    bounded(result)
}
