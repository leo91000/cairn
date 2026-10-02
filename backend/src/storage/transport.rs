//! VM block transport; publication and durability belong to the shared Volume.
use super::{runtime::Volume, vhost};
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::{io, path::Path, sync::Arc};

pub enum MountedDisk {
    Vhost(vhost::MountedDisk),
    #[cfg(feature = "ublk")]
    Ublk(super::ublk::MountedDisk),
}

impl MountedDisk {
    pub fn drive(&self) -> Value {
        match self {
            Self::Vhost(_) => json!({
                "drive_id": "data",
                "socket": "disk.sock",
                "is_root_device": false,
                "cache_type": "Writeback"
            }),
            #[cfg(feature = "ublk")]
            Self::Ublk(_) => json!({
                "drive_id": "data",
                "path_on_host": "disk.blk",
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
                "io_engine": "Async"
            }),
        }
    }

    pub fn vhost(&self) -> bool {
        matches!(self, Self::Vhost(_))
    }

    pub fn allocated_memory_bytes(&self) -> io::Result<u64> {
        match self {
            Self::Vhost(disk) => disk.allocated_memory_bytes(),
            // Native virtio-blk does not require another shared-RAM mapping.
            #[cfg(feature = "ublk")]
            Self::Ublk(_) => Ok(0),
        }
    }

    pub fn failed(&self) -> bool {
        match self {
            Self::Vhost(disk) => disk.failed(),
            #[cfg(feature = "ublk")]
            Self::Ublk(disk) => disk.failed(),
        }
    }

    pub fn close(self) -> io::Result<()> {
        match self {
            Self::Vhost(disk) => disk.close(),
            #[cfg(feature = "ublk")]
            Self::Ublk(disk) => disk.close(),
        }
    }
}

pub async fn mount(
    volume: Arc<Volume>,
    state: &Path,
    jail: &Path,
    uid: u32,
) -> Result<MountedDisk> {
    let selected = std::env::var("LEO_BLOCK_TRANSPORT").unwrap_or_else(|_| "vhost-user".into());
    if selected == "vhost-user" {
        return Ok(MountedDisk::Vhost(vhost::mount_disk(
            volume,
            &jail.join("disk.sock"),
            uid,
        )?));
    }
    if selected != "ublk" {
        return Err(Error::bad("Unsupported VM block transport."));
    }
    #[cfg(feature = "ublk")]
    {
        let state = state.to_owned();
        let jail = jail.to_owned();
        tokio::task::spawn_blocking(move || super::ublk::mount_disk(volume, &state, &jail, uid))
            .await
            .map_err(Error::internal)?
            .map(MountedDisk::Ublk)
            .map_err(Into::into)
    }
    #[cfg(not(feature = "ublk"))]
    {
        let _ = state;
        Err(Error::unavailable(
            "This node was built without ublk support.",
        ))
    }
}

pub async fn cleanup_stale(state: &Path) -> Result<()> {
    #[cfg(feature = "ublk")]
    {
        let state = state.to_owned();
        tokio::task::spawn_blocking(move || super::ublk::cleanup_stale(&state))
            .await
            .map_err(Error::internal)??;
    }
    #[cfg(not(feature = "ublk"))]
    if state.join("ublk-devices").exists()
        && std::fs::read_dir(state.join("ublk-devices"))?
            .next()
            .is_some()
    {
        return Err(Error::conflict(
            "This node requires ublk support to recover its owned devices.",
        ));
    }
    Ok(())
}
