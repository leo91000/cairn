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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    replacement: Option<Replacement>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Replacement {
    original: Option<RawIdentity>,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawIdentity {
    device: u64,
    inode: u64,
}

impl RawIdentity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

impl Replacement {
    fn detect(directory: &Path) -> io::Result<Option<Self>> {
        if super::image::exists(directory) {
            return Ok(None);
        }
        let mut legacy = false;
        for name in ["lazy", "resize-source"] {
            match fs::symlink_metadata(directory.join(name)) {
                Ok(_) => legacy = true,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        if !legacy {
            return Ok(None);
        }
        let original = raw_metadata(&directory.join("data.ext4"))?
            .as_ref()
            .map(RawIdentity::of);
        Ok(Some(Self { original }))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Epoch {
    capacity: u64,
    // Older in-development epochs cannot safely reveal their original reserve.
    #[serde(default)]
    reserve: Option<u64>,
    device: u64,
    inode: u64,
    filesystem: Vec<u8>,
}

impl Epoch {
    fn capacity_for(&self, reserve: u64) -> io::Result<u64> {
        let original = self.reserve.ok_or_else(|| {
            io::Error::other(
                "VM reservation epoch predates reserve accounting; retire its owners first",
            )
        })?;
        Ok(self
            .capacity
            .saturating_add(original)
            .saturating_sub(reserve))
    }
}

fn current_policy(
    state: &Path,
    fallback: &super::policy::Policy,
) -> io::Result<super::policy::Policy> {
    match fs::read(state.join("storage-policy.json")) {
        Ok(bytes) => super::policy::from_persisted(&bytes).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(fallback.clone()),
        Err(error) => Err(error),
    }
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

fn raw_metadata(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata)),
        Ok(_) => Err(io::Error::other("VM image must not be an alias")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn raw_bytes(directory: &Path, replacement: Option<&Replacement>) -> io::Result<u64> {
    if let Some(replacement) = replacement {
        // Conversion exports a separate file. The old raw resize output stays
        // charged until replacement, never earning credit for the new promise.
        // Its snapshot also lets accounting recognize the exported inode after
        // image.partial is renamed, before image.json becomes durable.
        for name in ["image.partial", "data.ext4"] {
            if let Some(metadata) = raw_metadata(&directory.join(name))?
                && replacement.original.as_ref() != Some(&RawIdentity::of(&metadata))
            {
                return Ok(metadata.blocks().saturating_mul(512));
            }
        }
        return Ok(0);
    }
    let name = if !directory.join("image.json").exists() && directory.join("image.partial").exists()
    {
        "image.partial"
    } else {
        "data.ext4"
    };
    Ok(raw_metadata(&directory.join(name))?
        .map_or(0, |metadata| metadata.blocks().saturating_mul(512)))
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
            .map(|record| {
                raw_bytes(&directory, record.replacement.as_ref())
                    .map(|raw| record.size.saturating_sub(raw))
            })
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

#[cfg(test)]
pub async fn reserve(
    state: &Path,
    directory: &Path,
    size: u64,
    policy: &super::policy::Policy,
) -> Result<Reservation> {
    reserve_cancellable(
        state,
        directory,
        size,
        policy,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
}

pub(crate) async fn reserve_cancellable(
    state: &Path,
    directory: &Path,
    size: u64,
    policy: &super::policy::Policy,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<Reservation> {
    crate::skills::private_dir(directory).await?;
    let gate = crate::file_lock::exclusive_after_release(
        &state.join("image-reservations.lock"),
        "VM disk capacity admission remained busy; retry the request.",
        stop,
        std::time::Duration::from_secs(30),
    )
    .await?;
    let (state, directory, policy) = (state.to_owned(), directory.to_owned(), policy.clone());
    tokio::task::spawn_blocking(move || -> Result<_> {
        let _gate = gate;
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
        let policy = current_policy(&state, &policy)?;
        let (total, free) = super::policy::physical_space(&state)?;
        let reserve = policy.reserve(total);
        let epoch = if records(&state)?.is_empty() {
            Epoch {
                capacity: charged.saturating_add(free.saturating_sub(reserve)),
                reserve: Some(reserve),
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
        let replacement = Replacement::detect(&directory)?;
        let added = size
            .saturating_sub(raw_bytes(&directory, replacement.as_ref())?)
            .saturating_add(MARGIN);
        if charged.saturating_add(added) > configured.min(epoch.capacity_for(reserve)?) {
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
                replacement,
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
    let reserve = match fs::read(state.join("storage-policy.json")) {
        Ok(bytes) => {
            let policy = super::policy::from_persisted(&bytes).map_err(io::Error::other)?;
            let (total, _) = super::policy::physical_space(state)?;
            policy.reserve(total)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            epoch.reserve.ok_or_else(|| {
                io::Error::other(
                    "VM reservation epoch predates reserve accounting; retire its owners first",
                )
            })?
        }
        Err(error) => return Err(error),
    };
    epoch.capacity_for(reserve)
}

pub(crate) fn reserved(directory: &Path) -> io::Result<bool> {
    Ok(read(directory)?.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Disk;
    use std::os::fd::AsRawFd;

    #[tokio::test]
    async fn existing_epoch_applies_current_reserve_to_new_promises() {
        let (root, first, second, mut policy) = fixture();
        configure_disk(root.path(), 1_000_000);
        let (total, _) = super::super::policy::physical_space(root.path()).unwrap();
        let save = |policy: &super::super::policy::Policy| {
            fs::write(
                root.path().join("storage-policy.json"),
                serde_json::to_vec(policy).unwrap(),
            )
            .unwrap();
        };
        save(&policy);
        let first_owner = reserve(root.path(), &first, 8 * MARGIN, &policy)
            .await
            .unwrap();
        // Fix this epoch's envelope, rather than racing unrelated host writes
        // between the test's free-space sample and the admission's sample.
        let mut epoch: Epoch =
            serde_json::from_slice(&fs::read(root.path().join(EPOCH)).unwrap()).unwrap();
        epoch.capacity = 32 * MARGIN;
        persist(&root.path().join(EPOCH), &epoch).unwrap();
        let original_capacity = epoch_capacity(root.path()).unwrap();
        let original_reserve = policy.reserve(total);
        // A stale caller must still use the newly persisted node policy.
        let old_policy = policy.clone();
        policy.reserve_mi_b = original_reserve.div_ceil(MARGIN) + 24;
        save(&policy);
        assert_eq!(
            epoch_capacity(root.path()).unwrap(),
            original_capacity.saturating_sub(policy.reserve(total) - original_reserve)
        );
        assert_eq!(
            reserve(root.path(), &second, 8 * MARGIN, &old_policy)
                .await
                .err()
                .unwrap()
                .status,
            507
        );
        assert!(!second.join(RECORD).exists());
        assert!(reserved(&first).unwrap());
        save(&old_policy);
        assert_eq!(epoch_capacity(root.path()).unwrap(), original_capacity);
        let second_owner = reserve(root.path(), &second, 8 * MARGIN, &policy)
            .await
            .unwrap();
        second_owner.release().await.unwrap();
        first_owner.release().await.unwrap();
    }

    #[test]
    fn legacy_epoch_without_reserve_is_refused_without_guessing_capacity() {
        let epoch = Epoch {
            capacity: 1024,
            reserve: None,
            device: 1,
            inode: 1,
            filesystem: vec![],
        };
        assert!(epoch.capacity_for(64).is_err());
    }

    #[tokio::test]
    async fn admission_waits_for_an_inflight_capacity_change_and_preserves_owner_conflicts() {
        let (root, first, second, policy) = fixture();
        let old = reserve(root.path(), &first, 4 * 1_048_576, &policy)
            .await
            .unwrap();
        let gate =
            crate::file_lock::exclusive(&root.path().join("image-reservations.lock"), "busy")
                .unwrap();
        let stop = tokio_util::sync::CancellationToken::new();
        let mut next = tokio::spawn({
            let state = root.path().to_owned();
            let second = second.clone();
            let policy = policy.clone();
            let stop = stop.clone();
            async move { reserve_cancellable(&state, &second, 4 * 1_048_576, &policy, &stop).await }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut next)
                .await
                .is_err()
        );
        drop(gate);
        let next = next.await.unwrap().unwrap();
        assert_eq!(
            reserve(root.path(), &second, 4 * 1_048_576, &policy)
                .await
                .err()
                .unwrap()
                .status,
            409
        );
        old.release().await.unwrap();
        next.release().await.unwrap();
        let gate =
            crate::file_lock::exclusive(&root.path().join("image-reservations.lock"), "busy")
                .unwrap();
        let cancelled = tokio_util::sync::CancellationToken::new();
        cancelled.cancel();
        assert!(
            reserve_cancellable(root.path(), &second, 4 * 1_048_576, &policy, &cancelled)
                .await
                .is_err()
        );
        assert!(!second.join(RECORD).exists());
        drop(gate);
    }

    fn configure_disk(state: &Path, disk_mib: u64) {
        let record = budget::Budget {
            slots: 2,
            limits: crate::nodes::Resources {
                cpu: 2,
                memory_mi_b: 1024,
                disk_mi_b: disk_mib,
            },
        };
        fs::write(
            state.join(budget::FILE),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        budget::invalidate_disk_usage(state).unwrap();
    }

    #[tokio::test]
    async fn interrupted_legacy_resize_reserves_both_raw_files_before_conversion() {
        const SIZE: u64 = 128 * 1_048_576;
        let (root, first, _, policy) = fixture();
        configure_disk(root.path(), 512);
        let stop = tokio_util::sync::CancellationToken::new();
        let context = serde_json::json!({ "owner": crate::config::id(), "policy": policy });
        super::super::bootstrap::prepare(&first, SIZE, &context, &stop)
            .await
            .unwrap();
        let volume = super::super::runtime::load(&first).await.unwrap();
        let canary = b"retained-resize-work";
        volume.write_at(96 * 1_048_576, canary).unwrap();
        volume.sync().unwrap();
        drop(volume);
        super::super::runtime::materialize(&first, &stop)
            .await
            .unwrap();
        fs::write(first.join("bootstrap.pending"), b"resizing").unwrap();
        let raw = File::options()
            .read(true)
            .write(true)
            .open(first.join("data.ext4"))
            .unwrap();
        assert_eq!(
            unsafe { libc::posix_fallocate(raw.as_raw_fd(), 0, SIZE as libc::off_t) },
            0
        );
        raw.sync_all().unwrap();
        drop(raw);
        assert!(
            raw_metadata(&first.join("data.ext4"))
                .unwrap()
                .unwrap()
                .blocks()
                * 512
                >= SIZE
        );
        configure_disk(root.path(), 192);
        assert_eq!(
            reserve(root.path(), &first, SIZE, &policy)
                .await
                .err()
                .unwrap()
                .status,
            507
        );
        assert!(!first.join(RECORD).exists());
        assert!(first.join("resize-source/journal-v2.sqlite").is_file());
        assert!(first.join("data.ext4").is_file());
        configure_disk(root.path(), 384);
        let promise = reserve(root.path(), &first, SIZE, &policy).await.unwrap();
        assert!(budget::committed_disk_bytes(root.path()).unwrap() >= 2 * SIZE);
        let image = super::super::image::prepare(&first, SIZE, &context, &stop)
            .await
            .unwrap();
        let disk = super::super::LocalDisk::open(&image, false).unwrap();
        let mut bytes = vec![0; canary.len()];
        disk.read_at(96 * 1_048_576, &mut bytes).unwrap();
        assert_eq!(bytes, canary);
        drop(disk);
        budget::invalidate_disk_usage(root.path()).unwrap();
        let charged = budget::committed_disk_bytes(root.path()).unwrap();
        assert!((SIZE..SIZE + MARGIN).contains(&charged));
        assert!(!first.join("resize-source").exists());
        promise.release().await.unwrap();
    }

    #[tokio::test]
    async fn exported_inode_credit_survives_rename_without_crediting_old_resize_output() {
        let (root, first, _, policy) = fixture();
        fs::create_dir(first.join("resize-source")).unwrap();
        let promise = reserve(root.path(), &first, 4 * 1_048_576, &policy)
            .await
            .unwrap();
        let partial = first.join("image.partial");
        let file = File::create(&partial).unwrap();
        assert_eq!(
            unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, 4 * 1_048_576) },
            0
        );
        file.sync_all().unwrap();
        drop(file);
        budget::invalidate_disk_usage(root.path()).unwrap();
        let before = budget::committed_disk_bytes(root.path()).unwrap();
        fs::rename(partial, first.join("data.ext4")).unwrap();
        budget::invalidate_disk_usage(root.path()).unwrap();
        let after = budget::committed_disk_bytes(root.path()).unwrap();
        assert_eq!(before, after);
        promise.release().await.unwrap();
    }

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
