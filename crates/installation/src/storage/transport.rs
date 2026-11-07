//! VM block transport; publication and durability belong to the shared Volume.
use super::runtime::Volume;
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::{io, path::Path, sync::Arc};

pub struct MountedDisk(super::ublk::MountedDisk);

impl MountedDisk {
    pub fn drives(&self) -> Vec<Value> {
        vec![native_drive("data", "disk.blk")]
    }

    pub fn failed(&self) -> bool {
        self.0.failed()
    }

    pub fn close(self) -> io::Result<()> {
        self.0.close()
    }
}

fn native_drive(id: &str, path: &str) -> Value {
    json!({
        "drive_id": id,
        "path_on_host": path,
        "is_root_device": false,
        "is_read_only": false,
        "cache_type": "Writeback",
        "io_engine": "Async",
        "discard": true
    })
}

pub async fn mount(
    volume: Arc<Volume>,
    state: &Path,
    jail: &Path,
    uid: u32,
) -> Result<MountedDisk> {
    let state = state.to_owned();
    let jail = jail.to_owned();
    tokio::task::spawn_blocking(move || {
        super::ublk::mount_disk(volume, &state, &jail, uid).map(MountedDisk)
    })
    .await
    .map_err(Error::internal)?
    .map_err(Into::into)
}

pub async fn cleanup_stale(state: &Path) -> Result<()> {
    {
        let state = state.to_owned();
        tokio::task::spawn_blocking(move || super::ublk::cleanup_stale(&state))
            .await
            .map_err(Error::internal)??;
    }
    Ok(())
}
