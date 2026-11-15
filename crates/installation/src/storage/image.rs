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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    conversion: Option<ConversionProof>,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConversionProof {
    version: u32,
    filesystem: Vec<u8>,
    inode: u64,
    digest: String,
}

const INVALID_FILESYSTEM: &str = "invalid_local_filesystem";

#[derive(Clone)]
struct Preparation {
    directory: PathBuf,
    context: Value,
    stop: CancellationToken,
}

impl Preparation {
    fn new(directory: &Path, context: &Value, stop: &CancellationToken) -> Self {
        Self {
            directory: directory.to_owned(),
            context: context.clone(),
            stop: stop.clone(),
        }
    }

    fn state(&self) -> Option<&Path> {
        if let Some(parent) = self.directory.parent()
            && super::environment::ROOTS
                .iter()
                .any(|name| parent.file_name().is_some_and(|value| value == *name))
        {
            return parent.parent();
        }
        self.directory
            .ancestors()
            .find(|path| path.join(crate::microvm::budget::FILE).is_file())
    }

    fn check_sync(&self, incoming: u64) -> Result<()> {
        if self.stop.is_cancelled() {
            return Err(Error::unavailable(
                "VM image preparation stopped; source retained.",
            ));
        }
        let state = self.state();
        let policy =
            match std::fs::read(state.unwrap_or(&self.directory).join("storage-policy.json")) {
                Ok(bytes) => super::policy::from_persisted(&bytes)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    super::policy::for_node(&self.context["policy"])?
                }
                Err(error) => return Err(error.into()),
            };
        let pending = super::cache::pending_write_bytes();
        let margin = 1_048_576_u64.saturating_add(incoming);
        let (total, free) = super::policy::physical_space(&self.directory)?;
        if free.saturating_sub(pending) <= policy.reserve(total).saturating_add(margin) {
            return Err(Error::new(
                507,
                "Physical reserve prevents VM image preparation; source retained.",
            ));
        }
        if let Some(state) = state {
            let configured = match std::fs::read(state.join(crate::microvm::budget::FILE)) {
                Ok(bytes) => serde_json::from_slice::<crate::microvm::budget::Budget>(&bytes)?
                    .limits
                    .disk_mi_b
                    .saturating_mul(1_048_576),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => u64::MAX,
                Err(error) => return Err(error.into()),
            };
            // A live reservation already charges all future writable bytes;
            // only unreserved callers need to debit the next export chunk.
            let charged = if super::reservation::reserved(&self.directory)? {
                crate::microvm::budget::committed_disk_bytes(state)?
            } else {
                crate::microvm::budget::disk_bytes(state)?.saturating_add(incoming)
            };
            let limit = configured.min(super::reservation::epoch_capacity(state)?);
            if charged.saturating_add(pending).saturating_add(1_048_576) > limit {
                return Err(Error::new(
                    507,
                    "Shared capacity prevents VM image preparation; source retained.",
                ));
            }
        }
        Ok(())
    }

    async fn check(&self, incoming: u64) -> Result<()> {
        let sample = self.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::task::spawn_blocking(move || sample.check_sync(incoming)),
        )
        .await
        .map_err(|_| {
            Error::unavailable("VM preparation capacity observations are stale; source retained.")
        })?
        .map_err(Error::internal)?
    }

    async fn command(&self, binary: &str, args: &[&str]) -> Result<()> {
        self.check(0).await?;
        // These fixed filesystem utilities do not launch user commands. Keep
        // their exact Child until reaped after cancellation or reserve loss.
        let mut child = tokio::process::Command::new(binary)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(180));
        tokio::pin!(deadline);
        let outcome = loop {
            tokio::select! {
                () = self.stop.cancelled() => {
                    break Err(Error::unavailable("VM image preparation stopped; source retained."));
                },
                () = &mut deadline => {
                    break Err(Error::unavailable("VM filesystem preparation timed out; source retained."));
                },
                result = child.wait() => {
                    break result.map_err(Into::into);
                },
                _ = interval.tick() => {
                    if let Err(error) = self.check(0).await {
                        break Err(error);
                    }
                }
            }
        };
        let status = match outcome {
            Ok(status) => status,
            Err(error) => {
                let _ = child.start_kill();
                loop {
                    match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
                        .await
                    {
                        Ok(result) => {
                            result?;
                            break;
                        }
                        Err(_) => tracing::warn!(
                            binary,
                            "VM preparation writer retirement pending; ownership and source retained"
                        ),
                    }
                }
                return Err(error);
            }
        };
        self.check(0).await?;
        if status.success() || binary == "e2fsck" && status.code() == Some(1) {
            return Ok(());
        }
        tracing::warn!(binary, %status, "VM filesystem preparation failed; source retained");
        if binary == "e2fsck" {
            return Err(Error::coded(
                409,
                INVALID_FILESYSTEM,
                "Local image filesystem check failed; source retained.",
            ));
        }
        Err(Error::unavailable(format!(
            "VM filesystem preparation failed: {binary}; source retained."
        )))
    }
}

pub fn exists(directory: &Path) -> bool {
    directory.join(RECORD).is_file()
}

pub async fn path_for_boot(directory: &Path, size: u64) -> Result<PathBuf> {
    let record = read_record(directory).await?;
    let image = directory.join("data.ext4");
    let metadata = tokio::fs::symlink_metadata(&image).await?;
    if record.conversion.is_some()
        || !metadata.is_file()
        || metadata.len() != size
        || record.size != size
    {
        return Err(Error::conflict(
            "VM image geometry does not match its reservation.",
        ));
    }
    Ok(image)
}

pub(crate) async fn sync_io(image: &Path) -> std::io::Result<()> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .open(image)
        .await?
        .sync_all()
        .await?;
    Ok(())
}

pub async fn sync(image: &Path) -> Result<()> {
    sync_io(image).await.map_err(Into::into)
}

pub async fn unassigned(directory: &Path) -> Result<bool> {
    Ok(read_record(directory).await?.context["unassigned"] == true)
}

pub async fn authorize(owner: &super::environment::OwnerLease, context: &Value) -> Result<()> {
    let mut record = read_record(&owner.directory).await?;
    let requested = super::assignment::Assignment::new(context)?.assigned()?;
    let expected = if owner
        .directory
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "disks")
    {
        owner
            .directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_owned()
    } else {
        tokio::fs::read_to_string(owner.directory.join("owner")).await?
    };
    crate::validation::uuid(&expected)?;
    if context["owner"]
        .as_str()
        .is_some_and(|owner| owner != expected)
    {
        return Err(Error::conflict(
            "Image assignment does not match its conversation namespace.",
        ));
    }
    if record.context["unassigned"] != true
        && record.context.get("owner").is_none()
        && record.context["grant"].is_string()
        && context["owner"] == expected
    {
        // Legacy grants are only historical local identities. The caller's
        // durable namespace lease, not that retired token, establishes ownership.
        record.context = context.clone();
        return persist(&owner.directory, &record).await;
    }
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
        "paused": false,
        "waitingFor": null
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
    if !exists(directory) {
        if let Some(source) = super::bootstrap::resize_source(directory)?
            && size < super::LazyDisk::geometry(&source)?
        {
            return Err(Error::conflict(
                "A retained legacy image cannot be shrunk during conversion.",
            ));
        }
        super::bootstrap::recover_resize_source(directory, context, "vm").await?;
    }
    let image = directory.join("data.ext4");
    let preparation = Preparation::new(directory, context, stop);
    let mut record = if exists(directory) {
        read_record(directory).await?
    } else if super::runtime::exists(directory) {
        migrate_journal(directory, size, context, stop).await?
    } else {
        let pending = directory.join("image.pending.json");
        require_fresh(directory, pending.exists())?;
        let record = match tokio::fs::read(&pending).await {
            Ok(bytes) => {
                let record: Record = serde_json::from_slice(&bytes)?;
                let owner = |context: &Value| -> Result<Option<String>> {
                    if context["unassigned"] == true {
                        Ok(None)
                    } else {
                        super::assignment::Assignment::new(context)?.assigned()
                    }
                };
                if record.format != FORMAT
                    || record.size == 0
                    || record.size > size
                    || owner(&record.context)? != owner(context)?
                {
                    return Err(Error::conflict(
                        "Local image initialization intent does not match this environment.",
                    ));
                }
                Record {
                    context: context.clone(),
                    ..record
                }
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
                    conversion: None,
                };
                atomic_write(&pending, &serde_json::to_vec(&record)?).await?;
                tokio::fs::File::open(directory).await?.sync_all().await?;
                record
            }
            Err(error) => return Err(error.into()),
        };
        // No writer has been admitted before image.json. This owned intent
        // permits rebuilding an incomplete mkfs, including after its rename.
        let temporary = directory.join("image.partial");
        let image_present = regular_image_present(&image).await?;
        let temporary_present = regular_image_present(&temporary).await?;
        let rebuild = if image_present {
            match check(&image, &preparation).await {
                Ok(()) => false,
                Err(error) if error.code.as_deref() == Some(INVALID_FILESYSTEM) => true,
                Err(error) => return Err(error),
            }
        } else {
            true
        };
        if rebuild {
            preparation.check(0).await?;
            let policy = super::policy::for_node(&context["policy"])?;
            let (total, free) = if super::reservation::reserved(directory)? {
                super::policy::physical_space(directory)?
            } else {
                super::policy::space(directory)?
            };
            if free <= policy.reserve(total).saturating_add(64 * 1024 * 1024) {
                return Err(Error::new(
                    507,
                    "Free disk reserve prevents preparing a local image.",
                ));
            }
            // This is a positively owned, never-booted initialization. Retire
            // failed mkfs files before allocating their replacement: admission
            // already credits those reclaimable bytes toward the one image.
            if image_present {
                tokio::fs::remove_file(&image).await?;
            }
            if temporary_present {
                tokio::fs::remove_file(&temporary).await?;
            }
            if image_present || temporary_present {
                tokio::fs::File::open(directory).await?.sync_all().await?;
            }
            let file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .await?;
            file.set_len(record.size).await?;
            drop(file);
            if let Err(error) = preparation
                .command("mkfs.ext4", &["-q", "-F", path(&temporary)?])
                .await
            {
                remove_partial(directory, &temporary).await?;
                return Err(error);
            }
            tokio::fs::File::open(&temporary).await?.sync_all().await?;
            tokio::fs::rename(&temporary, &image).await?;
        }
        record
    };
    compatible_owner(&record.context, context)?;
    let actual = tokio::fs::symlink_metadata(&image).await?;
    if !actual.is_file() || actual.len() < record.size || size < record.size || actual.len() > size
    {
        return Err(Error::conflict(
            "Local image geometry cannot shrink or exceed its reservation.",
        ));
    }
    // A committed conversion may still own its original journal after a crash.
    // Validate the untouched replacement before fsck or growth can mutate it.
    if let Some(proof) = &record.conversion {
        if proof.version != 1 || &conversion_proof(&image, stop).await? != proof {
            return Err(Error::conflict(
                "VM conversion replacement changed; legacy source retained.",
            ));
        }
        cleanup_legacy(directory).await?;
        record.conversion = None;
        persist(directory, &record).await?;
    } else if legacy_present(directory)? {
        return Err(Error::conflict(
            "VM conversion proof is missing; recover the retained legacy source before continuing.",
        ));
    }
    check(&image, &preparation).await?;
    if size > actual.len() {
        tokio::fs::OpenOptions::new()
            .write(true)
            .open(&image)
            .await?
            .set_len(size)
            .await?;
    }
    if size > record.size || actual.len() != record.size {
        preparation.command("resize2fs", &[path(&image)?]).await?;
        record.size = size;
    }
    preparation.check(0).await?;
    persist(directory, &record).await?;
    match tokio::fs::remove_file(directory.join("image.pending.json")).await {
        Ok(()) => tokio::fs::File::open(directory).await?.sync_all().await?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(image)
}

async fn regular_image_present(path: &Path) -> Result<bool> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(Error::conflict(
            "Local image initialization contains an alias; source retained.",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn compatible_owner(current: &Value, requested: &Value) -> Result<()> {
    if current["owner"]
        .as_str()
        .is_some_and(|owner| requested["owner"].as_str() != Some(owner))
    {
        return Err(Error::conflict(
            "The retained image belongs to another conversation; no filesystem changes were made.",
        ));
    }
    Ok(())
}

fn require_fresh(directory: &Path, initializing: bool) -> Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if super::environment::retained(&name) || name == "image-reservation.json" {
            continue;
        }
        let name = name.to_string_lossy();
        if initializing
            && matches!(
                name.as_ref(),
                "image.pending.json" | "image.partial" | "data.ext4"
            )
        {
            continue;
        }
        let atomic_temporary = name
            .strip_prefix('.')
            .and_then(|name| name.strip_suffix(".tmp"))
            .is_some_and(|name| crate::validation::uuid(name).is_ok())
            && entry.file_type()?.is_file();
        if atomic_temporary {
            continue;
        }
        return Err(Error::conflict(
            "Unrecognized retained storage; recover its source before creating a local image.",
        ));
    }
    Ok(())
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

async fn migrate_journal(
    directory: &Path,
    requested_size: u64,
    requested: &Value,
    stop: &CancellationToken,
) -> Result<Record> {
    if directory.join("runtime.json").exists() {
        let runtime: Value =
            serde_json::from_slice(&tokio::fs::read(directory.join("runtime.json")).await?)?;
        if runtime["backend"]
            .as_str()
            .is_some_and(|backend| !matches!(backend, "vm" | "firecracker"))
        {
            return Err(Error::conflict(
                "This retained disk belongs to another execution backend.",
            ));
        }
    }
    let _replacement = super::runtime::replacement(directory).await?;
    let root = directory.join("lazy");
    let context = super::LazyDisk::context(&root)?;
    compatible_owner(&context, requested)?;
    if requested_size < super::LazyDisk::geometry(&root)? {
        return Err(Error::conflict(
            "A retained legacy image cannot be shrunk during conversion.",
        ));
    }
    let temporary = directory.join("image.partial");
    match tokio::fs::remove_file(&temporary).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let preparation = Preparation::new(directory, requested, stop);
    preparation.check(0).await?;
    let output = temporary.clone();
    let copy_stop = stop.child_token();
    let _cancel_export_on_drop = copy_stop.clone().drop_guard();
    let copy_preparation = Preparation::new(directory, requested, &copy_stop);
    let exported = tokio::task::spawn_blocking(move || -> Result<u64> {
        let source = super::LazyDisk::open(&root)?;
        super::device::export_checked::<Error>(&source, &output, |incoming| {
            copy_preparation.check_sync(incoming)
        })?;
        let target = LocalDisk::open(&output, false)?;
        // Verify every logical byte, including zero ranges and the final tail.
        // This also forces legacy block checksums before changing the format.
        let mut original = vec![0; 4 * 1024 * 1024];
        let mut copied = vec![0; original.len()];
        let mut offset = 0;
        while offset < source.size() {
            copy_preparation.check_sync(0)?;
            if copy_stop.is_cancelled() {
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
    .map_err(Error::internal)
    .and_then(|result| result);
    let size = match exported {
        Ok(size) => size,
        Err(error) => {
            remove_partial(directory, &temporary).await?;
            return Err(error);
        }
    };
    let completed = async {
        check(&temporary, &preparation).await?;
        if requested_size > size {
            tokio::fs::OpenOptions::new()
                .write(true)
                .open(&temporary)
                .await?
                .set_len(requested_size)
                .await?;
            preparation
                .command("resize2fs", &[path(&temporary)?])
                .await?;
            check(&temporary, &preparation).await?;
        }
        sync(&temporary).await?;
        let conversion = Some(conversion_proof(&temporary, stop).await?);
        preparation.check(0).await?;
        tokio::fs::rename(&temporary, directory.join("data.ext4")).await?;
        let record = Record {
            format: FORMAT.into(),
            size: requested_size,
            context,
            conversion,
        };
        persist(directory, &record).await?;
        Ok(record)
    }
    .await;
    if completed.is_err() {
        remove_partial(directory, &temporary).await?;
    }
    completed
}

async fn remove_partial(directory: &Path, temporary: &Path) -> Result<()> {
    match tokio::fs::remove_file(temporary).await {
        Ok(()) => tokio::fs::File::open(directory).await?.sync_all().await?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn legacy_present(directory: &Path) -> Result<bool> {
    for name in ["lazy", "resize-source", "bootstrap.pending"] {
        match std::fs::symlink_metadata(directory.join(name)) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

async fn conversion_proof(image: &Path, stop: &CancellationToken) -> Result<ConversionProof> {
    let image = image.to_owned();
    let stop = stop.clone();
    tokio::task::spawn_blocking(move || -> Result<ConversionProof> {
        use std::{
            io::Read,
            os::unix::fs::{MetadataExt, OpenOptionsExt},
        };
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(image)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(Error::conflict(
                "VM conversion replacement is not a regular image.",
            ));
        }
        let mut hash = blake3::Hasher::new();
        let mut buffer = vec![0; 4 * 1024 * 1024];
        loop {
            if stop.is_cancelled() {
                return Err(Error::unavailable(
                    "VM conversion verification stopped; source retained.",
                ));
            }
            let length = file.read(&mut buffer)?;
            if length == 0 {
                break;
            }
            hash.update(&buffer[..length]);
        }
        Ok(ConversionProof {
            version: 1,
            filesystem: super::policy::filesystem_id(&file)?,
            inode: metadata.ino(),
            digest: hash.finalize().to_hex().to_string(),
        })
    })
    .await
    .map_err(Error::internal)?
}

async fn cleanup_legacy(directory: &Path) -> Result<()> {
    let _replacement = super::runtime::replacement(directory).await?;
    for name in ["lazy", "resize-source"] {
        let path = directory.join(name);
        match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                tokio::fs::remove_dir_all(&path).await?;
            }
            Ok(_) => tokio::fs::remove_file(&path).await?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    match tokio::fs::remove_file(directory.join("bootstrap.pending")).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    tokio::fs::File::open(directory).await?.sync_all().await?;
    Ok(())
}

fn path(value: &Path) -> Result<&str> {
    value
        .to_str()
        .ok_or_else(|| Error::bad("Invalid local image path."))
}

async fn check(image: &Path, preparation: &Preparation) -> Result<()> {
    preparation
        .command("e2fsck", &["-p", "-f", path(image)?])
        .await
}

pub(crate) async fn enforce_limits(
    state: &Path,
    attempt: &str,
    stop: &CancellationToken,
) -> Result<()> {
    use crate::microvm::suspension::{self, Reason};
    if stop.is_cancelled() {
        return Err(Error::conflict("VM execution stopped."));
    }
    let blocked = capacity_blocked(state).await?;
    if stop.is_cancelled() {
        return Err(Error::conflict("VM execution stopped."));
    }
    suspension::set_reason(state, attempt, &Reason::Storage, blocked).await?;
    Ok(())
}

/// Shared by startup protection and the bound attempt's suspension controller.
pub(crate) async fn capacity_blocked(state: &Path) -> Result<bool> {
    let observing = state.to_owned();
    let worker = tokio::task::spawn_blocking(move || -> Result<bool> {
        let policy = match std::fs::read(observing.join("storage-policy.json")) {
            Ok(bytes) => super::policy::from_persisted(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                super::policy::Policy::default()
            }
            Err(error) => return Err(error.into()),
        };
        let (total, free) = super::policy::physical_space(&observing)?;
        let configured = match std::fs::read(observing.join(crate::microvm::budget::FILE)) {
            Ok(bytes) => serde_json::from_slice::<crate::microvm::budget::Budget>(&bytes)?
                .limits
                .disk_mi_b
                .saturating_mul(1_048_576),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => u64::MAX,
            Err(error) => return Err(error.into()),
        };
        let limit = configured.min(super::reservation::epoch_capacity(&observing)?);
        Ok(free <= policy.reserve(total)
            || crate::microvm::budget::disk_bytes(&observing)? >= limit)
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), worker)
        .await
        .map_err(|_| Error::unavailable("VM disk observations are stale; execution must stop."))?
        .map_err(Error::internal)?
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
    async fn preparation_rechecks_reserve_and_reaps_its_exact_writer_before_returning() {
        use std::os::fd::{AsRawFd, FromRawFd};
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks").join(crate::config::id());
        std::fs::create_dir_all(&directory).unwrap();
        let retained = directory.join("retained-source");
        std::fs::write(&retained, b"retained user work").unwrap();
        let marker = root.path().join("writer-started");
        let preparation = Preparation::new(&directory, &context(), &CancellationToken::new());
        let command_marker = marker.clone();
        let operation = tokio::spawn(async move {
            preparation.command("/usr/bin/python3", &["-c", "import os,sys,time; p=sys.argv[1]; f=open(p+'.partial','w'); f.write(str(os.getpid())); f.close(); os.replace(p+'.partial',p); time.sleep(30)", path(&command_marker).unwrap()]).await
        });
        let pid = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(value) = tokio::fs::read_to_string(&marker).await {
                    break value.parse::<u32>().unwrap();
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
        assert!(raw >= 0);
        let process = unsafe { std::fs::File::from_raw_fd(raw) };
        let (total, _) = super::super::policy::physical_space(&directory).unwrap();
        atomic_write(
            &root.path().join("storage-policy.json"),
            &serde_json::to_vec(&json!({
                "reserveMiB": total / 1_048_576 + 1,
                "reservePercent": 1
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let error = tokio::time::timeout(std::time::Duration::from_secs(5), operation)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.status, 507);
        let mut poll = libc::pollfd {
            fd: process.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 2000) }, 1);
        assert_eq!(std::fs::read(retained).unwrap(), b"retained user work");
    }

    #[tokio::test]
    async fn an_owner_mismatch_is_refused_before_resizing_retained_data() {
        let root = tempfile::tempdir().unwrap();
        let context = context();
        let image = prepare(root.path(), SIZE, &context, &CancellationToken::new())
            .await
            .unwrap();
        let mut wrong = context.clone();
        wrong["owner"] = "another-conversation".into();
        assert_eq!(
            prepare(root.path(), SIZE * 2, &wrong, &CancellationToken::new())
                .await
                .unwrap_err()
                .status,
            409
        );
        assert_eq!(tokio::fs::metadata(&image).await.unwrap().len(), SIZE);
        assert_eq!(read_record(root.path()).await.unwrap().context, context);
    }

    #[tokio::test]
    async fn local_legacy_owner_upgrades_only_inside_its_durable_conversation_namespace() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let owner = super::super::environment::ownership(state.path(), &run, "busy")
            .await
            .unwrap();
        let mut legacy = context();
        legacy.as_object_mut().unwrap().remove("owner");
        legacy["grant"] = "retired-local-token".into();
        super::super::bootstrap::prepare(
            &owner.directory,
            SIZE,
            &legacy,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let current = json!({ "owner":run, "policy":legacy["policy"] });
        prepare(&owner.directory, SIZE, &current, &CancellationToken::new())
            .await
            .unwrap();
        authorize(&owner, &current).await.unwrap();
        assert_eq!(
            read_record(&owner.directory).await.unwrap().context["owner"],
            run
        );
        assert!(
            authorize(
                &owner,
                &json!({ "owner":crate::config::id(), "policy":current["policy"] })
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn missing_legacy_metadata_never_authorizes_empty_initialization() {
        for footprint in ["lazy/payload-1", "resize-source/payload-1", "runtime.json"] {
            let root = tempfile::tempdir().unwrap();
            let retained = root.path().join(footprint);
            tokio::fs::create_dir_all(retained.parent().unwrap())
                .await
                .unwrap();
            tokio::fs::write(&retained, b"retained source")
                .await
                .unwrap();
            assert_eq!(
                prepare(root.path(), SIZE, &context(), &CancellationToken::new())
                    .await
                    .unwrap_err()
                    .status,
                409
            );
            assert_eq!(
                tokio::fs::read(&retained).await.unwrap(),
                b"retained source"
            );
            assert!(!root.path().join("image.json").exists());
            assert!(!root.path().join("image.pending.json").exists());
        }
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
            conversion: None,
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
    async fn incomplete_fresh_mkfs_rejects_shrinks_aliases_and_overlapping_rebuilds() {
        use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};
        const CHILD: &str = "CAIRN_TEST_IMAGE_PENDING_REBUILD";
        if std::env::var_os(CHILD).is_none() {
            let fixture = tempfile::tempdir().unwrap();
            let wrapper = fixture.path().join("bin");
            std::fs::create_dir(&wrapper).unwrap();
            let real_path = std::env::var_os("PATH").unwrap();
            let original = std::env::split_paths(&real_path)
                .map(|path| path.join("mkfs.ext4"))
                .find(|path| path.is_file())
                .unwrap();
            let check = wrapper.join("mkfs.ext4");
            std::fs::write(&check, b"#!/bin/sh\ntest ! -e \"$CAIRN_IMAGE_EXPECT_RAW_ABSENT\" || exit 97\nexec \"$CAIRN_IMAGE_REAL_MKFS\" \"$@\"\n").unwrap();
            std::fs::set_permissions(&check, std::fs::Permissions::from_mode(0o755)).unwrap();
            let first = fixture.path().join("disks").join(crate::config::id());
            let paths = std::iter::once(wrapper).chain(std::env::split_paths(&real_path));
            let status = tokio::process::Command::new(std::env::current_exe().unwrap())
                .env_clear().env(CHILD, "1").env("PATH", std::env::join_paths(paths).unwrap())
                .env("CAIRN_IMAGE_TEST_ROOT", fixture.path()).env("CAIRN_IMAGE_TEST_DIRECTORY", &first)
                .env("CAIRN_IMAGE_EXPECT_RAW_ABSENT", first.join("data.ext4"))
                .env("CAIRN_IMAGE_REAL_MKFS", original).env("TMPDIR", std::env::temp_dir())
                .args(["--exact", "storage::image::tests::incomplete_fresh_mkfs_rejects_shrinks_aliases_and_overlapping_rebuilds", "--nocapture"])
                .status().await.unwrap();
            assert!(status.success());
            return;
        }
        let root = PathBuf::from(std::env::var_os("CAIRN_IMAGE_TEST_ROOT").unwrap());
        let first = PathBuf::from(std::env::var_os("CAIRN_IMAGE_TEST_DIRECTORY").unwrap());
        std::fs::create_dir_all(&first).unwrap();
        let context = context();
        let pending = Record {
            format: FORMAT.into(),
            size: SIZE,
            context: context.clone(),
            conversion: None,
        };
        atomic_write(
            &first.join("image.pending.json"),
            &serde_json::to_vec(&pending).unwrap(),
        )
        .await
        .unwrap();
        let image = first.join("data.ext4");
        std::fs::write(&image, b"uncommitted mkfs").unwrap();
        assert_eq!(
            prepare(&first, SIZE / 2, &context, &CancellationToken::new())
                .await
                .unwrap_err()
                .status,
            409
        );
        assert_eq!(std::fs::read(&image).unwrap(), b"uncommitted mkfs");
        let foreign = root.join("foreign-canary");
        std::fs::write(&foreign, b"foreign work").unwrap();
        std::os::unix::fs::symlink(&foreign, first.join("image.partial")).unwrap();
        assert_eq!(
            prepare(&first, SIZE, &context, &CancellationToken::new())
                .await
                .unwrap_err()
                .status,
            409
        );
        assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign work");
        std::fs::remove_file(first.join("image.partial")).unwrap();
        let budget = crate::microvm::budget::Budget {
            slots: 1,
            limits: crate::nodes::Resources {
                cpu: 1,
                memory_mi_b: 1024,
                disk_mi_b: 192,
            },
        };
        std::fs::write(
            root.join(crate::microvm::budget::FILE),
            serde_json::to_vec(&budget).unwrap(),
        )
        .unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&image)
            .unwrap();
        assert_eq!(
            unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, SIZE as libc::off_t) },
            0
        );
        drop(file);
        let policy = super::super::policy::for_node(&context["policy"]).unwrap();
        let promise = super::super::reservation::reserve(&root, &first, SIZE, &policy)
            .await
            .unwrap();
        prepare(&first, SIZE, &context, &CancellationToken::new())
            .await
            .unwrap();
        assert!(exists(&first));
        assert!(!first.join("image.pending.json").exists());
        promise.release().await.unwrap();
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
        // Recreate the committed-proof phase before partial source cleanup.
        let mut record = read_record(root.path()).await.unwrap();
        record.conversion = Some(conversion_proof(&image, &stop).await.unwrap());
        persist(root.path(), &record).await.unwrap();
        // A crash midway through deleting the old format may leave an invalid
        // journal. The verified committed image remains authoritative on restart.
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
    async fn committed_conversion_refuses_missing_replaced_or_changed_images_and_missing_proof() {
        for failure in ["missing", "replacement", "contents", "proof"] {
            let root = tempfile::tempdir().unwrap();
            let context = context();
            let stop = CancellationToken::new();
            super::super::bootstrap::prepare(root.path(), SIZE, &context, &stop)
                .await
                .unwrap();
            let volume = super::super::runtime::load(root.path()).await.unwrap();
            volume
                .write_at(96 * 1024 * 1024, b"original retained canary")
                .unwrap();
            volume.sync().unwrap();
            drop(volume);
            let mut record = migrate_journal(root.path(), SIZE, &context, &stop)
                .await
                .unwrap();
            let image = root.path().join("data.ext4");
            assert!(path_for_boot(root.path(), SIZE).await.is_err());
            match failure {
                "missing" => std::fs::remove_file(&image).unwrap(),
                "replacement" => {
                    // Retain the verified inode so the replacement cannot reuse it.
                    std::fs::rename(&image, root.path().join("verified-image-held")).unwrap();
                    super::super::local::prepare(root.path(), SIZE)
                        .await
                        .unwrap();
                }
                "contents" => {
                    let disk = LocalDisk::open(&image, true).unwrap();
                    disk.write_at(96 * 1024 * 1024, b"changed retained canary!")
                        .unwrap();
                    disk.sync().unwrap();
                }
                "proof" => {
                    record.conversion = None;
                    persist(root.path(), &record).await.unwrap();
                }
                _ => unreachable!(),
            }
            for _ in 0..2 {
                assert!(
                    prepare(root.path(), SIZE * 2, &context, &stop)
                        .await
                        .is_err(),
                    "{failure}"
                );
                assert!(
                    root.path().join("lazy/journal-v2.sqlite").is_file(),
                    "{failure}"
                );
                let source = super::super::runtime::load(root.path()).await.unwrap();
                let mut bytes = [0; 24];
                source.read_at(96 * 1024 * 1024, &mut bytes).unwrap();
                assert_eq!(&bytes, b"original retained canary");
            }
        }
    }

    #[tokio::test]
    async fn committed_conversion_retries_cleanup_before_further_growth_and_writable_resume() {
        let root = tempfile::tempdir().unwrap();
        let context = context();
        let stop = CancellationToken::new();
        super::super::bootstrap::prepare(root.path(), SIZE, &context, &stop)
            .await
            .unwrap();
        migrate_journal(root.path(), SIZE, &context, &stop)
            .await
            .unwrap();
        let image = prepare(root.path(), SIZE * 2, &context, &stop)
            .await
            .unwrap();
        assert!(!legacy_present(root.path()).unwrap());
        assert!(read_record(root.path()).await.unwrap().conversion.is_none());
        let disk = LocalDisk::open(&image, true).unwrap();
        disk.write_at(96 * 1024 * 1024, b"resumed user work")
            .unwrap();
        disk.sync().unwrap();
        drop(disk);
        prepare(root.path(), SIZE * 2, &context, &stop)
            .await
            .unwrap();
        let disk = LocalDisk::open(&image, false).unwrap();
        let mut bytes = [0; 17];
        disk.read_at(96 * 1024 * 1024, &mut bytes).unwrap();
        assert_eq!(&bytes, b"resumed user work");
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

#[cfg(test)]
mod legacy_resize_tests {
    use super::*;
    use crate::storage::{Disk, runtime};
    use serde_json::json;

    #[tokio::test]
    async fn interrupted_legacy_resize_converts_its_original_source_and_preserves_bytes() {
        let root = tempfile::tempdir().unwrap();
        let stop = CancellationToken::new();
        let size = 128 * 1024 * 1024;
        let context = json!({ "owner": crate::config::id(), "policy": {
            "memoryCacheMiB": 4, "reserveMiB": 64, "reservePercent": 1
        } });
        crate::storage::bootstrap::prepare(root.path(), size, &context, &stop)
            .await
            .unwrap();
        let volume = runtime::load(root.path()).await.unwrap();
        let retained = b"retained-resize-canary";
        volume.write_at(96 * 1024 * 1024, retained).unwrap();
        volume.sync().unwrap();
        drop(volume);
        atomic_write(&root.path().join("bootstrap.pending"), b"resizing")
            .await
            .unwrap();
        runtime::materialize(root.path(), &stop).await.unwrap();
        tokio::fs::write(
            root.path().join("data.ext4"),
            b"partly resized temporary image",
        )
        .await
        .unwrap();
        assert!(!runtime::exists(root.path()));
        // A different owner cannot even rename the recognized retained source.
        let wrong = json!({ "owner": crate::config::id(), "policy": context["policy"] });
        assert!(prepare(root.path(), size * 2, &wrong, &stop).await.is_err());
        assert!(root.path().join("resize-source/journal-v2.sqlite").exists());
        assert!(!root.path().join("lazy").exists());
        assert!(
            prepare(root.path(), size / 2, &context, &stop)
                .await
                .is_err()
        );
        assert!(root.path().join("resize-source/journal-v2.sqlite").exists());
        assert!(!root.path().join("lazy").exists());
        let image = prepare(root.path(), size * 2, &context, &stop)
            .await
            .unwrap();
        let disk = LocalDisk::open(&image, false).unwrap();
        let mut bytes = vec![0; retained.len()];
        disk.read_at(96 * 1024 * 1024, &mut bytes).unwrap();
        assert_eq!(bytes, retained);
        assert!(!root.path().join("resize-source").exists());
        assert!(!root.path().join("bootstrap.pending").exists());
        assert!(!root.path().join("lazy").exists());
    }
}
