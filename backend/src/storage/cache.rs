//! Eviction visits only immutable clean cache files, never journals or raw disks.
//! Unlike the master's backup cache, this retains a working set up to the budget
//! and holds admission through a fill/write. Backup eviction instead protects
//! master-only recovery points and retires all duplicate S3 payloads on local nodes.
use super::policy::{Policy, space};
use std::{
    collections::{BTreeSet, HashMap},
    io,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant, SystemTime},
};

static EVICTION: Mutex<()> = Mutex::new(());
const RECONCILE_AFTER: Duration = Duration::from_secs(60);

struct Index {
    bytes: u64,
    files: HashMap<PathBuf, (u64, SystemTime)>,
    oldest: BTreeSet<(SystemTime, PathBuf)>,
    scanned_at: Instant,
}

impl Index {
    fn scan(state: &Path) -> io::Result<Self> {
        let mut index = Self {
            bytes: 0,
            files: HashMap::new(),
            oldest: BTreeSet::new(),
            scanned_at: Instant::now(),
        };
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
                    index.insert(
                        file.path(),
                        meta.len(),
                        meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    );
                }
            }
        }
        Ok(index)
    }

    fn insert(&mut self, path: PathBuf, size: u64, modified: SystemTime) {
        self.remove(&path);
        self.bytes = self.bytes.saturating_add(size);
        self.oldest.insert((modified, path.clone()));
        self.files.insert(path, (size, modified));
    }

    fn remove(&mut self, path: &Path) -> Option<u64> {
        let (size, modified) = self.files.remove(path)?;
        self.oldest.remove(&(modified, path.to_owned()));
        self.bytes = self.bytes.saturating_sub(size);
        Some(size)
    }

    fn touch(&mut self, path: &Path, modified: SystemTime) {
        if let Some(size) = self.remove(path) {
            self.insert(path.to_owned(), size, modified);
        }
    }
}

fn indexes() -> &'static Mutex<HashMap<PathBuf, Index>> {
    static INDEXES: OnceLock<Mutex<HashMap<PathBuf, Index>>> = OnceLock::new();
    INDEXES.get_or_init(Default::default)
}

/// A fill holds node-wide admission until its clean file is indexed.
pub(crate) struct Reservation {
    _admission: MutexGuard<'static, ()>,
    state: PathBuf,
}

impl Reservation {
    pub(crate) fn filled(&self, path: &Path, size: u64) -> io::Result<()> {
        let modified = std::fs::metadata(path)?
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut indexes = indexes()
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        indexes
            .get_mut(&self.state)
            .ok_or_else(|| io::Error::other("Clean cache index is missing"))?
            .insert(path.to_owned(), size, modified);
        Ok(())
    }
}

/// Cache hits update the eviction order without scanning the directory.
pub(crate) fn touched(state: &Path, path: &Path, modified: SystemTime) -> io::Result<()> {
    let mut indexes = indexes()
        .lock()
        .map_err(|e| io::Error::other(e.to_string()))?;
    if let Some(index) = indexes.get_mut(state) {
        index.touch(path, modified);
    }
    Ok(())
}

/// Publication has drained readers of the old base. Retire only clean block
/// files no longer referenced by the current manifest, updating the shared index.
/// The caller must release its journal mutex before taking cache admission.
pub(super) fn retire(
    state: Option<&Path>,
    directory: &Path,
    needed: &std::collections::HashSet<&str>,
) -> io::Result<u64> {
    let _admission = admission()?;
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut indexes = indexes()
        .lock()
        .map_err(|e| io::Error::other(e.to_string()))?;
    let mut index = state.and_then(|state| indexes.get_mut(state));
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !crate::nodes::snapshots::valid_hash(&name)
            || needed.contains(name.as_ref())
            || !entry.file_type()?.is_file()
        {
            continue;
        }
        let bytes = entry.metadata()?.len();
        std::fs::remove_file(entry.path())?;
        if let Some(index) = &mut index {
            index.remove(&entry.path());
        }
        removed += bytes;
    }
    Ok(removed)
}

pub fn make_room(state: &Path, policy: &Policy, incoming: u64) -> io::Result<bool> {
    Ok(reserve(state, policy, incoming)?.is_some())
}

pub(crate) fn reserve(
    state: &Path,
    policy: &Policy,
    incoming: u64,
) -> io::Result<Option<Reservation>> {
    let guard = admission()?;
    if evict(state, policy, incoming, false)? {
        Ok(Some(Reservation {
            _admission: guard,
            state: state.to_owned(),
        }))
    } else {
        Ok(None)
    }
}

/// Shared admission for clean fills and durable writes on this controller.
pub(crate) fn admission() -> io::Result<MutexGuard<'static, ()>> {
    EVICTION.lock().map_err(|e| io::Error::other(e.to_string()))
}

fn evict(state: &Path, policy: &Policy, incoming: u64, reconcile: bool) -> io::Result<bool> {
    let mut indexes = indexes()
        .lock()
        .map_err(|e| io::Error::other(e.to_string()))?;
    if !indexes.contains_key(state) {
        indexes.insert(state.to_owned(), Index::scan(state)?);
    }
    let index = indexes.get_mut(state).unwrap();
    if reconcile && index.scanned_at.elapsed() >= RECONCILE_AFTER {
        *index = Index::scan(state)?;
    }
    // A foreign cleanup may have removed indexed files. Rebuild once if that
    // becomes visible while evicting; controller fills are recorded above.
    for attempt in 0..2 {
        let (total, free) = space(state)?;
        let required = index
            .bytes
            .saturating_add(incoming)
            .saturating_sub(policy.cache_mi_b * 1024 * 1024)
            .max(
                policy
                    .reserve(total)
                    .saturating_add(incoming)
                    .saturating_sub(free),
            );
        let mut removed = 0;
        let mut missing = false;
        while removed < required {
            let Some((_, file)) = index.oldest.iter().next().cloned() else {
                break;
            };
            match std::fs::remove_file(&file) {
                Ok(()) => removed += index.remove(&file).unwrap_or(0),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    index.remove(&file);
                    missing = true;
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        if missing && attempt == 0 {
            *index = Index::scan(state)?;
            continue;
        }
        return Ok(
            removed >= required && space(state)?.1 > policy.reserve(total).saturating_add(incoming)
        );
    }
    unreachable!()
}

/// The VM monitor calls frequently; reconcile the index at most once a minute.
pub fn maintain(state: &Path, policy: &Policy) -> io::Result<()> {
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
    let _admission = admission()?;
    evict(state, policy, 0, true)?;
    last.insert(state.to_owned(), Instant::now());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "explicit large-cache admission benchmark"]
    fn large_clean_cache_admission_performance() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("disks/conversation/lazy/cache");
        std::fs::create_dir_all(&cache).unwrap();
        for index in 0..8_000 {
            std::fs::write(cache.join(format!("{index:064x}")), [1_u8; 4096]).unwrap();
        }
        let policy = Policy {
            cache_mi_b: 102400,
            reserve_mi_b: 64,
            reserve_percent: 1,
            ..Default::default()
        };
        let mut measurements = Vec::new();
        for _ in 0..6 {
            let at = std::time::Instant::now();
            assert!(make_room(root.path(), &policy, 0).unwrap());
            measurements.push(at.elapsed().as_secs_f64() * 1000.0);
        }
        println!("CACHE_ADMISSION_PERF files=8000 wall_ms={measurements:?}");
    }

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
