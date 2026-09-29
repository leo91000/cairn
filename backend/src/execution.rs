//! Workspaces, homes and mounts an agent run executes with.
mod home;
mod prepared;
mod restore;
mod sandbox;
mod workspace;

pub use prepared::{Backend, Mount, Prepared, Workspace, WorkspaceKind};
pub use sandbox::Sandbox;

use crate::{
    auth::token,
    config::Config,
    error::{Error, Result},
    service::{isolated, policy, run_projects},
    skills::{atomic_write, private_dir},
    validation::text,
};
use home::HomeOptions;
use serde_json::Value;
use std::path::{Path, PathBuf};
use workspace::PRIVATE_STATE;

/// Guest home directory of an isolated execution.
const GUEST_HOME: &str = "/home/node";

/// Explicit access to remote nodes uses private VMs even on a control-only master.
pub fn uses_vm(run: &Value, config: &Config) -> bool {
    let access = policy(&run["snapshot"]["agent"]);
    let remote_nodes = access["nodes"]
        .as_array()
        .is_some_and(|nodes| nodes.iter().any(|node| node != crate::nodes::LOCAL_NODE_ID));
    !config.runner_url.is_empty() || access["nodes"].is_null() || remote_nodes
}

pub async fn secret(directory: &Path, name: &str) -> Result<String> {
    use tokio::io::AsyncWriteExt;
    let file = directory.join(name);
    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)
        .await
    {
        Ok(mut file) => {
            file.write_all(token().as_bytes()).await?;
            file.sync_all().await?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    Ok(tokio::fs::read_to_string(file).await?.trim().to_owned())
}

fn run_directory(run: &Value, config: &Config) -> PathBuf {
    config.data_dir.join("runs").join(text(run, "id"))
}

/// Workspace mounts follow the agent's read-only sandbox.
fn read_only_workspaces(run: &Value) -> bool {
    Sandbox::is_read_only(&policy(&run["snapshot"]["agent"])["sandbox"])
}

/// Prepare an immutable host seed. Guest working files are never copied back or replaced.
pub async fn project_seed(
    run: &Value,
    project: &Value,
    config: &Config,
    root: &Path,
) -> Result<Value> {
    let entry = workspace::seed(run, project, config, root).await?;
    Ok(serde_json::to_value(entry)?)
}

pub async fn prepare(
    run: &Value,
    config: &Config,
    github: Option<&str>,
    codex_home: Option<&Path>,
    generation: Option<&str>,
) -> Result<Value> {
    let prepared = prepare_execution(run, config, github, codex_home, generation).await?;
    Ok(serde_json::to_value(prepared)?)
}

pub async fn prepare_execution(
    run: &Value,
    config: &Config,
    github: Option<&str>,
    codex_home: Option<&Path>,
    generation: Option<&str>,
) -> Result<Prepared> {
    let directory = run_directory(run, config);
    let microvm = uses_vm(run, config);
    let is_isolated = microvm || isolated(&run["snapshot"]["agent"]);
    if is_isolated && !microvm {
        return Err(Error::unavailable(
            "Isolated runner is not configured. This agent will not fall back to shared execution.",
        ));
    }
    let root =
        directory.join(generation.map_or_else(|| "workspace".into(), |g| format!("workspace-{g}")));
    private_dir(&root).await?;
    // A VM starts with its task project; others are opened on demand.
    let projects = run_projects(run)
        .into_iter()
        .filter(|project| !microvm || project["id"] == run["snapshot"]["task"]["projectId"])
        .collect::<Vec<_>>();
    let single = projects.len() == 1;
    let read_only = read_only_workspaces(run);
    let mut workspaces = Vec::new();
    let mut mounts = Vec::new();
    for project in &projects {
        let entry =
            workspace::prepare_project(run, project, config, &root, single, generation).await?;
        if is_isolated {
            mounts.push(Mount::same(&entry.path, read_only));
        }
        workspaces.push(entry);
    }
    let cwd = match workspaces.as_slice() {
        [only] => only.path.clone(),
        _ => root.clone(),
    };
    let output_directory = directory.join("output");
    private_dir(&output_directory).await?;
    let output = output_directory.join("result.md");
    if !is_isolated {
        return Ok(Prepared {
            project_root: None,
            cwd,
            output,
            workspaces,
            isolated: false,
            mounts,
            skills: run["snapshot"]["skills"].clone(),
            backend: None,
        });
    }
    let home = directory.join("home");
    let options = HomeOptions {
        github,
        github_access: policy(&run["snapshot"]["agent"])["github"] == true,
        codex_home,
    };
    let skills = home::prepare_home(run, &home, config, &options).await?;
    mounts.insert(0, Mount::same(&root, false));
    mounts.push(Mount::new(&home, GUEST_HOME, false));
    mounts.push(Mount::same(&output_directory, false));
    // Hide repository-provided provider state behind an empty read-only directory.
    let empty = directory.join("empty");
    private_dir(&empty).await?;
    for workspace in &workspaces {
        for relative in PRIVATE_STATE {
            let target = workspace.path.join(relative);
            if tokio::fs::try_exists(&target).await? {
                mounts.push(Mount::new(&empty, target, true));
            }
        }
    }
    Ok(Prepared {
        project_root: Some(root),
        cwd,
        output,
        workspaces,
        isolated: true,
        mounts,
        skills: Value::Array(skills),
        backend: Some(Backend::Firecracker),
    })
}

pub async fn codex_home(config: &Config, home: &Path) -> Result<()> {
    private_dir(home).await?;
    let source = config.home.join(".codex");
    for file in ["config.toml", "AGENTS.md"] {
        match tokio::fs::read(source.join(file)).await {
            Ok(bytes) => atomic_write(&home.join(file), &bytes).await?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    for directory in ["rules", "skills", "plugins"] {
        if !tokio::fs::try_exists(source.join(directory)).await? {
            continue;
        }
        match tokio::fs::symlink(source.join(directory), home.join(directory)).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

pub async fn restore(
    run: &Value,
    prepared: Value,
    config: &Config,
    github: Option<&str>,
) -> Result<Value> {
    restore::restore(run, prepared, config, github).await
}
