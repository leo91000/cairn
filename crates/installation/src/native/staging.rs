//! Reserve announced staging files under the pool gate, then receive without it.
use crate::{
    error::{Error, Result},
    nodes::files::FileAdmission,
};
use std::{
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

const ROOT: &str = "native-staging";
const PREFIX: &str = "receiving-v1-";
const MARKER: &[u8] = b"cairn-native-staging-v1\n";

/// A private reception tree and its crash-recovery ownership. Payload publication
/// can rename the payload child while this container retains other promises.
pub(crate) struct Directory {
    payload: PathBuf,
    directory: tempfile::TempDir,
    _owner: crate::file_lock::Guard,
}

impl Directory {
    pub async fn new(storage: &Storage) -> Result<Self> {
        let owner = super::disk::lock_staging(&storage.state).await?;
        let data = storage.data.clone();
        tokio::task::spawn_blocking(move || Self::create_locked(&data, &owner))
            .await
            .map_err(Error::internal)?
    }

    fn create_locked(data: &Path, _pool: &super::disk::StorageGuard) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        recover_locked(data)?;
        let root = data.join(ROOT);
        std::fs::create_dir_all(&root)?;
        if !std::fs::symlink_metadata(&root)?.is_dir() {
            return Err(Error::bad(
                "Native staging root is not a private directory.",
            ));
        }
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        let directory = tempfile::Builder::new().prefix(PREFIX).tempdir_in(&root)?;
        let owner =
            crate::file_lock::exclusive_directory(directory.path(), "Native staging is owned.")?;
        let payload = directory.path().join("payload");
        std::fs::create_dir(&payload)?;
        let mut marker = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.path().join("owner"))?;
        marker.write_all(MARKER)?;
        marker.sync_all()?;
        std::fs::File::open(directory.path())?.sync_all()?;
        std::fs::File::open(root)?.sync_all()?;
        Ok(Self {
            payload,
            directory,
            _owner: owner,
        })
    }

    pub fn path(&self) -> &Path {
        &self.payload
    }

    pub fn container(&self) -> &Path {
        self.directory.path()
    }
}

/// Recover only recognizable, unlocked Cairn reception containers. A live
/// receiver keeps its inode lock even while no pool lock is held over network IO.
fn recover_locked(data: &Path) -> Result<()> {
    let root = data.join(ROOT);
    let metadata = match std::fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() {
        return Err(Error::bad(
            "Native staging root is not a private directory.",
        ));
    }
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with(PREFIX) || !entry.file_type()?.is_dir()
        {
            continue;
        }
        let path = entry.path();
        let _owner =
            match crate::file_lock::exclusive_directory(&path, "Native reception is still active.")
            {
                Ok(owner) => owner,
                Err(error) if error.status == 409 || !path.exists() => continue,
                Err(error) => return Err(error),
            };
        let marker_path = path.join("owner");
        if !std::fs::symlink_metadata(&marker_path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == MARKER.len() as u64)
        {
            continue;
        }
        let marker = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(marker_path);
        let mut bytes = Vec::new();
        match marker {
            Ok(marker) => {
                marker.take(64).read_to_end(&mut bytes)?;
            }
            Err(_) => continue,
        }
        if bytes == MARKER {
            std::fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

pub(crate) async fn recover(data: &Path, state: &Path) -> Result<()> {
    let owner = super::disk::lock(state).await?;
    let data = data.to_owned();
    tokio::task::spawn_blocking(move || {
        let _owner = owner;
        recover_locked(&data)
    })
    .await
    .map_err(Error::internal)?
}

#[derive(Clone)]
pub(crate) struct Storage {
    pub state: PathBuf,
    pub data: PathBuf,
    pub client: reqwest::Client,
    pub origin: String,
}

impl Storage {
    pub fn local() -> Result<Self> {
        Ok(Self {
            state: super::state_dir(),
            data: crate::nodes::node_data_dir(),
            client: crate::nodes::local_client()?,
            origin: "http://localhost".into(),
        })
    }
}

pub(crate) struct Admission {
    pub storage: Storage,
    pub keep: Vec<String>,
    /// Inbox replacement briefly holds a second copy. This sparse, private
    /// file charges that promise to the same logical staging pool.
    pub replacement: Option<std::fs::File>,
}

impl FileAdmission for Admission {
    async fn open(&mut self, target: &Path, size: u64, mode: u32) -> Result<tokio::fs::File> {
        let owner = super::disk::lock_staging(&self.storage.state).await?;
        let required = size
            .checked_mul(if self.replacement.is_some() { 2 } else { 1 })
            .ok_or_else(|| Error::bad("Workspace exceeds node disk budget."))?;
        let available =
            super::disk::staging_budget_from(&self.storage.client, &self.storage.origin).await?;
        if required > available {
            crate::project_archives::reclaim_for_foreground(
                &self.storage.data,
                required - available,
                &self.keep,
            )
            .await?;
            let available =
                super::disk::staging_budget_from(&self.storage.client, &self.storage.origin)
                    .await?;
            if required > available {
                return Err(Error::bad("Workspace exceeds node disk budget."));
            }
        }

        let target = target.to_owned();
        let replacement = self
            .replacement
            .as_ref()
            .map(std::fs::File::try_clone)
            .transpose()?;
        // Keep the gate with the actual metadata mutation if its caller cancels.
        // Logical lengths are charged even for sparse files, before any body IO.
        let file = tokio::task::spawn_blocking(move || -> Result<std::fs::File> {
            let _owner = owner;
            let file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(mode)
                .custom_flags(libc::O_NOFOLLOW)
                .open(target)?;
            file.set_len(size)?;
            if let Some(replacement) = replacement {
                let promised = replacement
                    .metadata()?
                    .len()
                    .checked_add(size)
                    .ok_or_else(|| Error::bad("Workspace exceeds node disk budget."))?;
                replacement.set_len(promised)?;
            }
            Ok(file)
        })
        .await
        .map_err(Error::internal)??;
        Ok(tokio::fs::File::from_std(file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn storage(root: &Path) -> Storage {
        Storage {
            data: root.join("data"),
            state: root.join("state"),
            client: reqwest::Client::new(),
            origin: "http://localhost".into(),
        }
    }

    #[tokio::test]
    #[ignore = "owned interrupted receiver child of killed_reception_recovers_only_abandoned_promises"]
    async fn interrupted_receiving_child() {
        let root = PathBuf::from(std::env::var_os("CAIRN_TEST_STAGING_CHILD").unwrap());
        let storage = storage(&root);
        let receiving = Directory::new(&storage).await.unwrap();
        let file = std::fs::File::create(receiving.container().join("promise")).unwrap();
        file.set_len(8 * super::super::disk::MIB).unwrap();
        file.sync_all().unwrap();
        std::fs::write(
            root.join("ready"),
            receiving.container().as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        std::future::pending::<()>().await;
    }

    #[tokio::test]
    async fn killed_reception_recovers_only_abandoned_promises_and_preserves_live_or_unrelated_trees()
     {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());
        crate::skills::private_dir(&storage.data).await.unwrap();
        crate::skills::private_dir(&storage.state).await.unwrap();
        let live = Directory::new(&storage).await.unwrap();
        std::fs::File::create(live.container().join("promise"))
            .unwrap()
            .set_len(4 * super::super::disk::MIB)
            .unwrap();
        let unknown = storage.data.join(ROOT).join(format!("{PREFIX}operator"));
        std::fs::create_dir(&unknown).unwrap();
        std::fs::write(unknown.join("owner"), b"unrelated").unwrap();
        let outside = root.path().join("operator-backup");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"preserve").unwrap();
        let linked = storage.data.join(ROOT).join(format!("{PREFIX}link"));
        std::os::unix::fs::symlink(&outside, &linked).unwrap();
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native::staging::tests::interrupted_receiving_child",
                "--ignored",
            ])
            .env("CAIRN_TEST_STAGING_CHILD", root.path())
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !root.path().join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let interrupted =
            PathBuf::from(std::fs::read_to_string(root.path().join("ready")).unwrap());
        recover(&storage.data, &storage.state).await.unwrap();
        assert!(
            interrupted.exists(),
            "a live receiver's reservation must remain charged"
        );
        assert!(
            super::super::disk::used(&storage.data, &storage.state)
                .await
                .unwrap()
                >= 12 * super::super::disk::MIB
        );
        child.kill().await.unwrap();
        let status = child.wait().await.unwrap();
        assert!(!status.success());
        recover(&storage.data, &storage.state).await.unwrap();
        assert!(!interrupted.exists());
        assert!(live.container().exists());
        assert!(
            super::super::disk::used(&storage.data, &storage.state)
                .await
                .unwrap()
                < 5 * super::super::disk::MIB
        );
        assert_eq!(std::fs::read(unknown.join("owner")).unwrap(), b"unrelated");
        assert!(
            std::fs::symlink_metadata(linked)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"preserve");
    }
}
