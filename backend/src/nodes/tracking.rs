//! Written-block tracking for conversation disks, recorded by dm-era inside the guest.
//!
//! Beside each `data.ext4`, the node keeps:
//! - `era.meta`: the guest's dm-era metadata. The guest reloads it at every boot, which
//!   also recovers the writes of a VM that stopped abruptly.
//! - `tracking/<snapshot>.json`: baselines, the era and manifest of recent captures.
//! - `sealed.json`: the era a clean guest shutdown archived, so a stopped disk can list
//!   its writes without booting.
//!
//! Every write to `data.ext4` must go through the guest's era target. Host-side writes
//! (creation, resize, import, restore) call [`invalidate`] first, and a boot without
//! tracking invalidates too. Tracking only narrows what a capture reads: any doubt
//! means a full copy.
use crate::{
    config::now,
    error::Result,
    skills::{atomic_write, private_dir},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Sparse metadata disk; its bitsets for 4 MiB blocks fit easily for disks up to 1 TiB.
pub const METADATA_BYTES: u64 = 64 * 1024 * 1024;
/// A full copy at least this often bounds the effect of a wrong or incomplete write list.
const FULL_EVERY_MS: i64 = 24 * 3_600_000;
/// Baselines kept per disk: the master may still name an older published point.
const BASELINES: usize = 3;

/// A recent capture this node can continue from.
pub struct Baseline {
    pub era: u64,
    pub manifest: Value,
    /// When the last full copy of this chain was taken.
    pub full_at: i64,
}

pub fn metadata(disk: &Path) -> PathBuf {
    disk.join("era.meta")
}
fn baselines(disk: &Path) -> PathBuf {
    disk.join("tracking")
}
fn seal_file(disk: &Path) -> PathBuf {
    disk.join("sealed.json")
}
async fn remove(path: &Path) -> Result<()> {
    let removed = if path.is_dir() {
        tokio::fs::remove_dir_all(path).await
    } else {
        tokio::fs::remove_file(path).await
    };
    match removed {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// Forgets all tracking state: the next boot starts fresh metadata and the next
/// capture copies the whole disk.
pub async fn invalidate(disk: &Path) -> Result<()> {
    remove(&metadata(disk)).await?;
    remove(&baselines(disk)).await?;
    remove(&seal_file(disk)).await
}

/// Before a boot: keep the metadata the guest will recover, but forget any seal since
/// the boot may write. Without metadata, earlier baselines cannot be continued.
pub async fn prepare_boot(disk: &Path, owner: u32) -> Result<PathBuf> {
    remove(&seal_file(disk)).await?;
    let file = metadata(disk);
    if !file.exists() {
        remove(&baselines(disk)).await?;
        let partial = disk.join("era.meta.partial");
        tokio::fs::File::create(&partial)
            .await?
            .set_len(METADATA_BYTES)
            .await?;
        std::os::unix::fs::chown(&partial, Some(owner), Some(owner))?;
        tokio::fs::rename(&partial, &file).await?;
    }
    Ok(file)
}

/// After the guest's first status of a boot. Writes of an untracked boot are invisible
/// to every later era, so no earlier baseline may be continued.
pub async fn booted(disk: &Path, tracked: bool) -> Result<()> {
    if tracked {
        Ok(())
    } else {
        invalidate(disk).await
    }
}

/// Records the era a clean guest shutdown archived after freezing its filesystem.
pub async fn seal(disk: &Path, era: u64) -> Result<()> {
    atomic_write(&seal_file(disk), &serde_json::to_vec(&json!({"era":era}))?).await
}

/// The era sealed by the last shutdown, if it was clean and nothing booted since.
pub fn sealed(disk: &Path) -> Option<u64> {
    let value: Value = serde_json::from_slice(&std::fs::read(seal_file(disk)).ok()?).ok()?;
    metadata(disk).exists().then_some(value["era"].as_u64()?)
}

/// The recorded baseline of `snapshot`, unless the disk changed size or its chain is
/// due for a full copy.
pub fn baseline(disk: &Path, snapshot: Option<&str>) -> Option<Baseline> {
    let snapshot = snapshot?;
    crate::validation::uuid(snapshot).ok()?;
    let record: Value = serde_json::from_slice(
        &std::fs::read(baselines(disk).join(format!("{snapshot}.json"))).ok()?,
    )
    .ok()?;
    let size = std::fs::metadata(disk.join("data.ext4")).ok()?.len();
    let full_at = record["fullAt"].as_i64()?;
    if record["manifest"]["size"] != size
        || now() - full_at >= FULL_EVERY_MS
        || super::snapshots::validate(&record["manifest"]).is_err()
    {
        return None;
    }
    Some(Baseline {
        era: record["era"].as_u64()?,
        manifest: record["manifest"].clone(),
        full_at,
    })
}

/// Block indexes from `[begin, end)` ranges, if they all fit the baseline's disk.
pub fn blocks(ranges: &[[u64; 2]], manifest: &Value) -> Option<Vec<u64>> {
    let count = manifest["blocks"].as_array()?.len() as u64;
    let mut blocks = Vec::new();
    for &[begin, end] in ranges {
        if begin >= end || end > count {
            return None;
        }
        blocks.extend(begin..end);
    }
    blocks.sort_unstable();
    blocks.dedup();
    Some(blocks)
}

/// Blocks written since `since` on a stopped disk whose guest sealed its last era.
pub async fn offline(disk: &Path, since: u64) -> Option<Vec<[u64; 2]>> {
    offline_with(disk, since, Path::new("era_invalidate")).await
}
async fn offline_with(disk: &Path, since: u64, tool: &Path) -> Option<Vec<[u64; 2]>> {
    sealed(disk)?;
    // A sealed shutdown archived every era, so the inactive metadata lists them all.
    let output = tokio::process::Command::new(tool)
        .arg("--written-since")
        .arg(since.to_string())
        .arg(metadata(disk))
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    crate::microvm::era::ranges(&String::from_utf8_lossy(&output.stdout))
}

/// Stores a capture as a baseline; `full_at` carries the chain's last full copy.
pub async fn remember(
    disk: &Path,
    snapshot: &str,
    era: u64,
    manifest: &Value,
    full_at: i64,
) -> Result<()> {
    let directory = baselines(disk);
    private_dir(&directory).await?;
    atomic_write(
        &directory.join(format!("{snapshot}.json")),
        &serde_json::to_vec(&json!({"era":era,"fullAt":full_at,"manifest":manifest}))?,
    )
    .await?;
    let mut records = Vec::new();
    let mut entries = tokio::fs::read_dir(&directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        records.push((entry.metadata().await?.modified()?, entry.path()));
    }
    records.sort();
    for (_, path) in records.iter().rev().skip(BASELINES) {
        let _ = tokio::fs::remove_file(path).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::snapshots;

    async fn disk() -> (tempfile::TempDir, PathBuf, Value) {
        let root = tempfile::tempdir().unwrap();
        let disk = root.path().join("disk");
        std::fs::create_dir(&disk).unwrap();
        std::fs::write(disk.join("data.ext4"), vec![1; 9 * 1024 * 1024]).unwrap();
        let manifest = snapshots::index(&disk.join("data.ext4")).await.unwrap();
        (root, disk, manifest)
    }

    #[tokio::test]
    async fn baselines_survive_boots_but_not_host_writes_resizes_or_age() {
        let (_root, disk, manifest) = disk().await;
        let snapshot = crate::config::id();
        let owner = nix_uid();
        prepare_boot(&disk, owner).await.unwrap();
        remember(&disk, &snapshot, 4, &manifest, now())
            .await
            .unwrap();
        prepare_boot(&disk, owner).await.unwrap();
        assert_eq!(baseline(&disk, Some(&snapshot)).unwrap().era, 4);
        // A disk that grew is not the disk the baseline describes.
        std::fs::File::options()
            .write(true)
            .open(disk.join("data.ext4"))
            .unwrap()
            .set_len(13 * 1024 * 1024)
            .unwrap();
        assert!(baseline(&disk, Some(&snapshot)).is_none());
        std::fs::File::options()
            .write(true)
            .open(disk.join("data.ext4"))
            .unwrap()
            .set_len(9 * 1024 * 1024)
            .unwrap();
        // A chain whose last full copy is a day old starts over.
        remember(&disk, &snapshot, 4, &manifest, now() - FULL_EVERY_MS)
            .await
            .unwrap();
        assert!(baseline(&disk, Some(&snapshot)).is_none());
        remember(&disk, &snapshot, 4, &manifest, now())
            .await
            .unwrap();
        invalidate(&disk).await.unwrap();
        assert!(baseline(&disk, Some(&snapshot)).is_none());
        assert!(!metadata(&disk).exists());
    }

    #[tokio::test]
    async fn an_untracked_boot_or_missing_metadata_forgets_every_baseline() {
        let (_root, disk, manifest) = disk().await;
        let snapshot = crate::config::id();
        prepare_boot(&disk, nix_uid()).await.unwrap();
        remember(&disk, &snapshot, 2, &manifest, now())
            .await
            .unwrap();
        booted(&disk, true).await.unwrap();
        assert!(baseline(&disk, Some(&snapshot)).is_some());
        booted(&disk, false).await.unwrap();
        assert!(baseline(&disk, Some(&snapshot)).is_none());
        remember(&disk, &snapshot, 2, &manifest, now())
            .await
            .unwrap();
        prepare_boot(&disk, nix_uid()).await.unwrap();
        assert!(baseline(&disk, Some(&snapshot)).is_none());
    }

    #[tokio::test]
    async fn stopped_disks_list_writes_only_after_a_seal_and_until_the_next_boot() {
        let (root, disk, _) = disk().await;
        prepare_boot(&disk, nix_uid()).await.unwrap();
        let tool = root.path().join("era_invalidate");
        std::fs::write(
            &tool,
            "#!/bin/sh\nprintf '<blocks>\\n  <block block=\"2\"/>\\n  <range begin=\"5\" end = \"7\"/>\\n</blocks>\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        assert_eq!(offline_with(&disk, 1, &tool).await, None);
        seal(&disk, 2).await.unwrap();
        assert_eq!(sealed(&disk), Some(2));
        assert_eq!(
            offline_with(&disk, 1, &tool).await,
            Some(vec![[2, 3], [5, 7]])
        );
        prepare_boot(&disk, nix_uid()).await.unwrap();
        assert_eq!(sealed(&disk), None);
        assert_eq!(offline_with(&disk, 1, &tool).await, None);
    }

    #[test]
    fn write_lists_outside_the_disk_are_rejected() {
        let manifest = json!({"blocks":[{},{},{}]});
        assert_eq!(blocks(&[[2, 3], [0, 2]], &manifest), Some(vec![0, 1, 2]));
        assert_eq!(blocks(&[[2, 4]], &manifest), None);
        assert_eq!(blocks(&[[1, 1]], &manifest), None);
    }

    fn nix_uid() -> u32 {
        unsafe { libc::getuid() }
    }
}
