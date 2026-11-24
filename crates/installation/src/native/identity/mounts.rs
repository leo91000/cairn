//! Temporary idmapped sources for the workload manager's private bind mounts.
//! The canonical data tree remains accessible to controller-side recovery.
use super::{COUNT, Identity, STORAGE_OWNER};
use crate::error::{Error, Result};
use std::{
    ffi::CString,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    time::Duration,
};

fn options(storage: &Identity, workload: &Identity) -> Result<String> {
    let Some((uid, gid)) = storage.host_engine else {
        return Err(Error::conflict("Native storage owner has no engine ID."));
    };
    if storage.user != STORAGE_OWNER || workload.host_engine.is_some() || uid == 0 || gid == 0 {
        return Err(Error::conflict("Invalid native mount identity."));
    }
    // Disk IDs map root, engine and the two halves of the subordinate range.
    // The engine slot is intentionally absent from the backing subordinate IDs.
    let mut mappings = Vec::with_capacity(8);
    for (kind, root, disk, engine, host_root, host_sub) in [
        (
            'u',
            storage.root_uid,
            storage.subuid,
            uid,
            workload.root_uid,
            workload.subuid,
        ),
        (
            'g',
            storage.root_gid,
            storage.subgid,
            gid,
            workload.root_gid,
            workload.subgid,
        ),
    ] {
        mappings.extend([
            format!("{kind}:{root}:{host_root}:1"),
            format!("{kind}:{engine}:{}:1", host_sub + 999),
            format!("{kind}:{disk}:{host_sub}:999"),
            format!(
                "{kind}:{}:{}:{}",
                disk + 1000,
                host_sub + 1000,
                COUNT - 1000
            ),
        ]);
    }
    Ok(format!("X-mount.idmap={}", mappings.join(" ")))
}

fn target(state: &Path, attempt: &str) -> Result<PathBuf> {
    crate::validation::uuid(attempt)?;
    Ok(state.join("roots").join(attempt).with_extension("idmap"))
}

fn detach(source: &Path, target: &Path) -> Result<()> {
    let target_metadata = match std::fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !target_metadata.is_dir() || target_metadata.file_type().is_symlink() {
        return Err(Error::conflict("Invalid native idmapped mount target."));
    }

    let original = std::fs::symlink_metadata(source)?;
    if (original.dev(), original.ino()) == (target_metadata.dev(), target_metadata.ino()) {
        let target =
            CString::new(target.as_os_str().as_encoded_bytes()).map_err(Error::internal)?;
        // systemd's workload namespace keeps its own bind mounts. Detaching
        // this alias leaves all of those consumers attributed to the same tree.
        if unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH | libc::UMOUNT_NOFOLLOW) } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    } else {
        super::super::workspace::require_no_mounts(target)?;
    }
    std::fs::remove_dir(target)?;
    Ok(())
}

pub(in crate::native) struct Mapped {
    source: PathBuf,
    path: PathBuf,
}

impl Mapped {
    pub async fn attach(
        state: &Path,
        attempt: &str,
        source: &Path,
        storage: &Identity,
        workload: &Identity,
    ) -> Result<Self> {
        let options = options(storage, workload)?;
        let path = target(state, attempt)?;
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        let mapping = Self {
            source: source.to_owned(),
            path,
        };
        let mut command = tokio::process::Command::new("/usr/bin/mount")
            .args(["--internal-only", "--bind", "--options", &options, "--"])
            .arg(source)
            .arg(&mapping.path)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let status = match tokio::time::timeout(Duration::from_secs(10), command.wait()).await {
            Ok(result) => result?,
            Err(_) => {
                // A late mount cannot appear after its cleanup and reassign
                // the alias. Reap the process while ownership is still held.
                let _ = command.start_kill();
                command.wait().await?;
                return Err(Error::timeout("Native idmapped mount timed out."));
            }
        };
        if !status.success() {
            return Err(Error::unavailable(format!(
                "Native idmapped mount failed ({status})."
            )));
        }
        let original = std::fs::metadata(source)?;
        let mounted = std::fs::metadata(&mapping.path)?;
        if (original.dev(), original.ino(), mounted.uid(), mounted.gid())
            != (mounted.dev(), mounted.ino(), workload.uid(), workload.gid())
        {
            return Err(Error::conflict(
                "Native idmapped mount exposes unexpected ownership.",
            ));
        }
        Ok(mapping)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// After confirming that the managed unit obtained its own bind mounts.
    pub fn close(self) -> Result<()> {
        detach(&self.source, &self.path)
    }

    pub fn recover(state: &Path, attempt: &str, run: &str) -> Result<()> {
        let source = super::super::workspace::workload(state, run)?.join("data");
        detach(&source, &target(state, attempt)?)
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        if let Err(error) = detach(&self.source, &self.path) {
            tracing::warn!(message = %error.message, "Native idmapped mount cleanup deferred");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::lchown;

    #[tokio::test]
    #[ignore = "requires an explicitly disposable host and the native quota fixture"]
    async fn kernel_mapped_view_keeps_disk_owners_and_uses_conversation_ids() {
        assert_eq!(
            std::env::var("CAIRN_NATIVE_DISPOSABLE_HOST").as_deref(),
            Ok("1")
        );
        let root = PathBuf::from(std::env::var("CAIRN_NATIVE_QUOTA_TEST_STATE").unwrap());
        let attempt = crate::config::id();
        let run = crate::config::id();
        let raw = super::super::super::workspace::workload(&root, &run).unwrap();
        let source = raw.join("data");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(root.join("roots")).unwrap();

        let storage = Identity::storage((1001, 1002));
        let workload = Identity {
            user: "fixture".into(),
            root_uid: 500_000_000,
            root_gid: 500_000_001,
            subuid: 1_000_013_824,
            subgid: 1_000_079_360,
            host_engine: None,
        };
        lchown(&source, Some(storage.uid()), Some(storage.gid())).unwrap();
        for (name, uid, gid) in [
            ("engine", storage.uid(), storage.gid()),
            ("root", storage.root_uid, storage.root_gid),
            ("volume", storage.subuid + 2000, storage.subgid + 3000),
        ] {
            std::fs::write(source.join(name), name).unwrap();
            lchown(source.join(name), Some(uid), Some(gid)).unwrap();
        }
        let mapped = Mapped::attach(&root, &attempt, &source, &storage, &workload)
            .await
            .unwrap();
        let path = mapped.path().to_owned();
        for (name, uid, gid) in [
            ("engine", workload.uid(), workload.gid()),
            ("root", workload.root_uid, workload.root_gid),
            ("volume", workload.subuid + 2000, workload.subgid + 3000),
        ] {
            let visible = std::fs::metadata(path.join(name)).unwrap();
            let raw = std::fs::metadata(source.join(name)).unwrap();
            assert_eq!((visible.uid(), visible.gid()), (uid, gid));
            assert_eq!((visible.dev(), visible.ino()), (raw.dev(), raw.ino()));
        }
        lchown(
            path.join("volume"),
            Some(workload.subuid + 4000),
            Some(workload.subgid + 5000),
        )
        .unwrap();
        let volume = std::fs::metadata(source.join("volume")).unwrap();
        assert_eq!(
            (volume.uid(), volume.gid()),
            (storage.subuid + 4000, storage.subgid + 5000)
        );

        mapped.close().unwrap();
        Mapped::recover(&root, &attempt, &run).unwrap();
        assert!(!path.exists());
        std::fs::remove_dir_all(raw).unwrap();
    }
}
