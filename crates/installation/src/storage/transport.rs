//! VM block transport; publication and durability belong to the shared Volume.
use super::runtime::Volume;
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::{io, path::Path, sync::Arc};

pub enum MountedDisk {
    Ublk(super::ublk::MountedDisk),

    Paired {
        system: super::ublk::MountedDisk,
        workspace: super::ublk::MountedDisk,
    },
}

impl MountedDisk {
    pub fn drives(&self) -> Vec<Value> {
        match self {
            Self::Ublk(_) => vec![native_drive("data", "disk.blk")],

            Self::Paired { .. } => vec![
                native_drive("data", "disk.blk"),
                native_drive("conversation", "workspace.blk"),
            ],
        }
    }

    pub fn paired(&self) -> bool {
        matches!(self, Self::Paired { .. })
    }

    pub fn failed(&self) -> bool {
        match self {
            Self::Ublk(disk) => disk.failed(),
            Self::Paired { system, workspace } => system.failed() || workspace.failed(),
        }
    }

    pub fn close(self) -> io::Result<()> {
        match self {
            Self::Ublk(disk) => disk.close(),
            Self::Paired { system, workspace } => {
                let first = system.close();
                let second = workspace.close();
                first.and(second)
            }
        }
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
    validate_transport(std::env::var("CAIRN_BLOCK_TRANSPORT").ok().as_deref())?;
    let layout = volume.disk.layout()?;

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
            match super::ublk::mount_named_disk(workspace, &state, &jail, uid, "workspace.blk") {
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

pub async fn cleanup_stale(state: &Path) -> Result<()> {
    {
        let state = state.to_owned();
        tokio::task::spawn_blocking(move || super::ublk::cleanup_stale(&state))
            .await
            .map_err(Error::internal)??;
    }
    Ok(())
}

/// An unset value uses the sole supported transport. Reject stale manual
/// selections before opening a disk; host supervisors migrate their config.
pub fn validate_transport(selected: Option<&str>) -> Result<()> {
    match selected {
        None | Some("ublk") => Ok(()),
        _ => Err(Error::bad(
            "VM storage requires ublk; update the node configuration.",
        )),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_ublk_is_supported_and_is_the_default() {
        assert!(super::validate_transport(None).is_ok());
        assert!(super::validate_transport(Some("ublk")).is_ok());
        assert!(super::validate_transport(Some("vhost-user")).is_err());
        assert!(super::validate_transport(Some("unknown")).is_err());
    }
}
