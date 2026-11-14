//! Local images handed directly to the VMM. Legacy journals are read only for conversion.
use super::{Disk, LocalDisk};
use crate::{
    error::{Error, Result},
    skills::{atomic_write, private_dir},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

const FORMAT: &str = "local-ext4-v1";
const RECORD: &str = "image.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Record {
    format: String,
    size: u64,
    context: Value,
}

pub fn exists(directory: &Path) -> bool {
    directory.join(RECORD).is_file()
}

pub async fn path_for_boot(directory: &Path, size: u64) -> Result<PathBuf> {
    let record = read_record(directory).await?;
    let image = directory.join("data.ext4");
    let metadata = tokio::fs::symlink_metadata(&image).await?;
    if !metadata.is_file() || metadata.len() != size || record.size != size {
        return Err(Error::conflict(
            "VM image geometry does not match its reservation.",
        ));
    }
    Ok(image)
}

pub async fn sync(image: &Path) -> Result<()> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .open(image)
        .await?
        .sync_all()
        .await?;
    Ok(())
}

pub async fn unassigned(directory: &Path) -> Result<bool> {
    Ok(read_record(directory).await?.context["unassigned"] == true)
}

pub async fn authorize(owner: &super::environment::OwnerLease, context: &Value) -> Result<()> {
    let mut record = read_record(&owner.directory).await?;
    let requested = super::assignment::Assignment::new(context)?.assigned()?;
    if record.context["unassigned"] != true {
        let current = super::assignment::Assignment::new(&record.context)?.assigned()?;
        if current != requested {
            return Err(Error::conflict(
                "Local image belongs to another environment.",
            ));
        }
        return Ok(());
    }
    if owner
        .directory
        .parent()
        .and_then(Path::file_name)
        .is_none_or(|name| name != "environments")
    {
        return Err(Error::conflict(
            "Only a claimed prepared image can be assigned.",
        ));
    }
    super::policy::for_node(&context["policy"])?;
    record.context = context.clone();
    persist(&owner.directory, &record).await
}

pub async fn status(directory: &Path) -> Result<Value> {
    let record = read_record(directory).await?;
    let bytes = crate::microvm::budget::allocated(directory)?;
    Ok(serde_json::json!({
        "mode": "on-demand",
        "storage": FORMAT,
        "availability": "available",
        "size": record.size,
        "localBytes": bytes,
        "activeLocalBytes": bytes,
        "paused": false
    }))
}

/// Caller holds the environment's logical and physical locks and has fenced its consumers.
/// A committed record, rather than the presence of an exported file, is the format switch.
pub async fn prepare(
    directory: &Path,
    size: u64,
    context: &Value,
    stop: &CancellationToken,
) -> Result<PathBuf> {
    private_dir(directory).await?;
    let _physical =
        crate::file_lock::exclusive(&directory.join("lock"), super::bootstrap::DISK_BUSY)?;
    prepare_locked(directory, size, context, stop).await
}

pub(crate) async fn prepare_locked(
    directory: &Path,
    size: u64,
    context: &Value,
    stop: &CancellationToken,
) -> Result<PathBuf> {
    if stop.is_cancelled() {
        return Err(Error::unavailable("Local image preparation stopped."));
    }
    let image = directory.join("data.ext4");
    let mut record = if exists(directory) {
        read_record(directory).await?
    } else if super::runtime::exists(directory) {
        migrate_journal(directory, stop).await?
    } else {
        let pending = directory.join("image.pending.json");
        let record = match tokio::fs::read(&pending).await {
            Ok(bytes) => {
                let record: Record = serde_json::from_slice(&bytes)?;
                if record.format != FORMAT || record.size == 0 || record.context != *context {
                    return Err(Error::conflict(
                        "Local image initialization intent does not match this environment.",
                    ));
                }
                record
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if image.exists() {
                    return Err(Error::conflict(
                        "Unrecognized local image; recover it before conversion.",
                    ));
                }
                let record = Record {
                    format: FORMAT.into(),
                    size,
                    context: context.clone(),
                };
                atomic_write(&pending, &serde_json::to_vec(&record)?).await?;
                tokio::fs::File::open(directory).await?.sync_all().await?;
                record
            }
            Err(error) => return Err(error.into()),
        };
        // No writer has been admitted before image.json. This owned intent
        // permits rebuilding an incomplete mkfs, including after its rename.
        if !image.exists() || check(&image).await.is_err() {
            let policy = super::policy::for_node(&context["policy"])?;
            let (total, free) = super::policy::space(directory)?;
            if free <= policy.reserve(total).saturating_add(64 * 1024 * 1024) {
                return Err(Error::new(
                    507,
                    "Free disk reserve prevents preparing a local image.",
                ));
            }
            let temporary = directory.join("image.partial");
            let file = tokio::fs::File::create(&temporary).await?;
            file.set_len(record.size).await?;
            drop(file);
            crate::microvm::host::command("mkfs.ext4", &["-q", "-F", path(&temporary)?]).await?;
            tokio::fs::File::open(&temporary).await?.sync_all().await?;
            tokio::fs::rename(&temporary, &image).await?;
        }
        record
    };
    let actual = tokio::fs::symlink_metadata(&image).await?;
    if !actual.is_file() || actual.len() < record.size || size < record.size || actual.len() > size
    {
        return Err(Error::conflict(
            "Local image geometry cannot shrink or exceed its reservation.",
        ));
    }
    check(&image).await?;
    if size > actual.len() {
        tokio::fs::OpenOptions::new()
            .write(true)
            .open(&image)
            .await?
            .set_len(size)
            .await?;
    }
    if size > record.size || actual.len() != record.size {
        crate::microvm::host::command("resize2fs", &[path(&image)?]).await?;
        record.size = size;
    }
    persist(directory, &record).await?;
    cleanup_legacy(directory).await?;
    match tokio::fs::remove_file(directory.join("image.pending.json")).await {
        Ok(()) => tokio::fs::File::open(directory).await?.sync_all().await?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(image)
}

async fn read_record(directory: &Path) -> Result<Record> {
    let record: Record = serde_json::from_slice(&tokio::fs::read(directory.join(RECORD)).await?)?;
    if record.format != FORMAT || record.size == 0 {
        return Err(Error::conflict("Unsupported local image format."));
    }
    Ok(record)
}

async fn persist(directory: &Path, record: &Record) -> Result<()> {
    tokio::fs::File::open(directory.join("data.ext4"))
        .await?
        .sync_all()
        .await?;
    atomic_write(&directory.join(RECORD), &serde_json::to_vec(record)?).await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    Ok(())
}

async fn migrate_journal(directory: &Path, stop: &CancellationToken) -> Result<Record> {
    let _replacement = super::runtime::replacement(directory).await?;
    let root = directory.join("lazy");
    let context = super::LazyDisk::context(&root)?;
    let temporary = directory.join("image.partial");
    match tokio::fs::remove_file(&temporary).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let output = temporary.clone();
    let stop = stop.clone();
    let size = tokio::task::spawn_blocking(move || -> Result<u64> {
        let source = super::LazyDisk::open(&root)?;
        super::export(&source, &output)?;
        let target = LocalDisk::open(&output, false)?;
        // Verify every logical byte, including zero ranges and the final tail.
        // This also forces legacy block checksums before changing the format.
        let mut original = vec![0; 4 * 1024 * 1024];
        let mut copied = vec![0; original.len()];
        let mut offset = 0;
        while offset < source.size() {
            if stop.is_cancelled() {
                return Err(Error::unavailable(
                    "Local image conversion stopped; journal retained.",
                ));
            }
            let length = (source.size() - offset).min(original.len() as u64) as usize;
            source.read_at(offset, &mut original[..length])?;
            target.read_at(offset, &mut copied[..length])?;
            if original[..length] != copied[..length] {
                return Err(Error::conflict(
                    "Local image verification failed; journal retained.",
                ));
            }
            offset += length as u64;
        }
        Ok(source.size())
    })
    .await
    .map_err(Error::internal)??;
    check(&temporary).await?;
    tokio::fs::rename(&temporary, directory.join("data.ext4")).await?;
    Ok(Record {
        format: FORMAT.into(),
        size,
        context,
    })
}

async fn cleanup_legacy(directory: &Path) -> Result<()> {
    if directory.join("lazy").exists() {
        let _replacement = super::runtime::replacement(directory).await?;
        tokio::fs::remove_dir_all(directory.join("lazy")).await?;
        tokio::fs::File::open(directory).await?.sync_all().await?;
    }
    Ok(())
}

fn path(value: &Path) -> Result<&str> {
    value
        .to_str()
        .ok_or_else(|| Error::bad("Invalid local image path."))
}

async fn check(image: &Path) -> Result<()> {
    let result = tokio::process::Command::new("e2fsck")
        .args(["-p", "-f"])
        .arg(image)
        .kill_on_drop(true)
        .output()
        .await?;
    if !matches!(result.status.code(), Some(0 | 1)) {
        return Err(Error::conflict(
            "Local image filesystem check failed; source retained.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SIZE: u64 = 128 * 1024 * 1024;

    fn context() -> Value {
        json!({ "owner": "local-image-test", "policy": { "reserveMiB": 64, "reservePercent": 1 } })
    }

    #[tokio::test]
    async fn stopped_image_reopens_with_retained_files_and_no_journal() {
        let root = tempfile::tempdir().unwrap();
        let stop = CancellationToken::new();
        let image = prepare(root.path(), SIZE, &context(), &stop).await.unwrap();
        let source = root.path().join("payload");
        tokio::fs::write(&source, b"retained work").await.unwrap();
        crate::microvm::host::command(
            "debugfs",
            &[
                "-w",
                "-R",
                &format!("write {} /retained", source.display()),
                path(&image).unwrap(),
            ],
        )
        .await
        .unwrap();
        let resumed = prepare(root.path(), SIZE, &context(), &stop).await.unwrap();
        let result = tokio::process::Command::new("debugfs")
            .args(["-R", "cat /retained"])
            .arg(&resumed)
            .output()
            .await
            .unwrap();
        assert_eq!(result.stdout, b"retained work");
        assert_eq!(path_for_boot(root.path(), SIZE).await.unwrap(), image);
        assert!(!root.path().join("lazy").exists());
        assert!(
            prepare(root.path(), SIZE / 2, &context(), &stop)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn restart_finishes_initialization_after_raw_image_rename() {
        let root = tempfile::tempdir().unwrap();
        let context = context();
        let record = Record {
            format: FORMAT.into(),
            size: SIZE,
            context: context.clone(),
        };
        atomic_write(
            &root.path().join("image.pending.json"),
            &serde_json::to_vec(&record).unwrap(),
        )
        .await
        .unwrap();
        super::super::local::prepare(root.path(), SIZE)
            .await
            .unwrap();
        prepare(root.path(), SIZE, &context, &CancellationToken::new())
            .await
            .unwrap();
        assert!(exists(root.path()));
        assert!(!root.path().join("image.pending.json").exists());
    }

    #[tokio::test]
    async fn journal_conversion_applies_growth_and_retries_committed_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let context = context();
        let stop = CancellationToken::new();
        super::super::bootstrap::prepare(root.path(), SIZE, &context, &stop)
            .await
            .unwrap();
        let volume = super::super::runtime::load(root.path()).await.unwrap();
        volume
            .write_at(96 * 1024 * 1024, b"retained-byte-canary")
            .unwrap();
        volume.sync().unwrap();
        drop(volume);
        let image = prepare(root.path(), SIZE * 2, &context, &stop)
            .await
            .unwrap();
        assert_eq!(path_for_boot(root.path(), SIZE * 2).await.unwrap(), image);
        let disk = LocalDisk::open(&image, false).unwrap();
        let mut bytes = [0; 20];
        disk.read_at(96 * 1024 * 1024, &mut bytes).unwrap();
        assert_eq!(&bytes, b"retained-byte-canary");
        // A crash midway through deleting the old format may leave an invalid
        // journal. The committed image remains authoritative on restart.
        tokio::fs::create_dir(root.path().join("lazy"))
            .await
            .unwrap();
        tokio::fs::write(root.path().join("lazy/obsolete-payload"), b"old")
            .await
            .unwrap();
        prepare(root.path(), SIZE * 2, &context, &stop)
            .await
            .unwrap();
        assert!(!root.path().join("lazy").exists());
        disk.read_at(96 * 1024 * 1024, &mut bytes).unwrap();
        assert_eq!(&bytes, b"retained-byte-canary");
    }

    #[tokio::test]
    async fn unrecognized_work_and_owned_images_are_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("data.ext4");
        tokio::fs::write(&image, b"unknown retained work")
            .await
            .unwrap();
        assert!(
            prepare(root.path(), SIZE, &context(), &CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(
            tokio::fs::read(&image).await.unwrap(),
            b"unknown retained work"
        );
        let lock = crate::file_lock::exclusive(&root.path().join("lock"), "busy").unwrap();
        assert_eq!(
            prepare(root.path(), SIZE, &context(), &CancellationToken::new())
                .await
                .unwrap_err()
                .status,
            409
        );
        drop(lock);
    }
}
