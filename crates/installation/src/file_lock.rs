//! Process-scoped locks release ownership even if a concurrent fork inherited the descriptor.
use crate::error::{Error, Result};
use std::{
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::Path,
};

pub struct Guard(std::fs::File);

impl Drop for Guard {
    fn drop(&mut self) {
        // CLOEXEC closes inherited descriptors only after exec; explicit unlock
        // releases this operation even while a child is still preparing to exec.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub fn exclusive(path: &Path, busy: &str) -> Result<Guard> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::conflict(busy));
    }
    Ok(Guard(file))
}

/// Locks a directory itself. No entry is created in it, so a lock attempt
/// cannot race with removal of the directory and leave it non-empty.
pub fn exclusive_directory(path: &Path, busy: &str) -> Result<Guard> {
    let directory = std::fs::File::open(path)?;

    if unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::conflict(busy));
    }
    Ok(Guard(directory))
}

/// Waits for the current owner instead of failing. Use only where every owner
/// holds the lock for a bounded time.
pub async fn exclusive_directory_after_release(path: &Path) -> Result<Guard> {
    let directory = std::fs::File::open(path)?;

    tokio::task::spawn_blocking(move || {
        if unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Guard(directory))
    })
    .await
    .map_err(Error::internal)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn releasing_an_operation_unlocks_even_while_a_child_holds_the_open_description() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lock");
        let owner = exclusive(&path, "busy").unwrap();
        assert!(exclusive(&path, "busy").is_err());
        // dup and fork both retain the same open file description and flock.
        let inherited = owner.0.try_clone().unwrap();
        drop(owner);
        let next = exclusive(&path, "busy").expect("finished operation must release ownership");
        drop(inherited);
        assert!(
            exclusive(&path, "busy").is_err(),
            "closing the old child must not unlock the new owner"
        );
        drop(next);
        assert!(exclusive(&path, "busy").is_ok());
    }

    #[tokio::test]
    async fn a_waiting_directory_owner_proceeds_once_the_current_owner_releases() {
        let root = tempfile::tempdir().unwrap();
        let current = exclusive_directory(root.path(), "busy").unwrap();
        let waiting = tokio::spawn({
            let path = root.path().to_owned();
            async move { exclusive_directory_after_release(&path).await }
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiting.is_finished());

        drop(current);
        let next = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("released ownership must be acquired")
            .unwrap()
            .unwrap();
        assert!(exclusive_directory(root.path(), "busy").is_err());
        drop(next);
        assert!(exclusive_directory(root.path(), "busy").is_ok());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
