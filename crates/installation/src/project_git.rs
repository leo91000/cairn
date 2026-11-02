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
    git_authorized(config, credentials, args, || std::future::ready(Ok(()))).await
}

async fn git_authorized<F, Fut>(
    config: &Config,
    credentials: &[GitAccess],
    args: &[&str],
    authorize: F,
) -> Result<String>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
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
    authorize().await?;
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

pub async fn revision(source: &Path, config: &Config) -> Result<String> {
    git(config, &[], &["-C", path(source)?, "rev-parse", "HEAD"]).await
}

pub(crate) async fn origin(source: &Path, config: &Config) -> Option<String> {
    git(
        config,
        &[],
        &[
            "-C",
            path(source).ok()?,
            "config",
            "--get",
            "remote.origin.url",
        ],
    )
    .await
    .ok()
    .filter(|origin| !origin.is_empty())
}

pub(crate) async fn remote_revision(
    source: &Path,
    project: &Value,
    config: &Config,
    credentials: &[GitAccess],
) -> Result<Option<(String, String)>> {
    remote_revision_authorized(source, project, config, credentials, || {
        std::future::ready(Ok(()))
    })
    .await
}

pub(crate) async fn remote_revision_authorized<F, Fut>(
    source: &Path,
    project: &Value,
    config: &Config,
    credentials: &[GitAccess],
    authorize: F,
) -> Result<Option<(String, String)>>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let origin = git_authorized(
        config,
        &[],
        &["-C", path(source)?, "config", "--get", "remote.origin.url"],
        &authorize,
    )
    .await;
    let Ok(origin) = origin else {
        authorize().await?;
        return Ok(None);
    };
    let remote = remote(&origin, credentials)?;
    let branch = format!("refs/heads/{}", text(project, "baseBranch"));
    git_authorized(
        config,
        credentials,
        &["check-ref-format", &branch],
        &authorize,
    )
    .await?;
    let result = git_authorized(
        config,
        credentials,
        &["ls-remote", "--exit-code", "--", &remote, &branch],
        &authorize,
    )
    .await?;
    let revision = result
        .split_whitespace()
        .next()
        .ok_or_else(|| Error::bad("Project branch is missing."))?;
    Ok(Some((revision.to_owned(), remote)))
}

pub async fn clone(
    source: &Path,
    target: &Path,
    project: &Value,
    config: &Config,
    credentials: &[GitAccess],
) -> Result<()> {
    clone_authorized(source, target, project, config, credentials, || {
        std::future::ready(Ok(()))
    })
    .await
}

pub(crate) async fn clone_authorized<F, Fut>(
    source: &Path,
    target: &Path,
    project: &Value,
    config: &Config,
    credentials: &[GitAccess],
    authorize: F,
) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let source = path(source)?;
    let target = path(target)?;
    let branch = text(project, "baseBranch");
    git_authorized(
        config,
        credentials,
        &["check-ref-format", &format!("refs/heads/{branch}")],
        &authorize,
    )
    .await?;
    let origin = git_authorized(
        config,
        &[],
        &["-C", source, "config", "--get", "remote.origin.url"],
        &authorize,
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
        return git_authorized(
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
            &authorize,
        )
        .await
        .map(|_| ());
    }
    // Copy the object store first, then check out after restoring the real
    // promisor remote. upload-pack on a partial local clone cannot lazy-fetch.
    git_authorized(
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
        &authorize,
    )
    .await?;
    if let Some(origin) = origin {
        if git_authorized(
            config,
            credentials,
            &["-C", source, "config", "--get", "remote.origin.promisor"],
            &authorize,
        )
        .await
        .is_ok_and(|value| value == "true")
        {
            git_authorized(
                config,
                credentials,
                &["-C", target, "config", "remote.origin.promisor", "true"],
                &authorize,
            )
            .await?;
            git_authorized(
                config,
                credentials,
                &[
                    "-C",
                    target,
                    "config",
                    "remote.origin.partialclonefilter",
                    "blob:none",
                ],
                &authorize,
            )
            .await?;
        }
        git_authorized(
            config,
            credentials,
            &["-C", target, "remote", "set-url", "origin", &origin],
            &authorize,
        )
        .await?;
    }
    git_authorized(
        config,
        credentials,
        &["-C", target, "checkout", "--force", branch],
        &authorize,
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
    import_authorized(config, credentials, origin, target, branch, || {
        std::future::ready(Ok(()))
    })
    .await
}

pub(crate) async fn import_authorized<F, Fut>(
    config: &Config,
    credentials: &[GitAccess],
    origin: &str,
    target: &Path,
    branch: &str,
    authorize: F,
) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    git_authorized(
        config,
        credentials,
        &["check-ref-format", &format!("refs/heads/{branch}")],
        &authorize,
    )
    .await?;
    git_authorized(
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
        &authorize,
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clone_rechecks_authority_after_reading_the_remote() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        std::fs::create_dir(&source).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["remote", "add", "origin", "https://127.0.0.1:1/revoked.git"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&source)
                    .env("HOME", root.path())
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let config: Config = serde_json::from_value(serde_json::json!({
            "dataDir": root.path().join("data"),
            "home": root.path(),
            "workspaceRoots": [root.path()],
            "publicUrl": "http://localhost:4310",
            "host": "127.0.0.1",
            "port": 0,
            "codexBin": "false",
            "concurrency": 1,
            "logger": false,
            "workerEnabled": false,
            "runnerUrl": "",
        }))
        .unwrap();
        let calls = AtomicUsize::new(0);
        let result = clone_authorized(
            &source,
            &target,
            &serde_json::json!({
                "sourceMode": "remote", "baseBranch": "main",
            }),
            &config,
            &[],
            || {
                std::future::ready(if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                    Ok(())
                } else {
                    Err(Error::forbidden("Execution revoked before clone."))
                })
            },
        )
        .await;
        assert_eq!(result.unwrap_err().status, 403);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(!target.exists());
    }

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
