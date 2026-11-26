use super::*;

pub(super) async fn remove_environment_sockets(root: &Path) -> Result<()> {
    for extension in ["e", "r"] {
        require_removed(tokio::fs::remove_dir_all(root.with_extension(extension)).await)?;
    }
    Ok(())
}

/// A completed retirement reaps only its captured old attempts, never the new
/// environment's canonical disk, bootstrap state or manager namespace.
pub(super) async fn erase_native_attempts(
    controller: &Controller,
    run: &str,
    known: &[String],
    only_known: bool,
) -> Result<()> {
    if only_known && known.is_empty() {
        return Ok(());
    }
    {
        let active = controller.active.lock().await;
        if known.iter().any(|id| active.contains_key(id)) {
            return Err(Error::conflict("A retired native attempt is still active."));
        }
    }
    crate::storage::retirement::seed_attempts(&controller.state, &controller.data, run, known)
        .await?;
    let mut identities = Vec::new();
    let mut entries = tokio::fs::read_dir(&controller.state).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry
            .path()
            .extension()
            .is_none_or(|extension| extension != "run")
        {
            continue;
        }
        let id = entry
            .path()
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if only_known && !known.contains(&id) {
            continue;
        }
        let owner = match tokio::fs::read_to_string(entry.path()).await {
            Ok(owner) => owner,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if owner != run {
            continue;
        }
        uuid(&id)?;
        if controller.active.lock().await.contains_key(&id) {
            return Err(Error::conflict("A native attempt is still active."));
        }
        atomic_write(&controller.file(&id, "stopped"), b"1").await?;
        for extension in [
            "active",
            "vm.json",
            "log",
            "lease",
            "terminal",
            "cleanup-result",
        ] {
            require_removed(tokio::fs::remove_file(controller.file(&id, extension)).await)?;
        }
        require_removed(
            tokio::fs::remove_file(
                controller
                    .state
                    .join("roots")
                    .join(&id)
                    .with_extension("plan.json"),
            )
            .await,
        )?;
        identities.push(entry.path());
        require_removed(tokio::fs::remove_dir_all(controller.state.join("roots").join(&id)).await)?;
        remove_environment_sockets(&controller.state.join("roots").join(&id)).await?;
        require_removed(
            tokio::fs::remove_file(
                controller
                    .data
                    .join("runner-plans")
                    .join(format!("{id}.json")),
            )
            .await,
        )?;
    }
    for directory in [
        controller.state.clone(),
        controller.state.join("roots"),
        controller.data.join("runner-plans"),
    ] {
        if directory.is_dir() {
            tokio::fs::File::open(directory).await?.sync_all().await?;
        }
    }
    // Keep run ownership until private output and plan removals are durable.
    // A failed erase must remain discoverable even without request hints.
    for identity in identities {
        require_removed(tokio::fs::remove_file(identity).await)?;
    }
    tokio::fs::File::open(&controller.state)
        .await?
        .sync_all()
        .await?;
    Ok(())
}

pub(super) fn require_removed(result: std::io::Result<()>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) const MAX_PLAN: u64 = 8_000_000;
pub(super) const MAX_LOG: u64 = 64 * 1024 * 1024;
pub(super) const MAX_TERMINAL_METADATA: u64 = 128;

#[cfg(test)]
mod environment_cleanup_tests {
    use super::*;

    #[tokio::test]
    async fn retirement_removes_only_its_environment_socket_directories() {
        let root = tempfile::tempdir().unwrap();
        let attempt = root.path().join("attempt");
        let other = root.path().join("other.e");
        for path in [
            attempt.with_extension("e"),
            attempt.with_extension("r"),
            other.clone(),
        ] {
            tokio::fs::create_dir(&path).await.unwrap();
            tokio::fs::write(path.join("marker"), b"keep")
                .await
                .unwrap();
        }
        remove_environment_sockets(&attempt).await.unwrap();
        remove_environment_sockets(&attempt).await.unwrap();
        assert!(!attempt.with_extension("e").exists());
        assert!(!attempt.with_extension("r").exists());
        assert!(other.join("marker").exists());
    }
}
