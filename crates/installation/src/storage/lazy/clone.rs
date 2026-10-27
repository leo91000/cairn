//! Offline local bases share only sealed append segments. Every child owns its
//! SQLite state, connection context, locks and all later journal generations.
use super::*;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

fn base_metadata(source: &Path, db: &Connection) -> io::Result<(u64, Vec<String>)> {
    let (generation, manifest): (i64, String) = db
        .query_row(
            "SELECT generation,manifest FROM state WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(failure)?;
    let manifest: Value = serde_json::from_str(&manifest).map_err(failure)?;
    let size = validate(&manifest)?;
    let context: String = db
        .query_row(
            "SELECT value FROM settings WHERE key='context'",
            [],
            |row| row.get(0),
        )
        .map_err(failure)?;
    let context: Value = serde_json::from_str(&context).map_err(failure)?;
    if context["unassigned"] != true
        || context.get("master").is_some()
        || context.get("grant").is_some()
        || manifest["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| !block["hash"].is_null())
    {
        return Err(failure("A base must be anonymous and entirely local"));
    }
    let mut statement = db
        .prepare("SELECT name,generation,first_sequence FROM segments")
        .map_err(failure)?;
    let segments = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(failure)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(failure)?;
    let mut names = Vec::new();
    for (name, stored_generation, first) in segments {
        if stored_generation < 1
            || stored_generation >= generation
            || first < 1
            || name != format!("payload-{stored_generation}-{first}.segment")
        {
            return Err(failure("A base must contain only sealed journal segments"));
        }
        let metadata = std::fs::symlink_metadata(source.join(&name))?;
        if !metadata.is_file() || metadata.is_symlink() {
            return Err(failure("Invalid base payload"));
        }
        names.push(name);
    }
    Ok((size, names))
}

struct OfflineLock(File);

impl Drop for OfflineLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn offline(source: &Path) -> io::Result<(OfflineLock, Connection)> {
    let lock = File::options()
        .read(true)
        .write(true)
        .open(source.join("journal.lock"))?;
    lock.try_lock()?;
    if std::fs::metadata(source.join("journal-v2.sqlite-wal"))
        .is_ok_and(|metadata| metadata.len() != 0)
    {
        return Err(failure("A base must be offline before cloning"));
    }
    let db = Connection::open_with_flags(
        source.join("journal-v2.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(failure)?;
    db.execute_batch("BEGIN").map_err(failure)?;
    Ok((OfflineLock(lock), db))
}

impl LazyDisk {
    /// Validate the sealed prefix without allowing interrupted-tail recovery to
    /// truncate a shared payload. Both checks use the same SQLite read snapshot.
    pub(crate) fn verify_sealed(source: &Path) -> io::Result<()> {
        let (_lock, db) = offline(source)?;
        let (size, _) = base_metadata(source, &db)?;
        let _journal = journal::Journal::open_verified(source, &db, size)?;
        Ok(())
    }

    pub(crate) fn clone_sealed(source: &Path, target: &Path) -> io::Result<()> {
        let (_lock, db) = offline(source)?;
        let (_, segments) = base_metadata(source, &db)?;
        std::fs::DirBuilder::new().mode(0o700).create(target)?;
        File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(target.join("journal.lock"))?;
        {
            let name = "journal-v2.sqlite";
            let mut input = File::options()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(source.join(name))?;
            let mut output = File::options()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(target.join(name))?;
            if unsafe {
                libc::ioctl(
                    std::os::fd::AsRawFd::as_raw_fd(&output),
                    libc::FICLONE,
                    std::os::fd::AsRawFd::as_raw_fd(&input),
                )
            } != 0
            {
                output.set_len(0)?;
                std::io::copy(&mut input, &mut output)?;
            }
            output.sync_all()?;
        }
        for name in segments {
            std::fs::hard_link(source.join(&name), target.join(name))?;
        }
        File::open(target)?.sync_all()?;
        Ok(())
    }

    /// This new, still-unclaimed clone becomes a conventional authorized disk
    /// before its existing environment pointer can make it visible to a run.
    pub(crate) fn bind_cloned_context(target: &Path, context: &Value) -> io::Result<()> {
        let (_lock, db) = offline(target)?;
        base_metadata(target, &db)?;
        drop(db);
        super::super::policy::Policy::for_node(&context["policy"]).map_err(failure)?;
        let encoded = super::state::encode_context(context)?;
        let db = journal::metadata(target, None)?;
        db.execute(
            "UPDATE settings SET value=?1 WHERE key='context'",
            [encoded],
        )
        .map_err(failure)?;
        let busy: i64 = db
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .map_err(failure)?;
        if busy != 0 {
            return Err(failure("Private clone metadata is still busy"));
        }
        drop(db);
        File::open(target.join("journal-v2.sqlite"))?.sync_all()?;
        File::open(target)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Disk;
    use std::os::unix::fs::MetadataExt;

    struct LocalOnly;

    impl BlockSource for LocalOnly {
        fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
            Err(failure("A local base cannot fetch remote data"))
        }
    }

    #[tokio::test]
    async fn malformed_base_metadata_never_enables_truncation_of_a_shared_payload() {
        use std::io::Write;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let manifest = serde_json::json!({
            "version": 1,
            "size": BLOCK,
            "blockSize": BLOCK,
            "blocks": [{ "offset": 0, "size": BLOCK, "hash": null }]
        });
        let context = serde_json::json!({
            "unassigned": true,
            "policy": super::super::super::policy::Policy::default()
        });
        let disk = crate::storage::runtime::create_prepared(&source, &manifest, &context)
            .await
            .unwrap();
        disk.write_at(0, b"original").unwrap();
        disk.seal().unwrap();
        drop(disk);
        let child = root.path().join("child");
        LazyDisk::clone_sealed(&source, &child).unwrap();
        let payload = std::fs::read_dir(&source)
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("payload-"))
            .unwrap()
            .path();
        File::options()
            .append(true)
            .open(&payload)
            .unwrap()
            .write_all(&[1, 2, 3])
            .unwrap();
        let length = std::fs::metadata(&payload).unwrap().len();
        let db = Connection::open(source.join("journal-v2.sqlite")).unwrap();
        db.execute("UPDATE state SET generation=1 WHERE id=1", [])
            .unwrap();
        drop(db);
        assert!(LazyDisk::verify_sealed(&source).is_err());
        assert_eq!(std::fs::metadata(&payload).unwrap().len(), length);
        assert_eq!(
            std::fs::metadata(child.join(payload.file_name().unwrap()))
                .unwrap()
                .len(),
            length
        );
    }

    #[tokio::test]
    async fn sealed_children_share_payloads_and_keep_independent_writes_after_base_eviction() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let manifest = serde_json::json!({
            "version": 1,
            "size": BLOCK,
            "blockSize": BLOCK,
            "blocks": [{ "offset": 0, "size": BLOCK, "hash": null }]
        });
        let context = serde_json::json!({
            "unassigned": true,
            "policy": super::super::super::policy::Policy::default()
        });
        let disk = crate::storage::runtime::create_prepared(&source, &manifest, &context)
            .await
            .unwrap();
        disk.write_at(0, b"original").unwrap();
        disk.sync().unwrap();
        assert!(LazyDisk::clone_sealed(&source, &root.path().join("active")).is_err());
        disk.seal().unwrap();
        drop(disk);
        let first = root.path().join("first");
        let second = root.path().join("second");
        LazyDisk::clone_sealed(&source, &first).unwrap();
        LazyDisk::clone_sealed(&source, &second).unwrap();
        let segment = std::fs::read_dir(&source)
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("payload-"))
            .unwrap()
            .file_name();
        let inode = std::fs::metadata(source.join(&segment)).unwrap().ino();
        assert_eq!(
            std::fs::metadata(first.join(&segment)).unwrap().ino(),
            inode
        );
        assert_eq!(
            std::fs::metadata(second.join(&segment)).unwrap().ino(),
            inode
        );
        assert_ne!(
            std::fs::metadata(first.join("journal-v2.sqlite"))
                .unwrap()
                .ino(),
            std::fs::metadata(second.join("journal-v2.sqlite"))
                .unwrap()
                .ino()
        );
        let first_disk = LazyDisk::open(&first, Arc::new(LocalOnly)).unwrap();
        first_disk.write_at(0, b"first!!!").unwrap();
        first_disk.seal().unwrap();
        drop(first_disk);
        let second_disk = LazyDisk::open(&second, Arc::new(LocalOnly)).unwrap();
        second_disk.write_at(8, b"second").unwrap();
        second_disk.sync().unwrap();
        drop(second_disk);
        let original = LazyDisk::open(&source, Arc::new(LocalOnly)).unwrap();
        let mut bytes = [0; 8];
        original.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"original");
        drop(original);
        std::fs::remove_dir_all(&source).unwrap();
        let first_disk = LazyDisk::open(&first, Arc::new(LocalOnly)).unwrap();
        first_disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"first!!!");
        let second_disk = LazyDisk::open(&second, Arc::new(LocalOnly)).unwrap();
        let mut bytes = [0; 14];
        second_disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"originalsecond");
    }
}
