//! Materialize new work without updating the registered source or an existing run.
use crate::{
    config::Config,
    error::{Error, Result},
    process::{bounded_output, codex_environment, command},
    validation::text,
};
use serde_json::Value;
use std::{path::Path, time::Duration};

fn path(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::bad("Workspace paths must use UTF-8."))
}

async fn git(config: &Config, args: &[&str]) -> Result<String> {
    let mut env = codex_environment(config, &config.home.join(".codex"));
    env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    env.insert("GIT_NO_LAZY_FETCH".into(), "0".into());
    let helper = format!(
        "credential.helper=!'{}' auth git-credential",
        config.gh_bin.replace('\'', "'\"'\"'")
    );
    let mut arguments = vec![
        "-c".to_owned(),
        "credential.helper=".into(),
        "-c".into(),
        helper,
    ];
    arguments.extend(args.iter().map(|s| (*s).to_owned()));
    let output = bounded_output(
        command("git", &arguments, &env, None),
        Duration::from_secs(180),
        100000,
    )
    .await?;
    if !output.success {
        return Err(Error::bad(
            "Could not fetch the project branch. Check its remote, branch and GitHub \
                connection; no stale fallback was used.",
        ));
    }
    Ok(output.stdout.trim().into())
}

fn remote(raw: &str) -> Result<String> {
    let raw = if let Some(repo) = raw
        .strip_prefix("git@github.com:")
        .or_else(|| raw.strip_prefix("ssh://git@github.com/"))
    {
        format!("https://github.com/{repo}")
    } else {
        raw.to_owned()
    };
    if raw.starts_with("http:") || raw.starts_with("https:") {
        let mut url = url::Url::parse(&raw).map_err(|_| Error::bad("Invalid project remote."))?;
        let _ = url.set_username("");
        let _ = url.set_password(None);
        return Ok(url.to_string());
    }
    Ok(raw)
}

pub async fn clone(source: &Path, target: &Path, project: &Value, config: &Config) -> Result<()> {
    let source = path(source)?;
    let target = path(target)?;
    let branch = text(project, "baseBranch");
    git(
        config,
        &["check-ref-format", &format!("refs/heads/{branch}")],
    )
    .await?;
    let origin = git(
        config,
        &["-C", source, "config", "--get", "remote.origin.url"],
    )
    .await
    .ok()
    .map(|s| remote(&s))
    .transpose()?;
    // Repositories without a remote remain usable as local projects. A failed
    // remote fetch must never silently turn into an outdated local snapshot.
    if project["sourceMode"] != "local"
        && let Some(origin) = origin.as_deref().filter(|s| !s.is_empty())
    {
        return git(
            config,
            &[
                "clone",
                "--no-local",
                "--single-branch",
                "--branch",
                branch,
                "--",
                origin,
                target,
            ],
        )
        .await
        .map(|_| ());
    }
    // Copy the object store first, then check out after restoring the real
    // promisor remote. upload-pack on a partial local clone cannot lazy-fetch.
    git(
        config,
        &[
            "clone",
            "--local",
            "--no-hardlinks",
            "--no-checkout",
            "--",
            source,
            target,
        ],
    )
    .await?;
    if let Some(origin) = origin {
        if git(
            config,
            &["-C", source, "config", "--get", "remote.origin.promisor"],
        )
        .await
        .is_ok_and(|value| value == "true")
        {
            git(
                config,
                &["-C", target, "config", "remote.origin.promisor", "true"],
            )
            .await?;
            git(
                config,
                &[
                    "-C",
                    target,
                    "config",
                    "remote.origin.partialclonefilter",
                    "blob:none",
                ],
            )
            .await?;
        }
        git(
            config,
            &["-C", target, "remote", "set-url", "origin", &origin],
        )
        .await?;
    }
    git(config, &["-C", target, "checkout", "--force", branch]).await?;
    Ok(())
}

/// Clone a selected GitHub repository into a newly allocated managed directory.
pub async fn import(config: &Config, origin: &str, target: &Path, branch: &str) -> Result<()> {
    git(
        config,
        &["check-ref-format", &format!("refs/heads/{branch}")],
    )
    .await?;
    git(
        config,
        &[
            "clone",
            "--single-branch",
            "--branch",
            branch,
            "--",
            origin,
            path(target)?,
        ],
    )
    .await
    .map(|_| ())
}
