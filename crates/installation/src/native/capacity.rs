//! Native observations and manager-issued allowances; workloads cannot grant space.
use crate::{
    error::Result,
    nodes::disk_capacity::{GrowthRequest, MIB, Observation},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

#[derive(Clone)]
pub(super) struct Allowance {
    pub run: String,
    pub bytes: u64,
    pub requested: bool,
    pub reason: String,
    notice: Option<(u64, u8, String)>,
    last_notice_at: i64,
    workspace_bytes: u64,
}

#[derive(Clone, Default)]
pub(super) struct Capacity {
    pub negotiated: bool,
    pub attempts: HashMap<String, Allowance>,
}

impl Capacity {
    pub fn register(&mut self, attempt: &str, run: &str, bytes: u64) {
        self.attempts.insert(
            attempt.into(),
            Allowance {
                run: run.into(),
                bytes,
                requested: false,
                reason: String::new(),
                notice: None,
                last_notice_at: 0,
                workspace_bytes: 0,
            },
        );
    }

    pub fn apply(&mut self, reply: &Value, limit: u64) {
        if reply["protocol"] != 1 {
            return;
        }
        self.negotiated = true;
        for grant in reply["grants"].as_array().into_iter().flatten() {
            let Some(attempt) = grant["attemptId"]
                .as_str()
                .and_then(|id| self.attempts.get_mut(id))
            else {
                continue;
            };
            let Some(bytes) = grant["allowanceMiB"]
                .as_u64()
                .and_then(|mib| mib.checked_mul(MIB))
            else {
                continue;
            };
            if bytes < attempt.bytes || bytes > limit.min(1 << 40) {
                continue;
            }
            attempt.requested = false;
            attempt.bytes = bytes;
            attempt.reason = grant["reason"].as_str().unwrap_or("shared-capacity").into();
        }
    }

    pub fn observe(
        &mut self,
        id: &str,
        workspace: u64,
        temporary: u64,
        filesystem: u64,
    ) -> Option<(u64, Option<Value>)> {
        let attempt = self.attempts.get_mut(id)?;
        attempt.workspace_bytes = workspace;
        let used = workspace.saturating_add(temporary);
        let pressure = used >= attempt.bytes.saturating_mul(4) / 5
            || workspace >= filesystem.saturating_mul(4) / 5;
        if self.negotiated && pressure && filesystem >= attempt.bytes {
            attempt.requested = true;
        }
        let tier = if used >= attempt.bytes.saturating_mul(9) / 10 {
            2
        } else if pressure {
            1
        } else {
            0
        };
        let state = (
            attempt.bytes,
            tier,
            format!("{}:{}", self.negotiated, attempt.reason),
        );
        let now = crate::config::now();
        let immediate = attempt.notice.as_ref().is_none_or(|previous| {
            previous.0 != state.0 || previous.2 != state.2 || tier > previous.1
        });
        let recovering = !immediate && now.saturating_sub(attempt.last_notice_at) < 60_000;
        let notice = if attempt.notice.as_ref() == Some(&state)
            || tier == 0 && attempt.notice.is_none()
            || recovering
        {
            None
        } else {
            attempt.notice = Some(state);
            attempt.last_notice_at = now;
            let gib = |bytes: u64| bytes as f64 / (1024.0 * 1024.0 * 1024.0);
            let outcome = if !self.negotiated {
                "Automatic growth is not negotiated with this installation; the current allowance remains in effect."
            } else {
                match attempt.reason.as_str() {
                    "granted" => {
                        "Cairn granted additional shared capacity and is expanding the filesystem."
                    }
                    "shared-capacity" => {
                        "Additional capacity is unavailable while the node protects other conversations and its free-space reserve."
                    }
                    _ => "Cairn is checking shared capacity for automatic growth.",
                }
            };
            let message = format!(
                "Cairn runtime disk notice (operational information, not a new user request or authorization). \
                Workspace: {:.2} GiB; managed /tmp: {:.2} GiB; combined allowance: {:.2} GiB. {outcome} \
                Moving files between the workspace and /tmp does not reduce this allowance's usage. \
                Avoid large copies while space is low; reclaim only confirmed regenerable build output or your own obsolete temporary files. \
                Preserve source edits, sessions, credentials and unrecognized data.",
                gib(workspace),
                gib(temporary),
                gib(attempt.bytes),
            );
            Some(json!({ "id": crate::config::id(), "operationalNotice": true, "text": message }))
        };
        Some((attempt.bytes, notice))
    }

    pub async fn report(&self, state: &Path, used_bytes: u64) -> Result<Observation> {
        let (state, attempts) = (state.to_owned(), self.attempts.clone());
        tokio::task::spawn_blocking(move || {
            let active = attempts
                .values()
                .map(|attempt| attempt.run.clone())
                .collect();
            let retained_bytes = retained_credits(&state, &active)?;
            let mut owned_bytes = HashMap::new();
            let mut requests = Vec::new();
            for (id, attempt) in attempts {
                let directory = crate::storage::environment::directory(&state, &attempt.run)?;
                let local = crate::storage::LazyDisk::cached_backing_bytes(&directory.join("lazy"))
                    .unwrap_or(0);
                let temporary = super::workspace::directory(&state, &attempt.run)?.join("tmp");
                // Withhold journal credit: obsolete append frames are not
                // current backing, even below the logical consumption cap.
                let owned = local
                    .min(attempt.workspace_bytes)
                    .saturating_add(super::disk::exclusive_bytes(&temporary, false)?);
                owned_bytes.insert(id.clone(), owned);
                if attempt.requested {
                    requests.push(GrowthRequest {
                        attempt_id: id,
                        current_mi_b: attempt.bytes / MIB,
                    });
                }
            }
            let (_, free_bytes) = crate::storage::policy::space(&state)?;
            Ok(Observation {
                protocol: 1,
                captured_at: crate::config::now(),
                used_bytes,
                free_bytes,
                owned_bytes,
                retained_bytes,
                requests,
            })
        })
        .await
        .map_err(crate::error::Error::internal)?
    }
}

/// A quota observation is valid only for this owned journal directory and
/// execution. Restoring/rebuilding a journal changes its inode and invalidates it.
pub(super) async fn record_retained(directory: &Path, attempt: &str, workspace: u64) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = tokio::fs::metadata(directory).await?;
    super::atomic_write(
        &directory.join("capacity.json"),
        &serde_json::to_vec(&json!({
            "attempt": attempt, "workspaceBytes": workspace,
            "journalDevice": metadata.dev(), "journalInode": metadata.ino(),
        }))?,
    )
    .await
}

fn retained_credits(state: &Path, active: &HashSet<String>) -> Result<HashMap<String, u64>> {
    use std::{io::Read, os::unix::fs::MetadataExt};
    let mut credits = HashMap::new();
    let entries = match std::fs::read_dir(state.join("native")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(credits),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let run = entry.file_name().to_string_lossy().into_owned();
        if credits.len() >= 4096 {
            break;
        }
        if !entry.file_type()?.is_dir()
            || active.contains(&run)
            || crate::validation::uuid(&run).is_err()
        {
            continue;
        }
        let root = entry.path();
        let Ok(_owner) =
            crate::file_lock::exclusive(&root.join("owner.lock"), "Native workspace is active.")
        else {
            continue;
        };
        let directory = crate::storage::environment::directory(state, &run)?.join("lazy");
        let marker = directory.join("capacity.json");
        let Ok(metadata) = std::fs::symlink_metadata(&marker) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > 1024 {
            continue;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(marker)?
            .take(1025)
            .read_to_end(&mut bytes)?;
        let Ok(usage) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let attempt = std::fs::read_to_string(root.join("attempt")).unwrap_or_default();
        let metadata = std::fs::metadata(&directory)?;
        let Some(workspace) = usage["workspaceBytes"]
            .as_u64()
            .filter(|bytes| *bytes <= 1 << 40)
        else {
            continue;
        };
        if usage["attempt"] != attempt
            || usage["journalDevice"] != metadata.dev()
            || usage["journalInode"] != metadata.ino()
        {
            continue;
        }
        let owned = crate::storage::LazyDisk::cached_backing_bytes(&directory)
            .unwrap_or(0)
            .min(workspace)
            .saturating_add(super::disk::exclusive_bytes(&root.join("tmp"), false)?);
        credits.insert(run, owned);
    }
    Ok(credits)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Offline;

    impl crate::storage::BlockSource for Offline {
        fn fetch(&self, _: &str) -> std::io::Result<Vec<u8>> {
            panic!("Capacity must not fetch remote blocks")
        }
    }

    fn remote_manifest(hash: &str) -> Value {
        let block = 4 * MIB;
        json!({ "version": 1, "size": 5 * block, "blockSize": block,
            "blocks": (0..5).map(|index| json!({ "offset": index * block, "size": block, "hash": hash })).collect::<Vec<_>>() })
    }

    #[tokio::test]
    async fn retained_credit_survives_controller_restart_but_not_a_journal_replacement() {
        let state = tempfile::tempdir().unwrap();
        let (run, attempt) = (crate::config::id(), crate::config::id());
        let directory = crate::storage::environment::directory(state.path(), &run)
            .unwrap()
            .join("lazy");
        let root = super::super::workspace::directory(state.path(), &run).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("attempt"), &attempt).unwrap();
        let payload = vec![7; 4 * MIB as usize];
        let hash = crate::storage::digest::block(&payload);
        let disk = crate::storage::LazyDisk::create(
            &directory,
            &remote_manifest(&hash),
            std::sync::Arc::new(Offline),
        )
        .unwrap();
        std::fs::create_dir(directory.join("cache")).unwrap();
        std::fs::write(directory.join("cache").join(&hash), &payload).unwrap();
        record_retained(&directory, &attempt, 20 * MIB)
            .await
            .unwrap();
        let sample = Capacity::default()
            .report(state.path(), 4 * MIB)
            .await
            .unwrap();
        assert!(sample.retained_bytes[&run] > 0);
        drop(disk);
        let original = directory.with_file_name("previous");
        std::fs::rename(&directory, &original).unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::fs::copy(
            original.join("capacity.json"),
            directory.join("capacity.json"),
        )
        .unwrap();
        let sample = Capacity::default().report(state.path(), MIB).await.unwrap();
        assert!(!sample.retained_bytes.contains_key(&run));
    }

    #[tokio::test]
    async fn observations_exclude_repeated_journal_overwrites_sparse_tmp_and_old_cache_generations()
    {
        use crate::storage::Disk;
        let state = tempfile::tempdir().unwrap();
        let (run, attempt) = (crate::config::id(), crate::config::id());
        let directory = crate::storage::environment::directory(state.path(), &run)
            .unwrap()
            .join("lazy");
        let payload = vec![7; 4 * MIB as usize];
        let hash = crate::storage::digest::block(&payload);
        let disk = crate::storage::LazyDisk::create(
            &directory,
            &remote_manifest(&hash),
            std::sync::Arc::new(Offline),
        )
        .unwrap();
        for byte in 1..=16 {
            disk.write_at(0, &vec![byte; MIB as usize]).unwrap();
        }
        disk.sync().unwrap();
        assert!(super::super::disk::exclusive_bytes(&directory, true).unwrap() > 8 * MIB);
        let temporary = super::super::workspace::directory(state.path(), &run)
            .unwrap()
            .join("tmp");
        std::fs::create_dir_all(&temporary).unwrap();
        std::fs::File::create(temporary.join("sparse"))
            .unwrap()
            .set_len(16 * MIB)
            .unwrap();
        let mut capacity = Capacity::default();
        capacity.register(&attempt, &run, 64 * MIB);
        capacity
            .observe(&attempt, 20 * MIB, 16 * MIB, 64 * MIB)
            .unwrap();
        let sample = capacity.report(state.path(), 40 * MIB).await.unwrap();
        assert_eq!(sample.owned_bytes[&attempt], 0);
        std::fs::create_dir(directory.join("cache")).unwrap();
        std::fs::write(directory.join("cache").join(&hash), &payload).unwrap();
        std::fs::write(directory.join("cache").join("obsolete-hash"), &payload).unwrap();
        let sample = capacity.report(state.path(), 48 * MIB).await.unwrap();
        assert!(sample.owned_bytes[&attempt] > 0 && sample.owned_bytes[&attempt] <= 4 * MIB);
        std::fs::hard_link(
            directory.join("cache").join(&hash),
            state.path().join("shared"),
        )
        .unwrap();
        assert_eq!(
            capacity
                .report(state.path(), 48 * MIB)
                .await
                .unwrap()
                .owned_bytes[&attempt],
            0
        );
    }

    #[test]
    fn threshold_oscillation_does_not_flood_the_coding_agent() {
        let mut capacity = Capacity::default();
        capacity.register("attempt", "run", 100 * MIB);
        assert!(
            capacity
                .observe("attempt", 81 * MIB, 0, 100 * MIB)
                .unwrap()
                .1
                .is_some()
        );
        for used in [79, 81, 79, 81] {
            assert!(
                capacity
                    .observe("attempt", used * MIB, 0, 100 * MIB)
                    .unwrap()
                    .1
                    .is_none()
            );
        }
        assert!(
            capacity
                .observe("attempt", 91 * MIB, 0, 100 * MIB)
                .unwrap()
                .1
                .is_some()
        );
        capacity.attempts.get_mut("attempt").unwrap().last_notice_at -= 60_001;
        assert!(
            capacity
                .observe("attempt", 70 * MIB, 0, 100 * MIB)
                .unwrap()
                .1
                .is_some()
        );
    }

    #[test]
    fn warnings_count_tmp_preserve_user_authority_and_deduplicate_until_state_changes() {
        let gib = 1024 * MIB;
        let mut capacity = Capacity::default();
        capacity.register("attempt", "run", 32 * gib);
        capacity.negotiated = true;
        let (_, notice) = capacity
            .observe("attempt", 24 * gib, 5 * gib, 32 * gib)
            .unwrap();
        let notice = notice.unwrap();
        assert_eq!(notice["operationalNotice"], true);
        assert!(notice["text"].as_str().unwrap().contains("Moving files"));
        assert!(capacity.attempts["attempt"].requested);
        assert!(
            capacity
                .observe("attempt", 24 * gib, 5 * gib, 32 * gib)
                .unwrap()
                .1
                .is_none()
        );
        capacity.apply(&json!({ "protocol": 1, "grants": [{ "attemptId": "attempt", "allowanceMiB": 40960, "reason": "granted" }] }), 100 * gib);
        capacity.observe("attempt", 29 * gib, 0, 32 * gib).unwrap();
        assert!(
            !capacity.attempts["attempt"].requested,
            "An unapplied grant must not compound every heartbeat"
        );
        assert_eq!(
            capacity
                .observe("attempt", 24 * gib, 5 * gib, 40 * gib)
                .unwrap()
                .0,
            40 * gib
        );
        capacity.apply(&json!({ "protocol": 1, "grants": [{ "attemptId": "attempt", "allowanceMiB": 32768 }] }), 100 * gib);
        assert_eq!(capacity.attempts["attempt"].bytes, 40 * gib);
    }
}
