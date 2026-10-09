//! VM block transport; publication and durability belong to the shared Volume.
use super::{runtime::Volume, vhost};
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::{io, path::Path, sync::Arc};

pub enum MountedDisk {
    Vhost(vhost::MountedDisk),
    #[cfg(feature = "ublk")]
    Ublk(super::ublk::MountedDisk),
    #[cfg(feature = "ublk")]
    Paired {
        system: super::ublk::MountedDisk,
        workspace: super::ublk::MountedDisk,
    },
}

impl MountedDisk {
    pub fn drives(&self) -> Vec<Value> {
        match self {
            Self::Vhost(_) => vec![json!({
                "drive_id": "data",
                "socket": "disk.sock",
                "is_root_device": false,
                "cache_type": "Writeback"
            })],
            #[cfg(feature = "ublk")]
            Self::Ublk(_) => vec![native_drive("data", "disk.blk")],
            #[cfg(feature = "ublk")]
            Self::Paired { .. } => vec![
                native_drive("data", "disk.blk"),
                native_drive("conversation", "workspace.blk"),
            ],
        }
    }

    pub fn paired(&self) -> bool {
        #[cfg(feature = "ublk")]
        {
            matches!(self, Self::Paired { .. })
        }
        #[cfg(not(feature = "ublk"))]
        {
            false
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
            Self::Ublk(_) | Self::Paired { .. } => Ok(0),
        }
    }

    pub fn failed(&self) -> bool {
        match self {
            Self::Vhost(disk) => disk.failed(),
            #[cfg(feature = "ublk")]
            Self::Ublk(disk) => disk.failed(),
            #[cfg(feature = "ublk")]
            Self::Paired { system, workspace } => system.failed() || workspace.failed(),
        }
    }

    pub fn close(self) -> io::Result<()> {
        match self {
            Self::Vhost(disk) => disk.close(),
            #[cfg(feature = "ublk")]
            Self::Ublk(disk) => disk.close(),
            #[cfg(feature = "ublk")]
            Self::Paired { system, workspace } => {
                let first = system.close();
                let second = workspace.close();
                first.and(second)
            }
        }
    }
}

#[cfg(feature = "ublk")]
fn native_drive(id: &str, path: &str) -> Value {
    json!({
        "drive_id": id,
        "path_on_host": path,
        "is_root_device": false,
        "is_read_only": false,
        "cache_type": "Writeback",
        "io_engine": "Async"
    })
}

pub async fn mount(
    volume: Arc<Volume>,
    state: &Path,
    jail: &Path,
    uid: u32,
) -> Result<MountedDisk> {
    let selected = std::env::var("CAIRN_BLOCK_TRANSPORT").unwrap_or_else(|_| "vhost-user".into());
    let layout = volume.disk.layout()?;
    if selected == "vhost-user" {
        if layout != super::layout::Layout::Flat {
            return Err(Error::bad(
                "Paired disks require the native ublk transport.",
            ));
        }
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
        tokio::task::spawn_blocking(move || -> io::Result<MountedDisk> {
            let super::layout::Layout::Paired { system_bytes } = layout else {
                return super::ublk::mount_disk(volume, &state, &jail, uid).map(MountedDisk::Ublk);
            };
            let total = super::Disk::size(volume.as_ref());
            let system = Arc::new(super::DiskView::new(volume.clone(), 0, system_bytes)?);
            let workspace = Arc::new(super::DiskView::new(
                volume,
                system_bytes,
                total - system_bytes,
            )?);
            let system = super::ublk::mount_disk(system, &state, &jail, uid)?;
            let workspace =
                match super::ublk::mount_named_disk(workspace, &state, &jail, uid, "workspace.blk")
                {
                    Ok(workspace) => workspace,
                    Err(error) => {
                        // mount() precedes VMM creation. Retire the first device if
                        // the second cannot start; no VMM can have opened it yet.
                        if let Err(cleanup) = system.close() {
                            tracing::warn!(%cleanup, "Could not retire partial paired mount");
                        }
                        return Err(error);
                    }
                };
            Ok(MountedDisk::Paired { system, workspace })
        })
        .await
        .map_err(Error::internal)?
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
