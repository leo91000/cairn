//! Validation and upgrade of a saved execution descriptor before a run resumes.
use super::{
    home::github_home,
    prepare_execution,
    prepared::{Backend, Mount, Prepared},
    read_only_workspaces, run_directory, uses_vm,
    workspace::{args, copy_tree, git, head, path, remove, seed},
};
use crate::{
    config::Config,
    error::{Error, Result},
    service::{isolated, policy, run_projects},
    skills::workspace,
    validation::text,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn is_firecracker(prepared: &Value) -> bool {
    prepared["backend"] == Backend::Firecracker.as_str()
}

fn has_workspace(prepared: &Value, project_id: &Value) -> bool {
    prepared["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|w| w["projectId"] == *project_id)
}

fn saved_list<'a>(prepared: &'a mut Value, key: &str) -> Result<&'a mut Vec<Value>> {
    prepared[key]
        .as_array_mut()
        .ok_or_else(|| Error::bad("Invalid saved execution workspace."))
}

pub(super) async fn restore(
    run: &Value,
    mut prepared: Value,
    config: &Config,
    github: Option<&str>,
) -> Result<Value> {
    let microvm = uses_vm(run, config);
    if microvm && !is_firecracker(&prepared) {
        let migrated = migrate_to_vm(run, &prepared, config, github).await?;
        return Ok(serde_json::to_value(migrated)?);
    }
    if is_firecracker(&prepared) && !prepared["projectRoot"].is_string() {
        let project_root = infer_project_root(run, &prepared, config)?;
        prepared["projectRoot"] = serde_json::to_value(project_root)?;
    }
    if prepared["projectRoot"].is_string() {
        adopt_opened_projects(run, &mut prepared)?;
    }
    let expected_isolation = microvm || isolated(&run["snapshot"]["agent"]);
    if prepared["isolated"] != expected_isolation {
        return Err(Error::conflict(
            "Execution isolation changed; this run cannot be resumed.",
        ));
    }
    let is_isolated = prepared["isolated"] == true;
    if is_isolated && !microvm {
        return Err(Error::unavailable("The isolated runner is not configured."));
    }
    verify_project_locations(run, &prepared, config).await?;
    verify_saved_paths(run, &prepared, config).await?;
    if is_isolated {
        let root = run_directory(run, config);
        let github_access = policy(&run["snapshot"]["agent"])["github"] == true;
        github_home(&root.join("home"), config, github_access, github).await?;
    }
    Ok(prepared)
}

/// One-time migration of saved container/shared conversations. Keep the old
/// checkout intact and seed the new private disk with its uncommitted files.
async fn migrate_to_vm(
    run: &Value,
    prepared: &Value,
    config: &Config,
    github: Option<&str>,
) -> Result<Prepared> {
    let directory = run_directory(run, config);
    let old_home = directory.join(if prepared["isolated"] == true {
        "home/.codex"
    } else {
        "codex"
    });
    let generation = format!("microvm-{}", &crate::config::id()[..8]);
    let mut migrated =
        prepare_execution(run, config, github, Some(&old_home), Some(&generation)).await?;
    let saved = prepared["workspaces"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    for old in saved {
        let known = migrated
            .workspaces
            .iter()
            .any(|w| old["projectId"] == w.project_id);
        if known {
            continue;
        }
        let project = run_projects(run)
            .into_iter()
            .find(|p| p["id"] == old["projectId"])
            .ok_or_else(|| Error::bad("Saved project is no longer authorized."))?;
        let project_root = migrated.project_root.clone().unwrap_or_default();
        let entry = seed(run, &project, config, &project_root).await?;
        migrated
            .mounts
            .push(Mount::same(&entry.path, read_only_workspaces(run)));
        migrated.workspaces.push(entry);
    }
    for old in saved {
        let Some(new) = migrated
            .workspaces
            .iter()
            .find(|new| old["projectId"] == new.project_id)
        else {
            continue;
        };
        let source = Path::new(text(old, "path"));
        import_working_files(run, &directory, source, &new.path).await?;
    }
    let new_home = directory.join("home/.codex");
    if old_home != new_home {
        for relative in ["sessions", "archived_sessions", "session_index.jsonl"] {
            let source = old_home.join(relative);
            if source.exists() {
                copy_tree(&source, &new_home.join(relative), false).await?;
            }
        }
    }
    Ok(migrated)
}

/// Replace the initial clone's working files so tracked deletions remain
/// deleted. A prior independent clone also keeps its Git history.
async fn import_working_files(
    run: &Value,
    directory: &Path,
    old: &Path,
    target: &Path,
) -> Result<()> {
    let source = workspace(old, &[directory.to_owned(), old.to_owned()]).await?;
    let copy_git = tokio::fs::symlink_metadata(source.join(".git"))
        .await
        .is_ok_and(|m| m.is_dir());
    if !copy_git && source.join(".git").exists() {
        import_linked_worktree(run, &source, target).await?;
    }
    let skip = |name: &std::ffi::OsStr| name == ".git" && !copy_git;
    let mut targets = tokio::fs::read_dir(target).await?;
    while let Some(entry) = targets.next_entry().await? {
        if skip(&entry.file_name()) {
            continue;
        }
        remove(&entry.path(), entry.file_type().await?.is_dir()).await?;
    }
    let mut entries = tokio::fs::read_dir(&source).await?;
    while let Some(entry) = entries.next_entry().await? {
        if skip(&entry.file_name()) {
            continue;
        }
        copy_tree(&entry.path(), &target.join(entry.file_name()), false).await?;
    }
    Ok(())
}

/// A linked worktree's .git file points outside the guest. Its branch and
/// commits must become independent before import.
async fn import_linked_worktree(run: &Value, source: &Path, target: &Path) -> Result<()> {
    let head = head(source).await?;
    let branch = git(
        args(&["-C", path(source)?, "symbolic-ref", "--short", "HEAD"]),
        10,
    )
    .await
    .unwrap_or_else(|_| format!("feat/recovered-{}", &text(run, "id")[..8]));
    git(
        args(&["-C", path(target)?, "fetch", path(source)?, &head]),
        120,
    )
    .await?;
    git(
        args(&["-C", path(target)?, "checkout", "-B", &branch, &head]),
        30,
    )
    .await?;
    Ok(())
}

/// Descriptors saved before `projectRoot` existed derive it from their workspaces.
fn infer_project_root(run: &Value, prepared: &Value, config: &Config) -> Result<PathBuf> {
    let cwd = Path::new(text(prepared, "cwd"));
    let in_project = prepared["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|w| w["path"] == prepared["cwd"]);
    let project_root = if in_project {
        cwd.parent()
            .ok_or_else(|| Error::bad("Invalid saved project root."))?
    } else {
        cwd
    };
    let directory = run_directory(run, config);
    if !project_root.starts_with(&directory) || project_root == directory {
        return Err(Error::bad("Invalid saved project root."));
    }
    Ok(project_root.to_owned())
}

/// Adds projects opened during the run (`run.workspaces`) to the descriptor.
fn adopt_opened_projects(run: &Value, prepared: &mut Value) -> Result<()> {
    let project_root = PathBuf::from(text(prepared, "projectRoot"));
    let read_only = read_only_workspaces(run);
    for entry in run["workspaces"].as_array().into_iter().flatten() {
        if has_workspace(prepared, &entry["projectId"]) {
            continue;
        }
        let project_id = text(entry, "projectId");
        crate::validation::uuid(project_id)?;
        let path = Path::new(text(entry, "path"));
        let authorized = crate::project_workspaces::catalog(run)
            .iter()
            .any(|p| p["id"] == project_id);
        if path != project_root.join(project_id) || !authorized {
            return Err(Error::bad("Invalid saved project workspace."));
        }
        saved_list(prepared, "workspaces")?.push(entry.clone());
        let mount = serde_json::to_value(Mount::same(path, read_only))?;
        saved_list(prepared, "mounts")?.push(mount);
    }
    Ok(())
}

async fn verify_project_locations(run: &Value, prepared: &Value, config: &Config) -> Result<()> {
    let firecracker = is_firecracker(prepared);
    for project in run_projects(run) {
        if firecracker && !has_workspace(prepared, &project["id"]) {
            continue;
        }
        let path = Path::new(text(&project, "path"));
        if workspace(path, &config.workspace_roots).await? != path {
            return Err(Error::conflict(
                "A project moved outside its permitted location.",
            ));
        }
    }
    Ok(())
}

/// Every saved path must still resolve inside the run or one of its direct projects.
async fn verify_saved_paths(run: &Value, prepared: &Value, config: &Config) -> Result<()> {
    let projects = run_projects(run);
    let mut allowed = vec![run_directory(run, config)];
    let workspaces = prepared["workspaces"]
        .as_array()
        .ok_or_else(|| Error::bad("Invalid saved execution workspace."))?;
    for w in workspaces.iter().filter(|w| w["kind"] == "direct") {
        let project = projects
            .iter()
            .find(|p| p["id"] == w["projectId"])
            .ok_or_else(|| {
                Error::conflict("A saved workspace is no longer in the agent’s project scope.")
            })?;
        allowed.push(PathBuf::from(text(project, "path")));
    }
    let output_directory = Path::new(text(prepared, "output"))
        .parent()
        .ok_or_else(|| Error::bad("Invalid output directory."))?;
    let mut paths = vec![
        PathBuf::from(text(prepared, "cwd")),
        output_directory.to_owned(),
    ];
    paths.extend(workspaces.iter().map(|w| PathBuf::from(text(w, "path"))));
    paths.extend(
        prepared["mounts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| PathBuf::from(text(m, "source"))),
    );
    paths.sort();
    paths.dedup();
    for source in paths {
        if workspace(&source, &allowed).await? != source {
            return Err(Error::conflict(
                "A saved workspace changed location. Working files were preserved.",
            ));
        }
    }
    Ok(())
}
