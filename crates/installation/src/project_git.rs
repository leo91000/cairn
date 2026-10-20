//! Materialize new work without updating the registered source or an existing run.
use crate::{
    config::Config,
    error::{Error, Result},
    plugins::run_credentials::GitAccess,
    process::{bounded_output, codex_environment, command},
    skills::atomic_write,
    validation::text,
};
use serde_json::Value;
use std::{path::Path, time::Duration};

fn path(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::bad("Workspace paths must use UTF-8."))
}

/// Runs Git with only the given credentials, from a private credential-store file
/// removed afterwards. Credentials configured on the server are never used.
async fn git(config: &Config, credentials: &[GitAccess], args: &[&str]) -> Result<String> {
    let mut env = codex_environment(config, &config.home.join(".codex"));
    env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    env.insert("GIT_NO_LAZY_FETCH".into(), "0".into());
    let store = tempfile::Builder::new().prefix("cairn-git-").tempdir()?;
    let file = store.path().join("credentials");
    let lines = credentials
        .iter()
        .filter_map(GitAccess::store_line)
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    atomic_write(&file, lines.as_bytes()).await?;
    let helper = format!(
        "credential.helper=store --file='{}'",
        path(&file)?.replace('\'', "'\"'\"'")
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
            "Could not fetch the project branch. Check its remote, its branch and the plugin \
                instance that grants access to it; no stale fallback was used.",
        ));
    }
    Ok(output.stdout.trim().into())
}

/// The HTTPS form of a remote: `user@host:path`, `host:path` and `ssh://user@host/path`
/// become `https://host/path`. HTTP(S) remotes lose their user information.
pub fn https_remote(raw: &str) -> Option<String> {
    if let Ok(mut url) = url::Url::parse(raw)
        && raw.contains("://")
    {
        match url.scheme() {
            "http" | "https" => {
                let _ = url.set_username("");
                let _ = url.set_password(None);
                return Some(url.to_string());
            }
            "ssh" | "git" => {
                let host = url.host_str()?;
                return Some(format!("https://{host}{}", url.path()));
            }
            _ => return None,
        }
    }
    let (host, path) = raw.split_once(':')?;
    let host = host.rsplit('@').next().unwrap_or(host);
    let valid = !host.is_empty()
        && !path.is_empty()
        && !host.contains('/')
        && !path.starts_with('/')
        && host.contains('.');
    valid.then(|| format!("https://{host}/{path}"))
}

/// The remote Git uses: HTTPS when some credential serves it, the original otherwise.
fn remote(raw: &str, credentials: &[GitAccess]) -> Result<String> {
    let https = https_remote(raw);
    if let Some(https) = &https
        && (raw.starts_with("http:")
            || raw.starts_with("https:")
            || credentials.iter().any(|git| git.covers(https)))
    {
        return Ok(https.clone());
    }
    if raw.starts_with("http:") || raw.starts_with("https:") {
        return Err(Error::bad("Invalid project remote."));
    }
    Ok(raw.to_owned())
}

pub async fn clone(
    source: &Path,
    target: &Path,
    project: &Value,
    config: &Config,
    credentials: &[GitAccess],
) -> Result<()> {
    let source = path(source)?;
    let target = path(target)?;
    let branch = text(project, "baseBranch");
    git(
        config,
        credentials,
        &["check-ref-format", &format!("refs/heads/{branch}")],
    )
    .await?;
    let origin = git(
        config,
        &[],
        &["-C", source, "config", "--get", "remote.origin.url"],
    )
    .await
    .ok()
    .map(|s| remote(&s, credentials))
    .transpose()?;
    // Repositories without a remote remain usable as local projects. A failed
    // remote fetch must never silently turn into an outdated local snapshot.
    if project["sourceMode"] != "local"
        && let Some(origin) = origin.as_deref().filter(|s| !s.is_empty())
    {
        return git(
            config,
            credentials,
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
        credentials,
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
            credentials,
            &["-C", source, "config", "--get", "remote.origin.promisor"],
        )
        .await
        .is_ok_and(|value| value == "true")
        {
            git(
                config,
                credentials,
                &["-C", target, "config", "remote.origin.promisor", "true"],
            )
            .await?;
            git(
                config,
                credentials,
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
            credentials,
            &["-C", target, "remote", "set-url", "origin", &origin],
        )
        .await?;
    }
    git(
        config,
        credentials,
        &["-C", target, "checkout", "--force", branch],
    )
    .await?;
    Ok(())
}

/// Clone a repository a plugin resolved into a newly allocated managed directory.
pub async fn import(
    config: &Config,
    credentials: &[GitAccess],
    origin: &str,
    target: &Path,
    branch: &str,
) -> Result<()> {
    git(
        config,
        credentials,
        &["check-ref-format", &format!("refs/heads/{branch}")],
    )
    .await?;
    git(
        config,
        credentials,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_remotes_have_an_https_form() {
        for (raw, expected) in [
            (
                "git@forge.example:team/repo.git",
                "https://forge.example/team/repo.git",
            ),
            (
                "forge.example:team/repo.git",
                "https://forge.example/team/repo.git",
            ),
            (
                "ssh://git@forge.example/team/repo.git",
                "https://forge.example/team/repo.git",
            ),
            (
                "ssh://forge.example/team/repo.git",
                "https://forge.example/team/repo.git",
            ),
            (
                "https://user:pw@forge.example/team/repo.git",
                "https://forge.example/team/repo.git",
            ),
        ] {
            assert_eq!(https_remote(raw).as_deref(), Some(expected), "{raw}");
        }
        for raw in ["/srv/repo", "repo", "file:///srv/repo", "C:/repo"] {
            assert_eq!(https_remote(raw), None, "{raw}");
        }
    }

    #[test]
    fn ssh_remotes_switch_to_https_only_when_a_credential_serves_them() {
        let credentials = [GitAccess {
            url: "https://forge.example".into(),
            username: "x".into(),
            password: "y".into(),
        }];
        assert_eq!(
            remote("git@forge.example:team/repo.git", &credentials).unwrap(),
            "https://forge.example/team/repo.git"
        );
        assert_eq!(
            remote("git@other.example:team/repo.git", &credentials).unwrap(),
            "git@other.example:team/repo.git"
        );
        assert_eq!(
            remote("https://user@forge.example/team/repo.git", &[]).unwrap(),
            "https://forge.example/team/repo.git"
        );
    }
}
