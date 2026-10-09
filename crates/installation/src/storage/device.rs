//! Byte-addressed disks. An implementation must never return zeros for unavailable data.
use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::FileExt,
    path::Path,
};

/// One write whose bytes remain immutable until its durable acknowledgement.
#[derive(Clone, Copy)]
pub struct DiskWrite<'a> {
    pub offset: u64,
    pub bytes: &'a [u8],
}

/// Operations are exact and bounded by the disk size. Writes are durable before
/// acknowledgment; sync propagates a guest flush to persistent local storage.
/// Callers coordinate filesystem consistency before exporting a running disk.
pub trait Disk: Send + Sync {
    fn size(&self) -> u64;

    fn read_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()>;

    fn write_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()>;

    /// Preserve request order and acknowledge only when every write is durable.
    /// Failure may leave an unacknowledged prefix; this is not a transaction.
    fn write_batch(&self, writes: &[DiskWrite<'_>]) -> io::Result<()> {
        for write in writes {
            self.write_at(write.offset, write.bytes)?;
        }
        Ok(())
    }

    fn sync(&self) -> io::Result<()>;
}

pub(crate) fn range(size: u64, offset: u64, length: usize) -> io::Result<()> {
    if offset
        .checked_add(length as u64)
        .is_none_or(|end| end > size)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Disk access exceeds its size",
        ));
    }
    Ok(())
}

pub struct LocalDisk {
    file: File,
    size: u64,
}

impl LocalDisk {
    pub fn open(path: &Path, writable: bool) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(writable).open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Expected a nonempty disk image",
            ));
        }
        Ok(Self {
            size: metadata.len(),
            file,
        })
    }
}

impl Disk for LocalDisk {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        range(self.size, offset, bytes.len())?;
        self.file.read_exact_at(bytes, offset)
    }

    fn write_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        range(self.size, offset, bytes.len())?;
        self.file.write_all_at(bytes, offset)?;
        self.file.sync_data()
    }

    fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

/// Materialize a consistent view without replacing a retained image. Failed
/// exports are removed; success includes persistence of the containing directory.
pub fn export(disk: &dyn Disk, target: &Path) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing export directory"))?;
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(target)?;
    let result = (|| {
        file.set_len(disk.size())?;
        let mut buffer = vec![0; 4 * 1024 * 1024];
        let mut offset = 0;
        while offset < disk.size() {
            let length = (disk.size() - offset).min(buffer.len() as u64) as usize;
            disk.read_at(offset, &mut buffer[..length])?;
            if buffer[..length].iter().any(|byte| *byte != 0) {
                file.write_all_at(&buffer[..length], offset)?;
            }
            offset += length as u64;
        }
        file.sync_all()?;
        File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(target);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{Disk, LocalDisk, export};

    #[test]
    fn local_disk_keeps_writes_and_exports_without_replacing_existing_data() {
        let directory = std::env::temp_dir().join(format!(
            "cairn-disk-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let root = directory.as_path();
        let path = root.join("disk");
        std::fs::write(&path, b"abcdefghijklmnop").unwrap();
        let disk = LocalDisk::open(&path, true).unwrap();
        disk.write_at(5, b"XYZ").unwrap();
        disk.sync().unwrap();
        drop(disk);
        let disk = LocalDisk::open(&path, false).unwrap();
        let mut bytes = [0; 16];
        disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"abcdeXYZijklmnop");
        let target = root.join("export");
        export(&disk, &target).unwrap();
        let exported = LocalDisk::open(&target, false).unwrap();
        exported.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"abcdeXYZijklmnop");
        assert!(export(&disk, &target).is_err());
        assert!(disk.read_at(16, &mut [0; 1]).is_err());
        assert!(disk.read_at(u64::MAX, &mut [0; 2]).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
