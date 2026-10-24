//! Each conversation has a separate host account and disjoint subordinate IDs.
//! The engine runs as namespace UID 1000; only the managed namespace supervisor
//! and rootless Docker run as namespace root. Neither is host root.
use super::atomic_write;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

mod reconcile;

pub(super) use reconcile::reconcile;
use tokio::process::Command;

const COUNT: u32 = 65536;
const ROOT_FIRST: u32 = 500_000_000;
const ROOT_END: u32 = ROOT_FIRST + 1_048_576;

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

    pub(super) fn verify_maps(&self, uid_map: &str, gid_map: &str) -> Result<()> {
        for (actual, root, start) in [
            (uid_map, self.root_uid, self.subuid),
            (gid_map, self.root_gid, self.subgid),
        ] {
            let parsed = actual
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| {
                    line.split_whitespace()
                        .map(str::parse::<u32>)
                        .collect::<std::result::Result<Vec<_>, _>>()
                })
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Error::internal)?;
            if parsed != vec![vec![0, root, 1], vec![1, start, COUNT]] {
                return Err(Error::unavailable(
                    "RootlessKit UID/GID maps do not match this conversation's reserved identity.",
                ));
            }
        }
        Ok(())
    }
}

async fn output(binary: &str, args: &[&str]) -> Result<std::process::Output> {
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
    Ok(output)
}

async fn run(binary: &str, args: &[&str]) -> Result<String> {
    let output = output(binary, args).await?;
    if !output.status.success() {
        return Err(Error::unavailable(format!(
            "Native conversation identity provisioning failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    String::from_utf8(output.stdout).map_err(Error::internal)
}

async fn lookup(database: &str, key: &str) -> Result<Option<String>> {
    let output = output("/usr/bin/getent", &[database, key]).await?;
    if output.status.code() == Some(2) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err(Error::unavailable("Native identity lookup failed."));
    }
    Ok(Some(
        String::from_utf8(output.stdout).map_err(Error::internal)?,
    ))
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

fn occupied(uids: &str, gids: &str, passwd: &str, groups: &str) -> Result<Vec<(String, u32, u32)>> {
    let mut occupied = ranges(uids)?;
    occupied.extend(ranges(gids)?);
    // Numeric owner keys are valid subordinate-ID grants too. Do not assign a
    // new host identity an orphaned account's pre-existing mapping authority.
    let numeric_owners: Vec<u32> = occupied
        .iter()
        .filter_map(|(name, _, _)| name.parse().ok())
        .collect();
    for id in numeric_owners {
        occupied.push((String::new(), id, id.saturating_add(1)));
    }
    for (text, fields) in [(passwd, &[2usize, 3][..]), (groups, &[2usize][..])] {
        for line in text.lines().filter(|line| !line.is_empty()) {
            let columns: Vec<_> = line.split(':').collect();
            for &field in fields {
                let id = columns
                    .get(field)
                    .and_then(|value| value.parse::<u32>().ok())
                    .ok_or_else(|| Error::unavailable("Invalid native identity database."))?;
                occupied.push((String::new(), id, id.saturating_add(1)));
            }
        }
    }
    Ok(occupied)
}

fn free_range(uids: &str, gids: &str, passwd: &str, groups: &str) -> Result<u32> {
    let occupied = occupied(uids, gids, passwd, groups)?;
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

fn free_root_id(next: u32, uids: &str, gids: &str, passwd: &str, groups: &str) -> Result<u32> {
    if !(ROOT_FIRST..=ROOT_END).contains(&next) {
        return Err(Error::unavailable(
            "Native root identity sequence is invalid.",
        ));
    }
    let mut occupied = occupied(uids, gids, passwd, groups)?;
    occupied.sort_unstable_by_key(|(_, start, _)| *start);
    let mut candidate = next;
    for (_, start, end) in occupied {
        if end <= candidate {
            continue;
        }
        if start > candidate {
            break;
        }
        candidate = end;
    }
    if candidate >= ROOT_END {
        return Err(Error::unavailable(
            "Managed native root UID/GID pool is exhausted.",
        ));
    }
    Ok(candidate)
}

/// Advance durably before creating host accounts. Even a crash before account
/// creation burns the reserved number rather than recycling it into another run.
async fn reserve_root(
    registry: &Path,
    uids: &str,
    gids: &str,
    passwd: &str,
    groups: &str,
) -> Result<u32> {
    let path = registry.join("root-sequence");
    let next = match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice::<u32>(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ROOT_FIRST,
        Err(error) => return Err(error.into()),
    };
    let id = free_root_id(next, uids, gids, passwd, groups)?;
    atomic_write(&path, &serde_json::to_vec(&(id + 1))?).await?;
    Ok(id)
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
    let mut saved = match tokio::fs::read(&record).await {
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if lookup("passwd", &user).await?.is_some() || lookup("group", &user).await?.is_some() {
                return Err(Error::conflict(
                    "Native account exists without its conversation ownership record.",
                ));
            }
            let reservation = serde_json::json!({ "runId": run_id });
            atomic_write(&record, &serde_json::to_vec(&reservation)?).await?;
            reservation
        }
        Err(error) => return Err(error.into()),
    };
    if saved["runId"] != run_id {
        return Err(Error::conflict(
            "Native conversation account name collision.",
        ));
    }
    if lookup("passwd", &user).await?.is_none() {
        let group = lookup("group", &user).await?;
        let existing_gid = group
            .as_deref()
            .map(|group| {
                let fields: Vec<_> = group.trim().split(':').collect();
                let gid = fields
                    .get(2)
                    .and_then(|id| id.parse::<u32>().ok())
                    .ok_or_else(|| Error::unavailable("Invalid managed native group."))?;
                if saved["rootGid"].as_u64() != Some(gid as u64)
                    || fields.get(3).is_none_or(|members| !members.is_empty())
                {
                    return Err(Error::conflict(
                        "Native group is not a private reserved conversation group.",
                    ));
                }
                Ok(gid)
            })
            .transpose()?;
        let root = reserve_root(
            &registry,
            &tokio::fs::read_to_string("/etc/subuid").await?,
            &tokio::fs::read_to_string("/etc/subgid").await?,
            &run("/usr/bin/getent", &["passwd"]).await?,
            &run("/usr/bin/getent", &["group"]).await?,
        )
        .await?;
        // Numeric NSS lookups also protect hosts whose directory service does
        // not enumerate every identity. Do not override a conflicting account.
        if lookup("passwd", &root.to_string()).await?.is_some()
            || lookup("group", &root.to_string()).await?.is_some()
        {
            return Err(Error::conflict(
                "Reserved native root ID is already used by the host identity service.",
            ));
        }
        let gid = existing_gid.unwrap_or(root);
        saved["rootUid"] = root.into();
        saved["rootGid"] = gid.into();
        atomic_write(&record, &serde_json::to_vec(&saved)?).await?;
        if existing_gid.is_none() {
            run(
                "/usr/sbin/groupadd",
                &["--system", "--gid", &gid.to_string(), &user],
            )
            .await?;
        }
        run(
            "/usr/sbin/useradd",
            &[
                "--system",
                "--uid",
                &root.to_string(),
                "--gid",
                &user,
                "--no-user-group",
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
    if saved["rootUid"]
        .as_u64()
        .is_some_and(|expected| expected != root_uid as u64)
        || saved["rootGid"]
            .as_u64()
            .is_some_and(|expected| expected != root_gid as u64)
    {
        return Err(Error::conflict(
            "Native host account no longer matches its reserved identity.",
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
            &run("/usr/bin/getent", &["passwd"]).await?,
            &run("/usr/bin/getent", &["group"]).await?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_maps_must_match_the_reserved_identity_exactly() {
        let identity = Identity {
            user: "fixture".into(),
            root_uid: ROOT_FIRST,
            root_gid: ROOT_FIRST + 1,
            subuid: 100000,
            subgid: 200000,
        };
        let uid_map = format!("0 {} 1\n1 100000 65536\n", identity.root_uid);
        let gid_map = format!("0 {} 1\n1 200000 65536\n", identity.root_gid);
        assert!(identity.verify_maps(&uid_map, &gid_map).is_ok());
        assert!(
            identity
                .verify_maps(&uid_map.replace("100000", "300000"), &gid_map)
                .is_err()
        );
        assert!(
            identity
                .verify_maps(&uid_map, &gid_map.replace("65536", "65535"))
                .is_err()
        );
        assert_eq!(
            free_root_id(
                ROOT_FIRST,
                &format!("{ROOT_FIRST}:100000:65536\n"),
                "",
                "",
                ""
            )
            .unwrap(),
            ROOT_FIRST + 1
        );
    }

    #[test]
    fn root_ids_skip_host_accounts_groups_and_subordinates_beyond_system_pool_limits() {
        let users = (ROOT_FIRST..ROOT_FIRST + 5000)
            .map(|id| format!("u{id}:x:{id}:42::/:/bin/false\n"))
            .collect::<String>();
        let gid = ROOT_FIRST + 5000;
        let sub = format!("foreign:{}:65536\n", gid + 1);
        assert_eq!(
            free_root_id(ROOT_FIRST, &sub, "", &users, &format!("g:x:{gid}:\n")).unwrap(),
            gid + 1 + COUNT
        );
        assert!(free_root_id(ROOT_END, "", "", "", "").is_err());
    }

    #[tokio::test]
    async fn reserved_root_ids_are_not_recycled_after_interrupted_account_creation() {
        let temporary = tempfile::tempdir().unwrap();
        let first = reserve_root(temporary.path(), "", "", "", "")
            .await
            .unwrap();
        let second = reserve_root(temporary.path(), "", "", "", "")
            .await
            .unwrap();
        assert_eq!(first, ROOT_FIRST);
        assert_eq!(second, ROOT_FIRST + 1);
    }

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
