//! Point-in-time WAL counts and durability settings, without checkpoint work.
use rusqlite::{Connection, ffi};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Wal {
    pub frames: u64,
    pub checkpointed_frames: u64,
    pub pending_frames: u64,
    pub autocheckpoint_frames: u64,
    pub synchronous: u64,
}

impl Wal {
    pub fn read(connection: &Connection) -> Option<Self> {
        let mut frames = -1;
        let mut checkpointed_frames = -1;
        // SAFETY: the actor exclusively owns this live Connection. The bundled
        // SQLite NOOP mode checkpoints zero frames and provides counts in these
        // local output integers. The fixed main name and pointers are not retained.
        let result = unsafe {
            ffi::sqlite3_wal_checkpoint_v2(
                connection.handle(),
                c"main".as_ptr(),
                ffi::SQLITE_CHECKPOINT_NOOP,
                &mut frames,
                &mut checkpointed_frames,
            )
        };
        if result != ffi::SQLITE_OK {
            return None;
        }
        // SQLite reports -1 for a database without an available WAL snapshot.
        let frames = u64::try_from(frames).ok()?;
        let checkpointed_frames = u64::try_from(checkpointed_frames).ok()?;
        let pending_frames = frames.checked_sub(checkpointed_frames)?;
        let setting = |name| {
            let value = connection
                .pragma_query_value(None, name, |row| row.get::<_, i64>(0))
                .ok()?;
            u64::try_from(value).ok()
        };
        let synchronous = setting("synchronous")?;
        if synchronous > 3 {
            return None;
        }
        Some(Self {
            frames,
            checkpointed_frames,
            pending_frames,
            autocheckpoint_frames: setting("wal_autocheckpoint")?,
            synchronous,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_wal_is_not_reported_as_zero_pending_pages() {
        let connection = Connection::open_in_memory().unwrap();
        assert_eq!(Wal::read(&connection), None);
        assert_eq!(connection.total_changes(), 0);
    }

    #[test]
    #[ignore = "explicit WAL snapshot sampling overhead measurement"]
    fn measure_wal_snapshot_sampling() {
        let root = tempfile::tempdir().unwrap();
        let connection = Connection::open(root.path().join("wal.db")).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE pages(value BLOB);
                 INSERT INTO pages VALUES(zeroblob(8192));",
            )
            .unwrap();
        let before = Wal::read(&connection).unwrap();
        let changes_before = connection.total_changes();
        let samples = 10_000;
        let started = std::time::Instant::now();
        for _ in 0..samples {
            assert_eq!(std::hint::black_box(Wal::read(&connection)), Some(before));
        }
        let elapsed_ns = started.elapsed().as_nanos();
        assert_eq!(connection.total_changes(), changes_before);
        println!(
            "{}",
            serde_json::json!({
                "benchmark": "sqlite_wal_snapshot_sampling",
                "samples": samples,
                "elapsedNs": elapsed_ns,
                "meanSamplingNs": elapsed_ns / samples,
                "sqlite": rusqlite::version(),
                "changedRows": connection.total_changes() - changes_before,
            })
        );
    }
}
