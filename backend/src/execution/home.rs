//! Private home directory of an isolated execution.
use super::workspace::{args, copy_tree, git, path};
use crate::{
    config::Config,
    error::{Error, Result},
    process::{bounded_output, codex_environment, command},
    provider::Provider,
    skills::{atomic_write, private_dir},
    validation::text,
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

/// Skills are installed below this guest directory.
const GUEST_SKILLS: &str = "/home/node/.agents/skills";

pub(super) async fn github_home(
    home: &Path,
    config: &Config,
    shared: bool,
    github: Option<&str>,
) -> Result<()> {
    let destination = home.join(".config/gh");
    if shared {
        let source = config.home.join(".config/gh");
        if tokio::fs::try_exists(&source).await? {
            copy_tree(&source, &destination, false).await?;
        }
        return Ok(());
    }
    let Some(token) = github.filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    private_dir(&destination).await?;
    let hosts = json!({
        "github.com": { "oauth_token": token, "git_protocol": "https" }
    });
    let hosts = serde_yaml_ng::to_string(&hosts).map_err(Error::internal)?;
    atomic_write(&destination.join("hosts.yml"), hosts.as_bytes()).await
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

async fn git_identity(
    config: &Config,
    home: &Path,
    github_available: bool,
) -> Option<(String, String)> {
    // Copy only the identity, never the host's credential helpers or other config.
    let mut env = codex_environment(config, &config.home.join(".codex"));
    if let Some(identity) = host_git_identity(&env).await {
        return Some(identity);
    }
    if !github_available {
        return None;
    }
    // Resolve the account from the same credentials installed for this run.
    env.insert("HOME".into(), home.to_string_lossy().into_owned());
    env.insert(
        "GH_CONFIG_DIR".into(),
        home.join(".config/gh").to_string_lossy().into_owned(),
    );
    for key in [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
    ] {
        env.remove(key);
    }
    let output = bounded_output(
        command(
            &config.gh_bin,
            &args(&["api", "--hostname", "github.com", "user"]),
            &env,
            None,
        ),
        Duration::from_secs(20),
        100_000,
    )
    .await
    .ok()?;
    if !output.success {
        return None;
    }
    let account: Value = serde_json::from_str(&output.stdout).ok()?;
    let login = text(&account, "login").trim();
    let id = account["id"].as_u64()?;
    if login.is_empty() || id == 0 {
        return None;
    }
    let name = text(&account, "name").trim();
    let name = if name.is_empty() { login } else { name };
    Some((
        name.to_owned(),
        format!("{id}+{login}@users.noreply.github.com"),
    ))
}

async fn install_codex_auth(home: &Path, config: &Config, codex_home: Option<&Path>) -> Result<()> {
    let auth = codex_home
        .map_or_else(|| config.home.join(".codex"), Path::to_owned)
        .join("auth.json");
    let managed = auth
        .parent()
        .is_some_and(|p| p.join("leo-managed-auth").exists());
    if managed {
        atomic_write(&home.join(".codex/leo-managed-auth"), b"1").await?;
    } else {
        let bytes = tokio::fs::read(&auth)
            .await
            .map_err(|_| Error::bad("Connect Codex before starting an isolated agent."))?;
        atomic_write(&home.join(".codex/auth.json"), &bytes).await?;
    }
    atomic_write(
        &home.join(".codex/config.toml"),
        b"cli_auth_credentials_store = \"file\"\n",
    )
    .await
}

async fn configure_git(
    home: &Path,
    config: &Config,
    github: Option<&str>,
    github_access: bool,
) -> Result<()> {
    let git_config = home.join(".gitconfig");
    let github_available = github.is_some() || github_access;
    // Re-preparing a run must also remove the old synthetic agent identity.
    atomic_write(&git_config, b"[user]\n\tuseConfigOnly = true\n").await?;
    let file = path(&git_config)?;
    if let Some((name, email)) = git_identity(config, home, github_available).await {
        for (key, value) in [("user.name", name), ("user.email", email)] {
            git(args(&["config", "--file", file, key, &value]), 10).await?;
        }
    }
    if github_available {
        let helper = args(&[
            "config",
            "--file",
            file,
            "credential.https://github.com.helper",
            "!gh auth git-credential",
        ]);
        git(helper, 10).await?;
    }
    Ok(())
}

/// Copies the selected skills into the home and points their snapshot at the guest copy.
async fn install_skills(home: &Path, skills: &Value) -> Result<Vec<Value>> {
    let mut installed = Vec::new();
    for (index, skill) in skills.as_array().into_iter().flatten().enumerate() {
        let relative = format!("{index}/{}", text(skill, "name"));
        let target = home.join(".agents/skills").join(&relative);
        private_dir(target.parent().unwrap()).await?;
        let source = Path::new(text(skill, "path"))
            .parent()
            .ok_or_else(|| Error::bad("Invalid selected skill path."))?;
        copy_tree(source, &target, true).await?;
        atomic_write(&target.join("SKILL.md"), text(skill, "content").as_bytes()).await?;
        let mut skill = skill.clone();
        skill["path"] = format!("{GUEST_SKILLS}/{relative}/SKILL.md").into();
        installed.push(skill);
    }
    Ok(installed)
}

pub(super) struct HomeOptions<'a> {
    pub github: Option<&'a str>,
    pub github_access: bool,
    pub codex_home: Option<&'a Path>,
}

/// Populates `home` with provider credentials, Git configuration and skills.
pub(super) async fn prepare_home(
    run: &Value,
    home: &Path,
    config: &Config,
    options: &HomeOptions<'_>,
) -> Result<Vec<Value>> {
    private_dir(&home.join(".codex")).await?;
    if Provider::of_run(run) == Provider::Codex {
        install_codex_auth(home, config, options.codex_home).await?;
    }
    github_home(home, config, options.github_access, options.github).await?;
    configure_git(home, config, options.github, options.github_access).await?;
    install_skills(home, &run["snapshot"]["skills"]).await
}
