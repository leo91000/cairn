//! Eviction visits only immutable clean cache files, never journals or raw disks.
//! Unlike the master's backup cache, this retains a working set up to the budget
//! and holds admission through a fill/write. Backup eviction instead protects
//! master-only recovery points and retires all duplicate S3 payloads on local nodes.
use super::policy::{Policy, space};
use std::{
    io,
    path::Path,
    sync::{Mutex, MutexGuard},
};
static EVICTION: Mutex<()> = Mutex::new(());
pub fn make_room(state: &Path, policy: &Policy, incoming: u64) -> io::Result<bool> {
    Ok(reserve(state, policy, incoming)?.is_some())
}
pub(crate) fn reserve(
    state: &Path,
    policy: &Policy,
    incoming: u64,
) -> io::Result<Option<MutexGuard<'static, ()>>> {
    let guard = admission()?;
    if evict(state, policy, incoming)? {
        Ok(Some(guard))
    } else {
        Ok(None)
    }
}
/// Shared admission for clean fills and durable writes on this controller.
pub(crate) fn admission() -> io::Result<MutexGuard<'static, ()>> {
    EVICTION.lock().map_err(|e| io::Error::other(e.to_string()))
}
fn evict(state: &Path, policy: &Policy, incoming: u64) -> io::Result<bool> {
    let (total, free) = space(state)?;
    let mut entries = Vec::new();
    let disks = state.join("disks");
    if disks.exists() {
        for entry in std::fs::read_dir(disks)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let cache = entry.path().join("lazy/cache");
            if !cache.exists() {
                continue;
            }
            for file in std::fs::read_dir(cache)? {
                let file = file?;
                if !file.file_type()?.is_file()
                    || !crate::nodes::snapshots::valid_hash(&file.file_name().to_string_lossy())
                {
                    continue;
                }
                let meta = file.metadata()?;
                entries.push((
                    file.path(),
                    meta.len(),
                    meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                ));
            }
        }
    }
    entries.sort_by_key(|entry| entry.2);
    let used = entries.iter().map(|e| e.1).sum::<u64>();
    let required = used
        .saturating_add(incoming)
        .saturating_sub(policy.cache_mi_b * 1024 * 1024)
        .max(
            policy
                .reserve(total)
                .saturating_add(incoming)
                .saturating_sub(free),
        );
    let mut removed = 0;
    for (file, size, _) in entries {
        if removed >= required {
            break;
        }
        match std::fs::remove_file(file) {
            Ok(()) => removed += size,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(removed >= required && space(state)?.1 > policy.reserve(total).saturating_add(incoming))
}

/// The VM monitor calls frequently; full LRU scans run at most once a second.
pub fn maintain(state: &Path, policy: &Policy) -> io::Result<()> {
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::OnceLock,
        time::{Duration, Instant},
    };
    static LAST: OnceLock<Mutex<HashMap<PathBuf, Instant>>> = OnceLock::new();
    let mut last = LAST
        .get_or_init(Default::default)
        .lock()
        .map_err(|e| io::Error::other(e.to_string()))?;
    if last
        .get(state)
        .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
    {
        return Ok(());
    }
    make_room(state, policy, 0)?;
    last.insert(state.to_owned(), Instant::now());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budget_is_shared_across_conversations_and_never_evicts_journals() {
        let root = tempfile::tempdir().unwrap();
        for name in ["first", "second"] {
            let disk = root.path().join("disks").join(name).join("lazy");
            std::fs::create_dir_all(disk.join("cache")).unwrap();
            std::fs::write(disk.join("cache").join("a".repeat(64)), b"clean").unwrap();
            std::fs::write(disk.join("journal.sqlite"), b"unsaved work").unwrap();
        }
        let policy = Policy {
            cache_mi_b: 0,
            reserve_mi_b: 64,
            reserve_percent: 1,
            ..Default::default()
        };
        assert!(make_room(root.path(), &policy, 0).unwrap());
        for name in ["first", "second"] {
            let disk = root.path().join("disks").join(name).join("lazy");
            assert_eq!(std::fs::read_dir(disk.join("cache")).unwrap().count(), 0);
            assert_eq!(
                std::fs::read(disk.join("journal.sqlite")).unwrap(),
                b"unsaved work"
            );
        }
    }
}
