//! New empty ext4 images become journal-backed before any user command runs.
use super::Disk;
use crate::{
    error::{Error, Result},
    validation::text,
};
use serde_json::Value;
use std::path::Path;

pub(crate) const DISK_BUSY: &str = "VM disk is active.";

pub async fn prepare(
    directory: &Path,
    size: u64,
    context: &Value,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    prepare_source(directory, size, context, stop, false, false, None).await
}

/// Native host workspaces have one ext4 filesystem and no guest system view.
pub(crate) async fn prepare_native(
    directory: &Path,
    size: u64,
    context: &Value,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    prepare_source(directory, size, context, stop, false, true, None).await
}

/// Build an anonymous, entirely local disk without minting or borrowing any
/// conversation's S3 authority. Only prepared environments may use this path.
pub async fn prepare_unassigned(
    directory: &Path,
    size: u64,
    policy: &super::policy::Policy,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    if directory
        .parent()
        .and_then(Path::file_name)
        .is_none_or(|name| name != "environments")
        || super::runtime::exists(directory)
    {
        return Err(Error::conflict(
            "Anonymous preparation requires a new environment disk.",
        ));
    }
    crate::validation::uuid(
        directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(""),
    )?;
    let context = serde_json::json!({ "unassigned": true, "policy": policy });
    prepare_source(directory, size, &context, stop, true, false, None).await
}

/// Populate and seal an anonymous native base before it can be cloned.
pub(crate) async fn prepare_native_base(
    directory: &Path,
    size: u64,
    contents: &Path,
    identity: (u32, u32),
    policy: &super::policy::Policy,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    let context = serde_json::json!({ "unassigned": true, "policy": policy });
    prepare_source(
        directory,
        size,
        &context,
        stop,
        true,
        true,
        Some((contents, identity)),
    )
    .await
}

async fn prepare_source(
    directory: &Path,
    size: u64,
    context: &Value,
    stop: &tokio_util::sync::CancellationToken,
    prepared: bool,
    reuse_existing: bool,
    seed: Option<(&Path, (u32, u32))>,
) -> Result<()> {
    crate::skills::private_dir(directory).await?;
    let _lock = crate::file_lock::exclusive(&directory.join("lock"), DISK_BUSY)?;
    let marker = directory.join("bootstrap.pending");
    let resize_source = directory.join("resize-source");
    let mut context = context.clone();
    let mut generation = 1;
    let mut resizing = false;
    if reuse_existing
        && !prepared
        && seed.is_none()
        && super::runtime::exists(directory)
        && !marker.exists()
        && !resize_source.exists()
    {
        let metadata = directory.join("lazy");
        let geometry = tokio::task::spawn_blocking(move || super::LazyDisk::geometry(&metadata))
            .await
            .map_err(Error::internal)??;
        if geometry == size {
            return Ok(());
        }
    }
    if resize_source.exists() && !super::runtime::exists(directory) {
        // Never continue from a raw image that resize2fs may have only partly
        // changed. The original journal stays authoritative until installation.
        tokio::fs::rename(&resize_source, directory.join("lazy")).await?;
        tokio::fs::File::open(directory).await?.sync_all().await?;
    }
    if super::runtime::exists(directory) {
        if marker.exists() {
            // A crash after the atomic install must still reclaim the empty source.
            let _volume = super::runtime::load(directory).await?;
            if directory.join("data.ext4").exists() {
                tokio::fs::remove_file(directory.join("data.ext4")).await?;
            }
            tokio::fs::remove_file(&marker).await?;
            tokio::fs::File::open(directory).await?.sync_all().await?;
        }
        if resize_source.exists() {
            let _volume = super::runtime::load(directory).await?;
            tokio::fs::remove_dir_all(&resize_source).await?;
        }
        let volume = super::runtime::load(directory).await?;
        let current = volume.disk.size();
        drop(volume);
        if size < current {
            return Err(Error::conflict("A retained VM disk cannot be shrunk."));
        }
        if size == current {
            return Ok(());
        }
        let (previous, next_generation) = super::runtime::rebuild_identity(directory).await?;
        context["grant"] = previous["grant"].clone();
        generation = next_generation;
        // Resize tools still require a local ext4 image. Mark the transition
        // before exporting so a restart can finish rebuilding the journal.
        crate::skills::atomic_write(&marker, b"resizing").await?;
        super::runtime::materialize(directory, stop).await?;
        resizing = true;
    }
    if directory.join("data.ext4").exists() && !marker.exists() {
        return Err(Error::conflict(
            "This conversation still has a legacy local disk and cannot start with S3-backed storage.",
        ));
    }
    let policy: super::policy::Policy = serde_json::from_value(context["policy"].clone())?;
    policy.validate()?;
    let (total, free) = super::policy::space(directory)?;
    if free <= policy.reserve(total) + 64 * 1024 * 1024 {
        return Err(Error::new(
            507,
            "Free disk reserve prevents creating a new environment.",
        ));
    }
    if !resizing {
        if marker.exists() {
            let bytes = tokio::fs::read(&marker).await?;
            if bytes != b"initializing" {
                if bytes == b"resizing" {
                    return Err(Error::conflict(
                        "Disk resize requires its original journal.",
                    ));
                }
                let pending: Value = serde_json::from_slice(&bytes)?;
                if pending["phase"] != "initializing" {
                    return Err(Error::conflict(
                        "Disk resize requires its original journal.",
                    ));
                }
                super::layout::validate(&pending)?;
            }
        }
        let pending = serde_json::json!({
            "phase": "initializing",
            "layout": { "kind": "flat-ext4-v1" }
        });
        crate::skills::atomic_write(&marker, &serde_json::to_vec(&pending)?).await?;
        // No VM can have run before the journal's atomic installation. Rebuild
        // a partial first bootstrap rather than guessing its filesystem offsets.
        if directory.join("data.ext4").exists() {
            tokio::fs::remove_file(directory.join("data.ext4")).await?;
        }
    }
    let raw = if let Some((contents, identity)) = seed {
        super::local::prepare_seeded(directory, size, contents, identity, stop).await?
    } else {
        super::local::prepare(directory, size).await?
    };
    let manifest = crate::nodes::snapshots::index_with_stop(&raw, stop).await?;
    let staging = tempfile::Builder::new()
        .prefix("bootstrap-")
        .tempdir_in(directory)?;
    let root = staging.path().to_owned();
    let mut empty = manifest.clone();
    for block in empty["blocks"].as_array_mut().unwrap() {
        block["hash"] = Value::Null;
    }
    let disk = if prepared {
        super::runtime::create_prepared(&root, &empty, &context).await?
    } else {
        super::runtime::create_at_generation(&root, &empty, &context, generation).await?
    };
    let writer = disk.clone();
    let seal = seed.is_some();
    let copy_stop = stop.clone();
    tokio::task::spawn_blocking(move || {
        copy_blocks(&raw, &manifest, &writer, &policy, &copy_stop)?;
        if seal {
            writer.seal()?;
        }
        Ok::<_, Error>(())
    })
    .await
    .map_err(Error::internal)??;
    drop(disk);
    tokio::fs::rename(&root, directory.join("lazy")).await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    // The complete image is now in the durable, non-evictable journal.
    tokio::fs::remove_file(directory.join("data.ext4")).await?;
    tokio::fs::remove_file(marker).await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    if resize_source.exists() {
        tokio::fs::remove_dir_all(resize_source).await?;
    }
    Ok(())
}

/// Copies the image's non-empty blocks into the journal, keeping the free-space reserve.
fn copy_blocks(
    raw: &Path,
    manifest: &Value,
    writer: &super::LazyDisk,
    policy: &super::policy::Policy,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    let input = super::LocalDisk::open(raw, false)?;
    for block in manifest["blocks"].as_array().unwrap() {
        if stop.is_cancelled() {
            return Err(Error::unavailable("Disk initialization stopped."));
        }
        if text(block, "hash").is_empty() {
            continue;
        }
        let offset = block["offset"].as_u64().unwrap();
        let mut bytes = vec![0; block["size"].as_u64().unwrap() as usize];
        let _admission = super::cache::admission()?;
        let (total, free) = super::policy::space(raw.parent().unwrap())?;
        if free <= policy.reserve(total) + bytes.len() as u64 * 4 + 1048576 {
            return Err(Error::new(
                507,
                "Free disk reserve prevents initializing the journal.",
            ));
        }
        input.read_at(offset, &mut bytes)?;
        writer.write_at(offset, &bytes)?;
    }
    writer.sync()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn raw_legacy_disk_cannot_start_as_a_new_s3_disk() {
        let root = tempfile::tempdir().unwrap();
        let disk = root.path().join("disks/conversation");
        std::fs::create_dir_all(&disk).unwrap();
        std::fs::write(disk.join("data.ext4"), b"existing work").unwrap();
        let error = prepare(
            &disk,
            128 * 1024 * 1024,
            &Value::Null,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, 409);
        assert_eq!(
            std::fs::read(disk.join("data.ext4")).unwrap(),
            b"existing work"
        );
    }

    #[tokio::test]
    async fn anonymous_ext4_disk_bootstraps_without_remote_authority_and_is_adopted_in_place() {
        let state = tempfile::tempdir().unwrap();
        let environment = crate::config::id();
        let directory = state.path().join("environments").join(&environment);
        let policy = super::super::policy::Policy {
            reserve_mi_b: 64,
            reserve_percent: 1,
            ..Default::default()
        };
        let stop = CancellationToken::new();
        prepare_unassigned(&directory, 128 * 1024 * 1024, &policy, &stop)
            .await
            .unwrap();
        let volume = super::super::runtime::load(&directory).await.unwrap();
        let mut signature = [0; 2];
        volume.read_at(1080, &mut signature).unwrap();
        assert_eq!(signature, [0x53, 0xef]);
        assert!(!directory.join("data.ext4").exists());
        assert!(volume.source.grant_id().is_err());
        assert!(!volume.disk.has_remote_base().unwrap());
        let context = super::super::LazyDisk::context(&directory.join("lazy")).unwrap();
        assert!(context.get("master").is_none() && context.get("grant").is_none());
        let physical = crate::file_lock::exclusive(&directory.join("lock"), "busy").unwrap();
        let run = crate::config::id();
        let owner = super::super::environment::assign(state.path(), &environment, &run)
            .await
            .unwrap();
        let authorization = json!({
            "master": "http://127.0.0.1:1/",
            "grant": "real-owner-fixture",
            "policy": policy,
        });
        volume.authorize(&owner, &authorization).await.unwrap();
        volume.read_at(1080, &mut signature).unwrap();
        assert_eq!(signature, [0x53, 0xef]);
        assert_eq!(
            volume.source.grant_id().unwrap(),
            crate::auth::digest("real-owner-fixture")
        );
        assert_eq!(
            super::super::environment::directory(state.path(), &run).unwrap(),
            directory
        );
        drop((owner, physical));
    }

    #[tokio::test]
    async fn interrupted_resize_restarts_from_the_original_journal() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/conversation");
        let policy = super::super::policy::Policy {
            reserve_mi_b: 64,
            reserve_percent: 1,
            ..Default::default()
        };
        let context = json!({
            "master": "http://127.0.0.1:1/",
            "grant": "fixture",
            "policy": policy
        });
        let stop = CancellationToken::new();
        prepare(&directory, 128 * 1024 * 1024, &context, &stop)
            .await
            .unwrap();
        let volume = super::super::runtime::load(&directory).await.unwrap();
        volume
            .disk
            .write_at(33 * 1024 * 1024, b"retained work")
            .unwrap();
        volume.disk.sync().unwrap();
        let before = volume.seal().await.unwrap();
        let original_grant = volume.source.grant_id().unwrap();
        drop(volume);
        crate::skills::atomic_write(&directory.join("bootstrap.pending"), b"resizing")
            .await
            .unwrap();
        super::super::runtime::materialize(&directory, &stop)
            .await
            .unwrap();
        // Simulate a crash after resize2fs started modifying its temporary image.
        tokio::fs::write(directory.join("data.ext4"), b"partly resized image")
            .await
            .unwrap();
        let mut restarted_context = context.clone();
        restarted_context["grant"] = "new-attempt-grant".into();
        prepare(&directory, 256 * 1024 * 1024, &restarted_context, &stop)
            .await
            .unwrap();
        let volume = super::super::runtime::load(&directory).await.unwrap();
        assert_eq!(volume.source.grant_id().unwrap(), original_grant);
        assert!(volume.seal().await.unwrap() > before);
        let mut saved = [0; 13];
        volume.disk.read_at(33 * 1024 * 1024, &mut saved).unwrap();
        assert_eq!(&saved, b"retained work");
        // ext4's block count must grow too, not just the containing file.
        let mut header = [0; 28];
        volume.disk.read_at(1024, &mut header).unwrap();
        let blocks = u32::from_le_bytes(header[4..8].try_into().unwrap()) as u64;
        let block_size = 1024_u64 << u32::from_le_bytes(header[24..28].try_into().unwrap());
        assert_eq!(blocks * block_size, 256 * 1024 * 1024);
        assert!(!directory.join("resize-source").exists());
        assert!(!directory.join("data.ext4").exists());
        assert!(!directory.join("bootstrap.pending").exists());
    }
}
