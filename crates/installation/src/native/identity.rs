//! Each conversation has a separate host account and disjoint subordinate IDs.
//! The engine runs as namespace UID 1000; only the managed namespace supervisor
//! and rootless Docker run as namespace root. Neither is host root.
use super::atomic_write;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{os::unix::fs::MetadataExt, path::Path};
use tokio::process::Command;

const COUNT: u32 = 65536;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(super) struct Identity {
    pub user: String,
    pub root_uid: u32,
    pub root_gid: u32,
    pub subuid: u32,
    pub subgid: u32,
}

impl Identity {
    pub fn uid(&self) -> u32 {
        self.subuid + 999
    }

    pub fn gid(&self) -> u32 {
        self.subgid + 999
    }
}

async fn run(binary: &str, args: &[&str]) -> Result<String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Command::new(binary)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("LANG", "C")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| Error::timeout("Native conversation identity provisioning timed out."))??;
    if !output.status.success() {
        return Err(Error::unavailable(format!(
            "Native conversation identity provisioning failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    String::from_utf8(output.stdout).map_err(Error::internal)
}

fn ranges(text: &str) -> Result<Vec<(String, u32, u32)>> {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            let parts: Vec<_> = line.split(':').collect();
            if parts.len() != 3 {
                return Err(Error::unavailable(
                    "Invalid native subordinate ID database.",
                ));
            }
            let start = parts[1].parse::<u32>().map_err(Error::internal)?;
            let count = parts[2].parse::<u32>().map_err(Error::internal)?;
            let end = start
                .checked_add(count)
                .filter(|_| count > 0)
                .ok_or_else(|| Error::unavailable("Invalid native subordinate ID range."))?;
            Ok((parts[0].to_owned(), start, end))
        })
        .collect()
}

fn selected(user: &str, text: &str) -> Result<Option<u32>> {
    let ranges = ranges(text)?;
    let own: Vec<_> = ranges.iter().filter(|(name, _, _)| name == user).collect();
    let [(_, start, end)] = own.as_slice() else {
        if own.is_empty() {
            return Ok(None);
        }
        return Err(Error::unavailable(
            "Native account must have exactly one subordinate range.",
        ));
    };
    if *end - *start != COUNT
        || ranges.iter().any(|(name, other_start, other_end)| {
            name != user && *other_start < *end && *start < *other_end
        })
    {
        return Err(Error::unavailable(
            "Native conversation subordinate IDs overlap or have the wrong size.",
        ));
    }
    Ok(Some(*start))
}

fn free_range(uids: &str, gids: &str, passwd: &str, groups: &str) -> Result<u32> {
    let mut occupied = ranges(uids)?;
    occupied.extend(ranges(gids)?);
    for text in [passwd, groups] {
        for line in text.lines().filter(|line| !line.is_empty()) {
            let id = line
                .split(':')
                .nth(2)
                .and_then(|value| value.parse::<u32>().ok())
                .ok_or_else(|| Error::unavailable("Invalid native identity database."))?;
            occupied.push((String::new(), id, id.saturating_add(1)));
        }
    }
    (1_000_013_824..4_000_000_000u32)
        .step_by(COUNT as usize)
        .find(|start| {
            occupied
                .iter()
                .all(|(_, lo, hi)| *start >= *hi || start + COUNT <= *lo)
        })
        .ok_or_else(|| {
            Error::unavailable("No disjoint native conversation UID/GID range is available.")
        })
}

/// Called under the controller's disk lock, before exposing any files. Accounts
/// are retained with their environments; they are never recycled into another
/// conversation, including after a crash halfway through provisioning.
pub(super) async fn provision(state: &Path, run_id: &str) -> Result<Identity> {
    crate::validation::uuid(run_id)?;
    let user = format!("cairn-c-{}", run_id.replace('-', ""));
    // Linux account names are bounded to 32 characters. Keep 96 UUID bits and
    // verify the full conversation binding in root-private metadata below.
    let user = user[..32].to_owned();
    let registry = state.join("identities");
    crate::skills::private_dir(&registry).await?;
    let _lock = crate::file_lock::exclusive(
        &registry.join("lock"),
        "Native identity provisioning is busy.",
    )?;
    let record = registry.join(format!("{user}.json"));
    if let Ok(bytes) = tokio::fs::read(&record).await {
        let saved: serde_json::Value = serde_json::from_slice(&bytes)?;
        if saved["runId"] != run_id {
            return Err(Error::conflict(
                "Native conversation account name collision.",
            ));
        }
    } else {
        // Reserve before invoking useradd, so a partial failure cannot let a
        // second conversation adopt an already created account.
        if run("/usr/bin/id", &["-u", &user]).await.is_ok() {
            return Err(Error::conflict(
                "Native conversation account already exists without its ownership record.",
            ));
        }
        atomic_write(
            &record,
            &serde_json::to_vec(&serde_json::json!({ "runId": run_id }))?,
        )
        .await?;
    }
    if run("/usr/bin/id", &["-u", &user]).await.is_err() {
        run(
            "/usr/sbin/useradd",
            &[
                "--system",
                "--user-group",
                "--no-create-home",
                "--shell",
                "/usr/sbin/nologin",
                &user,
            ],
        )
        .await?;
    }
    let root_uid = run("/usr/bin/id", &["-u", &user])
        .await?
        .trim()
        .parse::<u32>()
        .map_err(Error::internal)?;
    let root_gid = run("/usr/bin/id", &["-g", &user])
        .await?
        .trim()
        .parse::<u32>()
        .map_err(Error::internal)?;
    if root_uid == 0 || root_gid == 0 {
        return Err(Error::unavailable(
            "Native conversation identity must be unprivileged.",
        ));
    }
    let uids = tokio::fs::read_to_string("/etc/subuid").await?;
    let gids = tokio::fs::read_to_string("/etc/subgid").await?;
    let uid_range = selected(&user, &uids)?;
    let gid_range = selected(&user, &gids)?;
    let start = match uid_range.or(gid_range) {
        Some(start) => start,
        None => free_range(
            &uids,
            &gids,
            &tokio::fs::read_to_string("/etc/passwd").await?,
            &tokio::fs::read_to_string("/etc/group").await?,
        )?,
    };
    let range = format!("{start}-{}", start + COUNT - 1);
    if uid_range.is_none() {
        run("/usr/sbin/usermod", &["--add-subuids", &range, &user]).await?;
    }
    if gid_range.is_none() {
        run("/usr/sbin/usermod", &["--add-subgids", &range, &user]).await?;
    }
    let subuid = selected(&user, &tokio::fs::read_to_string("/etc/subuid").await?)?
        .ok_or_else(|| Error::unavailable("Native UID mapping missing."))?;
    let subgid = selected(&user, &tokio::fs::read_to_string("/etc/subgid").await?)?
        .ok_or_else(|| Error::unavailable("Native GID mapping missing."))?;
    Ok(Identity {
        user,
        root_uid,
        root_gid,
        subuid,
        subgid,
    })
}

fn translate(id: u32, old_root: u32, old_start: u32, root: u32, start: u32) -> u32 {
    if id == old_root {
        root
    } else if id >= old_start && id - old_start < COUNT {
        start + id - old_start
    } else {
        id
    }
}

/// The mapping travels inside the encrypted disk, rather than node-local
/// metadata. Preserve container users and volume modes on cross-node recovery.
pub(super) async fn reconcile(data: &Path, identity: &Identity) -> Result<()> {
    let marker = data.join(".cairn-identity.json");
    let previous = match tokio::fs::read(&marker).await {
        Ok(bytes) => Some(serde_json::from_slice::<Identity>(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if previous.as_ref() == Some(identity) {
        return Ok(());
    }
    if let Some(old) = previous {
        let mut pending = vec![data.to_owned()];
        while let Some(path) = pending.pop() {
            let metadata = tokio::fs::symlink_metadata(&path).await?;
            let uid = translate(
                metadata.uid(),
                old.root_uid,
                old.subuid,
                identity.root_uid,
                identity.subuid,
            );
            let gid = translate(
                metadata.gid(),
                old.root_gid,
                old.subgid,
                identity.root_gid,
                identity.subgid,
            );
            if uid != metadata.uid() || gid != metadata.gid() {
                std::os::unix::fs::lchown(&path, Some(uid), Some(gid))?;
                if !metadata.file_type().is_symlink() {
                    tokio::fs::set_permissions(&path, metadata.permissions()).await?;
                }
            }
            if metadata.is_dir() {
                let mut entries = tokio::fs::read_dir(&path).await?;
                while let Some(entry) = entries.next_entry().await? {
                    pending.push(entry.path());
                }
            }
        }
    } else {
        super::workspace::own_tree(data, identity.uid(), identity.gid()).await?;
    }
    super::block::sync(data).await?;
    atomic_write(&marker, &serde_json::to_vec(identity)?).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_never_overlap_host_users_or_other_subordinates() {
        let first = 1_000_013_824;
        let text = format!("other:{first}:65536\n");
        let chosen = free_range(
            &text,
            "",
            &format!("person:x:{}:100::/:/bin/sh", first + COUNT),
            "root:x:0:",
        )
        .unwrap();
        assert_eq!(chosen, first + 2 * COUNT);
        assert!(selected("cairn", "cairn:100000:65536\nother:120000:65536").is_err());
        assert!(selected("cairn", "cairn:100000:65536\ncairn:200000:65536").is_err());
    }

    #[test]
    fn remapping_preserves_volume_users_and_is_idempotent_for_disjoint_ranges() {
        for id in [900, 100000, 100999, 165535, 0] {
            let mapped = translate(id, 900, 100000, 901, 200000);
            assert_eq!(translate(mapped, 900, 100000, 901, 200000), mapped);
        }
        assert_eq!(translate(100998, 900, 100000, 901, 200000), 200998);
    }
}
