use super::*;

impl LazyDisk {
    /// Append an empty, aligned tail without fetching or rewriting the base.
    /// Old sealed manifests must be acknowledged first: their publication may
    /// replace the base, and must never reinstall an earlier disk geometry.
    pub(crate) fn grow(&self, size: u64) -> io::Result<()> {
        let _writes = self.write_gate.write().map_err(failure)?;
        let _publication = self.publication.write().map_err(failure)?;
        let current = self.size();
        if size == current {
            return Ok(());
        }
        if size < current
            || size > 1 << 40
            || !size.is_multiple_of(BLOCK)
            || !current.is_multiple_of(BLOCK)
            || self.layout()? != super::super::layout::Layout::Flat
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unsupported disk growth geometry",
            ));
        }
        let journal = self.journal.lock().map_err(failure)?;
        journal.sync()?;
        let mut db = self.db.lock().map_err(failure)?;
        let tx = db.transaction().map_err(failure)?;
        let sealed: bool = tx
            .query_row("SELECT EXISTS(SELECT 1 FROM sealed)", [], |row| row.get(0))
            .map_err(failure)?;
        if sealed {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Disk growth is waiting for publication",
            ));
        }
        let mut base = self.base.lock().map_err(failure)?;
        let mut manifest = base.as_ref().clone();
        manifest["size"] = size.into();
        let blocks = manifest["blocks"].as_array_mut().unwrap();
        for offset in (current..size).step_by(BLOCK as usize) {
            blocks.push(serde_json::json!({ "offset": offset, "size": BLOCK, "hash": null }));
        }
        validate(&manifest)?;
        tx.execute(
            "UPDATE state SET manifest=?1 WHERE id=1",
            [manifest.to_string()],
        )
        .map_err(failure)?;
        tx.commit().map_err(failure)?;
        *base = Arc::new(manifest);
        self.size.store(size, Ordering::Release);
        #[cfg(test)]
        journal::crash_point(&self.directory, "growth_committed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    struct Empty;

    impl BlockSource for Empty {
        fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
            panic!("Growing a sparse disk must not fetch remote blocks")
        }
    }

    fn manifest() -> Value {
        serde_json::json!({
            "version": 1, "size": BLOCK, "blockSize": BLOCK,
            "blocks": [{ "offset": 0, "size": BLOCK, "hash": null }],
        })
    }

    #[test]
    fn growth_preserves_writes_and_zero_tail_across_reopen_without_materialization() {
        let root = tempfile::tempdir().unwrap();
        let disk = LazyDisk::create(root.path(), &manifest(), Arc::new(Empty)).unwrap();
        disk.write_at(0, b"retained").unwrap();
        disk.grow(2 * BLOCK).unwrap();
        assert_eq!(disk.size(), 2 * BLOCK);
        let mut tail = [1; 8];
        disk.read_at(BLOCK, &mut tail).unwrap();
        assert_eq!(tail, [0; 8]);
        disk.write_at(BLOCK, b"new-tail").unwrap();
        drop(disk);

        let disk = LazyDisk::open(root.path(), Arc::new(Empty)).unwrap();
        assert_eq!(disk.size(), 2 * BLOCK);
        for (offset, expected) in [(0, b"retained"), (BLOCK, b"new-tail")] {
            let mut bytes = [0; 8];
            disk.read_at(offset, &mut bytes).unwrap();
            assert_eq!(&bytes, expected);
        }
        let generation = disk.seal().unwrap();
        let captured = disk.capture(generation).unwrap();
        assert_eq!(captured["size"], 2 * BLOCK);
        assert_eq!(captured["blocks"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn committed_growth_survives_process_death_before_kernel_resize() {
        use std::io::BufRead;
        let root = tempfile::tempdir().unwrap();
        let disk = LazyDisk::create(root.path(), &manifest(), Arc::new(Empty)).unwrap();
        disk.write_at(0, b"retained").unwrap();
        disk.sync().unwrap();
        drop(disk);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["crash_growth_writer", "--nocapture"])
            .env("CAIRN_JOURNAL_CRASH_DIR", root.path())
            .env("CAIRN_JOURNAL_CRASH_PHASE", "growth_committed")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let reached = std::io::BufReader::new(child.stdout.take().unwrap())
            .lines()
            .any(|line| line.unwrap().contains("JOURNAL_CRASH_READY"));
        let _ = child.kill();
        child.wait().unwrap();
        assert!(reached);
        let disk = LazyDisk::open(root.path(), Arc::new(Empty)).unwrap();
        assert_eq!(disk.size(), 2 * BLOCK);
        let mut bytes = [0; 8];
        disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"retained");
        disk.write_at(BLOCK, b"new-tail").unwrap();
        disk.sync().unwrap();
    }

    #[test]
    fn crash_growth_writer() {
        let Some(root) = std::env::var_os("CAIRN_JOURNAL_CRASH_DIR") else {
            return;
        };
        if std::env::var("CAIRN_JOURNAL_CRASH_PHASE").as_deref() != Ok("growth_committed") {
            return;
        }
        LazyDisk::open(Path::new(&root), Arc::new(Empty))
            .unwrap()
            .grow(2 * BLOCK)
            .unwrap();
    }

    #[test]
    fn growth_rejects_paired_disks_without_rewriting_their_geometry() {
        let root = tempfile::tempdir().unwrap();
        let mut manifest = manifest();
        let size = 256 * 1024 * 1024;
        manifest["size"] = size.into();
        manifest["blocks"] = (0..size)
            .step_by(BLOCK as usize)
            .map(|offset| serde_json::json!({ "offset": offset, "size": BLOCK, "hash": null }))
            .collect();
        super::super::super::layout::Layout::paired(size)
            .unwrap()
            .store(&mut manifest)
            .unwrap();
        let disk = LazyDisk::create(root.path(), &manifest, Arc::new(Empty)).unwrap();
        assert!(disk.grow(2 * size).is_err());
        assert_eq!(disk.size(), size);
        assert_eq!(LazyDisk::geometry(root.path()).unwrap().0, size);
    }

    #[test]
    fn growth_waits_for_old_publication_and_never_shrinks_or_changes_layout() {
        let root = tempfile::tempdir().unwrap();
        let disk = LazyDisk::create(root.path(), &manifest(), Arc::new(Empty)).unwrap();
        let generation = disk.seal().unwrap();
        assert_eq!(
            disk.grow(2 * BLOCK).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        disk.capture(generation).unwrap();
        disk.commit_published(generation, "published").unwrap();
        disk.grow(2 * BLOCK).unwrap();
        // An acknowledgement replay must not reinstall the old geometry.
        disk.commit_published(generation, "published").unwrap();
        assert_eq!(disk.size(), 2 * BLOCK);
        assert!(disk.grow(BLOCK).is_err());
        assert!(disk.grow(2 * BLOCK + 512).is_err());
        assert_eq!(disk.size(), 2 * BLOCK);
    }
}
