//! A prebooted disk keeps its physical path when a conversation claims it.
//!
//! Ownership is permanent. Reserve the conversation pointer before recording
//! its physical owner; after the controller has fenced old VMMs, recovery
//! finishes an interrupted claim. Never move a live journal or follow an alias.
use crate::{
    error::{Error, Result},
    file_lock::{self, Guard},
    skills::{atomic_write, private_dir},
};
use std::{
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const POINTER: &str = "environment";
const OWNER: &str = "owner";
const OWNERSHIP_LOCK: &str = "ownership.lock";
pub(crate) const ROOTS: [&str; 2] = ["disks", "environments"];

fn identity(value: &str) -> Result<()> {
    crate::validation::uuid(value)?;
    if uuid::Uuid::parse_str(value)
        .map_err(Error::internal)?
        .to_string()
        != value
    {
        return Err(Error::bad("Disk identities must be canonical UUIDs."));
    }
    Ok(())
}

fn read_identity(path: &Path) -> Result<Option<String>> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !file.metadata()?.is_file() {
        return Err(Error::conflict("Invalid disk ownership record."));
    }
    let mut value = String::new();
    file.take(37).read_to_string(&mut value)?;
    identity(&value)?;
    Ok(Some(value))
}

fn real_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::conflict("Disk directory must not be an alias."));
    }
    Ok(())
}

/// Resolve on control operations only; block reads keep their existing Volume.
pub fn directory(state: &Path, run: &str) -> Result<PathBuf> {
    identity(run)?;
    let logical = state.join("disks").join(run);
    if logical.exists() {
        real_directory(&logical)?;
    }
    let Some(environment) = read_identity(&logical.join(POINTER))? else {
        return Ok(logical);
    };
    let physical = state.join("environments").join(environment);
    real_directory(&physical)?;
    if read_identity(&physical.join(OWNER))?.as_deref() != Some(run) {
        return Err(Error::conflict(
            "Disk ownership does not match its conversation.",
        ));
    }
    Ok(physical)
}

/// Hold the logical lock before resolving, so a concurrent claim cannot change
/// the path between an operation's lookup and acquisition of the physical lock.
pub struct Lease {
    pub directory: PathBuf,
    _owner: OwnerLease,
    _physical: Guard,
}

#[derive(Clone)]
pub struct OwnerLease {
    pub directory: PathBuf,
    _logical: std::sync::Arc<Guard>,
}

/// Bootstrap and VMM boot acquire their own physical lock. Keep this lease
/// across both operations and through shutdown to fence attribution changes.
pub async fn ownership(state: &Path, run: &str, busy: &str) -> Result<OwnerLease> {
    ownership_after_release(state, run, busy, Duration::ZERO).await
}

/// Like `ownership`, but waits up to `wait` for another holder of the disk,
/// such as an interrupted attempt whose VMM is still stopping.
pub async fn ownership_after_release(
    state: &Path,
    run: &str,
    busy: &str,
    wait: Duration,
) -> Result<OwnerLease> {
    identity(run)?;
    let logical = state.join("disks").join(run);
    if logical.exists() {
        real_directory(&logical)?;
    }
    private_dir(&logical).await?;

    let deadline = Instant::now() + wait;
    let logical_lock = loop {
        match file_lock::exclusive(&logical.join(OWNERSHIP_LOCK), busy) {
            Err(error) if error.is_conflict() && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            result => break result?,
        }
    };
    let physical = directory(state, run)?;
    Ok(OwnerLease {
        directory: physical,
        _logical: std::sync::Arc::new(logical_lock),
    })
}

pub async fn lock(state: &Path, run: &str, busy: &str) -> Result<Lease> {
    let owner = ownership(state, run, busy).await?;
    let physical = file_lock::exclusive(&owner.directory.join("lock"), busy)?;
    Ok(Lease {
        directory: owner.directory.clone(),
        _owner: owner,
        _physical: physical,
    })
}

async fn persist(path: &Path, value: &str) -> Result<()> {
    atomic_write(path, value.as_bytes()).await?;
    let parent = path.parent().unwrap().to_owned();
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        std::fs::File::open(&parent)?.sync_all()?;
        std::fs::File::open(parent.parent().unwrap())?.sync_all()
    })
    .await
    .map_err(Error::internal)??;
    Ok(())
}

fn vacant(logical: &Path, pointer: bool) -> Result<()> {
    for entry in std::fs::read_dir(logical)? {
        let entry = entry?;
        let name = entry.file_name();
        let temporary = name.to_str().is_some_and(|name| {
            name.strip_prefix('.')
                .and_then(|name| name.strip_suffix(".tmp"))
                .is_some_and(|id| identity(id).is_ok())
        }) && entry.file_type()?.is_file();
        if name != "lock" && name != OWNERSHIP_LOCK && !(pointer && name == POINTER) && !temporary {
            return Err(Error::conflict("Conversation already has a disk."));
        }
    }
    Ok(())
}

/// Called while the VMM owns the physical disk lock. The returned logical lock
/// must remain held until that VMM and its backend have both stopped.
pub async fn assign(state: &Path, environment: &str, run: &str) -> Result<OwnerLease> {
    identity(environment)?;
    identity(run)?;
    let physical = state.join("environments").join(environment);
    real_directory(&physical)?;
    let logical = state.join("disks").join(run);
    if logical.exists() {
        real_directory(&logical)?;
    }
    private_dir(&logical).await?;
    let lock = file_lock::exclusive(
        &logical.join(OWNERSHIP_LOCK),
        "Conversation disk is in use.",
    )?;
    if read_identity(&physical.join(OWNER))?.is_some() {
        return Err(Error::conflict("Prepared disk has already been claimed."));
    }
    vacant(&logical, false)?;
    // Reserving the pointer first also fences a second claim for this run if
    // the owner write fails. Any failed assignment retires the anonymous VM.
    persist(&logical.join(POINTER), environment).await?;
    persist(&physical.join(OWNER), run).await?;
    Ok(OwnerLease {
        directory: physical,
        _logical: std::sync::Arc::new(lock),
    })
}

/// Keep these inodes/records when deleting contents, so overlapping operations
/// still contend and an erased disk cannot later be assigned to another owner.
pub(crate) fn retained(name: &std::ffi::OsStr) -> bool {
    name == "lock" || name == OWNERSHIP_LOCK || name == POINTER || name == OWNER
}

/// Only after the controller's PID fence. Validate every claim before deleting
/// abandoned anonymous environments; never erase an interrupted owned claim.
pub async fn recover(state: &Path) -> Result<()> {
    for root in ROOTS {
        private_dir(&state.join(root)).await?;
    }
    let mut claims = std::collections::HashMap::new();
    let mut conversations = std::collections::HashMap::new();
    let mut logicals = tokio::fs::read_dir(state.join("disks")).await?;
    while let Some(entry) = logicals.next_entry().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let Some(environment) = read_identity(&entry.path().join(POINTER))? else {
            continue;
        };
        let run = entry.file_name().to_string_lossy().into_owned();
        identity(&run)?;
        vacant(&entry.path(), true)?;
        real_directory(&state.join("environments").join(&environment))?;
        if claims.insert(environment.clone(), run.clone()).is_some() {
            return Err(Error::conflict(
                "Prepared disk has conflicting conversation claims.",
            ));
        }
        conversations.insert(run, environment);
    }
    let mut unclaimed = Vec::new();
    let mut repair = Vec::new();
    let mut owners = std::collections::HashSet::new();
    let mut entries = tokio::fs::read_dir(state.join("environments")).await?;
    while let Some(entry) = entries.next_entry().await? {
        real_directory(&entry.path())?;
        let environment = entry.file_name().to_string_lossy().into_owned();
        identity(&environment)?;
        let recorded = read_identity(&entry.path().join(OWNER))?;
        let claimed = claims.get(&environment);
        let run = match (&recorded, claimed) {
            (Some(owner), Some(claim)) if owner != claim => {
                return Err(Error::conflict(
                    "Disk ownership does not match its conversation.",
                ));
            }
            (Some(owner), _) => owner.clone(),
            (None, Some(claim)) => {
                repair.push((entry.path().join(OWNER), claim.clone()));
                claim.clone()
            }
            (None, None) => {
                unclaimed.push(entry.path());
                continue;
            }
        };
        if !owners.insert(run.clone()) {
            return Err(Error::conflict("Conversation has conflicting disk owners."));
        }
        if conversations
            .get(&run)
            .is_some_and(|pointer| pointer != &environment)
        {
            return Err(Error::conflict("Conversation has conflicting disk owners."));
        }
        let logical = state.join("disks").join(&run);
        if logical.exists() {
            real_directory(&logical)?;
            match read_identity(&logical.join(POINTER))? {
                Some(pointer) if pointer == environment => continue,
                Some(_) => {
                    return Err(Error::conflict("Conversation has conflicting disk owners."));
                }
                None => vacant(&logical, false)?,
            }
        }
        repair.push((logical.join(POINTER), environment));
    }
    for (record, value) in repair {
        private_dir(record.parent().unwrap()).await?;
        persist(&record, &value).await?;
    }
    for directory in unclaimed {
        tokio::fs::remove_dir_all(directory).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::MetadataExt, sync::Arc};

    async fn prepared(state: &Path) -> (String, PathBuf, Guard) {
        let environment = crate::config::id();
        let physical = state.join("environments").join(&environment);
        private_dir(&physical).await.unwrap();
        let lock = file_lock::exclusive(&physical.join("lock"), "busy").unwrap();
        (environment, physical, lock)
    }

    #[tokio::test]
    async fn a_replacement_attempt_waits_for_the_interrupted_vm_to_release_its_disk() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let interrupted = ownership(state.path(), &run, "busy").await.unwrap();
        assert!(
            ownership_after_release(state.path(), &run, "busy", Duration::from_millis(100))
                .await
                .is_err(),
            "a disk that stays in use is still refused"
        );

        // Production 2026-10-05: the replacement arrived 3 s after the
        // interruption, while the old VMM still held the disk.
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            drop(interrupted);
        });
        let owner = ownership_after_release(state.path(), &run, "busy", Duration::from_secs(5))
            .await
            .expect("the disk is claimed once the old VMM releases it");
        release.await.unwrap();
        assert_eq!(owner.directory, state.path().join("disks").join(&run));
    }

    #[tokio::test]
    async fn claim_keeps_open_disk_and_both_locks_until_vm_retires() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let (environment, physical, physical_lock) = prepared(state.path()).await;
        let journal = physical.join("journal-fixture");
        std::fs::write(&journal, b"durable bytes").unwrap();
        let open = std::fs::File::open(&journal).unwrap();
        let logical_lock = assign(state.path(), &environment, &run).await.unwrap();
        assert_eq!(directory(state.path(), &run).unwrap(), physical);
        assert_eq!(
            open.metadata().unwrap().ino(),
            std::fs::metadata(journal).unwrap().ino()
        );
        assert!(lock(state.path(), &run, "busy").await.is_err());
        assert!(file_lock::exclusive(&physical.join("lock"), "busy").is_err());
        let pending_write = logical_lock.clone();
        drop(logical_lock);
        assert!(lock(state.path(), &run, "busy").await.is_err());
        drop(physical_lock);
        assert!(
            lock(state.path(), &run, "busy").await.is_err(),
            "pending authorization retains the owner after VMM retirement"
        );
        drop(pending_write);
        let lease = lock(state.path(), &run, "busy").await.unwrap();
        assert_eq!(lease.directory, physical);
        drop(lease);
        assert!(
            assign(state.path(), &environment, &crate::config::id())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn legacy_mutation_fences_claim_and_existing_disk_is_never_overwritten() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let (environment, physical, _lock) = prepared(state.path()).await;
        let legacy = lock(state.path(), &run, "busy").await.unwrap();
        assert!(assign(state.path(), &environment, &run).await.is_err());
        std::fs::write(legacy.directory.join("runtime.json"), b"existing").unwrap();
        drop(legacy);
        assert!(assign(state.path(), &environment, &run).await.is_err());
        assert!(!physical.join(OWNER).exists());
    }

    #[tokio::test]
    async fn interrupted_pointer_claim_fences_another_disk_and_recovery_finishes_it() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let (environment, physical, physical_lock) = prepared(state.path()).await;
        let (other, other_physical, other_lock) = prepared(state.path()).await;
        let logical = state.path().join("disks").join(&run);
        private_dir(&logical).await.unwrap();
        persist(&logical.join(POINTER), &environment).await.unwrap();
        assert!(directory(state.path(), &run).is_err());
        assert!(assign(state.path(), &other, &run).await.is_err());
        assert!(!other_physical.join(OWNER).exists());
        drop((physical_lock, other_lock));
        recover(state.path()).await.unwrap();
        assert_eq!(directory(state.path(), &run).unwrap(), physical);
        assert_eq!(
            read_identity(&physical.join(OWNER)).unwrap().as_deref(),
            Some(run.as_str())
        );
        assert!(!other_physical.exists());
    }

    #[tokio::test]
    async fn two_conversation_pointers_cannot_claim_one_environment() {
        let state = tempfile::tempdir().unwrap();
        let (environment, physical, physical_lock) = prepared(state.path()).await;
        for _ in 0..2 {
            let logical = state.path().join("disks").join(crate::config::id());
            private_dir(&logical).await.unwrap();
            persist(&logical.join(POINTER), &environment).await.unwrap();
        }
        drop(physical_lock);
        assert!(recover(state.path()).await.is_err());
        assert!(physical.exists());
        assert!(!physical.join(OWNER).exists());
    }

    #[tokio::test]
    async fn restart_recovers_crash_between_owner_and_pointer_without_reassigning() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let (environment, physical, lock) = prepared(state.path()).await;
        persist(&physical.join(OWNER), &run).await.unwrap();
        std::fs::write(physical.join("journal-fixture"), b"owned").unwrap();
        drop(lock); // Controller's PID fence has completed.
        recover(state.path()).await.unwrap();
        assert_eq!(directory(state.path(), &run).unwrap(), physical);
        assert!(physical.join("journal-fixture").exists());
        assert!(
            assign(state.path(), &environment, &crate::config::id())
                .await
                .is_err()
        );
        recover(state.path()).await.unwrap();
    }

    #[tokio::test]
    async fn conflicting_owners_fail_before_cleanup_or_partial_recovery() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let (_, first, first_lock) = prepared(state.path()).await;
        let (_, second, second_lock) = prepared(state.path()).await;
        let (_, anonymous, anonymous_lock) = prepared(state.path()).await;
        persist(&first.join(OWNER), &run).await.unwrap();
        persist(&second.join(OWNER), &run).await.unwrap();
        drop((first_lock, second_lock, anonymous_lock));
        assert!(recover(state.path()).await.is_err());
        assert!(anonymous.exists());
        assert!(!state.path().join("disks").join(run).join(POINTER).exists());
    }

    #[tokio::test]
    async fn claimed_crash_conflicting_with_legacy_disk_preserves_both() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let (_, physical, lock) = prepared(state.path()).await;
        persist(&physical.join(OWNER), &run).await.unwrap();
        let legacy = state.path().join("disks").join(&run);
        private_dir(&legacy).await.unwrap();
        std::fs::write(legacy.join("lazy-fixture"), b"old").unwrap();
        drop(lock);
        assert!(recover(state.path()).await.is_err());
        assert!(physical.join(OWNER).exists());
        assert_eq!(std::fs::read(legacy.join("lazy-fixture")).unwrap(), b"old");
    }

    #[tokio::test]
    async fn aliases_and_mismatched_owners_are_rejected() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let (environment, physical, _lock) = prepared(state.path()).await;
        let logical_lock = assign(state.path(), &environment, &run).await.unwrap();
        drop(logical_lock);
        persist(&physical.join(OWNER), &crate::config::id())
            .await
            .unwrap();
        assert!(directory(state.path(), &run).is_err());
        persist(&physical.join(OWNER), &run).await.unwrap();
        std::fs::remove_file(physical.join(OWNER)).unwrap();
        let secret = state.path().join("external-owner");
        std::fs::write(&secret, &run).unwrap();
        std::os::unix::fs::symlink(secret, physical.join(OWNER)).unwrap();
        assert!(directory(state.path(), &run).is_err());
        let invalid = Arc::new(state.path().to_owned());
        assert!(directory(&invalid, "../outside").is_err());
    }

    #[tokio::test]
    async fn restart_removes_only_unclaimed_environments() {
        let state = tempfile::tempdir().unwrap();
        let (_, anonymous, anonymous_lock) = prepared(state.path()).await;
        let (environment, owned, owned_lock) = prepared(state.path()).await;
        let run = crate::config::id();
        let logical_lock = assign(state.path(), &environment, &run).await.unwrap();
        drop((anonymous_lock, owned_lock, logical_lock));
        recover(state.path()).await.unwrap();
        assert!(!anonymous.exists());
        assert_eq!(directory(state.path(), &run).unwrap(), owned);
        assert!(retained(std::ffi::OsStr::new(OWNER)));
    }
}
