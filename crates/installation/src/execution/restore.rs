//! Validation of a saved execution descriptor before a run resumes.
use super::{
    home::{configure_git, run_credentials},
    prepared::{Backend, Mount},
    read_only_workspaces, run_directory, uses_vm,
};
use crate::{
    config::Config,
    error::{Error, Result},
    plugins::run_credentials::RunCredentials,
    service::{isolated, run_projects},
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
    credentials: &RunCredentials,
    authorization: Option<&crate::plugins::run_credentials::Authorization>,
) -> Result<Value> {
    if let Some(authorization) = authorization {
        authorization.recheck().await?;
    }
    let microvm = uses_vm(run, config);
    let native = crate::native::policy::native(&run["snapshot"]["agent"]);
    if prepared["backend"] == "native" && !native || native && prepared["backend"] != "native" {
        return Err(Error::conflict(
            "Execution backend changed; start a new conversation.",
        ));
    }
    if microvm && !native && !is_firecracker(&prepared) {
        return Err(Error::conflict(
            "Unsupported saved execution backend; a new conversation is required.",
        ));
    }
    if (microvm || native) && !prepared["projectRoot"].is_string() {
        return Err(Error::bad("Missing saved execution project root."));
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
        if let Some(authorization) = authorization {
            authorization.recheck().await?;
        }
        let root = run_directory(run, config);
        run_credentials(&root.join("home"), &root, credentials).await?;
        if let Some(authorization) = authorization {
            authorization.recheck().await?;
        }
        // Helpers and identity follow the current grants too.
        configure_git(&root.join("home"), config, credentials).await?;
    }
    if let Some(authorization) = authorization {
        authorization.recheck().await?;
    }
    Ok(prepared)
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
    let managed = is_firecracker(prepared) || prepared["backend"] == Backend::Native.as_str();
    for project in run_projects(run) {
        if managed && !has_workspace(prepared, &project["id"]) {
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
