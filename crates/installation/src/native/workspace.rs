//! Native workspace ownership, file copying and disk accounting.
use crate::{
    error::{Error, Result},
    nodes::files,
    skills::private_dir,
};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tokio::io::BufReader;

pub fn directory(state: &Path, run: &str) -> Result<PathBuf> {
    crate::validation::uuid(run)?;
    Ok(state.join("native").join(run))
}

/// A new execution invalidates restore idempotence after acquiring exclusive
/// ownership. Retrying an old backup after later work must really restore it.
pub async fn execution_ownership(state: &Path, run: &str) -> Result<crate::file_lock::Guard> {
    let root = directory(state, run)?;
    private_dir(&root).await?;
    let owner = crate::file_lock::exclusive(
        &root.join("owner.lock"),
        "Native workspace is still active.",
    )?;
    if root.join("block-restore.pending.json").exists() {
        return Err(Error::conflict(
            "Native block restore must finish before execution resumes.",
        ));
    }
    Ok(owner)
}

pub async fn copy(source: &Path, target: &Path, budget: u64) -> Result<()> {
    let (mut writer, reader) = tokio::io::duplex(131072);
    let source = source.to_owned();
    let send = tokio::spawn(async move { files::send(&source, &mut writer).await });
    let result = files::receive(&mut BufReader::new(reader), target, budget).await;
    if result.is_err() {
        send.abort();
    }
    result?;
    send.await.map_err(Error::internal)??;
    sync_directories(target).await
}

/// Files received through nodes::files are already synced; sync each directory
/// from leaves upward before a journal can name this prepared tree.
pub async fn sync_directories(root: &Path) -> Result<()> {
    let mut pending = vec![root.to_owned()];
    let mut directories = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = tokio::fs::symlink_metadata(&path).await?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let mut entries = tokio::fs::read_dir(&path).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                pending.push(entry.path());
            }
        }
        directories.push(path);
    }
    for directory in directories.into_iter().rev() {
        tokio::fs::File::open(directory).await?.sync_all().await?;
    }
    Ok(())
}

/// Call only while holding the conversation's physical lock and after its cgroup is empty.
pub async fn own_tree(root: &Path, uid: u32, gid: u32) -> Result<()> {
    own_tree_with_stop(root, uid, gid, &tokio_util::sync::CancellationToken::new()).await
}

pub(crate) async fn own_tree_with_stop(
    root: &Path,
    uid: u32,
    gid: u32,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        if stop.is_cancelled() {
            return Err(Error::unavailable("Native base ownership stopped."));
        }
        let metadata = tokio::fs::symlink_metadata(&path).await?;
        std::os::unix::fs::lchown(&path, Some(uid), Some(gid))?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            let mut entries = tokio::fs::read_dir(&path).await?;
            while let Some(entry) = entries.next_entry().await? {
                pending.push(entry.path());
            }
            tokio::fs::set_permissions(
                &path,
                std::fs::Permissions::from_mode(metadata.permissions().mode() | 0o700),
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn bytes(root: &Path) -> Result<u64> {
    let root = root.to_owned();
    tokio::task::spawn_blocking(move || measure(&root, false))
        .await
        .map_err(Error::internal)?
}

/// Descriptor-relative traversal is shared by synchronous block admission and
/// asynchronous status sampling. It never follows workload links or opens FIFOs.
#[derive(Clone, Copy)]
enum Measurement {
    Logical,
    Allocated,
    ExclusiveCredit,
}

pub(crate) fn measure(root: &Path, physical: bool) -> Result<u64> {
    let basis = if physical {
        Measurement::Allocated
    } else {
        Measurement::Logical
    };
    measure_with_seen(root, basis, None)
}

pub(crate) fn measure_unique(
    root: &Path,
    physical: bool,
    seen: &mut std::collections::HashSet<(u64, u64)>,
) -> Result<u64> {
    let basis = if physical {
        Measurement::Allocated
    } else {
        Measurement::Logical
    };
    measure_with_seen(root, basis, Some(seen))
}

/// Future-space credit uses pinned ancestry, unique inodes and allocated bytes
/// capped by logical tmp usage. Unknown paths never create a reservation credit.
pub(super) fn credit_bytes(root: &Path) -> Result<u64> {
    let mut seen = std::collections::HashSet::new();
    measure_with_seen(root, Measurement::ExclusiveCredit, Some(&mut seen))
}

fn measure_with_seen(
    root: &Path,
    basis: Measurement,
    mut seen: Option<&mut std::collections::HashSet<(u64, u64)>>,
) -> Result<u64> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{
                ffi::OsStrExt,
                fs::{MetadataExt, OpenOptionsExt},
            },
        },
    };
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(root)?;
    let entries = std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?;
    let mut stack = vec![(directory, entries)];
    let mut total = 0u64;
    while let Some((directory, entries)) = stack.last_mut() {
        let Some(entry) = entries.next() else {
            stack.pop();
            continue;
        };
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(Error::from(error)),
        };
        let name = CString::new(entry.file_name().as_bytes()).map_err(Error::internal)?;
        // O_PATH never opens a device or FIFO. Each directory descriptor pins
        // its ancestry, so renames and symlink swaps cannot escape this tree.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                continue;
            }
            return Err(error.into());
        }
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if metadata.is_dir() {
            if matches!(basis, Measurement::ExclusiveCredit)
                && seen
                    .as_mut()
                    .is_some_and(|seen| !seen.insert((metadata.dev(), metadata.ino())))
            {
                continue;
            }
            let entries = std::fs::read_dir(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
            stack.push((file, entries));
        } else if metadata.is_file() {
            if seen
                .as_mut()
                .is_some_and(|seen| !seen.insert((metadata.dev(), metadata.ino())))
            {
                continue;
            }
            let allocated = metadata.blocks().saturating_mul(512);
            let size = match basis {
                Measurement::Logical => metadata.len(),
                Measurement::Allocated => allocated,
                Measurement::ExclusiveCredit if metadata.nlink() == 1 => {
                    metadata.len().min(allocated)
                }
                Measurement::ExclusiveCredit => 0,
            };
            total = total.saturating_add(size);
        }
    }
    Ok(total)
}

/// Host extraction and sparse images use this filesystem's allocation unit.
pub(crate) fn filesystem(path: &Path) -> Result<libc::statvfs> {
    let path =
        std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(Error::internal)?;
    let mut capacity = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), capacity.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { capacity.assume_init() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_credit_counts_a_concurrently_renamed_inode_at_most_once() {
        use std::{
            os::unix::fs::MetadataExt,
            sync::{
                Arc, Barrier,
                atomic::{AtomicBool, Ordering},
            },
        };
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (first, last) = (root.path().join("first"), root.path().join("last"));
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&last).unwrap();
        let original = first.join("payload");
        std::fs::write(&original, vec![1; 4 * 1024 * 1024]).unwrap();
        std::fs::File::open(&original).unwrap().sync_all().unwrap();
        let metadata = std::fs::metadata(&original).unwrap();
        let backing = metadata.len().min(metadata.blocks() * 512);
        std::fs::write(outside.path().join("unrelated"), vec![1; 8 * 1024 * 1024]).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let barrier = Arc::new(Barrier::new(2));
        let thread = std::thread::spawn({
            let (running, barrier) = (running.clone(), barrier.clone());
            move || {
                barrier.wait();
                while running.load(Ordering::Relaxed) {
                    std::fs::rename(first.join("payload"), last.join("payload")).unwrap();
                    std::fs::rename(last.join("payload"), first.join("payload")).unwrap();
                }
            }
        });
        barrier.wait();
        let mut maximum = 0;
        for _ in 0..512 {
            maximum = maximum.max(credit_bytes(root.path()).unwrap_or(u64::MAX));
        }
        running.store(false, Ordering::Relaxed);
        thread.join().unwrap();
        assert!(
            maximum <= backing,
            "credited {maximum} bytes for {backing} allocated bytes"
        );
    }

    #[tokio::test]
    async fn native_disk_accounting_ignores_links_and_never_opens_fifos() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        tokio::fs::write(outside.path().join("secret"), vec![0; 4096])
            .await
            .unwrap();
        tokio::fs::write(root.path().join("owned"), b"12345")
            .await
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        let pipe = std::ffi::CString::new(root.path().join("pipe").as_os_str().as_encoded_bytes())
            .unwrap();
        assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
        let count = tokio::time::timeout(std::time::Duration::from_secs(2), bytes(root.path()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, 5);
    }
}
