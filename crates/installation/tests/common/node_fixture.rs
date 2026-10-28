//! Complete current node setup for neighboring domain tests, without records writes.
#![allow(dead_code)]

use cairn_contracts::ExecutionNode;
use cairn_installation::{
    config::{id, now},
    error::Result,
    store::{Db, Store},
};
use serde_json::{Value, json};

pub fn node(mut value: Value) -> Result<ExecutionNode> {
    let mut complete = json!({
        "id": id(), "name":"Fixture node", "revision": id(), "createdAt":now(),
        "capabilities":{"os":"linux","arch":"x86_64","backend":"firecracker","kvm":true,"cpu":8,"memoryMiB":16384,"diskMiB":102400},
        "storage":cairn_contracts::NodeStoragePolicy::default(), "staleDisks":cairn_contracts::StaleDisks::default(),
        "limits":{"cpu":4,"memoryMiB":8192,"diskMiB":81920}
    });
    for field in ["capabilities", "limits", "storage", "staleDisks"] {
        if let Some(patch) = value.get_mut(field) {
            let mut defaults = complete[field].clone();
            cairn_installation::store::merge(&mut defaults, patch);
            *patch = defaults;
        }
    }
    cairn_installation::store::merge(&mut complete, &value);
    if let Some(object) = complete.as_object_mut() {
        object.remove("exclusiveAgents");
        object.remove("releasedPlacements");
        object.remove("sharedResources");
    }
    serde_json::from_value(complete)
        .map_err(|e| cairn_installation::error::Error::bad(format!("Invalid node fixture: {e}")))
}

pub trait StoreNodeFixture {
    fn put_node_fixture(&self, value: Value) -> impl std::future::Future<Output = Result<Value>>;
}

impl StoreNodeFixture for Store {
    async fn put_node_fixture(&self, value: Value) -> Result<Value> {
        let node = node(value)?;
        self.transaction(move |db| Ok(serde_json::to_value(db.save_node(&node)?)?))
            .await
    }
}

pub trait DbNodeFixture {
    fn put_node_fixture(&self, value: &Value) -> Result<Value>;
}

impl DbNodeFixture for Db<'_> {
    fn put_node_fixture(&self, value: &Value) -> Result<Value> {
        Ok(serde_json::to_value(
            self.save_node(&node(value.clone())?)?,
        )?)
    }
}

pub async fn configuration(
    s: &cairn_installation::service::Service,
    path: &str,
    mut body: Value,
) -> Result<Value> {
    let parts: Vec<_> = path.trim_start_matches("/api/").split('/').collect();
    if parts.len() == 2
        && parts[0] == "nodes"
        && body.is_object()
        && body.get("revision").is_none()
        && let Some(current) = s.store.node(parts[1]).await?
    {
        body["revision"] = current.revision.into();
    }
    Ok(body)
}
