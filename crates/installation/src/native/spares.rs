//! Bounded, account-free native environments. Only a verified base enters a
//! spare, and the conversation's ordinary ownership lease fences adoption.
use super::{atomic_write, bases::Bases, directory, disk, quota, workspace};
use crate::{
    error::{Error, Result},
    skills::private_dir,
    storage::policy::Policy,
};
use serde::{Deserialize, Serialize};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

const MAX_SPARES: usize = 2;
pub(super) const CLAIM: &str = "base-spare-claim.json";
pub(super) const ADOPTED_BY: &str = "spare-adopted-by";

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Spare {
    run: String,
    key: String,
}

#[derive(Serialize, Deserialize)]
struct Claim {
    spare: Spare,
    project: u32,
    filesystem: Vec<u8>,
    inode: u64,
}

impl Claim {
    fn prepare(state: &Path, spare: Spare) -> Result<Self> {
        let workload = workspace::workload(state, &spare.run)?;
        let file = std::fs::File::open(&workload)?;
        Ok(Self {
            project: quota::owned_project(&workload, &file)?,
            filesystem: quota::pool_identity(&file)?,
            inode: file.metadata()?.ino(),
            spare,
        })
    }

    fn check(&self, path: &Path) -> Result<()> {
        let file = std::fs::File::open(path)?;
        if self.inode != file.metadata()?.ino()
            || self.project != quota::project_io(&file)?
            || self.filesystem != quota::pool_identity(&file)?
        {
            return Err(Error::conflict("Claimed native spare has been replaced."));
        }
        Ok(())
    }
}

fn index(state: &Path) -> PathBuf {
    state.join("native-base-spares")
}

fn files(state: &Path) -> Result<Vec<Spare>> {
    let entries = match std::fs::read_dir(index(state)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut spares = Vec::new();
    for file in entries {
        let file = file?;
        let name = file.file_name();
        let Some(run) = name
            .to_str()
            .filter(|run| crate::validation::uuid(run).is_ok())
        else {
            continue;
        };
        let spare: Spare = serde_json::from_slice(&std::fs::read(file.path())?)?;
        if spare.run != run {
            return Err(Error::conflict(
                "Native spare index contains the wrong identity.",
            ));
        }
        spares.push(spare);
    }
    Ok(spares)
}

async fn remove_file(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn forget(state: &Path, spare: &Spare) -> Result<()> {
    // Stop advertising the old owner before deleting its control files. If a
    // claim already moved the project, startup must never try to retire it
    // through the old owner after finding a partially removed source receipt.
    let index = index(state);
    remove_file(&index.join(&spare.run)).await?;
    tokio::fs::File::open(index).await?.sync_all().await?;
    let control = workspace::directory(state, &spare.run)?;
    match tokio::fs::remove_dir_all(&control).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    tokio::fs::File::open(state.join("native"))
        .await?
        .sync_all()
        .await?;
    Ok(())
}

async fn retire(state: &Path, spare: &Spare) -> Result<()> {
    let control = workspace::directory(state, &spare.run)?;
    if control.exists() {
        quota::retire(&control).await?;
    } else if workspace::workload(state, &spare.run)?.try_exists()? {
        return Err(Error::conflict(
            "Native spare still holds a workload after its quota control record was lost.",
        ));
    }
    forget(state, spare).await
}

/// Called only while no background builder can own the index.
pub(super) async fn discard_unclaimed(state: &Path, keep: Option<&str>) -> Result<bool> {
    let mut removed = false;
    for spare in files(state)? {
        if keep == Some(spare.key.as_str()) {
            continue;
        }
        let control = workspace::directory(state, &spare.run)?;
        if !control.join(ADOPTED_BY).exists() {
            retire(state, &spare).await?;
            removed = true;
        }
    }
    Ok(removed)
}

pub(super) async fn retire_for_base(state: &Path, key: &str) -> Result<()> {
    for spare in files(state)?.into_iter().filter(|spare| spare.key == key) {
        let control = workspace::directory(state, &spare.run)?;
        if !control.join(ADOPTED_BY).exists() {
            retire(state, &spare).await?;
        }
    }
    Ok(())
}

fn ready(state: &Path, key: &str) -> Result<Option<Spare>> {
    for spare in files(state)?.into_iter().filter(|spare| spare.key == key) {
        let source = workspace::directory(state, &spare.run)?;
        if !source.join(ADOPTED_BY).exists()
            && std::fs::read_to_string(source.join("spare-ready")).is_ok_and(|ready| ready == key)
        {
            return Ok(Some(spare));
        }
    }
    Ok(None)
}

pub(super) fn has_ready(state: &Path, key: &str) -> Result<bool> {
    Ok(ready(state, key)?.is_some())
}

#[cfg(test)]
pub(super) fn ready_inode(state: &Path, key: &str) -> Result<Option<u64>> {
    ready(state, key)?
        .map(|spare| {
            let workload = workspace::workload(state, &spare.run)?;
            Ok(std::fs::metadata(workload)?.ino())
        })
        .transpose()
}

pub(super) async fn warm(
    bases: &Bases,
    key: &str,
    disk_mib: u64,
    required: u64,
    limit: u64,
    policy: &Policy,
    stop: &CancellationToken,
) -> Result<()> {
    let state = &bases.state;
    let pool = workspace::data_root(state);
    let prepared = files(state)?;
    if stop.is_cancelled()
        || !quota::reflink(&std::fs::File::open(&pool)?)?
        || required > disk::available(limit, disk::used(&bases.data, state).await?)
    {
        return Ok(());
    }
    if let Some(existing) = prepared.iter().find(|spare| spare.key == key) {
        let record = std::fs::File::open(index(state).join(&existing.run))?;
        record.set_modified(std::time::SystemTime::now())?;
        return Ok(());
    }
    if prepared.len() >= MAX_SPARES {
        let Some(oldest) = oldest_unclaimed(state, &prepared)? else {
            return Ok(());
        };
        retire(state, oldest).await?;
    }
    let spare = Spare {
        run: crate::config::id(),
        key: key.into(),
    };
    let control = workspace::directory(state, &spare.run)?;
    let workload = workspace::workload(state, &spare.run)?;
    if control.try_exists()? || workload.try_exists()? {
        return Err(Error::conflict("Native spare identity is occupied."));
    }
    private_dir(&index(state)).await?;
    private_dir(&state.join("native")).await?;
    tokio::fs::File::open(state).await?.sync_all().await?;
    atomic_write(&index(state).join(&spare.run), &serde_json::to_vec(&spare)?).await?;
    let reservation = match disk::Preparation::reserve(state, &spare.run, required).await {
        Ok(reservation) => reservation,
        Err(error) => {
            forget(state, &spare).await?;
            return Err(error);
        }
    };
    let prepared = async {
        private_dir(&control).await?;
        tokio::fs::File::open(control.parent().unwrap())
            .await?
            .sync_all()
            .await?;
        private_dir(&workload).await?;
        quota::prepare(state, &workload, disk_mib * disk::MIB).await?;
        let data = workload.join("data");
        private_dir(&data).await?;
        let capacity = directory::CopyCapacity::new(state, &workload, required, policy.clone())
            .with_reservation(reservation.bytes)
            .with_shared_blocks(true);
        directory::copy_verified(&bases.contents(key), &data, stop, &capacity, true).await?;
        atomic_write(&control.join("spare-ready"), key.as_bytes()).await?;
        Ok(())
    }
    .await;
    if prepared.is_err() {
        retire(state, &spare).await?;
    }
    prepared
}

fn oldest_unclaimed<'a>(state: &Path, prepared: &'a [Spare]) -> Result<Option<&'a Spare>> {
    let mut oldest = None;
    for spare in prepared {
        if workspace::directory(state, &spare.run)?
            .join(ADOPTED_BY)
            .exists()
        {
            continue;
        }
        let modified = std::fs::metadata(index(state).join(&spare.run))?.modified()?;
        if oldest.is_none_or(|(timestamp, _)| modified < timestamp) {
            oldest = Some((modified, spare));
        }
    }
    Ok(oldest.map(|(_, spare)| spare))
}

fn read_claim(control: &Path) -> Result<Option<Claim>> {
    match std::fs::read(control.join(CLAIM)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Caller serializes the selection with publication and eviction. The two
/// control receipts survive a crash on either side of the physical rename.
pub(super) async fn claim(state: &Path, run: &str, key: &str) -> Result<bool> {
    let control = workspace::directory(state, run)?;
    let workload = workspace::workload(state, run)?;
    let claim = match read_claim(&control)? {
        Some(claim) if claim.spare.key == key => claim,
        Some(_) => return Err(Error::conflict("Prepared native claim changed its base.")),
        None if workload.try_exists()? => return Ok(false),
        None => {
            let Some(spare) = ready(state, key)? else {
                return Ok(false);
            };
            let claim = Claim::prepare(state, spare)?;
            tokio::fs::File::open(control.parent().unwrap())
                .await?
                .sync_all()
                .await?;
            atomic_write(&control.join(CLAIM), &serde_json::to_vec(&claim)?).await?;
            claim
        }
    };
    complete_claim(state, run, &claim).await
}

async fn complete_claim(state: &Path, run: &str, claim: &Claim) -> Result<bool> {
    let spare = &claim.spare;
    let control = workspace::directory(state, run)?;
    let workload = workspace::workload(state, run)?;
    let source = workspace::directory(state, &spare.run)?;
    let available = workspace::workload(state, &spare.run)?;
    let adopted_by = source.join(ADOPTED_BY);

    if !workload.try_exists()? {
        if !available.try_exists()? {
            // A crash before writing the source receipt allowed startup to
            // retire the unclaimed spare. Continue with a regular clone.
            finish(&control).await?;
            return Ok(false);
        }
        claim.check(&available)?;
        match tokio::fs::read_to_string(&adopted_by).await {
            Ok(owner) if owner != run => {
                finish(&control).await?;
                return Ok(false);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                atomic_write(&adopted_by, run.as_bytes()).await?;
            }
            Err(error) => return Err(error.into()),
        }
        workspace::require_no_mounts(&available)?;
        tokio::fs::rename(&available, &workload).await?;
        tokio::fs::File::open(workspace::data_root(state))
            .await?
            .sync_all()
            .await?;
    }
    if available.try_exists()? {
        return Err(Error::conflict(
            "Both workload roots exist for one native spare claim.",
        ));
    }
    claim.check(&workload)?;
    if index(state).join(&spare.run).try_exists()?
        && tokio::fs::read_to_string(&adopted_by).await? != run
    {
        return Err(Error::conflict(
            "Native spare belongs to a different claimant.",
        ));
    }
    quota::adopt_prepared(&source, &control, &workload).await?;
    if source.try_exists()? || index(state).join(&spare.run).try_exists()? {
        forget(state, spare).await?;
    }
    Ok(true)
}

pub(super) async fn finish(control: &Path) -> Result<()> {
    let marker = control.join(CLAIM);
    if marker.try_exists()? {
        remove_file(&marker).await?;
        tokio::fs::File::open(control).await?.sync_all().await?;
    }
    Ok(())
}

/// Settle a partially adopted spare before its ordinary quota retirement.
pub(super) async fn settle_for_retirement(state: &Path, run: &str) -> Result<()> {
    let control = workspace::directory(state, run)?;
    if let Some(claim) = read_claim(&control)? {
        complete_claim(state, run, &claim).await?;
        finish(&control).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn startup_discards_spare_controls_without_workloads_or_quota_receipts() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path();
        private_dir(&index(state)).await.unwrap();
        let mut controls = Vec::new();

        for was_ready in [false, true] {
            let spare = Spare {
                run: crate::config::id(),
                key: "a".repeat(64),
            };
            let control = workspace::directory(state, &spare.run).unwrap();
            private_dir(&control).await.unwrap();
            if was_ready {
                std::fs::write(control.join("spare-ready"), &spare.key).unwrap();
            }
            atomic_write(
                &index(state).join(&spare.run),
                &serde_json::to_vec(&spare).unwrap(),
            )
            .await
            .unwrap();
            controls.push(control);
        }

        assert!(discard_unclaimed(state, None).await.unwrap());
        assert!(files(state).unwrap().is_empty());
        assert!(controls.iter().all(|control| !control.exists()));
        assert!(!discard_unclaimed(state, None).await.unwrap());
    }

    #[tokio::test]
    async fn a_new_base_replaces_the_least_recent_unclaimed_spare() {
        use std::time::{Duration, UNIX_EPOCH};
        let root = tempfile::tempdir().unwrap();
        let state = root.path();
        private_dir(&index(state)).await.unwrap();
        let entries = [("a", 1), ("b", 3), ("c", 2)].map(|(key, modified)| {
            let spare = Spare {
                run: crate::config::id(),
                key: key.repeat(64),
            };
            let file = std::fs::File::create(index(state).join(&spare.run)).unwrap();
            file.set_modified(UNIX_EPOCH + Duration::from_secs(modified))
                .unwrap();
            spare
        });
        assert_eq!(
            oldest_unclaimed(state, &entries).unwrap().unwrap().key,
            "a".repeat(64)
        );
        let control = workspace::directory(state, &entries[0].run).unwrap();
        private_dir(&control).await.unwrap();
        std::fs::write(control.join(ADOPTED_BY), b"claimed").unwrap();
        assert_eq!(
            oldest_unclaimed(state, &entries).unwrap().unwrap().key,
            "c".repeat(64)
        );
    }

    #[tokio::test]
    #[ignore = "requires an explicitly disposable host and the native quota fixture"]
    async fn kernel_spare_claim_recovers_after_rename_before_quota_adoption() {
        assert_eq!(
            std::env::var("CAIRN_NATIVE_DISPOSABLE_HOST").as_deref(),
            Ok("1")
        );
        let state = PathBuf::from(std::env::var("CAIRN_NATIVE_QUOTA_TEST_STATE").unwrap());
        let ready = Spare {
            run: crate::config::id(),
            key: "a".repeat(64),
        };
        let run = crate::config::id();
        let source = workspace::directory(&state, &ready.run).unwrap();
        let workload = workspace::workload(&state, &ready.run).unwrap();
        let target = workspace::directory(&state, &run).unwrap();
        let claimed = workspace::workload(&state, &run).unwrap();
        for path in [&source, &target, &workload, &index(&state)] {
            private_dir(path).await.unwrap();
        }
        quota::prepare(&state, &workload, 128 * disk::MIB)
            .await
            .unwrap();
        std::fs::write(workload.join("payload"), b"verified base").unwrap();
        let inode = std::fs::metadata(workload.join("payload")).unwrap().ino();
        atomic_write(
            &index(&state).join(&ready.run),
            &serde_json::to_vec(&ready).unwrap(),
        )
        .await
        .unwrap();
        let claim = Claim::prepare(&state, ready.clone()).unwrap();
        atomic_write(&target.join(CLAIM), &serde_json::to_vec(&claim).unwrap())
            .await
            .unwrap();
        atomic_write(&source.join(ADOPTED_BY), run.as_bytes())
            .await
            .unwrap();
        std::fs::rename(&workload, &claimed).unwrap();

        discard_unclaimed(&state, None).await.unwrap();
        assert!(!target.join("quota-owner.json").exists());
        assert!(super::claim(&state, &run, &ready.key).await.unwrap());
        assert!(super::claim(&state, &run, &ready.key).await.unwrap());
        assert!(quota::usage(&claimed).unwrap() > 0);
        assert_eq!(
            std::fs::metadata(claimed.join("payload")).unwrap().ino(),
            inode
        );
        assert_eq!(
            std::fs::read(claimed.join("payload")).unwrap(),
            b"verified base"
        );
        assert!(!source.exists());
        assert!(!index(&state).join(&ready.run).exists());

        finish(&target).await.unwrap();
        quota::retire(&target).await.unwrap();
        std::fs::remove_dir(target).unwrap();
    }
}
