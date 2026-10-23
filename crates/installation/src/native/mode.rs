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

pub async fn sudo(identity: &systemd::Identity, enabled: bool) -> Result<()> {
    let path = sudoers(identity);
    if !enabled {
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {
                tokio::fs::File::open(path.parent().unwrap())
                    .await?
                    .sync_all()
                    .await?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    }
    if !available() {
        return Err(Error::unavailable(
            "Install sudo with its standard sudoers.d include before enabling native administrator.",
        ));
    }
    let rule = format!(
        "# Cairn native administrator; protected units retain NoNewPrivileges.\n{} ALL=(ALL:ALL) NOPASSWD: ALL\n",
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
