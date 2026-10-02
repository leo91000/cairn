//! Guest mounts and barriers over both views of the conversation's journal.
use crate::error::{Error, Result};
use std::{
    os::unix::fs::FileTypeExt,
    path::Path,
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::process::Command;

const SYSTEM: &str = "/oldroot/run/data";
const WORKSPACE: &str = "/data";
static MOUNTED: AtomicBool = AtomicBool::new(false);

pub(super) fn mounted() -> bool {
    MOUNTED.load(Ordering::Acquire)
}

fn targets() -> Vec<&'static str> {
    if MOUNTED.load(Ordering::Acquire) {
        vec![WORKSPACE, SYSTEM]
    } else {
        vec![SYSTEM]
    }
}

pub(super) async fn mount() -> Result<()> {
    if MOUNTED.load(Ordering::Acquire) {
        return Ok(());
    }
    if !tokio::fs::metadata("/dev/vdc")
        .await?
        .file_type()
        .is_block_device()
        || !tokio::fs::symlink_metadata(WORKSPACE).await?.is_dir()
    {
        return Err(Error::bad("Invalid guest workspace mount."));
    }
    // PATCH changes the host backing while this guest device is unmounted.
    // Discard any buffers populated by kernel discovery before reading ext4.
    run("blockdev", &["--flushbufs", "/dev/vdc"]).await?;
    run(
        "mount",
        &["-t", "ext4", "-o", "noatime", "/dev/vdc", WORKSPACE],
    )
    .await?;
    let mut bound = Vec::new();
    let mounted = async {
        let private = Path::new("/data/.leo-system");
        crate::skills::private_dir(private).await?;
        clear_staged_caches(private).await?;
        for directory in ["docker", "containerd"] {
            let source = format!("/data/.leo-system/{directory}");
            let target = format!("/var/lib/{directory}");
            tokio::fs::create_dir_all(&source).await?;
            run("mount", &["--bind", &source, &target]).await?;
            bound.push(target);
        }
        // Toolkit installs and Android userdata can exceed the small system
        // view. Keep them on the conversation's larger workspace view while
        // the native Codex database and managed account remain on the system.
        let local = Path::new("/home/node/.local");
        tokio::fs::create_dir_all(local).await?;
        std::os::unix::fs::chown(local, Some(super::AGENT_ID), Some(super::AGENT_ID))?;
        for (name, target) in [
            ("home-cache", "/home/node/.cache"),
            ("home-share", "/home/node/.local/share"),
            ("android", "/home/node/.android"),
        ] {
            let source = private.join(name);
            seed_cache(&source, Path::new(target)).await?;
            // Installation is durable before removing the old system copy.
            // A crash at either point can safely retry using the workspace.
            match tokio::fs::remove_dir_all(target).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            tokio::fs::create_dir_all(target).await?;
            for path in [&source, Path::new(target)] {
                std::os::unix::fs::chown(path, Some(super::AGENT_ID), Some(super::AGENT_ID))?;
            }
            run("mount", &["--bind", source.to_str().unwrap(), target]).await?;
            bound.push(target.to_owned());
        }
        std::os::unix::fs::chown(WORKSPACE, Some(1000), Some(1000))?;
        Ok::<(), Error>(())
    }
    .await;
    if let Err(error) = mounted {
        for target in bound.iter().rev() {
            let _ = run("umount", &[target]).await;
        }
        let _ = run("umount", &[WORKSPACE]).await;
        return Err(error);
    }
    MOUNTED.store(true, Ordering::Release);
    Ok(())
}

/// Preserve an existing cache once, installing its copy atomically. A partial
/// copy is never used as the source on a later guest boot.
async fn seed_cache(source: &Path, target: &Path) -> Result<()> {
    match tokio::fs::symlink_metadata(source).await {
        Ok(metadata) if metadata.is_dir() => return Ok(()),
        Ok(_) => return Err(Error::bad("Invalid workspace cache directory.")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = source.parent().unwrap();
    let staging = tempfile::Builder::new()
        .prefix("partial-cache-")
        .tempdir_in(parent)?;
    match tokio::fs::symlink_metadata(target).await {
        Ok(metadata) if metadata.is_dir() => {
            let contents = format!("{}/.", target.display());
            run(
                "cp",
                &["-a", "--", &contents, staging.path().to_str().unwrap()],
            )
            .await?;
        }
        Ok(_) => return Err(Error::bad("Invalid guest cache directory.")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let copied = staging.path().to_owned();
    tokio::task::spawn_blocking(move || sync_cache(&copied))
        .await
        .map_err(Error::internal)??;
    tokio::fs::rename(staging.path(), source).await?;
    tokio::fs::File::open(parent).await?.sync_all().await?;
    Ok(())
}

async fn clear_staged_caches(parent: &Path) -> Result<()> {
    let mut entries = tokio::fs::read_dir(parent).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("partial-cache-")
            && entry.file_type().await?.is_dir()
        {
            tokio::fs::remove_dir_all(entry.path()).await?;
        }
    }
    Ok(())
}

fn sync_cache(root: &Path) -> std::io::Result<()> {
    let mut directories = vec![root.to_owned()];
    let mut position = 0;
    while position < directories.len() {
        for entry in std::fs::read_dir(&directories[position])? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                directories.push(entry.path());
            } else if kind.is_file() {
                std::fs::File::open(entry.path())?.sync_all()?;
            } else if !kind.is_symlink() {
                return Err(std::io::Error::other("Invalid guest cache entry"));
            }
        }
        position += 1;
    }
    for directory in directories.iter().rev() {
        std::fs::File::open(directory)?.sync_all()?;
    }
    Ok(())
}

async fn run(program: &str, arguments: &[&str]) -> Result<()> {
    if Command::new(program)
        .args(arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?
        .success()
    {
        return Ok(());
    }
    Err(Error::unavailable("Guest filesystem operation failed."))
}

pub(super) async fn sync(program: &str) -> Result<()> {
    for target in targets() {
        run(program, &["--file-system", target]).await?;
    }
    Ok(())
}

pub(super) async fn freeze(freeze: bool) -> Result<bool> {
    let targets = targets();
    if !freeze {
        for target in targets.iter().rev() {
            let _ = run("fsfreeze", &["--unfreeze", target]).await;
        }
        return Ok(true);
    }
    for target in &targets {
        if run("fsfreeze", &["--freeze", target]).await.is_err() {
            // An interrupted/failed response can still have frozen a mount.
            // Roll back every view before reporting failure to the host.
            for target in targets.iter().rev() {
                let _ = run("fsfreeze", &["--unfreeze", target]).await;
            }
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn installed_workspace_cache_survives_reboot_without_old_data_overwriting_it() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let source = root.path().join("cache");
        tokio::fs::create_dir_all(old.join("nested")).await.unwrap();
        tokio::fs::write(old.join("nested/installed"), b"original")
            .await
            .unwrap();
        std::os::unix::fs::symlink("nested/installed", old.join("link")).unwrap();
        seed_cache(&source, &old).await.unwrap();
        assert_eq!(
            tokio::fs::read(source.join("link")).await.unwrap(),
            b"original"
        );
        tokio::fs::write(source.join("nested/installed"), b"new")
            .await
            .unwrap();
        seed_cache(&source, &old).await.unwrap();
        assert_eq!(tokio::fs::read(source.join("link")).await.unwrap(), b"new");
        assert_eq!(
            tokio::fs::read(old.join("nested/installed")).await.unwrap(),
            b"original"
        );
    }

    #[tokio::test]
    async fn failed_cache_seed_leaves_old_data_and_never_installs_a_partial_source() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let source = root.path().join("cache");
        tokio::fs::write(&old, b"unexpected-file").await.unwrap();
        assert!(seed_cache(&source, &old).await.is_err());
        assert!(!source.exists());
        assert_eq!(tokio::fs::read(&old).await.unwrap(), b"unexpected-file");
        tokio::fs::remove_file(&old).await.unwrap();
        tokio::fs::create_dir(&old).await.unwrap();
        seed_cache(&source, &old).await.unwrap();
        assert!(source.is_dir());
    }

    #[tokio::test]
    async fn crashed_cache_copies_are_removed_without_deleting_installed_caches() {
        let root = tempfile::tempdir().unwrap();
        let partial = root.path().join("partial-cache-crashed");
        let installed = root.path().join("android");
        tokio::fs::create_dir(&partial).await.unwrap();
        tokio::fs::create_dir(&installed).await.unwrap();
        tokio::fs::write(partial.join("incomplete"), b"partial")
            .await
            .unwrap();
        clear_staged_caches(root.path()).await.unwrap();
        assert!(!partial.exists());
        assert!(installed.is_dir());
    }
}
