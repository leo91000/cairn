//! Node-wide write admission. Base block files are the conversation disk
//! itself, never a cache: nothing here evicts them. Writes and consolidation
//! reserve headroom before releasing admission for durable I/O.
use std::{
    io,
    path::Path,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
};

static ADMISSION: Mutex<()> = Mutex::new(());
static PENDING_WRITES: PendingWrites = PendingWrites(AtomicU64::new(0));
pub(super) const TEMPORARY_BLOCK: &str = ".block-";

struct PendingWrites(AtomicU64);

pub(crate) struct WriteReservation<'a> {
    pending: &'a PendingWrites,
    bytes: u64,
}

impl PendingWrites {
    fn reserve(&self, bytes: u64) -> io::Result<WriteReservation<'_>> {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                pending.checked_add(bytes)
            })
            .map_err(|_| io::Error::other("Write headroom reservation overflow"))?;
        Ok(WriteReservation {
            pending: self,
            bytes,
        })
    }
}

impl Drop for WriteReservation<'_> {
    fn drop(&mut self) {
        // Callers retain the reservation until the durable write has finished,
        // including a partial/failed write. statvfs then accounts for its blocks.
        self.pending.0.fetch_sub(self.bytes, Ordering::Release);
    }
}

/// Sample before filesystem/quota space, while serialized with new admissions.
/// A concurrent release then leaves a conservative reservation in that snapshot.
pub(crate) fn pending_write_bytes() -> u64 {
    PENDING_WRITES.0.load(Ordering::Acquire)
}

/// The caller has checked space and must hold admission while reserving it.
pub(crate) fn reserve_write(
    _admission: &MutexGuard<'_, ()>,
    bytes: u64,
) -> io::Result<WriteReservation<'static>> {
    PENDING_WRITES.reserve(bytes)
}

/// Consolidation has drained readers of the old base. Remove only block files
/// referenced by neither the current base nor a retained sealed manifest.
/// The caller must release its journal mutex before taking node admission.
pub(super) fn retire(
    directory: &Path,
    needed: &std::collections::HashSet<&str>,
) -> io::Result<u64> {
    let _admission = admission()?;
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // An interrupted block write leaves only an unnamed temporary file.
        let abandoned = name.starts_with(TEMPORARY_BLOCK);
        if !(abandoned || crate::nodes::snapshots::valid_hash(&name))
            || needed.contains(name.as_ref())
            || !entry.file_type()?.is_file()
        {
            continue;
        }
        let bytes = entry.metadata()?.len();
        std::fs::remove_file(entry.path())?;
        removed += bytes;
    }
    std::fs::File::open(directory)?.sync_all()?;
    Ok(removed)
}

/// Shared admission for journal writes and base block files on this controller.
pub(crate) fn admission() -> io::Result<MutexGuard<'static, ()>> {
    ADMISSION
        .lock()
        .map_err(|e| io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_fixtures_are_isolated_from_another_test_admission() {
        crate::test_process::run_ignored(
            "storage::cache::tests::recovery_fixtures_isolation_child",
        );
    }

    #[test]
    #[ignore = "executes recovery fixture children while this process holds admission"]
    fn recovery_fixtures_isolation_child() {
        let _held = admission().unwrap();
        crate::test_process::run_ignored(
            "native::bases_tests::background_preparation_yields_to_foreground_intent_and_then_finishes_child",
        );
    }

    #[test]
    fn pending_write_headroom_is_withheld_from_free_space() {
        // Isolate the controller-global reservation from parallel unit tests.
        crate::test_process::run_ignored("storage::cache::tests::pending_write_headroom_child");
    }

    #[test]
    #[ignore = "isolated controller-global headroom child"]
    fn pending_write_headroom_child() {
        let root = tempfile::tempdir().unwrap();
        let admission = admission().unwrap();
        let reservation = reserve_write(&admission, u64::MAX / 2).unwrap();
        drop(admission);
        assert_eq!(super::super::policy::space(root.path()).unwrap().1, 0);
        drop(reservation);
        assert!(super::super::policy::space(root.path()).unwrap().1 > 0);
    }

    #[test]
    fn pending_write_reservations_cover_overlapping_io_and_release_on_failure() {
        let pending = PendingWrites(AtomicU64::new(0));
        let entered = std::sync::Barrier::new(5);
        let release = std::sync::Barrier::new(5);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    let _reservation = pending.reserve(1024 * 1024).unwrap();
                    entered.wait();
                    release.wait();
                });
            }
            entered.wait();
            assert_eq!(pending.0.load(Ordering::Acquire), 4 * 1024 * 1024);
            release.wait();
        });
        assert_eq!(pending.0.load(Ordering::Acquire), 0);

        let aborted = std::panic::catch_unwind(|| {
            let _reservation = pending.reserve(4096).unwrap();
            panic!("injected writer failure");
        });
        assert!(aborted.is_err());
        assert_eq!(pending.0.load(Ordering::Acquire), 0);
    }

    #[test]
    fn overflowing_write_reservation_does_not_change_prior_ownership() {
        let pending = PendingWrites(AtomicU64::new(0));
        let reservation = pending.reserve(u64::MAX - 1).unwrap();
        assert!(pending.reserve(2).is_err());
        assert_eq!(pending.0.load(Ordering::Acquire), u64::MAX - 1);
        drop(reservation);
        assert_eq!(pending.0.load(Ordering::Acquire), 0);
    }

    #[test]
    fn retirement_keeps_referenced_blocks_and_never_touches_journals() {
        let root = tempfile::tempdir().unwrap();
        let disk = root.path().join("environments/owned/lazy");
        let cache = disk.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let current = format!("b3-{}", "a".repeat(64));
        let obsolete = format!("b3-{}", "b".repeat(64));
        let abandoned = format!("{TEMPORARY_BLOCK}interrupted");
        for name in [&current, &obsolete, &abandoned] {
            std::fs::write(cache.join(name), b"block").unwrap();
        }
        std::fs::write(cache.join("unrelated"), b"kept").unwrap();
        std::fs::write(disk.join("journal-v2.sqlite"), b"unsaved work").unwrap();
        let needed = std::collections::HashSet::from([current.as_str()]);
        assert_eq!(retire(&cache, &needed).unwrap(), 10);
        assert!(cache.join(&current).exists());
        assert!(!cache.join(&obsolete).exists());
        assert!(!cache.join(&abandoned).exists());
        assert!(cache.join("unrelated").exists());
        assert_eq!(
            std::fs::read(disk.join("journal-v2.sqlite")).unwrap(),
            b"unsaved work"
        );
    }
}
