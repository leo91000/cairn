//! Attribute filesystem references without treating a shared mount view as ownership.
use std::os::{
    fd::{AsRawFd, FromRawFd, OwnedFd},
    unix::fs::{FileTypeExt, MetadataExt},
};
use std::{
    fs,
    io::{self, Read},
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Holder {
    pub pid: u32,
    pub started: u64,
    pub name: String,
    pub cgroup: String,
}

fn read(path: impl AsRef<Path>) -> io::Result<String> {
    const LIMIT: u64 = 4 * 1024 * 1024;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(io::Error::other(
            "Filesystem holder record exceeds its limit",
        ));
    }
    // Numeric proc fields are ASCII; mapped filenames may contain arbitrary bytes.
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn namespace_holds(path: &Path, device: u64) -> io::Result<bool> {
    let namespace = fs::File::open(path)?;
    std::thread::spawn(move || {
        if unsafe { libc::unshare(libc::CLONE_FS) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // Keep the host proc directory before entering a namespace whose root
        // may have no procfs, or a procfs for a different PID namespace.
        let host_proc = fs::File::open("/proc")?;
        let mut pending = vec![namespace];
        let mut visited = std::collections::HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while let Some(namespace) = pending.pop() {
            if Instant::now() >= deadline || visited.len() > 1024 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Namespace graph inspection exceeded its bound",
                ));
            }
            // NS_GET_NSTYPE also filters nsfs mounts of other namespace types.
            let kind = unsafe { libc::ioctl(namespace.as_raw_fd(), 0xb703) };
            if kind < 0 {
                return Err(io::Error::last_os_error());
            }
            if kind != libc::CLONE_NEWNS {
                continue;
            }
            if !visited.insert(namespace.metadata()?.ino()) {
                continue;
            }
            if unsafe { libc::setns(namespace.as_raw_fd(), libc::CLONE_NEWNS) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let descriptor = unsafe {
                libc::openat(
                    host_proc.as_raw_fd(),
                    c"thread-self/mountinfo".as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC,
                )
            };
            if descriptor < 0 {
                return Err(io::Error::other(format!(
                    "Cannot inspect retained namespace: {}",
                    io::Error::last_os_error()
                )));
            }
            let file = unsafe { fs::File::from_raw_fd(descriptor) };
            let mut mounts = String::new();
            file.take(4 * 1024 * 1024 + 1)
                .read_to_string(&mut mounts)
                .map_err(|error| {
                    io::Error::other(format!("Cannot inspect retained namespace: {error}"))
                })?;
            if mounts.len() > 4 * 1024 * 1024 {
                return Err(io::Error::other("Namespace inventory exceeds its limit"));
            }
            if has_device(&mounts, device) {
                return Ok(true);
            }
            for path in namespace_mounts(&mounts)? {
                // Open while in the namespace that owns this path, before setns.
                pending.push(fs::File::open(path).map_err(|error| {
                    io::Error::other(format!("Cannot open retained namespace: {error}"))
                })?);
            }
        }
        Ok(false)
    })
    .join()
    .map_err(|_| io::Error::other("Mount namespace inspection panicked"))?
}

fn namespace_mounts(mounts: &str) -> io::Result<Vec<std::path::PathBuf>> {
    let mut paths = Vec::new();
    for line in mounts.lines() {
        let Some((fields, filesystem)) = line.split_once(" - ") else {
            return Err(io::Error::other("Invalid mount table"));
        };
        if filesystem.split_whitespace().next() != Some("nsfs") {
            continue;
        }
        let path = fields
            .split_whitespace()
            .nth(4)
            .ok_or_else(|| io::Error::other("Invalid namespace mount"))?;
        paths.push(mount_path(path));
    }
    Ok(paths)
}

fn mount_path(value: &str) -> std::path::PathBuf {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
        .into()
}

fn started(stat: &str) -> io::Result<u64> {
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| io::Error::other("Invalid filesystem holder identity"))
}

fn disappeared(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH))
}

fn has_device(mounts: &str, device: u64) -> bool {
    let expected = format!("{}:{}", libc::major(device), libc::minor(device));
    mounts
        .lines()
        .any(|line| line.split_whitespace().nth(2) == Some(expected.as_str()))
}

fn mapped_device(maps: &str, device: u64) -> bool {
    maps.lines().any(|line| {
        let Some(field) = line.split_whitespace().nth(3) else {
            return false;
        };
        let Some((major, minor)) = field.split_once(':') else {
            return false;
        };
        u32::from_str_radix(major, 16).ok() == Some(libc::major(device))
            && u32::from_str_radix(minor, 16).ok() == Some(libc::minor(device))
    })
}

fn references(
    task: &Path,
    device: u64,
    host_namespace: u64,
    private_copies: bool,
    deadline: Instant,
) -> io::Result<bool> {
    let private_view = fs::metadata(task.join("ns/mnt"))?.ino() != host_namespace;
    let mounts = read(task.join("mountinfo"))?;
    if private_copies && private_view && has_device(&mounts, device) {
        return Ok(true);
    }
    for reference in ["root", "cwd"] {
        match fs::metadata(task.join(reference)) {
            Ok(metadata)
                if metadata.dev() == device
                    || (metadata.file_type().is_block_device() && metadata.rdev() == device) =>
            {
                return Ok(true);
            }
            Ok(_) => {}
            Err(error) if disappeared(&error) => {}
            Err(error) => return Err(error),
        }
    }
    if mapped_device(&read(task.join("maps"))?, device) {
        return Ok(true);
    }
    for entry in fs::read_dir(task.join("fd"))? {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Filesystem holder inventory exceeded its deadline",
            ));
        }
        let entry = entry?;
        if private_copies
            && let Ok(target) = fs::read_link(entry.path())
            && target
                .to_str()
                .is_some_and(|target| target.starts_with("mnt:["))
            && namespace_holds(&entry.path(), device)?
        {
            return Ok(true);
        }
        match fs::metadata(entry.path()) {
            Ok(metadata)
                if metadata.dev() == device
                    || (metadata.file_type().is_block_device() && metadata.rdev() == device) =>
            {
                return Ok(true);
            }
            Ok(_) => {}
            Err(error) if disappeared(&error) => {}
            Err(error) => return Err(error),
        }
    }
    if private_copies
        && (private_view || !namespace_mounts(&mounts)?.is_empty())
        && namespace_holds(&task.join("ns/mnt"), device)?
    {
        return Ok(true);
    }
    Ok(false)
}

fn inspect(
    process: &Path,
    pid: u32,
    device: u64,
    namespace: u64,
    private_copies: bool,
    deadline: Instant,
) -> io::Result<Option<Holder>> {
    let before = started(&read(process.join("stat"))?)?;
    let mut held = false;
    for task in fs::read_dir(process.join("task"))? {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Filesystem holder inventory exceeded its deadline",
            ));
        }
        let task = task?.path();
        match references(&task, device, namespace, private_copies, deadline) {
            Ok(true) => {
                held = true;
                break;
            }
            Ok(false) => {}
            Err(error) if disappeared(&error) => match read(task.join("stat")) {
                Err(missing) if disappeared(&missing) => continue,
                Ok(status)
                    if status
                        .rsplit_once(')')
                        .and_then(|(_, fields)| fields.split_whitespace().next())
                        .is_some_and(|state| matches!(state, "Z" | "X")) =>
                {
                    continue;
                }
                _ => {
                    return Err(io::Error::other(format!(
                        "Filesystem holder inspection incomplete: {error}"
                    )));
                }
            },
            Err(error) => return Err(error),
        }
    }
    if !held || started(&read(process.join("stat"))?)? != before {
        return Ok(None);
    }
    let name = read(process.join("comm"))?
        .chars()
        .filter(|value| !value.is_control())
        .take(64)
        .collect();
    let groups = read(process.join("cgroup"))?;
    let cgroup = groups
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| io::Error::other("Filesystem holder has no unified cgroup"))?
        .to_owned();
    Ok(Some(Holder {
        pid,
        started: before,
        name,
        cgroup,
    }))
}

pub(super) fn scan(device: u64, private_copies: bool) -> io::Result<Vec<Holder>> {
    let namespace = fs::metadata("/proc/self/ns/mnt")?.ino();
    if private_copies {
        let mounts = read("/proc/self/mountinfo")?;
        if has_device(&mounts, device) {
            return Err(io::Error::other(
                "Native filesystem remains mounted in the host namespace; ownership retained",
            ));
        }
        for path in namespace_mounts(&mounts)? {
            if namespace_holds(&path, device)? {
                return Err(io::Error::other(
                    "Persistent mounted namespace retains the native filesystem",
                ));
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut holders = Vec::new();
    for process in fs::read_dir("/proc")? {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Filesystem holder inventory exceeded its deadline",
            ));
        }
        let process = process?;
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        match inspect(
            &process.path(),
            pid,
            device,
            namespace,
            private_copies,
            deadline,
        ) {
            Ok(Some(holder)) => {
                if holders.len() >= 256 {
                    return Err(io::Error::other(
                        "Too many filesystem holders; ownership retained",
                    ));
                }
                holders.push(holder);
            }
            Ok(None) => {}
            Err(error) if disappeared(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(holders)
}

/// Only the exact attempt or a legacy Docker scope may be stopped. Inherited
/// copies in another attempt must fail closed, never sacrifice another run.
pub(super) fn unit(holder: &Holder, attempt: &str) -> Option<String> {
    if holder.pid <= 1 || holder.pid == std::process::id() {
        return None;
    }
    let expected = format!("cairn-native-{attempt}.service");
    let parts: Vec<_> = holder
        .cgroup
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let parent = super::super::systemd::workload_cgroup().ok()?;
    let expected_path = format!("{parent}/{expected}");
    if holder.cgroup == expected_path || holder.cgroup.starts_with(&format!("{expected_path}/")) {
        return Some(expected);
    }
    let ["system.slice", scope] = parts.as_slice() else {
        return None;
    };
    let id = scope.strip_prefix("docker-")?.strip_suffix(".scope")?;
    (id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then(|| (*scope).to_owned())
}

pub(super) fn pin(holder: &Holder, device: u64) -> io::Result<Option<OwnedFd>> {
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, holder.pid, 0) };
    if descriptor < 0 {
        let error = io::Error::last_os_error();
        return if disappeared(&error) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor as i32) };
    let namespace = fs::metadata("/proc/self/ns/mnt")?.ino();
    let current = match inspect(
        &std::path::PathBuf::from(format!("/proc/{}", holder.pid)),
        holder.pid,
        device,
        namespace,
        true,
        Instant::now() + Duration::from_secs(1),
    ) {
        Ok(value) => value,
        Err(error) if disappeared(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    if current.as_ref() != Some(holder) {
        return Ok(None);
    }
    Ok(Some(descriptor))
}

/// A pidfd retains the observed process instance through signalling and exit.
/// Never signal a PID after merely reading its numeric identity.
pub(super) fn signal(holder: &Holder, device: u64, signal: i32) -> io::Result<()> {
    let Some(descriptor) = pin(holder, device)? else {
        return Ok(());
    };
    if unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            descriptor.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    } < 0
    {
        let error = io::Error::last_os_error();
        if !disappeared(&error) {
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires CAP_MKNOD; disposable metadata fixture only"]
    fn raw_block_device_descriptor_is_a_holder() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("ns")).unwrap();
        fs::create_dir(root.path().join("fd")).unwrap();
        fs::write(root.path().join("ns/mnt"), b"fixture namespace").unwrap();
        fs::write(root.path().join("mountinfo"), b"").unwrap();
        fs::write(root.path().join("maps"), b"").unwrap();
        let device = libc::makedev(259, 200);
        let path = std::ffi::CString::new(root.path().join("fd/3").as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe { libc::mknod(path.as_ptr(), libc::S_IFBLK | 0o600, device) },
            0
        );
        let namespace = fs::metadata(root.path().join("ns/mnt")).unwrap().ino();
        assert!(
            references(
                root.path(),
                device,
                namespace,
                false,
                Instant::now() + Duration::from_secs(1)
            )
            .unwrap()
        );
    }

    #[test]
    fn zombie_task_does_not_hide_a_live_sibling_or_block_empty_inventory() {
        let root = tempfile::tempdir().unwrap();
        let stat = |state| format!("123 (process) {state} {} 123 0", vec!["0"; 18].join(" "));
        fs::create_dir_all(root.path().join("task/123")).unwrap();
        fs::write(root.path().join("stat"), stat("Z")).unwrap();
        fs::write(root.path().join("task/123/stat"), stat("Z")).unwrap();
        let device = libc::makedev(259, 10);
        assert!(
            inspect(
                root.path(),
                123,
                device,
                1,
                true,
                Instant::now() + Duration::from_secs(1)
            )
            .unwrap()
            .is_none()
        );
        fs::create_dir_all(root.path().join("task/124/ns")).unwrap();
        fs::write(root.path().join("task/124/ns/mnt"), b"namespace").unwrap();
        fs::write(
            root.path().join("task/124/mountinfo"),
            b"1 0 259:10 / /disk rw - ext4 /dev/ublkb1 rw",
        )
        .unwrap();
        fs::write(root.path().join("comm"), b"live sibling").unwrap();
        fs::write(
            root.path().join("cgroup"),
            b"0::/system.slice/example.service",
        )
        .unwrap();
        assert!(
            inspect(
                root.path(),
                123,
                device,
                1,
                true,
                Instant::now() + Duration::from_secs(1)
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    #[ignore = "requires CAP_SYS_ADMIN; disposable mount namespace only"]
    fn private_mount_outside_a_chroot_is_not_lost() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().to_owned();
        let (namespace, device) = std::thread::spawn(move || {
            use std::os::unix::ffi::OsStrExt;
            assert_eq!(
                unsafe { libc::unshare(libc::CLONE_NEWNS | libc::CLONE_FS) },
                0
            );
            assert_eq!(
                unsafe {
                    libc::mount(
                        std::ptr::null(),
                        c"/".as_ptr(),
                        std::ptr::null(),
                        libc::MS_REC | libc::MS_PRIVATE,
                        std::ptr::null(),
                    )
                },
                0
            );
            let disk = target.join("disk");
            let jail = target.join("jail");
            fs::create_dir(&disk).unwrap();
            fs::create_dir(&jail).unwrap();
            let disk_path = std::ffi::CString::new(disk.as_os_str().as_bytes()).unwrap();
            assert_eq!(
                unsafe {
                    libc::mount(
                        c"tmpfs".as_ptr(),
                        disk_path.as_ptr(),
                        c"tmpfs".as_ptr(),
                        0,
                        std::ptr::null(),
                    )
                },
                0
            );
            let device = fs::metadata(&disk).unwrap().dev();
            let namespace = fs::File::open("/proc/thread-self/ns/mnt").unwrap();
            let jail_path = std::ffi::CString::new(jail.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::chroot(jail_path.as_ptr()) }, 0);
            assert_eq!(unsafe { libc::chdir(c"/".as_ptr()) }, 0);
            (namespace, device)
        })
        .join()
        .unwrap();
        assert!(
            namespace_holds(
                Path::new(&format!("/proc/self/fd/{}", namespace.as_raw_fd())),
                device
            )
            .unwrap()
        );
    }

    #[test]
    #[ignore = "requires CAP_SYS_ADMIN; disposable mount namespace only"]
    fn retained_namespace_without_procfs_still_reports_the_device() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().to_owned();
        let (namespace, device) = std::thread::spawn(move || {
            use std::os::unix::ffi::OsStrExt;
            assert_eq!(
                unsafe { libc::unshare(libc::CLONE_NEWNS | libc::CLONE_FS) },
                0
            );
            assert_eq!(
                unsafe {
                    libc::mount(
                        std::ptr::null(),
                        c"/".as_ptr(),
                        std::ptr::null(),
                        libc::MS_REC | libc::MS_PRIVATE,
                        std::ptr::null(),
                    )
                },
                0
            );
            let path = std::ffi::CString::new(target.as_os_str().as_bytes()).unwrap();
            assert_eq!(
                unsafe {
                    libc::mount(
                        c"tmpfs".as_ptr(),
                        path.as_ptr(),
                        c"tmpfs".as_ptr(),
                        0,
                        std::ptr::null(),
                    )
                },
                0
            );
            let device = fs::metadata(&target).unwrap().dev();
            let namespace = fs::File::open("/proc/thread-self/ns/mnt").unwrap();
            assert_eq!(
                unsafe { libc::umount2(c"/proc".as_ptr(), libc::MNT_DETACH) },
                0
            );
            (namespace, device)
        })
        .join()
        .unwrap();
        // No member task survives. Only this FD retains the tmpfs namespace.
        assert!(
            namespace_holds(
                Path::new(&format!("/proc/self/fd/{}", namespace.as_raw_fd())),
                device
            )
            .unwrap()
        );
    }

    #[test]
    fn missing_task_inspection_is_not_process_disappearance() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("task/123/ns")).unwrap();
        fs::write(
            root.path().join("stat"),
            format!("123 (live) S {} 123 0", vec!["0"; 18].join(" ")),
        )
        .unwrap();
        fs::write(
            root.path().join("task/123/stat"),
            format!("123 (live) S {} 123 0", vec!["0"; 18].join(" ")),
        )
        .unwrap();
        let error = inspect(
            root.path(),
            123,
            libc::makedev(259, 10),
            1,
            true,
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.to_string().contains("inspection incomplete"));
    }

    #[test]
    fn persistent_namespace_mount_paths_preserve_escaped_bytes() {
        let mounts = "1 0 0:4 /mnt:[123] /saved\\040space rw - nsfs nsfs rw\n2 0 8:1 / / rw - ext4 /dev/root rw";
        assert_eq!(
            namespace_mounts(mounts).unwrap(),
            vec![std::path::PathBuf::from("/saved space")]
        );
        assert!(namespace_mounts("invalid").is_err());
    }

    #[test]
    fn namespace_type_is_checked_before_entering_other_namespaces() {
        assert!(!namespace_holds(Path::new("/proc/self/ns/net"), libc::makedev(259, 10)).unwrap());
        assert!(namespace_holds(Path::new("/dev/null"), libc::makedev(259, 10)).is_err());
    }

    #[test]
    fn namespace_copy_and_open_mapping_are_distinct_from_shared_visibility() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("ns")).unwrap();
        fs::create_dir(root.path().join("fd")).unwrap();
        fs::write(root.path().join("ns/mnt"), b"namespace fixture").unwrap();
        fs::write(root.path().join("maps"), b"").unwrap();
        fs::write(
            root.path().join("mountinfo"),
            b"1 0 259:10 / /disk rw - ext4 /dev/ublkb1 rw",
        )
        .unwrap();
        let namespace = fs::metadata(root.path().join("ns/mnt")).unwrap().ino();
        let device = libc::makedev(259, 10);
        let deadline = Instant::now() + Duration::from_secs(3);
        assert!(!references(root.path(), device, namespace, true, deadline).unwrap());
        assert!(references(root.path(), device, namespace + 1, true, deadline).unwrap());
        assert!(!references(root.path(), device, namespace + 1, false, deadline).unwrap());
        fs::write(
            root.path().join("maps"),
            b"7f-8f r--p 0 103:0a 8 /disk/lib.so",
        )
        .unwrap();
        assert!(references(root.path(), device, namespace, false, deadline).unwrap());
    }

    #[test]
    fn shared_mount_visibility_is_not_a_reference_but_an_open_file_is() {
        let root = tempfile::tempdir().unwrap();
        let file = fs::File::create(root.path().join("open")).unwrap();
        let device = file.metadata().unwrap().dev();
        let process = std::path::PathBuf::from(format!("/proc/{}", std::process::id()));
        let namespace = fs::metadata("/proc/self/ns/mnt").unwrap().ino();
        let observed = inspect(
            &process,
            std::process::id(),
            device,
            namespace,
            true,
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap()
        .unwrap();
        assert_eq!(observed.pid, std::process::id());
        assert!(pin(&observed, device).unwrap().is_some());
        let mut reused = observed.clone();
        reused.started += 1;
        assert!(pin(&reused, device).unwrap().is_none());
        // Self is diagnosable but never an automatic termination target.
        assert_eq!(unit(&observed, &crate::config::id()), None);
    }

    #[test]
    fn device_identity_is_numeric_and_exact() {
        let dev = libc::makedev(259, 10);
        assert!(has_device(
            "36 25 259:10 / /disk rw - ext4 /dev/ublkb1 rw",
            dev
        ));
        assert!(!has_device(
            "36 25 259:100 / /disk rw - ext4 /dev/ublkb1 rw",
            dev
        ));
        assert!(mapped_device("7f-8f r--p 0 103:0a 8 /disk/lib.so", dev));
        assert!(!mapped_device("7f-8f r--p 0 103:0b 8 /disk/lib.so", dev));
    }

    #[test]
    fn stopping_never_targets_another_attempt_or_an_ancestor_slice() {
        let attempt = crate::config::id();
        let mut holder = Holder {
            pid: std::process::id() + 1,
            started: 1,
            name: "valkey".into(),
            cgroup: format!(
                "{}/cairn-native-{attempt}.service/runner",
                super::super::super::systemd::workload_cgroup().unwrap()
            ),
        };
        assert_eq!(
            unit(&holder, &attempt),
            Some(format!("cairn-native-{attempt}.service"))
        );
        holder.cgroup = format!(
            "/system.slice/cairn-native-{}.service/runner",
            crate::config::id()
        );
        assert_eq!(unit(&holder, &attempt), None);
        holder.cgroup = format!(
            "{}/cairn-native-{}.service/runner/cairn-native-{attempt}.service",
            super::super::super::systemd::workload_cgroup().unwrap(),
            crate::config::id()
        );
        assert_eq!(unit(&holder, &attempt), None);
        holder.cgroup = format!("/system.slice/unrelated.service/cairn-native-{attempt}.service");
        assert_eq!(unit(&holder, &attempt), None);
        holder.cgroup = "/system.slice/docker-invalid.scope".into();
        assert_eq!(unit(&holder, &attempt), None);
        holder.cgroup = format!("/system.slice/docker-{}.scope", "a".repeat(64));
        assert!(unit(&holder, &attempt).is_some());
        holder.pid = 1;
        assert_eq!(unit(&holder, &attempt), None);
    }

    #[test]
    fn arbitrary_filename_bytes_do_not_invalidate_proc_numeric_fields() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("maps");
        fs::write(&path, b"7f-8f r--p 0 103:0a 8 /mapped-\xff\n").unwrap();
        assert!(mapped_device(&read(path).unwrap(), libc::makedev(259, 10)));
    }

    #[test]
    fn process_identity_accepts_parentheses_in_names() {
        let fields = format!("9 (name ) with parens) S {} 123 0", vec!["0"; 18].join(" "));
        assert_eq!(started(&fields).unwrap(), 123);
    }
}
