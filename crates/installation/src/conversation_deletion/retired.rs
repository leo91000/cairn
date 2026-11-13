//! Separate a stopped environment's manager files from the next environment.
//! Only namespace changes belong to admission; erasure keeps retrying independently.
use super::*;
use std::path::PathBuf;

pub(crate) const MANAGER_NAMESPACE: &str = ".cairn-manager-environment";

pub(crate) async fn mark_manager_namespace(s: &Service, run: &str) -> Result<()> {
    let root = s.config.data_dir.join("runs").join(run);
    crate::skills::private_dir(&root).await?;
    crate::skills::atomic_write(&root.join(MANAGER_NAMESPACE), run.as_bytes()).await?;
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::File::open(&root)?.sync_all()?;
        std::fs::File::open(root.parent().unwrap())?.sync_all()?;
        std::fs::File::open(root.parent().unwrap().parent().unwrap())?.sync_all()
    })
    .await
    .map_err(Error::internal)??;
    Ok(())
}

pub(crate) fn directory(s: &Service, retirement: &str) -> PathBuf {
    s.config
        .data_dir
        .join("retired-environments")
        .join(retirement)
}

async fn quarantine(source: &Path, target: &Path) -> Result<()> {
    match (
        tokio::fs::symlink_metadata(source).await,
        tokio::fs::symlink_metadata(target).await,
    ) {
        (Err(error), _) if error.kind() == std::io::ErrorKind::NotFound => {}
        (Err(error), _) => return Err(error.into()),
        (Ok(_), Ok(_)) => {
            return Err(Error::conflict(
                "The retired environment already has a detached directory.",
            ));
        }
        (Ok(_), Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::rename(source, target).await?;
        }
        (Ok(_), Err(error)) => return Err(error.into()),
    }
    // Also sync a replay after a rename whose caller was interrupted before its
    // receipt. Admission must never commit ahead of the namespace change.
    let source_parent = source.parent().unwrap().to_owned();
    let target_parent = target.parent().unwrap().to_owned();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        match std::fs::File::open(source_parent) {
            Ok(directory) => directory.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::File::open(&target_parent)?.sync_all()?;
        std::fs::File::open(target_parent.parent().unwrap())?.sync_all()?;
        std::fs::File::open(target_parent.parent().unwrap().parent().unwrap())?.sync_all()
    })
    .await
    .map_err(Error::internal)??;
    Ok(())
}

/// Caller holds preparation, publication and publication-reader ownership.
pub(crate) async fn detach(s: &Service, run: &str, retirement: &str) -> Result<()> {
    crate::validation::uuid(run)?;
    crate::validation::uuid(retirement)?;
    // The bundled VM controller shares this manager's data volume. Its older
    // unqualified mutations must be fenced by upgrade before namespace reuse.
    super::ensure_local_retirement_protocol(s).await?;
    let retired = directory(s, retirement);
    crate::skills::private_dir(&retired).await?;
    for kind in ["runs", "node-backups"] {
        let source = s.config.data_dir.join(kind).join(run);
        if kind == "runs" && source.join(MANAGER_NAMESPACE).exists() && retired.join(kind).exists()
        {
            continue;
        }
        quarantine(&source, &retired.join(kind)).await?;
    }
    mark_manager_namespace(s, run).await?;
    Ok(())
}

async fn git_worktree(
    project: &Path,
    arguments: &[&std::ffi::OsStr],
) -> Result<std::process::Output> {
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new("git")
            .arg("-C")
            .arg(project)
            .args(arguments)
            .env_clear()
            .envs(std::env::vars().filter(|(key, _)| !crate::process::storage_key(key)))
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| Error::unavailable("Retired worktree cleanup will retry."))?
    .map_err(Error::from)
}

async fn detach_worktree(project: &Path, old: &Path, retired: &Path) -> Result<()> {
    use std::ffi::OsStr;
    // Verify the registration still belongs to the old tree before repairing its
    // moved path. A later tree can reuse a Git administration directory's name.
    let common = git_worktree(
        project,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--git-common-dir"),
        ],
    )
    .await?;
    if !common.status.success() {
        return Err(Error::unavailable(
            "The retired worktree repository is unavailable.",
        ));
    }
    let common = String::from_utf8(common.stdout).map_err(Error::internal)?;
    let common = tokio::fs::canonicalize(Path::new(common.trim()).join("worktrees")).await?;
    let git = tokio::fs::read_to_string(retired.join(".git")).await?;
    let registration = git
        .trim()
        .strip_prefix("gitdir: ")
        .ok_or_else(|| Error::bad("Invalid retired worktree registration."))?;
    let registration = tokio::fs::canonicalize(registration).await?;
    if registration.parent() != Some(common.as_path()) {
        return Err(Error::bad(
            "Retired worktree registration is outside its repository.",
        ));
    }
    let recorded = tokio::fs::read_to_string(registration.join("gitdir")).await?;
    if Path::new(recorded.trim()) != old.join(".git")
        && Path::new(recorded.trim()) != retired.join(".git")
    {
        return Err(Error::conflict(
            "The retired worktree registration has changed owner.",
        ));
    }
    let repair = git_worktree(
        project,
        &[
            OsStr::new("worktree"),
            OsStr::new("repair"),
            retired.as_os_str(),
        ],
    )
    .await?;
    if !repair.status.success() {
        return Err(Error::unavailable(
            "The retired worktree registration will retry.",
        ));
    }
    super::remove_worktree(project, retired).await?;
    if common.is_dir() {
        tokio::fs::File::open(&common).await?.sync_all().await?;
    }
    tokio::fs::File::open(common.parent().unwrap())
        .await?
        .sync_all()
        .await?;
    Ok(())
}

/// Every target is relative to the captured environment, never the new run root.
pub(crate) async fn erase(
    s: &Service,
    run: &str,
    retirement: &str,
    workspaces: &[Value],
    projects: &[Value],
    publications: &crate::nodes::publication::RetiredPublications,
) -> Result<()> {
    let retired = directory(s, retirement);
    crate::nodes::publication::retire_detached_environment(
        s,
        run,
        publications,
        &retired.join("node-backups"),
    )
    .await?;
    let old_root = s.config.data_dir.join("runs").join(run);
    let moved_root = retired.join("runs");
    for workspace in workspaces
        .iter()
        .filter(|workspace| workspace["kind"] == "worktree")
    {
        let old = Path::new(text(workspace, "path"));
        let relative = old
            .strip_prefix(&old_root)
            .map_err(|_| Error::bad("Invalid conversation worktree path."))?;
        let target = moved_root.join(relative);
        if !tokio::fs::try_exists(&target).await? {
            continue;
        }
        if tokio::fs::canonicalize(&target).await? != target {
            return Err(Error::bad("Retired worktree path is not canonical."));
        }
        let Some(project) = projects
            .iter()
            .find(|project| project["id"] == workspace["projectId"])
        else {
            continue;
        };
        let project = Path::new(text(project, "path"));
        if project.is_dir() {
            detach_worktree(project, old, &target).await?;
        }
    }
    super::remove(&retired).await?;
    match tokio::fs::File::open(retired.parent().unwrap()).await {
        Ok(parent) => parent.sync_all().await.map_err(Error::from),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(project: &Path, arguments: &[&str]) {
        let result = std::process::Command::new("git")
            .arg("-C")
            .arg(project)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[tokio::test]
    async fn quarantined_worktree_cleanup_preserves_the_replacement_tree() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::common::config(root.path()))
            .await
            .unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        git(&project, &["init", "-b", "main"]);
        git(
            &project,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "--allow-empty",
                "-m",
                "Initial",
            ],
        );
        let (run, retirement) = (crate::config::id(), crate::config::id());
        let manager = service.config.data_dir.join("runs").join(&run);
        let old = manager.join("workspace-old");
        std::fs::create_dir_all(&manager).unwrap();
        git(
            &project,
            &[
                "worktree",
                "add",
                "-b",
                "old-environment",
                old.to_str().unwrap(),
                "main",
            ],
        );
        std::fs::write(old.join("old.txt"), b"old work").unwrap();
        detach(&service, &run, &retirement).await.unwrap();
        // Restart between namespace rename and the logical writer commit.
        detach(&service, &run, &retirement).await.unwrap();
        let next = manager.join("workspace-new");
        std::fs::create_dir_all(&manager).unwrap();
        git(
            &project,
            &[
                "worktree",
                "add",
                "-b",
                "new-environment",
                next.to_str().unwrap(),
                "main",
            ],
        );
        std::fs::write(next.join("new.txt"), b"new work").unwrap();
        let workspaces = vec![json!({ "kind":"worktree", "path":old, "projectId":"project" })];
        let projects = vec![json!({ "id":"project", "path":project })];
        erase(
            &service,
            &run,
            &retirement,
            &workspaces,
            &projects,
            &crate::nodes::publication::RetiredPublications::default(),
        )
        .await
        .unwrap();
        assert!(!directory(&service, &retirement).exists());
        assert_eq!(std::fs::read(next.join("new.txt")).unwrap(), b"new work");
        git(&next, &["status", "--porcelain"]);
        let entries = std::process::Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        let entries = String::from_utf8(entries.stdout).unwrap();
        assert!(entries.contains(next.to_str().unwrap()));
        assert!(!entries.contains(old.to_str().unwrap()));
        assert!(!entries.contains(directory(&service, &retirement).to_str().unwrap()));
    }
}
