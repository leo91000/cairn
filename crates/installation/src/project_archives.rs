//! Immutable, disposable project seeds shared across conversations. Only project
//! files are cached; homes, credentials and conversation changes remain private.
mod archive;
mod manager;
mod node;
#[cfg(test)]
mod tests;

pub(crate) use manager::{
    catalog, checkout, checkout_authorized, maintain, run_archives, serve_cached, serve_run,
};
pub(crate) use node::{abandon_stage, foreground_waiter, prefetch, stage};

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

const CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const LOGICAL_BYTES: u64 = 32 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Archive {
    pub project_id: String,
    pub revision: String,
    #[serde(default)]
    pub origin_hash: Option<String>,
    pub key: String,
    pub hash: String,
    pub bytes: u64,
    pub logical_bytes: u64,
}

impl Archive {
    pub(crate) fn validate(&self) -> Result<()> {
        crate::validation::uuid(&self.project_id)?;
        for (value, length) in [(&self.key, 64), (&self.hash, 64), (&self.revision, 40)] {
            if value.len() != length || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::bad("Invalid project archive identity."));
            }
        }
        if self.bytes > CACHE_BYTES || self.logical_bytes > LOGICAL_BYTES {
            return Err(Error::too_large("Project archive exceeds cache limits."));
        }
        Ok(())
    }

    fn path(&self, root: &Path) -> PathBuf {
        root.join("project-archives")
            .join(format!("{}.zip", self.hash))
    }
}

fn key(project: &Value) -> String {
    let identity = serde_json::json!({
        "id": project["id"], "path": project["path"], "origin": project["origin"],
        "branch": project["baseBranch"], "source": project["source"], "format": 1,
    });
    blake3::hash(identity.to_string().as_bytes())
        .to_hex()
        .to_string()
}

fn lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(tokio::sync::Mutex::default)
}

async fn cached(root: &Path, descriptor: &Archive) -> Result<Option<std::fs::File>> {
    descriptor.validate()?;
    match tokio::fs::File::open(descriptor.path(root)).await {
        Ok(file) if file.metadata().await?.len() == descriptor.bytes => {
            Ok(Some(file.into_std().await))
        }
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn fingerprint(file: std::fs::File) -> Result<(String, u64)> {
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut file = file;
        let mut hash = blake3::Hasher::new();
        let mut buffer = vec![0; 1024 * 1024];
        let mut bytes = 0;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
            bytes += read as u64;
        }
        Ok((hash.finalize().to_hex().to_string(), bytes))
    })
    .await
    .map_err(Error::internal)?
}

async fn extract(file: std::fs::File, target: &Path, budget: u64) -> Result<u64> {
    let target = target.to_owned();
    tokio::task::spawn_blocking(move || archive::extract(file, &target, budget))
        .await
        .map_err(Error::internal)?
}

async fn extract_verified(
    file: std::fs::File,
    descriptor: &Archive,
    target: &Path,
    budget: u64,
) -> Result<u64> {
    let (hash, bytes) = fingerprint(file.try_clone()?).await?;
    if hash != descriptor.hash || bytes != descriptor.bytes {
        return Err(Error::bad("Project archive checksum mismatch."));
    }
    extract(file, target, budget).await
}

/// A metadata-only preflight bounds expanded allocation before warming. The
/// mutation lock keeps this cache entry stable; extraction independently checks
/// the complete checksum before writing, so corrupt metadata cannot underreserve.
pub(crate) async fn cached_allocation(
    root: &Path,
    descriptor: &Archive,
    unit: u64,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<u64> {
    let file = cached(root, descriptor)
        .await?
        .ok_or_else(|| Error::not_found("Project archive is not cached."))?;
    let (logical, stop) = (descriptor.logical_bytes, stop.clone());
    tokio::task::spawn_blocking(move || archive::allocation_budget(file, logical, unit, &stop))
        .await
        .map_err(Error::internal)?
}

/// Native base preparation holds the shared storage lock throughout extraction.
pub(crate) async fn extract_cached(
    root: &Path,
    descriptor: &Archive,
    target: &Path,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    let file = cached(root, descriptor)
        .await?
        .ok_or_else(|| Error::not_found("Project archive is not cached."))?;
    let (file, target, descriptor, stop) =
        (file, target.to_owned(), descriptor.clone(), stop.clone());
    tokio::task::spawn_blocking(move || -> Result<()> {
        use std::io::{Read, Seek};
        let mut file = file;
        let mut hash = blake3::Hasher::new();
        let mut buffer = vec![0; 1024 * 1024];
        let mut bytes = 0;
        loop {
            if stop.is_cancelled() {
                return Err(Error::unavailable("Native base preparation stopped."));
            }
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            bytes += count as u64;
        }
        if bytes != descriptor.bytes || hash.finalize().to_hex().as_str() != descriptor.hash {
            return Err(Error::bad("Project archive checksum mismatch."));
        }
        file.rewind()?;
        archive::extract_with_stop(file, &target, descriptor.logical_bytes, &stop)?;
        Ok(())
    })
    .await
    .map_err(Error::internal)?
}

/// Called only with cache mutation serialized and before creating a new temp
/// file. Killed downloads/builds must not leave immortal budget charges.
async fn clean_interrupted_downloads(root: &Path) -> Result<u64> {
    let directory = root.join("project-archives");
    crate::skills::private_dir(&directory).await?;
    let mut entries = tokio::fs::read_dir(directory).await?;
    let mut freed = 0u64;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_name().to_string_lossy().starts_with(".tmp")
            && entry.file_type().await?.is_file()
        {
            let bytes = entry.metadata().await?.len();
            tokio::fs::remove_file(entry.path()).await?;
            freed = freed.saturating_add(bytes);
        }
    }
    Ok(freed)
}

/// Cache objects are disposable and pinned by an open descriptor or a run's hard
/// link. Eviction never traverses conversation files or follows symlinks.
async fn prune(root: &Path, reserve: u64) -> Result<()> {
    let directory = root.join("project-archives");
    crate::skills::private_dir(&directory).await?;
    let mut files = Vec::new();
    let mut bytes = reserve;
    let mut entries = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.path().extension().is_some_and(|value| value == "zip")
            && entry.file_type().await?.is_file()
        {
            let metadata = entry.metadata().await?;
            bytes = bytes.saturating_add(metadata.len());
            files.push((metadata.modified()?, entry.path(), metadata.len()));
        }
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, path, length) in files {
        if bytes <= CACHE_BYTES {
            break;
        }
        tokio::fs::remove_file(path).await?;
        bytes = bytes.saturating_sub(length);
    }
    Ok(())
}

/// Free unpinned disposable objects against actual shared-pool pressure, not
/// merely the separate cache cap. Run pins and conversation files are untouched.
async fn reclaim(root: &Path, needed: u64) -> Result<u64> {
    reclaim_except(root, needed, &[]).await
}

async fn reclaim_except(root: &Path, needed: u64, keep: &[String]) -> Result<u64> {
    use std::os::unix::fs::MetadataExt;
    let directory = root.join("project-archives");
    crate::skills::private_dir(&directory).await?;
    let mut entries = tokio::fs::read_dir(directory).await?;
    let mut files = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if entry
            .path()
            .extension()
            .is_none_or(|extension| extension != "zip")
            || !entry.file_type().await?.is_file()
        {
            continue;
        }
        let metadata = entry.metadata().await?;
        let protected = entry
            .path()
            .file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(|hash| keep.iter().any(|keep| keep == hash));
        if metadata.nlink() == 1 && !protected {
            files.push((metadata.modified()?, entry.path(), metadata.len()));
        }
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    let mut reclaimed = 0u64;
    for (_, path, bytes) in files {
        if reclaimed >= needed {
            break;
        }
        tokio::fs::remove_file(path).await?;
        reclaimed = reclaimed.saturating_add(bytes);
    }
    Ok(reclaimed)
}

pub(crate) async fn reclaim_for_foreground(
    root: &Path,
    needed: u64,
    keep: &[String],
) -> Result<u64> {
    let _guard = lock().lock().await;
    let freed = clean_interrupted_downloads(root).await?;
    let freed =
        freed.saturating_add(reclaim_except(root, needed.saturating_sub(freed), keep).await?);
    if crate::native::enabled() && freed < needed {
        let bases = crate::native::bases::Bases {
            data: root.to_owned(),
            state: crate::native::state_dir(),
            identity: (0, 0),
        };
        return Ok(freed.saturating_add(bases.trim(u64::MAX, needed - freed, None).await?));
    }
    Ok(freed)
}

pub(crate) async fn initialize(
    plan: &Value,
    source: &Path,
    target: &Path,
    budget: u64,
) -> Result<bool> {
    let Some(archives) = plan["nativeProjectArchives"].as_array() else {
        return Ok(false);
    };
    let mut remaining = budget;
    crate::skills::private_dir(target).await?;
    for value in archives {
        let descriptor: Archive = serde_json::from_value(value.clone())?;
        descriptor.validate()?;
        // This path is generated by the privileged connector, never by a repo.
        let path = source
            .parent()
            .ok_or_else(|| Error::bad("Missing project seed root."))?
            .join("project-archives")
            .join(format!("{}.zip", descriptor.project_id));
        let file = tokio::fs::File::open(path).await?.into_std().await;
        let used = extract(file, &target.join(&descriptor.project_id), remaining).await?;
        remaining = remaining.saturating_sub(used);
    }
    Ok(true)
}
