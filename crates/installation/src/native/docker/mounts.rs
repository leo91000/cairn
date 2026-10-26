//! Pin untrusted bind sources before Docker resolves them. Authorization alone
//! cannot make a mutable pathname safe between plugin approval and mount(2).
use super::{DATA, Policy, RUNTIME};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    ffi::CString,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::{Component, Path, PathBuf},
};

// Mirrors live outside the ext4 payload. Bound their count independently so a
// create/delete loop cannot grow runtime inodes beyond the conversation budget.
const MAX_BINDINGS: usize = 4096;
static BIND_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn capacity() -> Result<()> {
    match std::fs::read_dir(Path::new(RUNTIME).join("mounts")) {
        Ok(entries) => {
            let mut count = 0;
            for entry in entries.take(MAX_BINDINGS) {
                entry?;
                count += 1;
            }
            if count >= MAX_BINDINGS {
                return Err(Error::conflict(
                    "Native Docker bind mirror limit reached; continue in a new turn to reclaim deleted containers' mirrors.",
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[derive(Deserialize, Serialize)]
struct Binding {
    source: PathBuf,
    read_only: bool,
}

fn uuid(path: &Path) -> Result<&str> {
    let prefix = Path::new(RUNTIME).join("mounts");
    let suffix = path.strip_prefix(prefix).map_err(Error::internal)?;
    let parts: Vec<_> = suffix.components().collect();
    let [Component::Normal(id), Component::Normal(leaf)] = parts.as_slice() else {
        return Err(Error::forbidden("Invalid private Docker mount."));
    };
    if *leaf != "source" {
        return Err(Error::forbidden("Invalid private Docker mount."));
    }
    let id = id
        .to_str()
        .ok_or_else(|| Error::forbidden("Invalid Docker mount identity."))?;
    crate::validation::uuid(id)?;
    Ok(id)
}

fn record(id: &str) -> PathBuf {
    Path::new(DATA)
        .join(".cairn-bindings")
        .join(format!("{id}.json"))
}

fn confirmed_mount(path: &Path) -> Result<bool> {
    // These private targets contain only fixed ASCII components and a validated
    // UUID, so mountinfo does not escape their names. A leftover empty target
    // from a failed attach must never be mistaken for a pinned mount.
    let target = path
        .to_str()
        .ok_or_else(|| Error::forbidden("Invalid Docker mirror path."))?;
    Ok(std::fs::read_to_string("/proc/self/mountinfo")?
        .lines()
        .any(|line| line.split_whitespace().nth(4) == Some(target)))
}

pub(super) fn validate(policy: &Policy, path: &Path, read_only: bool) -> Result<()> {
    let binding: Binding = serde_json::from_slice(&std::fs::read(record(uuid(path)?))?)?;
    if binding.read_only && !read_only {
        return Err(Error::forbidden(
            "Docker must retain this bind's read-only permission.",
        ));
    }
    // This turn already pinned the source inode. Re-resolving its mutable
    // original name can reject attach/start midway through Docker run, leaving
    // the client's upgraded attach waiting for a start that was refused.
    // A new turn restores mirrors only after resolving the source again.
    if !confirmed_mount(path)? {
        return Err(Error::forbidden(
            "Docker bind mirror is unavailable in this turn.",
        ));
    }
    policy.bind_scope(
        binding
            .source
            .to_str()
            .ok_or_else(|| Error::forbidden("Invalid Docker source."))?,
        read_only,
    )
}

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

/// Descriptor-relative resolution rejects symlinks and proc magic links in
/// every component. The source inode stays pinned even if its name is replaced.
fn open_source(policy: &Policy, source: &Path, read_only: bool) -> Result<OwnedFd> {
    let text = source
        .to_str()
        .ok_or_else(|| Error::forbidden("Docker bind path must be UTF-8."))?;
    policy.bind_original(text, read_only)?;
    let root = policy
        .roots()
        .into_iter()
        .find(|root| source.starts_with(root))
        .ok_or_else(|| Error::forbidden("Docker bind source is outside this conversation."))?;
    let suffix = source.strip_prefix(&root).map_err(Error::internal)?;
    // Resolve the allowed root itself without symlinks as well: its ancestors
    // can include engine-writable directories and must not be raced either.
    let slash = std::fs::File::open("/")?;
    let root_name = CString::new(
        root.strip_prefix("/")
            .map_err(Error::internal)?
            .as_os_str()
            .as_encoded_bytes(),
    )
    .map_err(Error::internal)?;
    let root_how = OpenHow {
        flags: (libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
        mode: 0,
        resolve: 0x08 | 0x04 | 0x02,
    };
    let root_fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            slash.as_raw_fd(),
            root_name.as_ptr(),
            &raw const root_how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if root_fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let root = unsafe { OwnedFd::from_raw_fd(root_fd as i32) };
    let suffix = CString::new(if suffix.as_os_str().is_empty() {
        "."
    } else {
        suffix.to_str().unwrap()
    })
    .map_err(Error::internal)?;
    let how = OpenHow {
        flags: (libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
        mode: 0,
        resolve: 0x08 | 0x04 | 0x02, // BENEATH | NO_SYMLINKS | NO_MAGICLINKS
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            suffix.as_ptr(),
            &raw const how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

fn attach(fd: &OwnedFd, target: &Path, read_only: bool) -> Result<()> {
    const OPEN_TREE_CLONE: u32 = 1;
    const AT_RECURSIVE: u32 = 0x8000;
    const AT_EMPTY_PATH: u32 = 0x1000;
    let empty = c"";
    let tree = unsafe {
        libc::syscall(
            libc::SYS_open_tree,
            fd.as_raw_fd(),
            empty.as_ptr(),
            OPEN_TREE_CLONE | libc::O_CLOEXEC as u32 | AT_EMPTY_PATH | AT_RECURSIVE,
        )
    };
    if tree < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let tree = unsafe { OwnedFd::from_raw_fd(tree as i32) };
    if read_only {
        #[repr(C)]
        struct Attributes {
            set: u64,
            clear: u64,
            propagation: u64,
            userns: u64,
        }
        let attributes = Attributes {
            set: 1,
            clear: 0,
            propagation: 0,
            userns: 0,
        };
        if unsafe {
            libc::syscall(
                libc::SYS_mount_setattr,
                tree.as_raw_fd(),
                empty.as_ptr(),
                AT_EMPTY_PATH | AT_RECURSIVE,
                &raw const attributes,
                std::mem::size_of::<Attributes>(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let target = std::fs::File::open(target)?;
    if unsafe {
        libc::syscall(
            libc::SYS_move_mount,
            tree.as_raw_fd(),
            empty.as_ptr(),
            target.as_raw_fd(),
            empty.as_ptr(),
            0x04 | 0x40,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

async fn mount(policy: &Policy, binding: &Binding, id: &str) -> Result<PathBuf> {
    let fd = open_source(policy, &binding.source, binding.read_only)?;
    let metadata = std::fs::File::from(fd.try_clone()?).metadata()?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(Error::forbidden(
            "Docker bind mounts support regular files and directories only.",
        ));
    }
    let parent = Path::new(RUNTIME).join("mounts").join(id);
    crate::skills::private_dir(&parent).await?;
    let target = parent.join("source");
    if target.exists() {
        if !confirmed_mount(&target)? {
            return Err(Error::unavailable(
                "Docker bind attachment is unconfirmed; retry in a new turn.",
            ));
        }
        return Ok(target);
    }
    if metadata.is_dir() {
        tokio::fs::create_dir(&target).await?;
    } else {
        tokio::fs::File::create(&target).await?;
    }
    // Both ends are descriptors. The target is under an unwriteable root-owned
    // parent and the source is the inode resolved by openat2, not a second lookup.
    attach(&fd, &target, binding.read_only)?;
    Ok(target)
}

pub(super) async fn pin(policy: &Policy, source: &str, read_only: bool) -> Result<String> {
    let _guard = BIND_LOCK.lock().await;
    capacity()?;
    let binding = Binding {
        source: source.into(),
        read_only,
    };
    let id = crate::config::id();
    let target = mount(policy, &binding, &id).await?;
    let records = Path::new(DATA).join(".cairn-bindings");
    crate::skills::private_dir(&records).await?;
    super::super::atomic_write(&record(&id), &serde_json::to_vec(&binding)?).await?;
    Ok(target.to_string_lossy().into_owned())
}

async fn referenced_bindings(store: &Path) -> Result<std::collections::HashSet<String>> {
    let containers = store.join("containers");
    let mut used = std::collections::HashSet::new();
    let mut entries = match tokio::fs::read_dir(containers).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(used),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let id = entry.file_name();
        if id.len() != 64 || !id.as_encoded_bytes().iter().all(u8::is_ascii_hexdigit) {
            continue;
        }
        // A crash can retain a directory before Docker saves its metadata.
        // Isolate that container instead of blocking the entire daemon/store.
        // Activation still requires a valid saved HostConfig in Policy.
        let bytes = match tokio::fs::read(entry.path().join("hostconfig.json")).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let Ok(host) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        for bind in host["Binds"].as_array().into_iter().flatten() {
            if let Some(source) = bind.as_str().and_then(|bind| bind.split(':').next())
                && let Ok(id) = uuid(Path::new(source))
            {
                used.insert(id.to_owned());
            }
        }
        for bind in host["Mounts"].as_array().into_iter().flatten() {
            if bind["Type"] == "bind"
                && let Some(source) = bind["Source"].as_str()
                && let Ok(id) = uuid(Path::new(source))
            {
                used.insert(id.to_owned());
            }
        }
    }
    if used.len() > MAX_BINDINGS {
        return Err(Error::conflict(
            "Persisted Docker containers exceed the native bind mirror limit.",
        ));
    }
    Ok(used)
}

/// Before starting a daemon, recreate only bindings still referenced by saved
/// containers. Invalid current permissions leave the mirror absent; the policy
/// refuses starting that container. Restart policies are never accepted.
pub(super) async fn restore(policy: &Policy) -> Result<()> {
    let _guard = BIND_LOCK.lock().await;
    let used = referenced_bindings(Path::new(DATA)).await?;
    let records = Path::new(DATA).join(".cairn-bindings");
    let mut entries = match tokio::fs::read_dir(&records).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let id = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .ok_or_else(|| Error::conflict("Invalid native Docker mount record."))?;
        crate::validation::uuid(id)?;
        if !used.contains(id) {
            tokio::fs::remove_file(entry.path()).await?;
            continue;
        }
        let binding: Binding = serde_json::from_slice(&tokio::fs::read(entry.path()).await?)?;
        if policy
            .bind_original(binding.source.to_str().unwrap_or(""), binding.read_only)
            .is_ok()
        {
            mount(policy, &binding, id).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn incomplete_container_metadata_does_not_hide_valid_saved_bindings() {
        let root = tempfile::tempdir().unwrap();
        assert!(referenced_bindings(root.path()).await.unwrap().is_empty());
        let legacy = crate::config::id();
        let structured = crate::config::id();
        for id in ["a", "b", "c"] {
            std::fs::create_dir_all(root.path().join("containers").join(id.repeat(64))).unwrap();
        }
        let valid = root.path().join("containers").join("a".repeat(64));
        std::fs::write(
            valid.join("hostconfig.json"),
            serde_json::to_vec(&json!({
                "Binds": [format!("{RUNTIME}/mounts/{legacy}/source:/legacy:ro")],
                "Mounts": [{
                    "Type": "bind",
                    "Source": format!("{RUNTIME}/mounts/{structured}/source"),
                    "Target": "/structured",
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        let malformed = root.path().join("containers").join("c".repeat(64));
        std::fs::write(malformed.join("hostconfig.json"), b"{").unwrap();
        assert_eq!(
            referenced_bindings(root.path()).await.unwrap(),
            [legacy, structured]
                .into_iter()
                .collect::<std::collections::HashSet<_>>()
        );
        // Genuine filesystem errors must remain visible rather than silently
        // discarding otherwise valid durable bindings.
        std::fs::remove_file(malformed.join("hostconfig.json")).unwrap();
        std::fs::create_dir(malformed.join("hostconfig.json")).unwrap();
        assert!(referenced_bindings(root.path()).await.is_err());
    }

    #[test]
    fn unmounted_targets_are_not_confirmed_mirrors() {
        let root = tempfile::tempdir().unwrap();
        assert!(!confirmed_mount(root.path()).unwrap());
        let target = root.path().join("source");
        std::fs::write(&target, b"").unwrap();
        assert!(!confirmed_mount(&target).unwrap());
        assert!(confirmed_mount(Path::new("/proc")).unwrap());
    }

    #[test]
    fn replaced_source_names_cannot_redirect_a_pinned_descriptor() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        let denied = temporary.path().join("denied");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&denied).unwrap();
        std::fs::write(root.join("source"), b"allowed").unwrap();
        std::fs::write(denied.join("source"), b"private").unwrap();
        let policy = Policy {
            plan: Arc::new(json!({
                "projectRoot": root,
                "sandbox": "workspace-write",
            })),
        };
        let fd = open_source(&policy, &root.join("source"), false).unwrap();
        std::fs::rename(root.join("source"), root.join("retained")).unwrap();
        std::os::unix::fs::symlink(denied.join("source"), root.join("source")).unwrap();
        assert_eq!(
            std::fs::read(format!("/proc/self/fd/{}", fd.as_raw_fd())).unwrap(),
            b"allowed"
        );
        assert!(open_source(&policy, &root.join("source"), false).is_err());
        assert!(open_source(&policy, Path::new("/var/lib/cairn-docker/source"), false).is_err());
    }
}
