//! Reclaim immutable runtime images only while no references are being created.
use crate::error::{Error, Result};
use std::{collections::HashSet, fs, io, path::Path};

pub(crate) static CONTROL: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

pub(super) async fn collect(state: &Path, current: &Path) -> Result<usize> {
    let exclusive = CONTROL.write().await;
    let state = state.to_owned();
    let current = current.to_owned();
    tokio::task::spawn_blocking(move || {
        // Keep the lock in the worker even if its async caller is cancelled.
        let _exclusive = exclusive;
        collect_locked(&state, &current)
    })
    .await
    .map_err(Error::internal)?
}

fn entries(path: &Path) -> io::Result<Vec<fs::DirEntry>> {
    match fs::read_dir(path) {
        Ok(entries) => entries.collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn reference(path: &Path, field: &str, pins: &mut HashSet<String>) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(Error::conflict("Invalid runtime image reference."));
    }
    let record: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let runtime = record[field]
        .as_str()
        .ok_or_else(|| Error::conflict("Missing runtime image reference."))?;
    if runtime.is_empty() || runtime.len() > 100 || !super::host::valid_runtime_name(runtime) {
        return Err(Error::conflict("Invalid runtime image reference."));
    }
    pins.insert(runtime.to_owned());
    Ok(())
}

fn collect_locked(state: &Path, current: &Path) -> Result<usize> {
    let mut pins = HashSet::new();
    if let Some(runtime) = current.file_name().and_then(|name| name.to_str()) {
        pins.insert(runtime.to_owned());
    }
    // All disk references live at the physical directory root; logical aliases
    // are also scanned. Do not traverse guest journals or follow symlinks.
    for (root, file, field) in [
        ("disks", "runtime.json", "runtimeId"),
        ("environments", "runtime.json", "runtimeId"),
        ("templates", "key.json", "runtime"),
    ] {
        for entry in entries(&state.join(root))? {
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(Error::conflict(
                    "Runtime reference directory must not be an alias.",
                ));
            }
            if kind.is_dir() {
                reference(&entry.path().join(file), field, &mut pins)?;
            }
        }
    }
    // Finish reading every pin before making any destructive change.
    let mut removed = 0;
    for entry in entries(&state.join("images"))? {
        let name = entry.file_name();
        let Some(runtime) = name.to_str() else {
            continue;
        };
        if runtime.is_empty()
            || !super::host::valid_runtime_name(runtime)
            || pins.contains(runtime)
            || !entry.file_type()?.is_dir()
        {
            continue;
        }
        fs::remove_dir_all(entry.path())?;
        tracing::info!(target: "leo_performance", operation = "runtime_gc", runtime, event = "removed");
        removed += 1;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn collection_fails_closed_on_unreadable_references() {
        let root = tempfile::tempdir().unwrap();
        let unused = root.path().join("images/unused");
        tokio::fs::create_dir_all(&unused).await.unwrap();
        let disk = root.path().join("disks/conversation");
        tokio::fs::create_dir_all(&disk).await.unwrap();
        tokio::fs::write(disk.join("runtime.json"), b"invalid-json")
            .await
            .unwrap();
        assert!(
            collect(root.path(), &root.path().join("images/current"))
                .await
                .is_err()
        );
        assert!(unused.exists());
    }

    #[tokio::test]
    async fn collection_waits_for_a_starting_runtime_to_publish_its_pin() {
        let root = tempfile::tempdir().unwrap();
        let starting = root.path().join("images/starting");
        tokio::fs::create_dir_all(&starting).await.unwrap();
        let pin = CONTROL.read().await;
        let state = root.path().to_owned();
        let mut collection =
            tokio::spawn(async move { collect(&state, &state.join("images/current")).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut collection)
                .await
                .is_err()
        );
        let disk = root.path().join("environments/starting");
        tokio::fs::create_dir_all(&disk).await.unwrap();
        tokio::fs::write(disk.join("runtime.json"), br#"{"runtimeId":"starting"}"#)
            .await
            .unwrap();
        drop(pin);
        assert_eq!(collection.await.unwrap().unwrap(), 0);
        assert!(starting.exists());
    }

    #[tokio::test]
    async fn collection_preserves_current_and_all_pins_and_removes_unused_images() {
        let root = tempfile::tempdir().unwrap();
        for runtime in [
            "current",
            "disk-pin",
            "environment-pin",
            "template-pin",
            "unused",
        ] {
            let path = root.path().join("images").join(runtime);
            tokio::fs::create_dir_all(&path).await.unwrap();
            tokio::fs::write(path.join("root.ext4"), b"image")
                .await
                .unwrap();
        }
        for (directory, file, record) in [
            (
                "disks/conversation",
                "runtime.json",
                r#"{"runtimeId":"disk-pin"}"#,
            ),
            (
                "environments/environment",
                "runtime.json",
                r#"{"runtimeId":"environment-pin"}"#,
            ),
            (
                "templates/template",
                "key.json",
                r#"{"runtime":"template-pin"}"#,
            ),
        ] {
            let path = root.path().join(directory);
            tokio::fs::create_dir_all(&path).await.unwrap();
            tokio::fs::write(path.join(file), record).await.unwrap();
        }
        let current = root.path().join("images/current");
        assert_eq!(collect(root.path(), &current).await.unwrap(), 1);
        for runtime in ["current", "disk-pin", "environment-pin", "template-pin"] {
            assert!(
                root.path()
                    .join("images")
                    .join(runtime)
                    .join("root.ext4")
                    .exists(),
                "{runtime}"
            );
        }
        assert!(!root.path().join("images/unused").exists());
    }
}
