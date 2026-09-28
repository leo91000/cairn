//! New empty ext4 images become journal-backed before any user command runs.
use super::Disk;
use crate::{
    error::{Error, Result},
    validation::text,
};
use serde_json::Value;
use std::path::Path;
pub async fn prepare(
    directory: &Path,
    size: u64,
    context: &Value,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    crate::skills::private_dir(directory).await?;
    let _lock = crate::file_lock::exclusive(&directory.join("lock"), "VM disk is active.")?;
    let marker = directory.join("bootstrap.pending");
    let resize_source = directory.join("resize-source");
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
        let current = super::runtime::load(directory).await?.disk.size();
        if size < current {
            return Err(Error::new(409, "A retained VM disk cannot be shrunk."));
        }
        if size == current {
            return Ok(());
        }
        // Resize tools still require a local ext4 image. Mark the transition
        // before exporting so a restart can finish rebuilding the journal.
        crate::skills::atomic_write(&marker, b"resizing").await?;
        super::runtime::materialize(directory, stop).await?;
    }
    if directory.join("data.ext4").exists() && !marker.exists() {
        return Err(Error::new(
            409,
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
    if !marker.exists() {
        crate::skills::atomic_write(&marker, b"initializing").await?;
    }
    let raw = super::prepare(directory, size).await?;
    let manifest = crate::nodes::snapshots::index(&raw).await?;
    let staging = tempfile::Builder::new()
        .prefix("bootstrap-")
        .tempdir_in(directory)?;
    let root = staging.path().to_owned();
    let mut empty = manifest.clone();
    for block in empty["blocks"].as_array_mut().unwrap() {
        block["hash"] = Value::Null;
    }
    let disk = super::runtime::create(&root, &empty, context).await?;
    let writer = disk.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let input = super::LocalDisk::open(&raw, false)?;
        for block in manifest["blocks"].as_array().unwrap() {
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
    async fn growing_a_disk_returns_to_a_durable_journal_before_boot() {
        let root = tempfile::tempdir().unwrap();
        let disk = root.path().join("disks/conversation");
        let context = json!({"master":"http://127.0.0.1:1/","grant":"fixture","policy":super::super::policy::Policy {reserve_mi_b:64,reserve_percent:1,..Default::default()}});
        let stop = CancellationToken::new();
        prepare(&disk, 128 * 1024 * 1024, &context, &stop)
            .await
            .unwrap();
        assert!(super::super::runtime::exists(&disk));
        assert!(!disk.join("data.ext4").exists());
        prepare(&disk, 256 * 1024 * 1024, &context, &stop)
            .await
            .unwrap();
        assert_eq!(
            super::super::runtime::load(&disk)
                .await
                .unwrap()
                .disk
                .size(),
            256 * 1024 * 1024
        );
        assert!(!disk.join("data.ext4").exists());
        assert!(!disk.join("bootstrap.pending").exists());
    }

    #[tokio::test]
    async fn interrupted_resize_restarts_from_the_original_journal() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/conversation");
        let context = json!({"master":"http://127.0.0.1:1/","grant":"fixture","policy":super::super::policy::Policy {reserve_mi_b:64,reserve_percent:1,..Default::default()}});
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
        prepare(&directory, 256 * 1024 * 1024, &context, &stop)
            .await
            .unwrap();
        let volume = super::super::runtime::load(&directory).await.unwrap();
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
