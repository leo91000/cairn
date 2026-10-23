//! Immutable, disposable project seeds shared across conversations. Only project
//! files are cached; homes, credentials and conversation changes remain private.
mod archive;
mod manager;
mod node;
#[cfg(test)]
mod tests;

pub(crate) use manager::{catalog, checkout, maintain, run_archives, serve_cached, serve_run};
pub(crate) use node::{prefetch, stage};

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
    pub key: String,
    pub hash: String,
    pub bytes: u64,
    pub logical_bytes: u64,
}

impl Archive {
    fn validate(&self) -> Result<()> {
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
