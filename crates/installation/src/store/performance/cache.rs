//! Read-only pager counters on the actor's own SQLite connection.
use rusqlite::{Connection, ffi};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Cache {
    pub hits: u64,
    pub misses: u64,
    pub writes: u64,
    pub bytes: u64,
}

impl Cache {
    pub fn read(connection: &Connection) -> Option<Self> {
        let status = |operation| {
            let mut current = 0;
            let mut peak = 0;
            // SAFETY: the actor exclusively owns this live Connection. The bundled
            // SQLite function reads status, writes these local integers and retains
            // no pointers. The handle never escapes; reset=0 preserves its counters.
            let result = unsafe {
                ffi::sqlite3_db_status64(connection.handle(), operation, &mut current, &mut peak, 0)
            };
            (result == ffi::SQLITE_OK)
                .then(|| u64::try_from(current).ok())
                .flatten()
        };
        Some(Self {
            hits: status(ffi::SQLITE_DBSTATUS_CACHE_HIT)?,
            misses: status(ffi::SQLITE_DBSTATUS_CACHE_MISS)?,
            writes: status(ffi::SQLITE_DBSTATUS_CACHE_WRITE)?,
            bytes: status(ffi::SQLITE_DBSTATUS_CACHE_USED)?,
        })
    }

    pub fn delta(self, before: Self) -> Option<Self> {
        Some(Self {
            hits: self.hits.checked_sub(before.hits)?,
            misses: self.misses.checked_sub(before.misses)?,
            writes: self.writes.checked_sub(before.writes)?,
            bytes: self.bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_pager_counts_cold_and_warm_reads_without_resetting_or_writing() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("cache.db");
        let writer = Connection::open(&file).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE pages(value BLOB)")
            .unwrap();
        for _ in 0..8 {
            writer
                .execute("INSERT INTO pages VALUES(zeroblob(16384))", [])
                .unwrap();
        }
        let before_write = Cache::read(&writer).unwrap();
        writer
            .execute("UPDATE pages SET value=randomblob(16384)", [])
            .unwrap();
        assert!(
            Cache::read(&writer)
                .unwrap()
                .delta(before_write)
                .unwrap()
                .writes
                > 0
        );

        let reader = Connection::open(&file).unwrap();
        reader.execute_batch("PRAGMA query_only=ON").unwrap();
        let initial = Cache::read(&reader).unwrap();
        let read = || {
            reader
                .prepare("SELECT value FROM pages")
                .unwrap()
                .query_map([], |row| row.get::<_, Vec<u8>>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(read().len(), 8);
        let cold = Cache::read(&reader).unwrap();
        assert!(cold.delta(initial).unwrap().misses > 0);
        assert_eq!(read().len(), 8);
        let warm = Cache::read(&reader).unwrap();
        assert!(warm.delta(cold).unwrap().hits > 0);
        assert!(warm.bytes > 0);
        assert_eq!(warm.writes, 0);
        assert_eq!(reader.total_changes(), 0);
        assert_eq!(
            Cache::read(&reader),
            Some(warm),
            "sampling must not reset counters"
        );
    }

    #[test]
    fn a_reset_counter_invalidates_the_interval_instead_of_wrapping() {
        let before = Cache {
            hits: 10,
            ..Cache::default()
        };
        assert!(Cache::default().delta(before).is_none());
    }

    #[test]
    #[ignore = "explicit pager-counter sampling overhead measurement"]
    fn measure_pager_counter_sampling() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE pages(value BLOB); INSERT INTO pages VALUES(zeroblob(16384))",
            )
            .unwrap();
        let samples = 100_000;
        let started = std::time::Instant::now();
        for _ in 0..samples {
            std::hint::black_box(Cache::read(&connection)).unwrap();
        }
        let elapsed_ns = started.elapsed().as_nanos();
        println!(
            "{}",
            serde_json::json!({
                "benchmark": "sqlite_pager_counter_sampling",
                "samples": samples,
                "elapsedNs": elapsed_ns,
                "meanSamplingNs": elapsed_ns / samples,
                "sqlite": rusqlite::version(),
            })
        );
    }
}
