//! Native observations and manager-issued allowances; workloads cannot grant space.
use crate::{
    error::Result,
    nodes::disk_capacity::{GrowthRequest, MIB, Observation},
};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path};

#[derive(Clone)]
pub(super) struct Allowance {
    pub run: String,
    pub bytes: u64,
    pub requested: bool,
    pub reason: String,
    notice: Option<(u64, u8, String)>,
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
        let notice = if attempt.notice.as_ref() == Some(&state)
            || tier == 0 && attempt.notice.is_none()
        {
            None
        } else {
            attempt.notice = Some(state);
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
            let mut owned_bytes = HashMap::new();
            let mut requests = Vec::new();
            for (id, attempt) in attempts {
                let directory = crate::storage::environment::directory(&state, &attempt.run)?;
                let local = super::disk::exclusive_bytes(&directory, true)?;
                let temporary = super::workspace::directory(&state, &attempt.run)?.join("tmp");
                let owned = local.saturating_add(super::disk::exclusive_bytes(&temporary, false)?);
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
                requests,
            })
        })
        .await
        .map_err(crate::error::Error::internal)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
