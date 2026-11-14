//! Rootful Docker may place containers outside the native workload's cgroup.
//! Only Docker's own per-container binds on this private disk attribute a scope;
//! an inherited mount or an arbitrary bind is never permission to stop a unit.
use crate::error::{Error, Result};
use std::{
    collections::BTreeSet,
    fs, io,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    time::Duration,
};

fn vanished(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH))
}

fn device(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_block_device() {
        return Err(Error::conflict(
            "Native disk attachment is not a block device.",
        ));
    }
    Ok(format!(
        "{}:{}",
        libc::major(metadata.rdev()),
        libc::minor(metadata.rdev())
    ))
}

fn container(cgroup: &str) -> Option<&str> {
    let group = cgroup.trim().strip_prefix("0::/system.slice/docker-")?;
    let id = group.strip_suffix(".scope")?;
    (id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(id)
}

fn docker_bind(mountinfo: &str, device: &str, id: &str) -> bool {
    mountinfo.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let Some(found) = fields.nth(2) else {
            return false;
        };
        let Some(root) = fields.next() else {
            return false;
        };
        found == device
            && ["hostname", "hosts", "resolv.conf"]
                .iter()
                .any(|name| root.ends_with(&format!("/containers/{id}/{name}")))
    })
}

fn owned_scopes(proc: &Path, device: &str) -> io::Result<BTreeSet<String>> {
    let mut scopes = BTreeSet::new();
    for entry in fs::read_dir(proc)? {
        let entry = entry?;
        if !entry
            .file_name()
            .as_encoded_bytes()
            .iter()
            .all(u8::is_ascii_digit)
        {
            continue;
        }
        let process = entry.path();
        let inspect = || -> io::Result<Option<String>> {
            let cgroup = fs::read_to_string(process.join("cgroup"))?;
            let Some(id) = container(&cgroup) else {
                return Ok(None);
            };
            let mounts = fs::read_to_string(process.join("mountinfo"))?;
            // Revalidate membership after reading the namespace; a moved process
            // cannot attribute its previous scope through a later mount view.
            if fs::read_to_string(process.join("cgroup"))? != cgroup
                || !docker_bind(&mounts, device, id)
            {
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

async fn scopes(device: &str) -> Result<BTreeSet<String>> {
    let device = device.to_owned();
    tokio::task::spawn_blocking(move || owned_scopes(Path::new("/proc"), &device))
        .await
        .map_err(Error::internal)?
        .map_err(Into::into)
}

async fn populated(group: &Path) -> Result<bool> {
    match tokio::fs::read_to_string(group.join("cgroup.events")).await {
        Ok(events) => Ok(events.lines().any(|line| line == "populated 1")),
        Err(error) if vanished(&error) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Called after the workload and its daemon have been positively fenced, but
/// before flushing the filesystem. Graceful stop is bounded, then the attributed
/// scope is killed; its cgroup must be empty before disk close proceeds.
pub(in crate::native) async fn stop(device_path: &Path) -> Result<()> {
    let device = device(device_path)?;
    for scope in scopes(&device).await? {
        if !scopes(&device).await?.contains(&scope) {
            continue;
        }
        let group = PathBuf::from("/sys/fs/cgroup/system.slice").join(&scope);
        let identity = match tokio::fs::metadata(&group).await {
            Ok(metadata) => (metadata.dev(), metadata.ino()),
            Err(error) if vanished(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        super::output("/usr/bin/systemctl", &["--no-block", "stop", &scope]).await?;
        let graceful = tokio::time::Instant::now() + Duration::from_secs(10);
        let deadline = graceful + Duration::from_secs(10);
        let mut killed = false;
        while populated(&group).await? {
            let metadata = tokio::fs::metadata(&group).await?;
            if (metadata.dev(), metadata.ino()) != identity {
                return Err(Error::conflict(
                    "Native Docker scope changed during termination.",
                ));
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(Error::unavailable(
                    "Native Docker consumers did not stop before their deadline.",
                ));
            }
            if now >= graceful && !killed {
                if !scopes(&device).await?.contains(&scope) {
                    return Err(Error::conflict(
                        "Native Docker consumer ownership changed before termination.",
                    ));
                }
                super::output(
                    "/usr/bin/systemctl",
                    &["kill", "--kill-whom=all", "--signal=KILL", &scope],
                )
                .await?;
                killed = true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tracing::info!(target: "cairn_performance", parent: None, operation = "native_disk_consumers", forced = killed, "Stopped an attributed native Docker scope");
    }
    let current = fs::metadata("/proc/thread-self/ns/mnt")?;
    let exclude = Some((current.dev(), current.ino()));
    let remaining = tokio::task::spawn_blocking(move || {
        mounted_namespaces(Path::new("/proc"), &device, exclude)
    })
    .await
    .map_err(Error::internal)??;
    require_no_consumers(remaining)
}

fn require_no_consumers(remaining: usize) -> Result<()> {
    if remaining == 0 {
        return Ok(());
    }
    tracing::warn!(target: "cairn_performance", parent: None, operation = "native_disk_consumers", remaining_namespaces = remaining, "Native disk close is waiting for external consumers before flush");
    Err(Error::conflict(
        "Native disk remains mounted in another namespace; ownership is retained before flush.",
    ))
}

fn mounted_namespaces(proc: &Path, device: &str, exclude: Option<(u64, u64)>) -> io::Result<usize> {
    let mut seen = BTreeSet::new();
    let mut remaining = 0;
    for process in fs::read_dir(proc)? {
        let process = process?;
        if !process
            .file_name()
            .as_encoded_bytes()
            .iter()
            .all(u8::is_ascii_digit)
        {
            continue;
        }
        let tasks = match fs::read_dir(process.path().join("task")) {
            Ok(tasks) => tasks,
            Err(error) if vanished(&error) => continue,
            Err(error) => return Err(error),
        };
        for task in tasks {
            let task = task?;
            let mut inspect = || -> io::Result<()> {
                let namespace = fs::metadata(task.path().join("ns/mnt"))?;
                let identity = (namespace.dev(), namespace.ino());
                if Some(identity) == exclude || seen.contains(&identity) {
                    return Ok(());
                }
                let mounts = fs::read_to_string(task.path().join("mountinfo"))?;
                let after = fs::metadata(task.path().join("ns/mnt"))?;
                if (after.dev(), after.ino()) != identity {
                    return Err(io::Error::other(
                        "Native mount namespace changed during inspection",
                    ));
                }
                seen.insert(identity);
                if mounts
                    .lines()
                    .any(|line| line.split_whitespace().nth(2) == Some(device))
                {
                    remaining += 1;
                }
                Ok(())
            };
            match inspect() {
                Ok(()) => {}
                Err(error) if vanished(&error) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(remaining)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn process(proc: &Path, pid: u32, cgroup: &str, device: &str, root: &str) {
        let dir = proc.join(pid.to_string());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cgroup"), cgroup).unwrap();
        fs::write(
            dir.join("mountinfo"),
            format!("1 0 {device} {root} /etc/hosts rw - ext4 /dev/ublkb0 rw\n"),
        )
        .unwrap();
    }

    #[test]
    fn escaped_docker_scopes_require_their_own_metadata_on_the_owned_device() {
        let proc = tempfile::tempdir().unwrap();
        let group = format!("0::/system.slice/docker-{ID}.scope\n");
        let bind = format!("/home/pilot/docker-data/containers/{ID}/hosts");
        process(proc.path(), 10, &group, "259:9", &bind);
        process(proc.path(), 11, &group, "259:9", &bind);
        assert_eq!(
            owned_scopes(proc.path(), "259:9").unwrap(),
            BTreeSet::from([format!("docker-{ID}.scope")])
        );
        assert!(owned_scopes(proc.path(), "259:10").unwrap().is_empty());
    }

    #[test]
    fn inherited_or_arbitrary_mounts_and_unrelated_scopes_never_authorize_stop() {
        let proc = tempfile::tempdir().unwrap();
        let group = format!("0::/system.slice/docker-{ID}.scope\n");
        process(proc.path(), 10, &group, "259:9", "/");
        process(proc.path(), 11, &group, "259:9", "/customer/data");
        process(
            proc.path(),
            12,
            "0::/system.slice/other.service\n",
            "259:9",
            &format!("/containers/{ID}/hosts"),
        );
        process(
            proc.path(),
            13,
            &group,
            "259:9",
            &format!("/containers/{ID}/hosts.extra"),
        );
        assert!(owned_scopes(proc.path(), "259:9").unwrap().is_empty());
        assert!(container("0::/system.slice/docker-../../other.scope").is_none());
    }

    #[test]
    fn a_foreign_thread_mount_keeps_retirement_fenced_even_after_leader_unmount() {
        let proc = tempfile::tempdir().unwrap();
        let root = proc.path().join("10/task");
        for (tid, device) in [(10, "8:1"), (11, "259:9")] {
            let task = root.join(tid.to_string());
            fs::create_dir_all(task.join("ns")).unwrap();
            fs::write(task.join("ns/mnt"), b"namespace").unwrap();
            fs::write(
                task.join("mountinfo"),
                format!("1 0 {device} / / rw - ext4 /dev/test rw\n"),
            )
            .unwrap();
        }
        assert_eq!(mounted_namespaces(proc.path(), "259:9", None).unwrap(), 1);
        assert!(require_no_consumers(1).is_err());
        fs::write(root.join("11/mountinfo"), "").unwrap();
        assert_eq!(mounted_namespaces(proc.path(), "259:9", None).unwrap(), 0);
        assert!(require_no_consumers(0).is_ok());
    }
}
