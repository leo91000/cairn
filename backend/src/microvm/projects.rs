//! Publish guest projects only after their filesystem policy is effective.
use crate::{
    error::{Error, Result},
    skills::atomic_write,
};
use serde::{Deserialize, Serialize};
use std::{
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};
use tokio::process::Command;

const RECORDS: &str = "/var/lib/leo/projects";

/// Persisted mount of a published project, replayed after every guest boot.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(default)]
    path: PathBuf,
    /// Private data directory behind a restricted project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<PathBuf>,
    #[serde(default)]
    read_only: bool,
}

fn record_file(target: &Path) -> PathBuf {
    Path::new(RECORDS).join(crate::auth::hex_digest(&target.to_string_lossy()))
}

async fn save(record: &Record) -> Result<()> {
    let path = record_file(&record.path);
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    atomic_write(&path, &serde_json::to_vec(record)?).await
}

/// `source` is complete data inside a root-owned 0700 parent. The guest cannot
/// reach it while extraction, ownership changes or mount preparation happen.
pub async fn publish(source: &Path, target: &Path, restricted: bool) -> Result<()> {
    if !restricted {
        tokio::fs::rename(source, target).await?;
        let record = Record {
            path: target.to_owned(),
            source: None,
            read_only: false,
        };
        return save(&record).await;
    }
    let record = Record {
        path: target.to_owned(),
        source: Some(source.to_owned()),
        read_only: true,
    };
    // A reboot at any subsequent point can reconstruct the published mount.
    save(&record).await?;
    apply(&record).await
}

pub async fn reopen(target: &Path, restricted: bool) -> Result<bool> {
    let mut record = match tokio::fs::read(record_file(target)).await {
        Ok(bytes) => serde_json::from_slice::<Record>(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if !target.exists() {
                return Ok(false);
            }
            Record {
                path: target.to_owned(),
                source: None,
                read_only: false,
            }
        }
        Err(error) => return Err(error.into()),
    };
    record.read_only = restricted;
    save(&record).await?;
    apply(&record).await?;
    Ok(true)
}

pub async fn restore() -> Result<()> {
    let directory = Path::new(RECORDS);
    if !directory.exists() {
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        let record: Record = serde_json::from_slice(&tokio::fs::read(entry.path()).await?)?;
        apply(&record).await?;
    }
    Ok(())
}

async fn mounted(target: &Path) -> Result<bool> {
    Ok(Command::new("mountpoint")
        .arg("-q")
        .arg(target)
        .status()
        .await?
        .success())
}

async fn policy(target: &Path, restricted: bool) -> Result<()> {
    let options = if restricted {
        "remount,bind,ro"
    } else {
        "remount,bind,rw"
    };
    if !Command::new("mount")
        .args(["-o", options])
        .arg(target)
        .status()
        .await?
        .success()
    {
        return Err(Error::bad("Could not apply project filesystem policy."));
    }
    Ok(())
}

async fn bind(source: &Path, target: &Path) -> Result<()> {
    if !Command::new("mount")
        .arg("--bind")
        .arg(source)
        .arg(target)
        .status()
        .await?
        .success()
    {
        return Err(Error::bad("Could not bind guest project."));
    }
    Ok(())
}

async fn apply(record: &Record) -> Result<()> {
    let target = record.path.as_path();
    let restricted = record.read_only;
    if let Some(source) = &record.source {
        if !source.is_dir() {
            return Err(Error::bad("Retained project data is unavailable."));
        }
        if !target.exists() {
            std::fs::DirBuilder::new().mode(0o555).create(target)?;
        }
        if mounted(target).await? {
            return policy(target, restricted).await;
        }
        let view = source.with_file_name("view");
        if !view.exists() {
            std::fs::DirBuilder::new().mode(0o700).create(&view)?;
        }
        if !mounted(&view).await? {
            bind(source, &view).await?;
        }
        policy(&view, restricted).await?;
        // The private mount is already read-only before any guest can see data.
        if !Command::new("mount")
            .arg("--move")
            .arg(&view)
            .arg(target)
            .status()
            .await?
            .success()
        {
            return Err(Error::bad("Could not publish guest project."));
        }
    } else if restricted {
        // Compatibility with disks whose projects were stored at their public path.
        if !mounted(target).await? {
            bind(target, target).await?;
        }
        policy(target, true).await?;
    } else if mounted(target).await? {
        policy(target, false).await?;
    }
    Ok(())
}
