//! Reclaim private copies left by conversions that cannot resume after upgrade.
use super::*;
use crate::config::now;

const QUEUE: &str = "manager-staging-retirements";
const CONVERSION: &str = "backend-conversion";

fn queue_in(db: &Db<'_>) -> Result<()> {
    if db.get(QUEUE, CONVERSION)?.is_none() {
        db.put(
            QUEUE,
            &json!({ "id": CONVERSION, "retryAfter": null, "error": null }),
        )?;
    }
    Ok(())
}

async fn erase(s: &Service) -> Result<()> {
    let root = s.config.data_dir.join("backend-migration-staging");
    match tokio::fs::symlink_metadata(&root).await {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(Error::conflict(
                "Migration staging root must be a directory.",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    // This dedicated root can be a mountpoint. Reap its contents, never the root
    // itself; symlinks are removed without following their external targets.
    let mut entries = tokio::fs::read_dir(&root).await?;
    while let Some(entry) = entries.next_entry().await? {
        super::remove(&entry.path()).await?;
    }
    tokio::fs::File::open(&root).await?.sync_all().await?;
    Ok(())
}

pub(crate) async fn cleanup(s: &Service) -> Result<()> {
    let pending = s.store.get(QUEUE, CONVERSION).await?;
    if pending.is_none() {
        let root = s.config.data_dir.join("backend-migration-staging");
        let occupied = match tokio::fs::symlink_metadata(&root).await {
            Ok(metadata) if metadata.is_dir() => match tokio::fs::read_dir(&root).await {
                Ok(mut entries) => !matches!(entries.next_entry().await, Ok(None)),
                Err(_) => true,
            },
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => true,
        };
        if !occupied {
            return Ok(());
        }
        // Discovery is repeatable after restart; enqueue before any file erasure.
        s.store.write(|db| queue_in(db)).await?;
    }
    let Some(mut pending) = s.store.get(QUEUE, CONVERSION).await? else {
        return Ok(());
    };
    if pending["retryAfter"].as_i64().unwrap_or(0) > now() {
        return Ok(());
    }
    // These are the same owners held by every legacy converter. Busy ownership
    // defers this job without holding admission or other storage operations.
    let _process = match crate::conversation_lifecycle::storage_lock(&s.config.data_dir) {
        Ok(guard) => guard,
        Err(error) if error.is_conflict() => return Ok(()),
        Err(error) => return Err(error),
    };
    let Ok(_storage) = s.conversation_storage_lock.try_lock() else {
        return Ok(());
    };
    match erase(s).await {
        Ok(()) => s.store.write(|db| db.remove(QUEUE, CONVERSION)).await,
        Err(error) => {
            pending["error"] = error.message.clone().into();
            pending["retryAfter"] = (now() + 60_000).into();
            s.store.write(move |db| db.put(QUEUE, &pending)).await?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[tokio::test]
    async fn abandoned_conversion_copies_retry_after_restart_without_removing_the_staging_root() {
        let fixture = tempfile::tempdir().unwrap();
        let config = crate::common::config(fixture.path());
        let service = Service::new(config.clone()).await.unwrap();
        let root = service.config.data_dir.join("backend-migration-staging");
        tokio::fs::write(&root, b"temporarily invalid staging mount")
            .await
            .unwrap();
        service.worker.initialize(&service).await.unwrap();
        assert!(cleanup(&service).await.is_err());
        let pending = service.store.get(QUEUE, CONVERSION).await.unwrap().unwrap();
        assert!(pending["error"].is_string());
        assert!(pending["retryAfter"].as_i64().unwrap() > now());
        tokio::fs::remove_file(&root).await.unwrap();
        tokio::fs::create_dir_all(root.join("conversion-abandoned/tree"))
            .await
            .unwrap();
        for name in ["source.ext4", "converted.ext4", "tree/private.txt"] {
            tokio::fs::write(
                root.join("conversion-abandoned").join(name),
                b"private old files",
            )
            .await
            .unwrap();
        }
        let unrelated = fixture.path().join("other-storage");
        tokio::fs::write(&unrelated, b"kept").await.unwrap();
        std::os::unix::fs::symlink(&unrelated, root.join("external-link")).unwrap();
        let inode = std::fs::metadata(&root).unwrap().ino();
        service
            .store
            .write(|db| {
                let mut pending = db.get(QUEUE, CONVERSION)?.unwrap();
                pending["retryAfter"] = Value::Null;
                db.put(QUEUE, &pending)
            })
            .await
            .unwrap();
        let restarted = Service::new(config).await.unwrap();
        restarted.worker.initialize(&restarted).await.unwrap();
        let busy = crate::conversation_lifecycle::storage_lock(&restarted.config.data_dir).unwrap();
        cleanup(&restarted).await.unwrap();
        assert!(root.join("conversion-abandoned/source.ext4").exists());
        drop(busy);
        cleanup(&restarted).await.unwrap();
        assert_eq!(std::fs::metadata(&root).unwrap().ino(), inode);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        assert_eq!(tokio::fs::read(&unrelated).await.unwrap(), b"kept");
        assert!(
            restarted
                .store
                .get(QUEUE, CONVERSION)
                .await
                .unwrap()
                .is_none()
        );
        cleanup(&restarted).await.unwrap();
    }
}
