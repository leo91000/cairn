//! Publish guest projects only after their filesystem policy is effective.
use crate::{
    error::{Error, Result},
    skills::atomic_write,
    validation::text,
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};
use tokio::process::Command;

fn record(target: &Path) -> PathBuf {
    Path::new("/var/lib/leo/projects").join(crate::auth::hex_digest(&target.to_string_lossy()))
}

async fn save(value: &Value) -> Result<()> {
    let path = record(Path::new(text(value, "path")));
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    atomic_write(&path, &serde_json::to_vec(value)?).await
}

/// `source` is complete data inside a root-owned 0700 parent. The guest cannot
/// reach it while extraction, ownership changes or mount preparation happen.
pub async fn publish(source: &Path, target: &Path, restricted: bool) -> Result<()> {
    if !restricted {
        tokio::fs::rename(source, target).await?;
        return save(&json!({"path":target,"readOnly":false})).await;
    }
    let value = json!({"path":target,"source":source,"readOnly":true});
    // A reboot at any subsequent point can reconstruct the published mount.
    save(&value).await?;
    apply(&value).await
}

pub async fn reopen(target: &Path, restricted: bool) -> Result<bool> {
    let mut value = match tokio::fs::read(record(target)).await {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if !target.exists() {
                return Ok(false);
            }
            json!({"path":target})
        }
        Err(error) => return Err(error.into()),
    };
    value["readOnly"] = restricted.into();
    save(&value).await?;
    apply(&value).await?;
    Ok(true)
}

pub async fn restore() -> Result<()> {
    let directory = Path::new("/var/lib/leo/projects");
    if directory.exists() {
        let mut entries = tokio::fs::read_dir(directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            let value: Value = serde_json::from_slice(&tokio::fs::read(entry.path()).await?)?;
            apply(&value).await?;
        }
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
    if !Command::new("mount")
        .args([
            "-o",
            if restricted {
                "remount,bind,ro"
            } else {
                "remount,bind,rw"
            },
        ])
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

async fn apply(value: &Value) -> Result<()> {
    let target = Path::new(text(value, "path"));
    let restricted = value["readOnly"] == true;
    if let Some(source) = value["source"].as_str() {
        let source = Path::new(source);
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
