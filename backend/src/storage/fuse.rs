//! One raw disk file per mount, with the kernel page cache bypassed.
use super::Disk;
use fuser::*;
use std::{
    ffi::OsStr,
    io,
    path::Path,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

struct DiskFilesystem {
    disk: Arc<dyn Disk>,
    uid: u32,
}
impl DiskFilesystem {
    fn attr(&self, inode: INodeNo) -> FileAttr {
        let directory = inode == INodeNo::ROOT;
        FileAttr {
            ino: inode,
            size: if directory { 0 } else { self.disk.size() },
            blocks: if directory {
                0
            } else {
                self.disk.size().div_ceil(512)
            },
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: if directory {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            perm: if directory { 0o700 } else { 0o600 },
            nlink: if directory { 2 } else { 1 },
            uid: self.uid,
            gid: self.uid,
            rdev: 0,
            flags: 0,
            blksize: 4096,
        }
    }
}
impl Filesystem for DiskFilesystem {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        if parent == INodeNo::ROOT && name == "data.ext4" {
            reply.entry(&Duration::ZERO, &self.attr(INodeNo(2)), Generation(0));
        } else {
            reply.error(Errno::ENOENT);
        }
    }
    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        if ino == INodeNo::ROOT || ino == INodeNo(2) {
            reply.attr(&Duration::ZERO, &self.attr(ino));
        } else {
            reply.error(Errno::ENOENT);
        }
    }
    fn open(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        if ino != INodeNo(2) {
            reply.error(Errno::ENOENT);
            return;
        }
        reply.opened(FileHandle(0), FopenFlags::FOPEN_DIRECT_IO);
    }
    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        if ino != INodeNo(2) {
            reply.error(Errno::ENOENT);
            return;
        }
        if size > 8 * 1024 * 1024 {
            reply.error(Errno::EINVAL);
            return;
        }
        let length = (self.disk.size().saturating_sub(offset)).min(size as u64) as usize;
        let mut bytes = vec![0; length];
        match self.disk.read_at(offset.min(self.disk.size()), &mut bytes) {
            Ok(()) => reply.data(&bytes),
            Err(_) => reply.error(Errno::EIO),
        }
    }
    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        if ino != INodeNo(2) {
            reply.error(Errno::ENOENT);
            return;
        }
        match self.disk.write_at(offset, data) {
            Ok(()) => reply.written(data.len() as u32),
            Err(_) => reply.error(Errno::EIO),
        }
    }
    fn flush(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        if ino != INodeNo(2) {
            reply.error(Errno::ENOENT);
            return;
        }
        match self.disk.sync() {
            Ok(()) => reply.ok(),
            Err(_) => reply.error(Errno::EIO),
        }
    }
    fn fsync(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        if ino != INodeNo(2) {
            reply.error(Errno::ENOENT);
            return;
        }
        match self.disk.sync() {
            Ok(()) => reply.ok(),
            Err(_) => reply.error(Errno::EIO),
        }
    }
}

/// Mount under the jail root before launching Firecracker. The mount is private
/// to its configured UID (plus host root); dropping the handle unmounts it.
pub struct MountedDisk(Option<BackgroundSession>);
impl MountedDisk {
    /// Stop the VMM and cancel remote transfers before closing its mount.
    pub fn close(mut self) -> io::Result<()> {
        self.0.take().expect("owned mount").umount_and_join()
    }
}
impl Drop for MountedDisk {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            let _ = session.umount_and_join();
        }
    }
}

pub fn mount_disk(disk: Arc<dyn Disk>, target: &Path, uid: u32) -> io::Result<MountedDisk> {
    let mut config = Config::default();
    config.acl = SessionACL::All;
    config.mount_options = vec![
        MountOption::DefaultPermissions,
        MountOption::NoSuid,
        MountOption::NoDev,
        MountOption::NoExec,
        MountOption::FSName("leo-disk".into()),
    ];
    config.n_threads = Some(4);
    fuser::spawn_mount(DiskFilesystem { disk, uid }, target, &config)
        .map(|session| MountedDisk(Some(session)))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Disk, LocalDisk};
    use std::{os::unix::fs::FileExt, sync::Arc};
    #[test]
    #[ignore = "requires /dev/fuse and mount privileges"]
    fn mount_preserves_positional_io_and_flush() {
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("image");
        std::fs::write(&image, vec![0; 4096]).unwrap();
        let disk = Arc::new(LocalDisk::open(&image, true).unwrap());
        let mountpoint = root.path().join("mount");
        std::fs::create_dir(&mountpoint).unwrap();
        let mounted = mount_disk(disk.clone(), &mountpoint, 0).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(mountpoint.join("data.ext4"))
            .unwrap();
        file.write_all_at(b"guest-fsync", 17).unwrap();
        file.sync_all().unwrap();
        let mut bytes = [0; 11];
        disk.read_at(17, &mut bytes).unwrap();
        assert_eq!(&bytes, b"guest-fsync");
        drop(file);
        drop(mounted);
    }
}
