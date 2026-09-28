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
        return Err(Error::new(409, busy));
    }
    Ok(Guard(file))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
