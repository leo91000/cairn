//! Private home directory of an isolated execution.
use super::workspace::{args, copy_tree, git, path};
use crate::{
    config::Config,
    error::{Error, Result},
    plugins::run_credentials::{GitAccess, RunCredentials},
    process::{bounded_output, codex_environment, command},
    provider::Provider,
    skills::{atomic_write, private_dir},
    validation::text,
};
use serde_json::Value;
use std::{path::Path, time::Duration};

/// Skills are installed below this guest directory.
const GUEST_SKILLS: &str = "/home/node/.agents/skills";

/// Plugin run credentials (see `plugins::run_credentials`) live under these paths; the
/// record of every file written lives beside the home, outside the guest.
pub(crate) const GIT_CREDENTIALS: &str = ".config/cairn/git-credentials";
const CREDENTIAL_RECORD: &str = "credential-files.json";

/// Files plugin grants wrote in this run's home, including blanked ones.
pub async fn credential_files(run_directory: &Path) -> Vec<String> {
    let Ok(bytes) = tokio::fs::read(run_directory.join(CREDENTIAL_RECORD)).await else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<String>>(&bytes)
        .unwrap_or_default()
        .into_iter()
        .filter(|path| cairn_plugin_api::home_path(path))
        .collect()
}

/// Writes the granted instances' files and Git credentials into the home. Files a
/// withdrawn grant wrote earlier are emptied, so a resumed guest receives the blank.
pub(super) async fn run_credentials(
    home: &Path,
    run_directory: &Path,
    credentials: &RunCredentials,
) -> Result<()> {
    let mut written = credential_files(run_directory).await;
    for (relative, content) in &credentials.files {
        write_private(home, relative, content.as_bytes()).await?;
        if !written.contains(relative) {
            written.push(relative.clone());
        }
    }
    for relative in &written {
        if !credentials.files.iter().any(|(path, _)| path == relative) {
            write_private(home, relative, b"").await?;
        }
    }
    let store = credentials
        .git
        .iter()
        .filter_map(GitAccess::store_line)
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    write_private(home, GIT_CREDENTIALS, store.as_bytes()).await?;
    atomic_write(
        &run_directory.join(CREDENTIAL_RECORD),
        &serde_json::to_vec(&written)?,
    )
    .await
}

async fn write_private(home: &Path, relative: &str, bytes: &[u8]) -> Result<()> {
    let target = home.join(relative);
    let parent = target
        .parent()
        .ok_or_else(|| Error::bad("Invalid run credential path."))?;
    // A path the home redirected elsewhere is never followed.
    let mut current = home.to_owned();
    for part in Path::new(relative)
        .parent()
        .into_iter()
        .flat_map(Path::components)
    {
        current.push(part);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(Error::bad("A run credential path was redirected.")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    private_dir(parent).await?;
    if tokio::fs::symlink_metadata(&target)
        .await
        .is_ok_and(|metadata| !metadata.is_file())
    {
        return Err(Error::bad("A run credential path was redirected."));
    }
    atomic_write(&target, bytes).await
}

/// Removes every credential file of a finished run from its home.
pub async fn clear_run_credentials(run_directory: &Path) {
    let home = run_directory.join("home");
    for relative in credential_files(run_directory).await {
        let _ = tokio::fs::remove_file(home.join(relative)).await;
    }
    let _ = tokio::fs::remove_file(home.join(GIT_CREDENTIALS)).await;
}

async fn host_git_identity(env: &crate::process::Environment) -> Option<(String, String)> {
    let mut identity = Vec::new();
    for key in ["user.name", "user.email"] {
        let output = bounded_output(
            command(
                "git",
                &args(&["config", "--global", "--includes", "--get", key]),
                env,
                None,
            ),
            Duration::from_secs(10),
            10_000,
        )
        .await
        .ok();
        identity.push(
            output
                .filter(|o| o.success)
                .map(|o| o.stdout.trim().to_owned()),
        );
    }
    let [Some(name), Some(email)] = identity.as_slice() else {
        return None;
    };
    if name.is_empty() || email.is_empty() {
        return None;
    }
    Some((name.clone(), email.clone()))
}

async fn configure_git(home: &Path, config: &Config, credentials: &RunCredentials) -> Result<()> {
    let git_config = home.join(".gitconfig");
    // Re-preparing a run must also remove the old synthetic agent identity.
    atomic_write(&git_config, b"[user]\n\tuseConfigOnly = true\n").await?;
    let file = path(&git_config)?;
    // Copy only the identity, never the host's credential helpers or other config.
    let env = codex_environment(config, &config.home.join(".codex"));
    let identity = match host_git_identity(&env).await {
        Some(identity) => Some(identity),
        None => credentials.identity.clone(),
    };
    if let Some((name, email)) = identity {
        for (key, value) in [("user.name", name), ("user.email", email)] {
            git(args(&["config", "--file", file, key, &value]), 10).await?;
        }
    }
    let mut prefixes = credentials
        .git
        .iter()
        .map(|git| git.url.as_str())
        .collect::<Vec<_>>();
    prefixes.dedup();
    for prefix in prefixes {
        let key = format!("credential.{prefix}.helper");
        let helper = format!("store --file ~/{GIT_CREDENTIALS}");
        git(args(&["config", "--file", file, &key, &helper]), 10).await?;
    }
    Ok(())
}

/// Copies the selected skills into the home and points their snapshot at the guest copy.
/// Runs created before frozen bundles copy the live skill directory.
async fn install_skills(home: &Path, skills: &Value, config: &Config) -> Result<Vec<Value>> {
    let mut installed = Vec::new();
    for (index, skill) in skills.as_array().into_iter().flatten().enumerate() {
        let relative = format!("{index}/{}", text(skill, "name"));
        let target = home.join(".agents/skills").join(&relative);
        private_dir(target.parent().unwrap()).await?;
        let frozen = recorded_bundle(skill, config).await?;
        let live = Path::new(text(skill, "path"))
            .parent()
            .ok_or_else(|| Error::bad("Invalid selected skill path."))?;
        copy_tree(frozen.as_deref().unwrap_or(live), &target, true).await?;
        if frozen.is_none() {
            atomic_write(&target.join("SKILL.md"), text(skill, "content").as_bytes()).await?;
        }
        let mut skill = skill.clone();
        skill["path"] = format!("{GUEST_SKILLS}/{relative}/SKILL.md").into();
        installed.push(skill);
    }
    Ok(installed)
}

/// Shared runs receive writable working copies, leaving canonical frozen revisions intact.
pub(super) async fn frozen_skills(run: &Value, config: &Config) -> Result<Value> {
    let mut skills = run["snapshot"]["skills"].clone();
    let root = super::run_directory(run, config).join("skills");
    match tokio::fs::symlink_metadata(&root).await {
        Ok(metadata) => super::workspace::remove(&root, metadata.is_dir()).await?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    for (index, skill) in skills.as_array_mut().into_iter().flatten().enumerate() {
        if let Some(source) = recorded_bundle(skill, config).await? {
            let target = root.join(index.to_string()).join(text(skill, "name"));
            private_dir(target.parent().unwrap()).await?;
            copy_tree(&source, &target, true).await?;
            skill["path"] = target
                .join("SKILL.md")
                .to_string_lossy()
                .into_owned()
                .into();
        }
    }
    Ok(skills)
}

/// A live-directory fallback belongs only to snapshots created before bundles.
async fn recorded_bundle(skill: &Value, config: &Config) -> Result<Option<std::path::PathBuf>> {
    let Some(value) = skill.get("bundle") else {
        return Ok(None);
    };
    let path = value
        .as_str()
        .and_then(|revision| crate::skills::frozen(config, revision))
        .ok_or_else(|| Error::bad("Invalid frozen skill revision in the run snapshot."))?;
    if !tokio::fs::try_exists(&path).await? {
        return Err(Error::not_found(
            "The frozen skill bundle recorded by this run is missing.",
        ));
    }
    Ok(Some(path))
}

pub(super) struct HomeOptions<'a> {
    pub credentials: &'a RunCredentials,
    pub account_home: Option<&'a Path>,
}

/// Populates `home` with provider credentials, Git configuration and skills.
pub(super) async fn prepare_home(
    run: &Value,
    home: &Path,
    config: &Config,
    options: &HomeOptions<'_>,
) -> Result<Vec<Value>> {
    Provider::of_run(run)?
        .adapter()
        .prepare_isolated_home(config, home, options.account_home)
        .await?;
    let run_directory = super::run_directory(run, config);
    run_credentials(home, &run_directory, options.credentials).await?;
    configure_git(home, config, options.credentials).await?;
    install_skills(home, &run["snapshot"]["skills"], config).await
}
