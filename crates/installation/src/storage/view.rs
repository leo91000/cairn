//! Independently addressed devices over one disk and one durability boundary.
use super::{Disk, DiskWrite, device::range};
use std::{io, sync::Arc};

pub struct DiskView {
    disk: Arc<dyn Disk>,
    offset: u64,
    size: u64,
}

impl DiskView {
    pub fn new(disk: Arc<dyn Disk>, offset: u64, size: u64) -> io::Result<Self> {
        if size == 0 || offset.checked_add(size).is_none_or(|end| end > disk.size()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid disk view",
            ));
        }
        Ok(Self { disk, offset, size })
    }

    fn translate(&self, offset: u64, length: usize) -> io::Result<u64> {
        range(self.size, offset, length)?;
        // Construction validates the complete view, so a checked local range
        // cannot overflow when translated into the backing disk.
        Ok(self.offset + offset)
    }
}

impl Disk for DiskView {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        self.disk
            .read_at(self.translate(offset, bytes.len())?, bytes)
    }

    fn write_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.disk
            .write_at(self.translate(offset, bytes.len())?, bytes)
    }

    fn write_batch(&self, writes: &[DiskWrite<'_>]) -> io::Result<()> {
        let translated = writes
            .iter()
            .map(|write| {
                Ok(DiskWrite {
                    offset: self.translate(write.offset, write.bytes.len())?,
                    bytes: write.bytes,
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        self.disk.write_batch(&translated)
    }

    fn sync(&self) -> io::Result<()> {
        // A flush covers both devices when they share the same journal.
        self.disk.sync()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{BlockSource, LazyDisk, LocalDisk, digest};
    use serde_json::json;
    use std::{collections::HashMap, sync::Mutex};

    #[test]
    fn views_reject_cross_boundary_and_overflow_access_before_writing() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("disk");
        std::fs::write(&path, b"abcdefghijklmnop").unwrap();
        let disk: Arc<dyn Disk> = Arc::new(LocalDisk::open(&path, true).unwrap());
        assert!(DiskView::new(disk.clone(), 0, 0).is_err());
        assert!(DiskView::new(disk.clone(), u64::MAX, 2).is_err());
        assert!(DiskView::new(disk.clone(), 8, 9).is_err());
        let os = DiskView::new(disk.clone(), 0, 8).unwrap();
        let workspace = DiskView::new(disk.clone(), 8, 8).unwrap();
        assert!(workspace.read_at(u64::MAX, &mut [0; 2]).is_err());
        assert!(os.write_at(7, b"XX").is_err());
        assert!(
            workspace
                .write_batch(&[
                    DiskWrite {
                        offset: 0,
                        bytes: b"XX"
                    },
                    DiskWrite {
                        offset: 7,
                        bytes: b"YY"
                    },
                ])
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdefghijklmnop");
        workspace
            .write_batch(&[
                DiskWrite {
                    offset: 0,
                    bytes: b"XY",
                },
                DiskWrite {
                    offset: 6,
                    bytes: b"ZZ",
                },
            ])
            .unwrap();
        os.sync().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdefghXYklmnZZ");
    }

    #[derive(Default)]
    struct Published(Mutex<HashMap<String, Vec<u8>>>);

    impl BlockSource for Published {
        fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .get(hash)
                .cloned()
                .ok_or_else(|| io::Error::other("Unpublished block"))
        }
    }

    #[test]
    fn one_generation_captures_both_views_and_old_ack_preserves_new_writes() {
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(Published::default());
        let block = 4 * 1024 * 1024;
        let manifest = json!({
            "version": 1, "size": 2 * block, "blockSize": block,
            "blocks": [
                { "offset": 0, "size": block, "hash": null },
                { "offset": block, "size": block, "hash": null }
            ]
        });
        let disk = Arc::new(LazyDisk::create(root.path(), &manifest, source.clone()).unwrap());
        let os = DiskView::new(disk.clone(), 0, block).unwrap();
        let workspace = DiskView::new(disk.clone(), block, block).unwrap();
        os.write_at(0, b"old-os").unwrap();
        workspace.write_at(0, b"old-ws").unwrap();
        let generation = disk.seal_completed().unwrap();
        os.write_at(0, b"new-os").unwrap();
        workspace.write_at(0, b"new-ws").unwrap();
        let captured = disk.capture(generation).unwrap();
        for (extent, expected) in captured["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .zip([b"old-os", b"old-ws"])
        {
            let hash = extent["hash"].as_str().unwrap();
            let bytes = disk.captured_block(generation, hash).unwrap();
            assert_eq!(&bytes[..6], expected);
            assert!(digest::matches(hash, &bytes));
            source.0.lock().unwrap().insert(hash.to_owned(), bytes);
        }
        disk.commit_published(generation, "first-backup").unwrap();
        drop(os);
        drop(workspace);
        drop(disk);
        let disk = Arc::new(LazyDisk::open(root.path(), source).unwrap());
        for (offset, expected) in [(0, b"new-os"), (block, b"new-ws")] {
            let view = DiskView::new(disk.clone(), offset, block).unwrap();
            let mut bytes = [0; 6];
            view.read_at(0, &mut bytes).unwrap();
            assert_eq!(&bytes, expected);
        }
    }
}
