//! Immutable clean mount view for host backends, prepared before native admission.
use std::{
    fs::File,
    io,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub(crate) struct BackendNamespace(Arc<File>);

pub(crate) struct Entry {
    namespace: BackendNamespace,
    directories: Vec<(PathBuf, u64, u64)>,
    state: PathBuf,
    mounts: Vec<String>,
}

fn checked(result: i32) -> io::Result<()> {
    if result == 0 {
        return Ok(());
    }
    Err(io::Error::last_os_error())
}

fn private() -> io::Result<()> {
    checked(unsafe { libc::unshare(libc::CLONE_NEWNS) })?;
    checked(unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    })
}

fn mount_path(value: &str) -> PathBuf {
    PathBuf::from(
        value
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\"),
    )
}

/// Compare storage submounts as well as directory identities. Later host-native
/// consumer mounts are deliberately absent; every other storage mount must match.
fn backing_mounts(state: &Path, mounts: &str) -> io::Result<Vec<String>> {
    let native = state.join("native");
    let mut result = Vec::new();
    for line in mounts.lines() {
        let (mount, filesystem) = line
            .split_once(" - ")
            .ok_or_else(|| io::Error::other("Invalid mount inventory"))?;
        let fields: Vec<_> = mount.split_whitespace().collect();
        if fields.len() < 6 {
            return Err(io::Error::other("Invalid mount inventory"));
        }
        let path = mount_path(fields[4]);
        let storage_mount = path.starts_with(state) || state.starts_with(&path);
        if !storage_mount || path.starts_with(&native) {
            continue;
        }
        // Mount/parent IDs and propagation flags legitimately differ across
        // private clones. Device, filesystem root, path, options and source do not.
        result.push(format!(
            "{} {} {} {} {filesystem}",
            fields[2], fields[3], fields[4], fields[5]
        ));
    }
    result.sort();
    Ok(result)
}

impl BackendNamespace {
    #[cfg(test)]
    pub(crate) fn unavailable_for_test() -> Self {
        Self(Arc::new(File::open("/dev/null").unwrap()))
    }

    pub(crate) fn create(state: &Path) -> io::Result<Self> {
        let state = state.to_owned();
        std::thread::spawn(move || {
            private()?;
            let mountinfo = std::fs::read_to_string("/proc/thread-self/mountinfo")?;
            let native = state.join("native");
            for line in mountinfo.lines() {
                let path = line
                    .split_whitespace()
                    .nth(4)
                    .ok_or_else(|| io::Error::other("Invalid mount inventory"))?;
                if mount_path(path).starts_with(&native) {
                    return Err(io::Error::other(
                        "Native mounts must be recovered before preparing backend namespace",
                    ));
                }
            }
            Ok(Self(Arc::new(File::open("/proc/thread-self/ns/mnt")?)))
        })
        .join()
        .map_err(|_| io::Error::other("Backend namespace preparation panicked"))?
    }

    pub(crate) fn prepare(&self, state: &Path, directories: &[&Path]) -> io::Result<Entry> {
        let mut identities = Vec::new();
        for path in directories {
            let metadata = std::fs::symlink_metadata(path)?;
            if !metadata.is_dir() {
                return Err(io::Error::other(
                    "Backend storage path must be a real directory",
                ));
            }
            identities.push((path.to_path_buf(), metadata.dev(), metadata.ino()));
        }
        Ok(Entry {
            namespace: self.clone(),
            directories: identities,
            state: state.to_owned(),
            mounts: backing_mounts(
                state,
                &std::fs::read_to_string("/proc/thread-self/mountinfo")?,
            )?,
        })
    }
}

impl Entry {
    /// Dedicated backend OS thread only, before ADD and before queue threads.
    pub(crate) fn enter(&self) -> io::Result<()> {
        checked(unsafe { libc::unshare(libc::CLONE_FS) })?;
        checked(unsafe { libc::setns(self.namespace.0.as_raw_fd(), libc::CLONE_NEWNS) })?;
        // Backend-local changes can never mutate the reusable clean template.
        private()?;
        for (path, device, inode) in &self.directories {
            let metadata = std::fs::symlink_metadata(path)?;
            if !metadata.is_dir() || metadata.dev() != *device || metadata.ino() != *inode {
                return Err(io::Error::other(
                    "Backend storage mount changed; restart the native controller",
                ));
            }
        }
        let mounts = backing_mounts(
            &self.state,
            &std::fs::read_to_string("/proc/thread-self/mountinfo")?,
        )?;
        if mounts != self.mounts {
            return Err(io::Error::other(
                "Backend storage mount topology changed; restart the native controller",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::CString,
        os::unix::ffi::OsStrExt,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
    };

    struct Bind(PathBuf);

    impl Bind {
        fn new(source: &Path, target: &Path) -> Self {
            let source = CString::new(source.as_os_str().as_bytes()).unwrap();
            let destination = CString::new(target.as_os_str().as_bytes()).unwrap();
            checked(unsafe {
                libc::mount(
                    source.as_ptr(),
                    destination.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            })
            .unwrap();
            Self(target.to_owned())
        }
    }

    impl Drop for Bind {
        fn drop(&mut self) {
            let target = CString::new(self.0.as_os_str().as_bytes()).unwrap();
            checked(unsafe { libc::umount2(target.as_ptr(), 0) }).unwrap();
        }
    }

    #[test]
    fn storage_mount_comparison_ignores_consumers_and_namespace_ids_only() {
        let state = Path::new("/state");
        let first = "42 1 8:1 /source /state/cache rw shared:20 - ext4 /dev/old rw\n43 1 8:2 / /state/native/run/data rw - ext4 /owned rw\n";
        let private = "142 100 8:1 /source /state/cache rw - ext4 /dev/old rw\n";
        assert_eq!(
            backing_mounts(state, first).unwrap(),
            backing_mounts(state, private).unwrap()
        );
        let changed = private.replace("8:1", "8:3");
        assert_ne!(
            backing_mounts(state, first).unwrap(),
            backing_mounts(state, &changed).unwrap()
        );
        let ancestor = "1 0 8:0 / / rw - ext4 /dev/root rw\n";
        let writable = format!("{ancestor}{private}");
        let read_only = format!("{}{private}", ancestor.replace("/ rw -", "/ ro -"));
        assert_ne!(
            backing_mounts(state, &writable).unwrap(),
            backing_mounts(state, &read_only).unwrap()
        );
        assert!(backing_mounts(state, "malformed").is_err());
    }

    #[test]
    #[ignore = "run only inside a disposable private user/mount namespace with ordinary bind mounts"]
    fn isolated_namespace_qualification() {
        let original = std::fs::read_link("/proc/thread-self/ns/mnt").unwrap();
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let first = state.join("native/first/data");
        let second = state.join("native/second/data");
        let source = root.path().join("source");
        let journal = state.join("disks/journal");
        let jail = state.join("native-block/journal/root");
        let cache = state.join("cache");
        let unrelated = state.join("unrelated");
        for path in [
            &first, &second, &source, &journal, &jail, &cache, &unrelated,
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(source.join("sentinel"), b"ordinary bind data").unwrap();
        let unrelated_mount = Bind::new(&source, &unrelated);
        let namespace = BackendNamespace::create(&state).unwrap();
        let first_mount = Bind::new(&source, &first);
        assert!(
            BackendNamespace::create(&state).is_err(),
            "template refreshed with a native consumer"
        );
        let entry = namespace
            .prepare(&state, &[&state, &journal, &jail])
            .unwrap();
        let (ready_sender, ready_receiver) = mpsc::channel();
        let (continue_sender, continue_receiver) = mpsc::channel();
        let worker = std::thread::spawn({
            let first = first.clone();
            let second = second.clone();
            let unrelated = unrelated.clone();
            move || {
                entry.enter().unwrap();
                assert!(
                    !first.join("sentinel").exists(),
                    "backend inherited an earlier native consumer"
                );
                assert!(
                    unrelated.join("sentinel").exists(),
                    "preexisting ordinary mount lost"
                );
                ready_sender.send(()).unwrap();
                continue_receiver.recv().unwrap();
                assert!(
                    !second.join("sentinel").exists(),
                    "backend received a later native consumer"
                );
            }
        });
        ready_receiver.recv().unwrap();
        let second_mount = Bind::new(&source, &second);
        continue_sender.send(()).unwrap();
        worker.join().unwrap();
        assert!(first.join("sentinel").exists());
        assert!(second.join("sentinel").exists());
        for target in [&journal, &cache] {
            let changed = Bind::new(&source, target);
            let entry = namespace
                .prepare(&state, &[&state, &journal, &jail])
                .unwrap();
            let admitted = Arc::new(AtomicBool::new(false));
            let marker = admitted.clone();
            let error = std::thread::spawn(move || {
                entry.enter()?;
                marker.store(true, Ordering::SeqCst);
                Ok::<_, io::Error>(())
            })
            .join()
            .unwrap()
            .unwrap_err();
            assert!(error.to_string().contains("changed"));
            assert!(
                !admitted.load(Ordering::SeqCst),
                "changed storage crossed the ADD boundary"
            );
            drop(changed);
        }
        let invalid = BackendNamespace::unavailable_for_test()
            .prepare(&state, &[&state])
            .unwrap();
        assert!(
            std::thread::spawn(move || invalid.enter())
                .join()
                .unwrap()
                .is_err()
        );
        for _ in 0..2 {
            let entry = namespace
                .prepare(&state, &[&state, &journal, &jail])
                .unwrap();
            std::thread::spawn(move || entry.enter())
                .join()
                .unwrap()
                .unwrap();
        }
        drop((second_mount, first_mount, unrelated_mount));
        assert_eq!(
            std::fs::read_link("/proc/thread-self/ns/mnt").unwrap(),
            original
        );
        assert!(!first.join("sentinel").exists());
        assert!(!second.join("sentinel").exists());
    }
}
