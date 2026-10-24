//! Cooperative host privilege changes; root on this host can defeat this supervisor.
use super::{atomic_write, systemd};
use crate::{
    error::{Error, Result},
    nodes::native_mode::Mode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub const FILE: &str = "native-mode.json";
const SUDO_HEADER: &str = "# Cairn native administrator; protected units retain NoNewPrivileges.";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub mode: Mode,
    pub generation: Option<String>,
}

pub fn request(value: Value) -> Result<Request> {
    let request: Request = serde_json::from_value(value)
        .map_err(|_| Error::bad("Unknown native privilege mode or protocol."))?;
    if let Some(generation) = &request.generation {
        crate::validation::uuid(generation)?;
    } else if request.mode != Mode::Protected {
        return Err(Error::bad(
            "Administrative execution needs a reviewed generation.",
        ));
    }
    Ok(request)
}

pub async fn load(state: &Path) -> Result<Value> {
    match tokio::fs::read(state.join(FILE)).await {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)?;
            request(json!({ "mode": value["mode"], "generation": value["generation"] }))?;
            if !matches!(
                value["status"].as_str(),
                Some("applied" | "pending" | "failed")
            ) {
                return Err(Error::bad("Unknown native privilege transition state."));
            }
            Ok(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(json!({ "mode": "protected", "generation": null, "status": "applied" }))
        }
        Err(error) => Err(error.into()),
    }
}

pub fn matches(state: &Value, expected: &Value) -> bool {
    state["status"] == "applied"
        && state["mode"] == expected["mode"]
        && state["generation"] == expected["generation"]
}

pub fn check_plan(state: &Value, plan: &Value) -> Result<()> {
    let expected = plan
        .get("nativeMode")
        .cloned()
        .unwrap_or_else(|| json!({ "mode": "protected", "generation": null }));
    request(expected.clone())?;
    if !matches(state, &expected) {
        return Err(Error::conflict(
            "Native privilege generation is not confirmed; placement must wait.",
        ));
    }
    Ok(())
}

pub fn available() -> bool {
    Path::new("/usr/bin/sudo").is_file()
        && Path::new("/usr/sbin/visudo").is_file()
        && Path::new("/usr/sbin/runuser").is_file()
        && Path::new("/etc/sudoers.d").is_dir()
}

fn sudoers(identity: &systemd::Identity) -> PathBuf {
    PathBuf::from(format!("/etc/sudoers.d/cairn-native-{}", identity.uid))
}

async fn remove_sudo_policies(directory: &Path, user: &str) -> Result<()> {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let grant = format!("{user} ALL=(ALL:ALL) NOPASSWD: ALL");
    let mut changed = false;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let Some(suffix) = name
            .to_str()
            .and_then(|name| name.strip_prefix("cairn-native-"))
        else {
            continue;
        };
        if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        if !entry.file_type().await?.is_file() {
            return Err(Error::conflict(
                "An unrecognized native sudo policy entry needs host-owner review.",
            ));
        }
        let rule = tokio::fs::read_to_string(entry.path()).await?;
        if rule.lines().any(|line| line == SUDO_HEADER) && rule.lines().any(|line| line == grant) {
            tokio::fs::remove_file(entry.path()).await?;
            changed = true;
        }
    }
    if changed {
        tokio::fs::File::open(directory).await?.sync_all().await?;
    }
    Ok(())
}

pub async fn sudo(identity: &systemd::Identity, enabled: bool) -> Result<()> {
    if !enabled {
        // A supported reinstall can assign this username a different UID.
        // Remove its owned grants across generations, not just today's filename.
        return remove_sudo_policies(Path::new("/etc/sudoers.d"), &identity.user).await;
    }
    let path = sudoers(identity);
    if !available() {
        return Err(Error::unavailable(
            "Install sudo with its standard sudoers.d include before enabling native administrator.",
        ));
    }
    let rule = format!(
        "{SUDO_HEADER}\n{} ALL=(ALL:ALL) NOPASSWD: ALL\n",
        identity.user
    );
    atomic_write(&path, rule.as_bytes()).await?;
    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o440)).await?;
    let checked = tokio::process::Command::new("/usr/sbin/visudo")
        .args(["--check", "--file", "/etc/sudoers"])
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .output()
        .await?;
    if !checked.status.success() {
        return Err(Error::unavailable(
            "The host sudo policy could not be validated.",
        ));
    }
    // Verify the ordinary workload identity can really obtain root, including
    // include and PAM policy, rather than advertising cosmetic sudo support.
    let checked = tokio::process::Command::new("/usr/sbin/runuser")
        .args([
            "-u",
            &identity.user,
            "--",
            "/usr/bin/sudo",
            "-n",
            "/usr/bin/true",
        ])
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .output()
        .await?;
    if !checked.status.success() {
        return Err(Error::unavailable(
            "Passwordless host root access could not be confirmed.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn protected_cleanup_removes_owned_grants_across_uid_changes_only() {
        let root = tempfile::tempdir().unwrap();
        for uid in [1000, 2000] {
            tokio::fs::write(
                root.path().join(format!("cairn-native-{uid}")),
                format!("{SUDO_HEADER}\ncairn-workload ALL=(ALL:ALL) NOPASSWD: ALL\n"),
            )
            .await
            .unwrap();
        }
        let other = root.path().join("cairn-native-3000");
        let manual = root.path().join("manual-policy");
        tokio::fs::write(
            &other,
            format!("{SUDO_HEADER}\nother-workload ALL=(ALL:ALL) NOPASSWD: ALL\n"),
        )
        .await
        .unwrap();
        tokio::fs::write(&manual, "cairn-workload ALL=(ALL:ALL) NOPASSWD: ALL\n")
            .await
            .unwrap();
        remove_sudo_policies(root.path(), "cairn-workload")
            .await
            .unwrap();
        assert!(!root.path().join("cairn-native-1000").exists());
        assert!(!root.path().join("cairn-native-2000").exists());
        assert!(other.exists());
        assert!(manual.exists());
        remove_sudo_policies(root.path(), "cairn-workload")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn protected_cleanup_refuses_unrecognized_entries_without_following_links() {
        let root = tempfile::tempdir().unwrap();
        let target = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(target.path(), root.path().join("cairn-native-1000")).unwrap();
        assert!(
            remove_sudo_policies(root.path(), "cairn-workload")
                .await
                .is_err()
        );
        assert!(target.path().exists());
    }

    #[test]
    fn unknown_modes_and_stale_or_unconfirmed_plans_fail_closed() {
        assert!(request(json!({ "mode": "root", "generation": null })).is_err());
        assert!(request(json!({ "mode": "administrator", "generation": null })).is_err());
        let protected = json!({ "mode": "protected", "generation": null, "status": "applied" });
        check_plan(&protected, &json!({})).unwrap();
        let generation = crate::config::id();
        let admin =
            json!({ "mode": "administrator", "generation": generation, "status": "applied" });
        assert!(check_plan(&admin, &json!({})).is_err());
        let plan = json!({ "nativeMode": { "mode": "administrator", "generation": generation } });
        check_plan(&admin, &plan).unwrap();
        for status in ["pending", "failed"] {
            let mut state = admin.clone();
            state["status"] = status.into();
            assert!(check_plan(&state, &plan).is_err());
        }
        assert!(check_plan(&protected, &plan).is_err());
    }
}
