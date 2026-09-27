//! New empty ext4 images become journal-backed before any user command runs.
use super::{Disk, LazyDisk, remote::RemoteSource};
use crate::{
    error::{Error, Result},
    validation::text,
};
use serde_json::Value;
use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;
pub async fn prepare(directory: &Path, size: u64, context: &Value) -> Result<()> {
    crate::skills::private_dir(directory).await?;
    let _lock = crate::file_lock::exclusive(&directory.join("lock"), "VM disk is active.")?;
    let marker = directory.join("bootstrap.pending");
    if super::runtime::exists(directory) {
        if marker.exists() {
            // A crash after the atomic install must still reclaim the empty source.
            let _volume = super::runtime::load(directory).await?;
            if directory.join("data.ext4").exists() {
                tokio::fs::remove_file(directory.join("data.ext4")).await?;
            }
            tokio::fs::remove_file(&marker).await?;
            std::fs::File::open(directory)?.sync_all()?;
        }
        return Ok(());
    }
    if directory.join("data.ext4").exists() && !marker.exists() {
        return Ok(());
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
    crate::skills::atomic_write(&marker, b"initializing").await?;
    let raw = super::prepare(directory, size).await?;
    let manifest = crate::nodes::snapshots::index(&raw).await?;
    let staging = tempfile::Builder::new()
        .prefix("bootstrap-")
        .tempdir_in(directory)?;
    let root = staging.path().to_owned();
    let source = Arc::new(RemoteSource::new(
        context,
        tokio::runtime::Handle::current(),
        CancellationToken::new(),
    )?);
    let mut empty = manifest.clone();
    for block in empty["blocks"].as_array_mut().unwrap() {
        block["hash"] = Value::Null;
    }
    let disk = Arc::new(LazyDisk::create(&root, &empty, source)?);
    disk.set_context(context)?;
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
    disk.sync()?;
    drop(disk);
    tokio::fs::rename(&root, directory.join("lazy")).await?;
    std::fs::File::open(directory)?.sync_all()?;
    // This raw image contained only freshly formatted filesystem metadata. Its
    // complete nonzero contents are now in the durable, non-evictable journal.
    tokio::fs::remove_file(directory.join("data.ext4")).await?;
    tokio::fs::remove_file(marker).await?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}
