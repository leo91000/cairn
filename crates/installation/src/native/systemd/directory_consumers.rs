//! Directory consumers are attributed by a pinned tree and its filesystem root,
//! never by the shared device number used by every conversation on this node.
use crate::error::{Error, Result};
use crate::native::workspace::mount_path as unescape;
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

struct Source {
    path: PathBuf,
    directory: File,
    device: String,
    filesystem_root: PathBuf,
    project: u32,
}

fn vanished(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH))
}

fn directory(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

fn descriptor(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

fn read(path: &Path) -> io::Result<String> {
    let mut value = String::new();
    File::open(path)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_string(&mut value)?;
    if value.len() > 16 * 1024 * 1024 {
        return Err(io::Error::other("Consumer inventory exceeded its bound"));
    }
    Ok(value)
}

fn origin(path: &Path, device: &str, mounts: &str) -> io::Result<PathBuf> {
    let mut selected: Option<(PathBuf, PathBuf)> = None;
    for line in mounts.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 6 || fields[2] != device {
            continue;
        }
        let mount = unescape(fields[4])?;
        if path.starts_with(&mount)
            && selected
                .as_ref()
                .is_none_or(|(old, _)| mount.components().count() > old.components().count())
        {
            selected = Some((mount, unescape(fields[3])?));
        }
    }
    let (mount, root) = selected
        .ok_or_else(|| io::Error::other("Native directory filesystem origin is unknown"))?;
    Ok(root.join(path.strip_prefix(mount).map_err(io::Error::other)?))
}

impl Source {
    fn open(path: &Path) -> Result<Self> {
        crate::native::workspace::require_no_mounts(path)?;
        let directory = directory(path)?;
        let metadata = directory.metadata()?;
        let device = format!(
            "{}:{}",
            libc::major(metadata.dev()),
            libc::minor(metadata.dev())
        );
        let filesystem_root = origin(path, &device, &read(Path::new("/proc/self/mountinfo"))?)?;
        let project = crate::native::quota::owned_project(path, &directory)?;
        Ok(Self {
            path: path.to_owned(),
            directory,
            device,
            filesystem_root,
            project,
        })
    }

    fn revalidate(&self) -> Result<()> {
        let current = fs::symlink_metadata(&self.path)?;
        let owned = self.directory.metadata()?;
        if !current.is_dir() || current.dev() != owned.dev() || current.ino() != owned.ino() {
            return Err(Error::conflict(
                "Native consumer inventory belongs to a replaced directory.",
            ));
        }
        if crate::native::quota::owned_project(&self.path, &self.directory)? != self.project {
            return Err(Error::conflict("Native consumer quota identity changed."));
        }
        Ok(())
    }

    fn owns_mount(&self, device: &str, root: &Path) -> bool {
        device == self.device && root.starts_with(&self.filesystem_root)
    }

    fn docker_bind(&self, mounts: &str, id: &str) -> io::Result<bool> {
        for line in mounts.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 5 {
                return Err(io::Error::other("Invalid consumer mount inventory"));
            }
            let root = unescape(fields[3])?;
            if self.owns_mount(fields[2], &root)
                && ["hostname", "hosts", "resolv.conf"]
                    .iter()
                    .any(|name| root.ends_with(Path::new("containers").join(id).join(name)))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn container(cgroup: &str) -> Option<&str> {
    let id = cgroup
        .trim()
        .strip_prefix("0::/system.slice/docker-")?
        .strip_suffix(".scope")?;
    (id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(id)
}

fn owned_scopes(source: &Source) -> io::Result<BTreeSet<String>> {
    let mut scopes = BTreeSet::new();
    for process in fs::read_dir("/proc")? {
        let process = process?;
        if !process
            .file_name()
            .as_encoded_bytes()
            .iter()
            .all(u8::is_ascii_digit)
        {
            continue;
        }
        let inspect = || -> io::Result<Option<String>> {
            // Pin procfs before reading identity, so PID reuse cannot redirect
            // a later read to the replacement process's mount namespace.
            let process_file = directory(&process.path())?;
            let process = descriptor(&process_file);
            let group = read(&process.join("cgroup"))?;
            let Some(id) = container(&group) else {
                return Ok(None);
            };
            let mounts = read(&process.join("mountinfo"))?;
            if read(&process.join("cgroup"))? != group || !source.docker_bind(&mounts, id)? {
                return Ok(None);
            }
            Ok(Some(format!("docker-{id}.scope")))
        };
        match inspect() {
            Ok(Some(scope)) => {
                scopes.insert(scope);
            }
            Ok(None) => {}
            Err(error) if vanished(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(scopes)
}

fn populated(group: &File) -> io::Result<bool> {
    match read(&descriptor(group).join("cgroup.events")) {
        Ok(events) => Ok(events.lines().any(|line| line == "populated 1")),
        Err(error) if vanished(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

fn terminate(group: &File, scope: &str) -> io::Result<()> {
    for pid in read(&descriptor(group).join("cgroup.procs"))?.lines() {
        let pid = pid.parse::<libc::pid_t>().map_err(io::Error::other)?;
        let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if pidfd < 0 {
            let error = io::Error::last_os_error();
            if vanished(&error) {
                continue;
            }
            return Err(error);
        }
        let pidfd = unsafe { File::from_raw_fd(pidfd as i32) };
        let membership = match read(&PathBuf::from(format!("/proc/{pid}/cgroup"))) {
            Ok(group) => group,
            Err(error) if vanished(&error) => continue,
            Err(error) => return Err(error),
        };
        if !membership
            .lines()
            .any(|line| line == format!("0::/system.slice/{scope}"))
        {
            continue;
        }
        let original = group.metadata()?;
        let current = match fs::metadata(Path::new("/sys/fs/cgroup/system.slice").join(scope)) {
            Ok(metadata) => metadata,
            Err(error) if vanished(&error) => continue,
            Err(error) => return Err(error),
        };
        if (original.dev(), original.ino()) != (current.dev(), current.ino()) {
            return Err(io::Error::other(
                "Native Docker cgroup was replaced before signalling",
            ));
        }
        if unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd.as_raw_fd(),
                libc::SIGTERM,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            if !vanished(&error) {
                return Err(error);
            }
        }
    }
    Ok(())
}

fn inspect_namespace(namespace: File, device: String, root: PathBuf) -> io::Result<bool> {
    let host_proc = directory(Path::new("/proc"))?;
    // The target may mount a procfs in a different PID namespace. Open its
    // mountinfo through pinned host procfs after setns, not through target /proc.
    std::thread::spawn(move || -> io::Result<bool> {
        if unsafe { libc::unshare(libc::CLONE_FS) } != 0
            || unsafe { libc::setns(namespace.as_raw_fd(), libc::CLONE_NEWNS) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe {
            libc::openat(
                host_proc.as_raw_fd(),
                c"thread-self/mountinfo".as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut mounts = String::new();
        unsafe { File::from_raw_fd(fd) }
            .take(16 * 1024 * 1024 + 1)
            .read_to_string(&mut mounts)?;
        if mounts.len() > 16 * 1024 * 1024 {
            return Err(io::Error::other(
                "Namespace mount inventory exceeded its bound",
            ));
        }
        for line in mounts.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 5 {
                return Err(io::Error::other("Invalid mount inventory"));
            }
            if fields[2] == device && unescape(fields[3])?.starts_with(&root) {
                return Ok(true);
            }
        }
        Ok(false)
    })
    .join()
    .map_err(|_| io::Error::other("Native namespace inventory thread failed"))?
}

fn remaining(source: &Source) -> io::Result<usize> {
    let mut namespaces = std::collections::BTreeMap::new();
    let mut remaining = 0;
    let current = std::process::id().to_string();
    for process in fs::read_dir("/proc")? {
        let process = process?;
        let name = process.file_name();
        if !name.as_encoded_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        let inspect = || -> io::Result<(Vec<File>, bool)> {
            let process_file = directory(&process.path())?;
            let path = descriptor(&process_file);
            let mut held = Vec::new();
            for task in fs::read_dir(path.join("task"))? {
                let task = task?;
                match File::open(task.path().join("ns/mnt")) {
                    Ok(namespace) => held.push(namespace),
                    Err(error) if vanished(&error) => {}
                    Err(error) => return Err(error),
                }
            }
            // Include task-less namespaces pinned only by an FD, even those
            // held by the controller. Snapshot names before opening duplicates.
            let descriptors = fs::read_dir(path.join("fd"))?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<io::Result<Vec<_>>>()?;
            let mut files = false;
            for path in descriptors {
                let target = match fs::read_link(&path) {
                    Ok(target) => target,
                    Err(error) if vanished(&error) => continue,
                    Err(error) => return Err(error),
                };
                if target.as_os_str().as_encoded_bytes().starts_with(b"mnt:[") {
                    match File::open(&path) {
                        Ok(namespace) => held.push(namespace),
                        Err(error) if vanished(&error) => {}
                        Err(error) => return Err(error),
                    }
                    continue;
                }
                if name == current.as_str() || source.project == 0 {
                    continue;
                }
                let metadata = match fs::metadata(&path) {
                    Ok(value) => value,
                    Err(error) if vanished(&error) => continue,
                    Err(error) => return Err(error),
                };
                if metadata.dev() != source.directory.metadata()?.dev()
                    || !(metadata.is_file() || metadata.is_dir())
                {
                    continue;
                }
                let file = match OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(path)
                {
                    Ok(file) => file,
                    Err(error) if vanished(&error) => continue,
                    Err(error) => return Err(error),
                };
                if crate::native::quota::project_io(&file)? == source.project {
                    files = true;
                }
            }
            Ok((held, files))
        };
        match inspect() {
            Ok((held, files)) => {
                remaining += usize::from(files);
                for namespace in held {
                    let metadata = namespace.metadata()?;
                    namespaces
                        .entry((metadata.dev(), metadata.ino()))
                        .or_insert(namespace);
                }
            }
            Err(error) if vanished(&error) => {}
            Err(error) => return Err(error),
        }
    }
    for namespace in namespaces.into_values() {
        remaining += usize::from(inspect_namespace(
            namespace,
            source.device.clone(),
            source.filesystem_root.clone(),
        )?);
    }
    Ok(remaining)
}

async fn scopes(source: &Arc<Source>) -> Result<BTreeSet<String>> {
    let source = source.clone();
    tokio::task::spawn_blocking(move || owned_scopes(&source))
        .await
        .map_err(Error::internal)?
        .map_err(Into::into)
}

/// Workload and daemon fencing precede this call. Only a positively attributed
/// Docker scope can be signalled; unknown consumers keep ownership and an error.
pub(in crate::native) async fn stop(path: &Path) -> Result<()> {
    let source = Arc::new(Source::open(path)?);
    source.revalidate()?;
    let owned = scopes(&source).await?;
    for scope in owned {
        source.revalidate()?;
        if !scopes(&source).await?.contains(&scope) {
            continue;
        }
        let group = match directory(&Path::new("/sys/fs/cgroup/system.slice").join(&scope)) {
            Ok(group) => group,
            Err(error) if vanished(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        terminate(&group, &scope)?;
        let graceful = tokio::time::Instant::now() + Duration::from_secs(10);
        let deadline = graceful + Duration::from_secs(10);
        let mut forced = false;
        while populated(&group)? {
            source.revalidate()?;
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(Error::unavailable(
                    "Attributed native Docker consumers did not stop; directory ownership is retained.",
                ));
            }
            if now >= graceful && !forced {
                // Open relative to the pinned original cgroup. A replacement
                // scope with the same pathname can never receive this kill.
                let kill = descriptor(&group).join("cgroup.kill");
                OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(kill)?
                    .write_all(b"1")?;
                forced = true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tracing::info!(target: "cairn_performance", parent: None, operation = "native_directory_consumers", forced, "Stopped a directory-attributed Docker scope");
    }
    source.revalidate()?;
    let observing = source.clone();
    let remaining = tokio::task::spawn_blocking(move || remaining(&observing))
        .await
        .map_err(Error::internal)??;
    source.revalidate()?;
    if remaining != 0 {
        tracing::warn!(target: "cairn_performance", parent: None, operation = "native_directory_consumers", remaining, "Directory close waits for unattributed external consumers");
        return Err(Error::conflict(
            "Native directory has external mount or file consumers; ownership is retained and cleanup will retry.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_device_and_inherited_mounts_do_not_attribute_another_environment() {
        let root = tempfile::tempdir().unwrap();
        let source = Source {
            path: root.path().to_owned(),
            directory: directory(root.path()).unwrap(),
            device: "8:1".into(),
            filesystem_root: "/native/ours/workload".into(),
            project: 1,
        };
        let id = "a".repeat(64);
        let line = |path: &str| format!("1 0 8:1 {path} /etc/hosts rw - ext4 /dev/test rw\n");
        assert!(
            source
                .docker_bind(
                    &line(&format!(
                        "/native/ours/workload/data/docker/containers/{id}/hosts"
                    )),
                    &id
                )
                .unwrap()
        );
        for path in [
            "/".to_owned(),
            format!("/native/other/workload/data/docker/containers/{id}/hosts"),
            "/native/ours/workload-other".to_owned(),
            "/native/ours/workload/user-file".to_owned(),
        ] {
            assert!(!source.docker_bind(&line(&path), &id).unwrap());
        }
        assert_eq!(
            origin(
                Path::new("/state/native/ours/workload"),
                "8:1",
                "1 0 8:1 /subtree /state rw - ext4 /dev/test rw\n"
            )
            .unwrap(),
            Path::new("/subtree/native/ours/workload")
        );
    }

    #[test]
    fn replaced_directory_invalidates_a_saved_consumer_inventory() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("workload");
        fs::create_dir(&path).unwrap();
        let source = Source {
            path: path.clone(),
            directory: directory(&path).unwrap(),
            device: "8:1".into(),
            filesystem_root: "/workload".into(),
            project: 1,
        };
        fs::rename(&path, root.path().join("old")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(source.revalidate().is_err());
    }
}
