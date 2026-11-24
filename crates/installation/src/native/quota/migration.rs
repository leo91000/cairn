//! Offline conversion of the native data pool to another filesystem, used by
//! `native-host.py migrate-pool` while the node is stopped. The host script
//! copies and verifies the files; this side owns project IDs, kernel limits and
//! the controller's ownership records, which bind the pool's identity and the
//! workload inode and would otherwise reject every migrated environment.
//!
//! Rewritten records are staged in a work directory on the controller
//! filesystem and replace the originals only once the new pool is mounted at
//! the pool path. The originals are kept there for a rollback.
use super::{Ownership, Pool, Quota, current, open, operation, pool_identity, set_project};
use crate::{
    error::{Error, Result},
    native::{atomic_write, identity, workspace},
};
use serde_json::{Value, json};
use std::{
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
};

const OWNER: &str = "quota-owner.json";
const RETIREMENT: &str = "quota-retirement.json";
const RECORDS: [&str; 2] = [OWNER, RETIREMENT];

const USAGE: &str = "Use native-pool-migrate plan|prepare|commit|switch|rollback|check <state> [<work> [<target> <run>]].";

struct Environment {
    run: String,
    control: PathBuf,
    workload: PathBuf,
    project: u32,
    owner: Option<Ownership>,
    retirement: Option<Ownership>,
    quota: Quota,
}

impl Environment {
    fn summary(&self) -> Value {
        json!({
            "run": self.run,
            "project": self.project,
            "recorded": self.owner.is_some(),
            "retiring": self.retirement.is_some(),
            "blockHardBytes": self.quota.block_hard.saturating_mul(1024),
            "blockSoftBytes": self.quota.block_soft.saturating_mul(1024),
            "inodeHard": self.quota.inode_hard,
            "inodeSoft": self.quota.inode_soft,
            "usedBytes": self.quota.space,
            "inodes": self.quota.inodes,
        })
    }
}

struct Inventory {
    environments: Vec<Environment>,
    skipped: Vec<String>,
    blockers: Vec<String>,
}

fn read_record(path: &Path) -> Result<Option<Ownership>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn classify(state: &Path, pool: &std::fs::File, run: &str) -> Result<Environment> {
    let workload = workspace::workload(state, run)?;
    workspace::require_no_mounts(&workload)?;
    let control = workspace::directory(state, run)?;
    let file = open(&workload)?;
    if !file.metadata()?.is_dir() || file.metadata()?.dev() != pool.metadata()?.dev() {
        return Err(Error::conflict("is not a directory of the data pool"));
    }
    if control.join("directory-conversion-verified").exists() {
        return Err(Error::conflict(
            "has an unfinished legacy journal conversion; start the node and let it finish first",
        ));
    }
    let project = super::attributes(&file)?.project;
    let owner = read_record(&control.join(OWNER))?;
    let retirement = read_record(&control.join(RETIREMENT))?;
    let recorded = match (&owner, &retirement) {
        (Some(owner), Some(retirement)) if owner != retirement => {
            return Err(Error::conflict(
                "has a quota retirement for a replaced environment",
            ));
        }
        (Some(record), _) | (None, Some(record)) => Some(record),
        (None, None) => None,
    };
    let Some(record) = recorded else {
        // A preparation that failed before allocating an ID has no quota; the
        // controller removes it on retirement. Anything else is unrecognized.
        if project != 0 {
            return Err(Error::conflict(
                "has a project ID without an ownership record",
            ));
        }
        return Ok(Environment {
            run: run.into(),
            control,
            workload,
            project,
            owner,
            retirement,
            quota: Quota::default(),
        });
    };
    record
        .validate(&control, &file)
        .map_err(|_| Error::conflict("does not match its ownership record"))?;
    if project != 0 && project != record.project {
        return Err(Error::conflict(
            "has a project ID other than its recorded one",
        ));
    }
    let quota = if project == 0 {
        Quota::default()
    } else {
        current(pool, project)?
    };
    Ok(Environment {
        run: run.into(),
        control,
        workload,
        project,
        owner,
        retirement,
        quota,
    })
}

/// Every direct child of the pool is a run directory, `lost+found` or a
/// dot-entry. Dot-entries hold rebuildable caches such as `.cairn-bases`.
fn inventory(state: &Path) -> Result<Inventory> {
    let root = workspace::data_root(state);
    let pool = open(&root)?;
    if pool.metadata()?.dev() == open(state)?.metadata()?.dev() {
        return Err(Error::unavailable(
            "The native data pool is not mounted at state/native-data.",
        ));
    }
    let mut names = std::fs::read_dir(&root)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    let mut inventory = Inventory {
        environments: Vec::new(),
        skipped: Vec::new(),
        blockers: Vec::new(),
    };
    for name in names {
        let Some(name) = name.to_str().map(str::to_owned) else {
            inventory
                .blockers
                .push("The pool contains an entry with a non-UTF-8 name.".into());
            continue;
        };
        if name == "lost+found" || name.starts_with('.') {
            inventory.skipped.push(name);
            continue;
        }
        if crate::validation::uuid(&name).is_err() {
            inventory
                .blockers
                .push(format!("Pool entry {name} is not a native environment."));
            continue;
        }
        match classify(state, &pool, &name) {
            Ok(environment) => inventory.environments.push(environment),
            Err(error) => inventory
                .blockers
                .push(format!("Environment {name} {}.", error.message)),
        }
    }
    Ok(inventory)
}

fn environment(state: &Path, run: &str) -> Result<Environment> {
    crate::validation::uuid(run)?;
    let pool = open(&workspace::data_root(state))?;
    classify(state, &pool, run)
        .map_err(|error| Error::conflict(format!("Environment {run} {}.", error.message)))
}

/// The target is a different filesystem whose project quotas are enforced.
fn target_pool(state: &Path, target: &Path) -> Result<std::fs::File> {
    let file = open(target)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(Error::conflict(
            "Migration target must be a root-owned directory not writable by other accounts.",
        ));
    }
    if Pool::of(&file)?.is_none() || !super::project_enforced(&file) {
        return Err(Error::unavailable(
            "Migration target must be an XFS or ext4 filesystem mounted with enforced project quotas.",
        ));
    }
    let source = open(&workspace::data_root(state))?;
    if metadata.dev() == source.metadata()?.dev()
        || pool_identity(&file)? == pool_identity(&source)?
    {
        return Err(Error::conflict(
            "Migration target is the current data pool.",
        ));
    }
    Ok(file)
}

fn copy_path(target: &Path, run: &str) -> Result<PathBuf> {
    crate::validation::uuid(run)?;
    let path = target.join(run);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(path),
        Ok(_) => Err(Error::conflict("Migrated environment is not a directory.")),
        Err(error) => Err(error.into()),
    }
}

fn set_limits(pool: &std::fs::File, project: u32, limits: &Quota) -> Result<()> {
    let mut quota = Quota {
        block_hard: limits.block_hard,
        block_soft: limits.block_soft,
        inode_hard: limits.inode_hard,
        inode_soft: limits.inode_soft,
        valid: 1 | 4,
        ..Default::default()
    };
    operation(pool, 0x800008, project, &mut quota)
}

fn syncfs(file: &std::fs::File) -> Result<()> {
    if unsafe { libc::syncfs(file.as_raw_fd()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Create the environment's directory on the target with its original project
/// ID. Limits stay cleared during the copy: XFS enforces them even for root,
/// and an administrator may have exceeded them on ext4 with CAP_SYS_RESOURCE.
async fn prepare(state: &Path, target: &Path, run: &str) -> Result<Value> {
    let pool = target_pool(state, target)?;
    let environment = environment(state, run)?;
    let original = std::fs::metadata(&environment.workload)?;
    let path = target.join(run);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&path)?;
            std::os::unix::fs::chown(&path, Some(original.uid()), Some(original.gid()))?;
            std::fs::set_permissions(
                &path,
                std::os::unix::fs::PermissionsExt::from_mode(original.mode() & 0o7777),
            )?;
            pool.sync_all()?;
        }
        Err(error) => return Err(error.into()),
    }
    let copy = open(&copy_path(target, run)?)?;
    if environment.project != 0 {
        match super::attributes(&copy)?.project {
            0 => set_project(&copy, environment.project)?,
            project if project == environment.project => {}
            _ => {
                return Err(Error::conflict(
                    "Migrated environment already has another project ID.",
                ));
            }
        }
        set_limits(&pool, environment.project, &Quota::default())?;
    }
    Ok(json!({ "run": run, "project": environment.project }))
}

fn record_directory(work: &Path, run: &str, kind: &str) -> PathBuf {
    work.join("records").join(run).join(kind)
}

async fn stage(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::skills::private_dir(path.parent().unwrap()).await?;
    atomic_write(path, bytes).await
}

/// After a verified copy: confirm the target charges the same inodes to the
/// same project, apply the original limits and stage rewritten records.
async fn commit(state: &Path, work: &Path, target: &Path, run: &str) -> Result<Value> {
    let pool = target_pool(state, target)?;
    let environment = environment(state, run)?;
    let path = copy_path(target, run)?;
    let copy = open(&path)?;
    if super::attributes(&copy)?.project != environment.project {
        return Err(Error::conflict(
            "Migrated environment lost its project ID; rerun the migration.",
        ));
    }
    let mut charged = Quota::default();
    if environment.project != 0 {
        // XFS releases inodes removed by a resumed copy in the background.
        syncfs(&pool)?;
        charged = current(&pool, environment.project)?;
        if charged.inodes != environment.quota.inodes {
            return Err(Error::conflict(format!(
                "Environment {run} has {} inodes in project {} on the new pool but {} on the old pool; rerun the copy.",
                charged.inodes, environment.project, environment.quota.inodes
            )));
        }
        set_limits(&pool, environment.project, &environment.quota)?;
    }
    let source = environment.workload.join("data");
    let rebound = if source.is_dir() {
        identity::rebind_pending(&source, &path.join("data")).await?
    } else {
        false
    };
    let metadata = copy.metadata()?;
    for (name, record) in [
        (OWNER, environment.owner.as_ref()),
        (RETIREMENT, environment.retirement.as_ref()),
    ] {
        let Some(record) = record else { continue };
        let original = std::fs::read(environment.control.join(name))?;
        let saved = record_directory(work, run, "original").join(name);
        match std::fs::read(&saved) {
            Ok(bytes) if bytes != original => {
                return Err(Error::conflict(format!(
                    "Environment {run} changed its {name} since the migration started."
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                stage(&saved, &original).await?;
            }
            Err(error) => return Err(error.into()),
        }
        let migrated = Ownership {
            project: record.project,
            filesystem: pool_identity(&copy)?,
            control_device: record.control_device,
            control_inode: record.control_inode,
            control_filesystem: record.control_filesystem.clone(),
            workload_device: metadata.dev(),
            workload_inode: metadata.ino(),
        };
        stage(
            &record_directory(work, run, "migrated").join(name),
            &serde_json::to_vec(&migrated)?,
        )
        .await?;
    }
    Ok(json!({
        "run": run,
        "project": environment.project,
        "inodes": charged.inodes,
        "usedBytes": charged.space,
        "identityJournalRebound": rebound,
    }))
}

fn staged_runs(work: &Path) -> Result<Vec<String>> {
    let mut runs = Vec::new();
    match std::fs::read_dir(work.join("records")) {
        Ok(entries) => {
            for entry in entries {
                let name = entry?.file_name();
                let name = name
                    .to_str()
                    .ok_or_else(|| Error::conflict("Invalid migration record."))?;
                crate::validation::uuid(name)?;
                runs.push(name.to_owned());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    runs.sort();
    Ok(runs)
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Replace one live record, but only from the expected previous version, so
/// a record the controller changed after the copy is never overwritten.
async fn replace(live: &Path, expected: &[u8], replacement: &[u8], run: &str) -> Result<bool> {
    match read_optional(live)? {
        Some(bytes) if bytes == replacement => Ok(false),
        Some(bytes) if bytes == expected => {
            atomic_write(live, replacement).await?;
            Ok(true)
        }
        _ => Err(Error::conflict(format!(
            "Environment {run} changed its quota records during the migration; nothing was replaced for it."
        ))),
    }
}

/// The new pool is mounted at the pool path. Each staged record must already
/// describe it before it replaces the original.
async fn switch(state: &Path, work: &Path) -> Result<Value> {
    let mut switched = 0;
    for run in staged_runs(work)? {
        let control = workspace::directory(state, &run)?;
        let workload = open(&workspace::workload(state, &run)?)?;
        for name in RECORDS {
            let Some(migrated) =
                read_optional(&record_directory(work, &run, "migrated").join(name))?
            else {
                continue;
            };
            let original = read_optional(&record_directory(work, &run, "original").join(name))?
                .ok_or_else(|| Error::conflict("Missing original migration record."))?;
            serde_json::from_slice::<Ownership>(&migrated)?
                .validate(&control, &workload)
                .map_err(|_| {
                    Error::conflict(format!(
                        "Environment {run} on the mounted pool is not the verified copy."
                    ))
                })?;
            if replace(&control.join(name), &original, &migrated, &run).await? {
                switched += 1;
            }
        }
    }
    Ok(json!({ "switchedRecords": switched }))
}

/// The original pool is mounted at the pool path again.
async fn rollback(state: &Path, work: &Path) -> Result<Value> {
    let mut restored = 0;
    for run in staged_runs(work)? {
        let control = workspace::directory(state, &run)?;
        for name in RECORDS {
            let Some(original) =
                read_optional(&record_directory(work, &run, "original").join(name))?
            else {
                continue;
            };
            serde_json::from_slice::<Ownership>(&original)?
                .validate(&control, &open(&workspace::workload(state, &run)?)?)
                .map_err(|_| {
                    Error::conflict(format!(
                        "Environment {run} on the mounted pool is not the original."
                    ))
                })?;
            let migrated = read_optional(&record_directory(work, &run, "migrated").join(name))?
                .unwrap_or_else(|| original.clone());
            if replace(&control.join(name), &migrated, &original, &run).await? {
                restored += 1;
            }
        }
    }
    Ok(json!({ "restoredRecords": restored }))
}

/// The controller's own startup check, plus every record it would validate.
fn check(state: &Path) -> Result<Value> {
    super::require(state)?;
    let inventory = inventory(state)?;
    if let Some(blocker) = inventory.blockers.first() {
        return Err(Error::conflict(blocker.clone()));
    }
    let mut checked = 0;
    for environment in &inventory.environments {
        if environment.owner.is_some() && environment.project != 0 {
            super::usage(&environment.workload)?;
            checked += 1;
        }
    }
    let mut value = super::describe(state)?;
    value["environments"] = inventory.environments.len().into();
    value["checkedRecords"] = checked.into();
    Ok(value)
}

fn plan(state: &Path) -> Result<Value> {
    let inventory = inventory(state)?;
    let pool = open(&workspace::data_root(state))?;
    let status = super::filesystem(&pool)?;
    let used = status
        .f_blocks
        .saturating_sub(status.f_bfree)
        .saturating_mul(status.f_bsize as u64);
    Ok(json!({
        "filesystem": Pool::of(&pool)?.map(Pool::name),
        "reflink": super::reflink(&pool)?,
        "usedBytes": used,
        "environments": inventory.environments.iter().map(Environment::summary).collect::<Vec<_>>(),
        "skipped": inventory.skipped,
        "blockers": inventory.blockers,
    }))
}

fn argument(args: &[String], index: usize) -> Result<&Path> {
    args.get(index)
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| Error::bad(USAGE))
}

pub async fn command(args: &[String]) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(Error::forbidden("Native pool migration must run as root."));
    }
    let action = args.first().map_or("", String::as_str);
    let state = argument(args, 1)?;
    let value = match action {
        "plan" if args.len() == 2 => plan(state)?,
        "check" if args.len() == 2 => check(state)?,
        "switch" if args.len() == 3 => switch(state, argument(args, 2)?).await?,
        "rollback" if args.len() == 3 => rollback(state, argument(args, 2)?).await?,
        "prepare" if args.len() == 5 => prepare(state, argument(args, 3)?, &args[4]).await?,
        "commit" if args.len() == 5 => {
            commit(state, argument(args, 2)?, argument(args, 3)?, &args[4]).await?
        }
        _ => return Err(Error::bad(USAGE)),
    };
    println!("{}", serde_json::to_string(&value)?);
    Ok(())
}

#[cfg(test)]
mod tests;
