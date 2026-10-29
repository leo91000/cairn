//! Complete current node setup for neighboring domain tests, without records writes.
#![allow(dead_code)]

use cairn_contracts::ExecutionNode;
use cairn_installation::{
    config::{id, now},
    error::Result,
    store::{Db, Store},
};
use serde_json::{Value, json};

/// Serve the shipped node-side transfer through synthetic snapshot controllers.
/// The fixture has a raw disk, while real nodes read their immutable journal prefix.
pub async fn direct_snapshot_upload(
    disk: &std::path::Path,
    mut manifest: Value,
    body: &[u8],
    corrupt: bool,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let root = tempfile::tempdir().unwrap();
    if corrupt {
        let mut bytes = tokio::fs::read(disk).await.unwrap();
        let plan: Value = serde_json::from_slice(body).unwrap();
        for object in plan["objects"].as_array().unwrap() {
            let block = manifest["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|block| block["hash"] == object["read"]["hash"])
                .unwrap();
            let offset = block["offset"].as_u64().unwrap() as usize;
            bytes[offset] ^= 255;
        }
        tokio::fs::write(root.path().join("disk"), bytes)
            .await
            .unwrap();
    } else {
        tokio::fs::hard_link(disk, root.path().join("disk"))
            .await
            .unwrap();
    }
    manifest["onDemand"] = false.into();
    tokio::fs::write(root.path().join("manifest.json"), manifest.to_string())
        .await
        .unwrap();
    // Synthetic raw-file controllers create a separate local file per batch.
    // Actual session ownership/replay is tested through real journal routes.
    let mut plan: Value = serde_json::from_slice(body).unwrap();
    plan["requireReader"] = false.into();
    match cairn_installation::nodes::snapshots::upload_direct(
        root.path(),
        &serde_json::to_vec(&plan).unwrap(),
    )
    .await
    {
        Ok(receipt) => axum::Json(receipt).into_response(),
        Err(error) => error.into_response(),
    }
}

/// Metadata-only session stub for raw-file controller fixtures. The real node
/// owns the journal through this response; these fixtures have no live journal.
type ReaderStub = (tokio_util::sync::CancellationToken, std::sync::Weak<()>);

type ReaderStubs = std::sync::Mutex<std::collections::HashMap<String, ReaderStub>>;

fn direct_readers() -> &'static ReaderStubs {
    static READERS: std::sync::OnceLock<ReaderStubs> = std::sync::OnceLock::new();
    READERS.get_or_init(Default::default)
}

pub fn direct_reader_response(path: &str) -> axum::response::Response {
    use axum::response::IntoResponse;
    let lifetime = std::sync::Arc::new(());
    let close = tokio_util::sync::CancellationToken::new();
    {
        let mut entries = direct_readers().lock().unwrap();
        entries.retain(|_, (_, lifetime)| lifetime.strong_count() > 0);
        entries.insert(
            path.to_owned(),
            (close.clone(), std::sync::Arc::downgrade(&lifetime)),
        );
    }
    let stream = futures_util::stream::unfold((close, lifetime), |(close, lifetime)| async move {
        tokio::select! {
            () = close.cancelled() => None,
            () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                Some((Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b" ")), (close, lifetime)))
            }
        }
    });
    axum::body::Body::from_stream(stream).into_response()
}

pub fn direct_reader_closed(path: &str) -> axum::response::Response {
    use axum::response::IntoResponse;
    if let Some((close, _)) = direct_readers().lock().unwrap().remove(path) {
        close.cancel();
    }
    axum::Json(json!({ "closed": true })).into_response()
}

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
