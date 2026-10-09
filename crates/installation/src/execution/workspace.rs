//! Working copies of a run's projects.
use super::{
    prepared::{Workspace, WorkspaceKind},
    uses_vm,
};
use crate::{
    config::Config,
    error::{Error, Result},
    process::{Environment, bounded_output, command},
    service::isolated,
    skills::{private_dir, workspace},
    validation::text,
};
use serde_json::Value;
use std::{path::Path, time::Duration};

/// Provider state a repository must never contribute to a seed.
pub(super) const PRIVATE_STATE: [&str; 2] = [".codex", ".agents/skills"];

pub(super) async fn git(args: Vec<String>, timeout: u64) -> Result<String> {
    let mut env = std::env::vars().collect::<Environment>();
    env.insert("GIT_NO_LAZY_FETCH".into(), "0".into());
    let output = bounded_output(
        command("git", &args, &env, None),
        Duration::from_secs(timeout),
        100_000,
    )
    .await?;
    if !output.success {
        return Err(Error::bad(
            "Git could not prepare this workspace. Check the branch and repository permissions.",
        ));
    }
    Ok(output.stdout.trim().into())
}

pub(super) fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}

pub(super) fn path(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::bad("Workspace paths must use UTF-8."))
}

pub(super) async fn head(repository: &Path) -> Result<String> {
    git(args(&["-C", path(repository)?, "rev-parse", "HEAD"]), 10).await
}

pub(super) async fn copy_tree(source: &Path, target: &Path, reject_symlinks: bool) -> Result<()> {
    if target.starts_with(source) {
        return Err(Error::bad(
            "Private workspace storage must be outside the source directory.",
        ));
    }
    let mut queue = vec![(source.to_owned(), target.to_owned())];
    while let Some((source, target)) = queue.pop() {
        let metadata = tokio::fs::symlink_metadata(&source).await?;
        if metadata.is_symlink() {
            if reject_symlinks {
                return Err(Error::bad(
                    "Selected skill resources must not contain symbolic links.",
                ));
            }
            tokio::fs::symlink(tokio::fs::read_link(&source).await?, target).await?;
            continue;
        }
        if metadata.is_dir() {
            private_dir(&target).await?;
            let mut entries = tokio::fs::read_dir(&source).await?;
            while let Some(entry) = entries.next_entry().await? {
                queue.push((entry.path(), target.join(entry.file_name())));
            }
            continue;
        }
        if !metadata.is_file() {
            return Err(Error::bad(
                "Only regular files can be copied into an execution workspace.",
            ));
        }
        tokio::fs::copy(source, target).await?;
    }
    Ok(())
}

/// Removes a file, symlink or directory tree.
pub(super) async fn remove(path: &Path, is_dir: bool) -> Result<()> {
    if is_dir {
        tokio::fs::remove_dir_all(path).await?;
    } else {
        tokio::fs::remove_file(path).await?;
    }
    Ok(())
}

async fn add_worktree(
    run: &Value,
    project: &Value,
    source: &Path,
    target: &Path,
    generation: Option<&str>,
) -> Result<()> {
    let suffix = generation.map(|g| format!("-{g}")).unwrap_or_default();
    let branch = format!(
        "feat/run-{}-{}{suffix}",
        &text(run, "id")[..8],
        &text(project, "id")[..8],
    );
    let arguments = args(&[
        "-C",
        path(source)?,
        "worktree",
        "add",
        "-b",
        &branch,
        path(target)?,
        text(project, "baseBranch"),
    ]);
    git(arguments, 30).await?;
    Ok(())
}

/// Checks out one project for a run, in place or as a private copy under `root`.
pub(super) async fn prepare_project(
    run: &Value,
    project: &Value,
    config: &Config,
    root: &Path,
    single: bool,
    generation: Option<&str>,
) -> Result<Workspace> {
    let microvm = uses_vm(run, config);
    let is_isolated = microvm || isolated(&run["snapshot"]["agent"]);
    let source = workspace(Path::new(text(project, "path")), &config.workspace_roots).await?;
    if path(&source)? != text(project, "path") {
        return Err(Error::bad(
            "Project directory changed location after this run was queued.",
        ));
    }
    let project_id = text(project, "id").to_owned();
    let private_copy = microvm || run["snapshot"]["task"]["worktree"] == true;
    if !private_copy {
        return Ok(Workspace {
            project_id,
            path: source,
            kind: WorkspaceKind::Direct,
            revision: None,
        });
    }
    let target = if !is_isolated && single {
        root.to_owned()
    } else {
        root.join(&project_id)
    };
    let kind = if !source.join(".git").exists() {
        copy_tree(&source, &target, false).await?;
        WorkspaceKind::Copy
    } else if is_isolated || project["sourceMode"] != "local" {
        crate::project_git::clone(&source, &target, project, config).await?;
        WorkspaceKind::Clone
    } else {
        if target == root {
            tokio::fs::remove_dir(&root).await?;
        }
        add_worktree(run, project, &source, &target, generation).await?;
        WorkspaceKind::Worktree
    };
    let revision = if kind.has_git() {
        head(&target).await.ok()
    } else {
        None
    };
    Ok(Workspace {
        project_id,
        path: target,
        kind,
        revision,
    })
}

/// Deletes repository-provided provider state from a fresh checkout.
async fn strip_private_state(checkout: &Path) -> Result<()> {
    // A repository-controlled .agents symlink must never make cleanup follow
    // a parent outside this private seed on the manager filesystem.
    let agents = checkout.join(".agents");
    if tokio::fs::symlink_metadata(&agents)
        .await
        .is_ok_and(|m| m.is_symlink())
    {
        tokio::fs::remove_file(agents).await?;
    }
    for relative in PRIVATE_STATE {
        let item = checkout.join(relative);
        if let Ok(meta) = tokio::fs::symlink_metadata(&item).await {
            remove(&item, meta.is_dir()).await?;
        }
    }
    Ok(())
}

/// Prepare an immutable host seed. Guest working files are never copied back or replaced.
pub(super) async fn seed(
    run: &Value,
    project: &Value,
    config: &Config,
    root: &Path,
) -> Result<Workspace> {
    let destination = root.join(text(project, "id"));
    let kind = if Path::new(text(project, "path")).join(".git").exists() {
        WorkspaceKind::Clone
    } else {
        WorkspaceKind::Copy
    };
    let mut entry = Workspace {
        project_id: text(project, "id").to_owned(),
        path: destination.clone(),
        kind,
        revision: None,
    };
    if destination.exists() {
        if destination.join(".git").exists() {
            entry.revision = Some(head(&destination).await?);
        }
        return Ok(entry);
    }
    let staging = root.join(format!(".prepare-{}", text(project, "id")));
    if staging.exists() {
        tokio::fs::remove_dir_all(&staging).await?;
    }
    private_dir(&staging).await?;
    let prepared = prepare_project(run, project, config, &staging, false, None).await?;
    strip_private_state(&prepared.path).await?;
    entry.revision = prepared.revision;
    tokio::fs::rename(&prepared.path, &destination).await?;
    tokio::fs::remove_dir(&staging).await?;
    Ok(entry)
}
