//! Admission promises for direct VM images. While a VMM can write, its whole
//! image capacity is charged, including holes released by guest discard.
use crate::{
    error::{Error, Result},
    microvm::budget,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const RECORD: &str = "image-reservation.json";
const EPOCH: &str = "image-reservation-epoch.json";
const MARGIN: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Record {
    nonce: String,
    size: u64,
    device: u64,
    inode: u64,
    filesystem: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Epoch {
    capacity: u64,
    device: u64,
    inode: u64,
    filesystem: Vec<u8>,
}

fn read(directory: &Path) -> io::Result<Option<Record>> {
    let path = directory.join(RECORD);
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() || file.metadata()?.len() > 4096 {
        return Err(io::Error::other("Invalid VM capacity reservation"));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let record: Record = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir()
        || record.size == 0
        || metadata.ino() != record.inode
        || super::policy::filesystem_id(&File::open(directory)?)? != record.filesystem
    {
        return Err(io::Error::other(
            "VM reservation directory identity changed",
        ));
    }
    Ok(Some(record))
}

fn raw_bytes(directory: &Path) -> io::Result<u64> {
    let name = if !directory.join("image.json").exists() && directory.join("image.partial").exists()
    {
        "image.partial"
    } else {
        "data.ext4"
    };
    match fs::symlink_metadata(directory.join(name)) {
        Ok(metadata) if metadata.is_file() => Ok(metadata.blocks().saturating_mul(512)),
        Ok(_) => Err(io::Error::other("VM image must not be an alias")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

pub(crate) fn accounted_root(root: &Path) -> io::Result<u64> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut total = 0_u64;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let directory = entry.path();
        let actual = budget::allocated(&directory)?;
        let unused = read(&directory)?
            .map(|record| raw_bytes(&directory).map(|raw| record.size.saturating_sub(raw)))
            .transpose()?
            .unwrap_or(0);
        total = total.saturating_add(actual).saturating_add(unused);
    }
    Ok(total)
}

fn records(state: &Path) -> io::Result<Vec<PathBuf>> {
    let mut records = Vec::new();
    for root in super::environment::ROOTS {
        let entries = match fs::read_dir(state.join(root)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() && read(&entry.path())?.is_some() {
                records.push(entry.path());
            }
        }
    }
    Ok(records)
}

fn persist(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary = path
        .parent()
        .unwrap()
        .join(format!(".{}.tmp", crate::config::id()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(path.parent().unwrap())?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[derive(Clone)]
pub struct Reservation {
    state: PathBuf,
    directory: PathBuf,
    nonce: String,
}

impl Reservation {
    /// Only after preparation has settled without a VMM, or its actual VMM has
    /// exited and image flush has settled. Drop never abandons an uncertain owner.
    pub async fn release(&self) -> Result<()> {
        let owned = self.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let _gate = crate::file_lock::exclusive(
                &owned.state.join("image-reservations.lock"),
                "VM disk admission is busy.",
            )?;
            if let Some(record) = read(&owned.directory)? {
                if record.nonce != owned.nonce {
                    return Err(Error::conflict(
                        "A replacement VM owns this capacity reservation.",
                    ));
                }
                fs::remove_file(owned.directory.join(RECORD))?;
                File::open(&owned.directory)?.sync_all()?;
            }
            if records(&owned.state)?.is_empty() {
                match fs::remove_file(owned.state.join(EPOCH)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                File::open(&owned.state)?.sync_all()?;
            }
            budget::invalidate_disk_usage(&owned.state)?;
            Ok(())
        })
        .await
        .map_err(Error::internal)?
    }
}

pub async fn reserve(
    state: &Path,
    directory: &Path,
    size: u64,
    policy: &super::policy::Policy,
) -> Result<Reservation> {
    crate::skills::private_dir(directory).await?;
    let (state, directory, policy) = (state.to_owned(), directory.to_owned(), policy.clone());
    tokio::task::spawn_blocking(move || -> Result<_> {
        let _gate = crate::file_lock::exclusive(
            &state.join("image-reservations.lock"),
            "VM disk admission is busy.",
        )?;
        // Legacy source writers use this same admission gate. Snapshot the host
        // envelope only at an epoch boundary with no direct VMM writers alive.
        let _legacy = super::cache::admission()?;
        if read(&directory)?.is_some() {
            return Err(Error::conflict(
                "Previous VM capacity ownership is not retired.",
            ));
        }
        budget::invalidate_disk_usage(&state)?;
        let charged = budget::committed_disk_bytes(&state)?;
        let metadata = fs::metadata(&state)?;
        let epoch = if records(&state)?.is_empty() {
            let (total, free) = super::policy::physical_space(&state)?;
            Epoch {
                capacity: charged.saturating_add(free.saturating_sub(policy.reserve(total))),
                device: metadata.dev(),
                inode: metadata.ino(),
                filesystem: super::policy::filesystem_id(&File::open(&state)?)?,
            }
        } else {
            serde_json::from_slice::<Epoch>(&fs::read(state.join(EPOCH))?)?
        };
        if epoch.inode != metadata.ino()
            || epoch.filesystem != super::policy::filesystem_id(&File::open(&state)?)?
        {
            return Err(Error::conflict("VM disk admission filesystem changed."));
        }
        let configured = match fs::read(state.join(budget::FILE)) {
            Ok(bytes) => serde_json::from_slice::<budget::Budget>(&bytes)?
                .limits
                .disk_mi_b
                .saturating_mul(1_048_576),
            Err(error) if error.kind() == io::ErrorKind::NotFound => u64::MAX,
            Err(error) => return Err(error.into()),
        };
        let added = size
            .saturating_sub(raw_bytes(&directory)?)
            .saturating_add(MARGIN);
        if charged.saturating_add(added) > configured.min(epoch.capacity) {
            return Err(Error::new(
                507,
                "Shared disk capacity and its reserve cannot promise this VM image allowance.",
            ));
        }
        persist(&state.join(EPOCH), &epoch)?;
        let metadata = fs::metadata(&directory)?;
        let nonce = crate::config::id();
        persist(
            &directory.join(RECORD),
            &Record {
                nonce: nonce.clone(),
                size,
                device: metadata.dev(),
                inode: metadata.ino(),
                filesystem: super::policy::filesystem_id(&File::open(&directory)?)?,
            },
        )?;
        budget::invalidate_disk_usage(&state)?;
        Ok(Reservation {
            state,
            directory,
            nonce,
        })
    })
    .await
    .map_err(Error::internal)?
}

/// The exclusive controller/PID-namespace fence has completed before Pool::new.
pub(crate) async fn recover(state: &Path) -> Result<()> {
    let state = state.to_owned();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let _gate = crate::file_lock::exclusive(
            &state.join("image-reservations.lock"),
            "VM disk admission is busy.",
        )?;
        for directory in records(&state)? {
            fs::remove_file(directory.join(RECORD))?;
            File::open(directory)?.sync_all()?;
        }
        match fs::remove_file(state.join(EPOCH)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        budget::invalidate_disk_usage(&state)?;
        File::open(state)?.sync_all()?;
        Ok(())
    })
    .await
    .map_err(Error::internal)?
}

pub(crate) fn epoch_capacity(state: &Path) -> io::Result<u64> {
    let epoch: Epoch = match fs::read(state.join(EPOCH)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(u64::MAX),
        Err(error) => return Err(error),
    };
    let metadata = fs::metadata(state)?;
    if epoch.inode != metadata.ino()
        || epoch.filesystem != super::policy::filesystem_id(&File::open(state)?)?
    {
        return Err(io::Error::other("VM reservation filesystem changed"));
    }
    Ok(epoch.capacity)
}

pub(crate) fn reserved(directory: &Path) -> io::Result<bool> {
    Ok(read(directory)?.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    fn fixture() -> (
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        super::super::policy::Policy,
    ) {
        let state = tempfile::tempdir().unwrap();
        let first = state.path().join("disks").join(crate::config::id());
        let second = state.path().join("disks").join(crate::config::id());
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let budget = budget::Budget {
            slots: 2,
            limits: crate::nodes::Resources {
                cpu: 2,
                memory_mi_b: 1024,
                disk_mi_b: 16,
            },
        };
        fs::write(
            state.path().join(budget::FILE),
            serde_json::to_vec(&budget).unwrap(),
        )
        .unwrap();
        let policy = super::super::policy::Policy {
            reserve_mi_b: 64,
            reserve_percent: 1,
            ..Default::default()
        };
        (state, first, second, policy)
    }

    #[tokio::test]
    async fn sparse_writes_and_discards_cannot_lend_an_active_vms_promised_space() {
        let (state, first, second, policy) = fixture();
        let original = reserve(state.path(), &first, 8 * MARGIN, &policy)
            .await
            .unwrap();
        assert_eq!(
            reserve(state.path(), &second, 8 * MARGIN, &policy)
                .await
                .err()
                .unwrap()
                .status,
            507
        );
        let image = first.join("data.ext4");
        let mut file = File::create(&image).unwrap();
        file.write_all(&vec![7; 6 * MARGIN as usize]).unwrap();
        file.set_len(8 * MARGIN).unwrap();
        file.sync_all().unwrap();
        budget::invalidate_disk_usage(state.path()).unwrap();
        assert!(budget::committed_disk_bytes(state.path()).unwrap() >= 8 * MARGIN);
        assert_eq!(
            unsafe {
                libc::fallocate(
                    file.as_raw_fd(),
                    libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
                    0,
                    8 * MARGIN as i64,
                )
            },
            0
        );
        file.sync_all().unwrap();
        budget::invalidate_disk_usage(state.path()).unwrap();
        assert!(budget::disk_bytes(state.path()).unwrap() < MARGIN);
        assert!(budget::committed_disk_bytes(state.path()).unwrap() >= 8 * MARGIN);
        assert_eq!(
            reserve(state.path(), &second, 8 * MARGIN, &policy)
                .await
                .err()
                .unwrap()
                .status,
            507
        );
        drop(file);
        original.release().await.unwrap();
        let replacement = reserve(state.path(), &first, 8 * MARGIN, &policy)
            .await
            .unwrap();
        assert_eq!(original.release().await.unwrap_err().status, 409);
        assert!(reserved(&first).unwrap());
        replacement.release().await.unwrap();
        let other = reserve(state.path(), &second, 8 * MARGIN, &policy)
            .await
            .unwrap();
        other.release().await.unwrap();
        assert!(!state.path().join(EPOCH).exists());
    }

    #[tokio::test]
    async fn controller_fence_recovery_releases_promises_and_preserves_images() {
        let (state, first, _, policy) = fixture();
        let reservation = reserve(state.path(), &first, 8 * MARGIN, &policy)
            .await
            .unwrap();
        fs::write(first.join("data.ext4"), b"retained work").unwrap();
        drop(reservation);
        assert!(reserved(&first).unwrap());
        recover(state.path()).await.unwrap();
        recover(state.path()).await.unwrap();
        assert!(!reserved(&first).unwrap());
        assert_eq!(fs::read(first.join("data.ext4")).unwrap(), b"retained work");
    }
}
