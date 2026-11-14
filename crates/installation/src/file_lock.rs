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
    acquire(path, busy, libc::LOCK_EX)
}

pub(crate) fn shared(path: &Path, busy: &str) -> Result<Guard> {
    acquire(path, busy, libc::LOCK_SH)
}

fn acquire(path: &Path, busy: &str, operation: libc::c_int) -> Result<Guard> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } != 0 {
        return Err(Error::conflict(busy));
    }
    Ok(Guard(file))
}

/// Wait only for real flock contention. Ownership and filesystem errors remain
/// distinct, and cancellation/deadline never leave a detached lock acquirer.
pub(crate) async fn exclusive_after_release(
    path: &Path,
    busy: &str,
    stop: &tokio_util::sync::CancellationToken,
    maximum_wait: std::time::Duration,
) -> Result<Guard> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    let deadline = tokio::time::Instant::now() + maximum_wait;
    loop {
        if stop.is_cancelled() {
            return Err(Error::unavailable("Storage lock acquisition stopped."));
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Guard(file));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(error.into());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::unavailable(busy));
        }
        tokio::select! {
            () = stop.cancelled() => {
                return Err(Error::unavailable("Storage lock acquisition stopped."));
            },
            () = tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + std::time::Duration::from_millis(10))) => {}
        }
    }
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
    async fn waiting_file_acquisition_settles_contention_cancellation_and_deadline() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("admission.lock");
        let held = exclusive(&path, "busy").unwrap();
        let stop = tokio_util::sync::CancellationToken::new();
        let mut waiting = tokio::spawn({
            let path = path.clone();
            let stop = stop.clone();
            async move { exclusive_after_release(&path, "busy", &stop, Duration::from_secs(2)).await }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut waiting)
                .await
                .is_err()
        );
        drop(held);
        let acquired = waiting.await.unwrap().unwrap();
        assert!(exclusive(&path, "busy").is_err());
        stop.cancel();
        assert!(
            exclusive_after_release(&path, "busy", &stop, Duration::from_secs(2))
                .await
                .is_err()
        );
        let uncancelled = tokio_util::sync::CancellationToken::new();
        assert_eq!(
            exclusive_after_release(&path, "still busy", &uncancelled, Duration::from_millis(30))
                .await
                .err()
                .unwrap()
                .message,
            "still busy"
        );
        drop(acquired);
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
