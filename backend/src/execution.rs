use crate::{
    auth::token,
    config::Config,
    error::{Error, Result},
    process::{Environment, bounded_output, codex_environment, command},
    service::{isolated, policy, run_projects},
    skills::{atomic_write, private_dir, workspace},
    validation::text,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
/// Explicit access to remote nodes uses private VMs even on a control-only master.
pub fn uses_vm(run: &Value, config: &Config) -> bool {
    let access = policy(&run["snapshot"]["agent"]);
    !config.runner_url.is_empty()
        || access["nodes"].is_null()
        || access["nodes"]
            .as_array()
            .is_some_and(|nodes| nodes.iter().any(|node| node != crate::nodes::LOCAL_NODE_ID))
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
async fn git(args: Vec<String>, timeout: u64) -> Result<String> {
    let mut env = std::env::vars().collect::<Environment>();
    env.insert("GIT_NO_LAZY_FETCH".into(), "0".into());
    let output = bounded_output(
        command("git", &args, &env, None),
        Duration::from_secs(timeout),
        100000,
    )
    .await?;
    if !output.success {
        return Err(Error::bad(
            "Git could not prepare this workspace. Check the branch and repository permissions.",
        ));
    }
    Ok(output.stdout.trim().into())
}
fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}
fn path(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::bad("Workspace paths must use UTF-8."))
}
async fn copy_tree(source: &Path, target: &Path, reject_symlinks: bool) -> Result<()> {
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
async fn github_home(
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
    }
    if !shared && let Some(token) = github.filter(|s| !s.is_empty()) {
        private_dir(&destination).await?;
        atomic_write(
            &destination.join("hosts.yml"),
            serde_yaml_ng::to_string(&json!({
            "github.com":{
            "oauth_token":token,"git_protocol":"https"}
            }
            ))
            .map_err(Error::internal)?
            .as_bytes(),
        )
        .await?;
    }
    Ok(())
}

async fn git_identity(
    config: &Config,
    home: &Path,
    github_available: bool,
) -> Option<(String, String)> {
    // Copy only the identity, never the host's credential helpers or other config.
    let env = codex_environment(config, &config.home.join(".codex"));
    let mut identity = Vec::new();
    for key in ["user.name", "user.email"] {
        let output = bounded_output(
            command(
                "git",
                &args(&["config", "--global", "--includes", "--get", key]),
                &env,
                None,
            ),
            Duration::from_secs(10),
            10000,
        )
        .await
        .ok();
        identity.push(
            output
                .filter(|o| o.success)
                .map(|o| o.stdout.trim().to_owned()),
        );
    }
    if let [Some(name), Some(email)] = identity.as_slice()
        && !name.is_empty()
        && !email.is_empty()
    {
        return Some((name.clone(), email.clone()));
    }
    if !github_available {
        return None;
    }
    // Resolve the account from the same credentials installed for this run.
    let mut env = env;
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
        100000,
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
    Some((
        if name.is_empty() { login } else { name }.to_owned(),
        format!("{id}+{login}@users.noreply.github.com"),
    ))
}

async fn prepare_project(
    run: &Value,
    project: &Value,
    config: &Config,
    root: &Path,
    single: bool,
    generation: Option<&str>,
) -> Result<Value> {
    let microvm = uses_vm(run, config);
    let is_isolated = microvm || isolated(&run["snapshot"]["agent"]);
    let source = workspace(Path::new(text(project, "path")), &config.workspace_roots).await?;
    if path(&source)? != text(project, "path") {
        return Err(Error::bad(
            "Project directory changed location after this run was queued.",
        ));
    }
    let mut target = source.clone();
    let mut kind = "direct";
    if microvm || run["snapshot"]["task"]["worktree"] == true {
        target = if !is_isolated && single {
            root.to_owned()
        } else {
            root.join(text(project, "id"))
        };
        if !source.join(".git").exists() {
            kind = "copy";
            copy_tree(&source, &target, false).await?;
        } else if is_isolated || project["sourceMode"] != "local" {
            kind = "clone";
            crate::project_git::clone(&source, &target, project, config).await?;
        } else {
            kind = "worktree";
            if target == root {
                tokio::fs::remove_dir(&root).await?;
            }
            let branch = format!(
                "feat/run-{}-{}{}",
                &text(run, "id")[..8],
                &text(project, "id")[..8],
                generation.map(|g| format!("-{g}")).unwrap_or_default()
            );
            git(
                args(&[
                    "-C",
                    path(&source)?,
                    "worktree",
                    "add",
                    "-b",
                    &branch,
                    path(&target)?,
                    text(project, "baseBranch"),
                ]),
                30,
            )
            .await?;
        }
    }
    let revision = if kind == "clone" || kind == "worktree" {
        git(args(&["-C", path(&target)?, "rev-parse", "HEAD"]), 10)
            .await
            .ok()
    } else {
        None
    };
    Ok(json!({"projectId":project["id"],"path":target,"kind":kind,"revision":revision}))
}

/// Prepare an immutable host seed. Guest working files are never copied back or replaced.
pub async fn project_seed(
    run: &Value,
    project: &Value,
    config: &Config,
    root: &Path,
) -> Result<Value> {
    let destination = root.join(text(project, "id"));
    let mut value = json!({"projectId":project["id"],"path":destination,"kind":if Path::new(text(project,"path")).join(".git").exists() {"clone"} else {"copy"}});
    if destination.exists() {
        if destination.join(".git").exists() {
            value["revision"] = git(args(&["-C", path(&destination)?, "rev-parse", "HEAD"]), 10)
                .await?
                .into();
        }
        return Ok(value);
    }
    let staging = root.join(format!(".prepare-{}", text(project, "id")));
    if staging.exists() {
        tokio::fs::remove_dir_all(&staging).await?;
    }
    private_dir(&staging).await?;
    let prepared = prepare_project(run, project, config, &staging, false, None).await?;
    let source = Path::new(text(&prepared, "path"));
    // A repository-controlled .agents symlink must never make cleanup follow
    // a parent outside this private seed on the manager filesystem.
    let agents = source.join(".agents");
    if tokio::fs::symlink_metadata(&agents)
        .await
        .is_ok_and(|m| m.is_symlink())
    {
        tokio::fs::remove_file(agents).await?;
    }
    for relative in [".codex", ".agents/skills"] {
        let item = source.join(relative);
        if let Ok(meta) = tokio::fs::symlink_metadata(&item).await {
            if meta.is_dir() {
                tokio::fs::remove_dir_all(item).await?;
            } else {
                tokio::fs::remove_file(item).await?;
            }
        }
    }
    value["revision"] = prepared["revision"].clone();
    tokio::fs::rename(source, &destination).await?;
    tokio::fs::remove_dir(&staging).await?;
    Ok(value)
}

pub async fn prepare(
    run: &Value,
    config: &Config,
    github: Option<&str>,
    codex_home: Option<&Path>,
    generation: Option<&str>,
) -> Result<Value> {
    let directory = config.data_dir.join("runs").join(text(run, "id"));
    let microvm = uses_vm(run, config);
    let is_isolated = microvm || isolated(&run["snapshot"]["agent"]);
    let access = policy(&run["snapshot"]["agent"]);
    if is_isolated && !microvm {
        return Err(Error::new(
            503,
            "Isolated runner is not configured. This agent will not fall back to shared execution.",
        ));
    }
    let root = directory.join(
        generation
            .map(|g| format!("workspace-{g}"))
            .unwrap_or_else(|| "workspace".into()),
    );
    private_dir(&root).await?;
    let projects = run_projects(run)
        .into_iter()
        .filter(|project| !microvm || project["id"] == run["snapshot"]["task"]["projectId"])
        .collect::<Vec<_>>();
    let mut workspaces = Vec::new();
    let mut mounts = Vec::new();
    for project in &projects {
        let entry =
            prepare_project(run, project, config, &root, projects.len() == 1, generation).await?;
        if is_isolated {
            mounts.push(json!({"source":entry["path"],"target":entry["path"],"readOnly":access["sandbox"]=="read-only"}));
        }
        workspaces.push(entry);
    }
    let cwd = if workspaces.len() == 1 {
        PathBuf::from(text(&workspaces[0], "path"))
    } else {
        root.clone()
    };
    let output_directory = directory.join("output");
    private_dir(&output_directory).await?;
    let output = output_directory.join("result.md");
    if !is_isolated {
        return Ok(json!({
        "cwd":cwd,"output":output,"workspaces":workspaces,"isolated":false,"mounts":mounts,"skills":run["snapshot"]["skills"]}
        ));
    }
    let home = directory.join("home");
    private_dir(&home.join(".codex")).await?;
    if crate::provider::Provider::of_run(run) == crate::provider::Provider::Codex {
        let auth = codex_home
            .map(Path::to_owned)
            .unwrap_or_else(|| config.home.join(".codex"))
            .join("auth.json");
        if auth
            .parent()
            .is_some_and(|p| p.join("leo-managed-auth").exists())
        {
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
        .await?;
    }
    github_home(&home, config, access["github"] == true, github).await?;
    let git_config = home.join(".gitconfig");
    // Re-preparing a run must also remove the old synthetic agent identity.
    atomic_write(&git_config, b"[user]\n\tuseConfigOnly = true\n").await?;
    if let Some((name, email)) =
        git_identity(config, &home, github.is_some() || access["github"] == true).await
    {
        for (key, value) in [("user.name", name), ("user.email", email)] {
            git(
                args(&["config", "--file", path(&git_config)?, key, &value]),
                10,
            )
            .await?;
        }
    }
    if github.is_some() || access["github"] == true {
        git(
            args(&[
                "config",
                "--file",
                path(&git_config)?,
                "credential.https://github.com.helper",
                "!gh auth git-credential",
            ]),
            10,
        )
        .await?;
    }
    let mut skills = Vec::new();
    for (index, skill) in run["snapshot"]["skills"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let relative = format!("{index}/{}", text(skill, "name"));
        let target = home.join(".agents/skills").join(&relative);
        private_dir(target.parent().unwrap()).await?;
        let source = Path::new(text(skill, "path"))
            .parent()
            .ok_or_else(|| Error::bad("Invalid selected skill path."))?;
        copy_tree(source, &target, true).await?;
        atomic_write(&target.join("SKILL.md"), text(skill, "content").as_bytes()).await?;
        let mut skill = skill.clone();
        skill["path"] = format!("/home/node/.agents/skills/{relative}/SKILL.md").into();
        skills.push(skill);
    }
    mounts.insert(
        0,
        json!({
        "source":root,"target":root,"readOnly":false}
        ),
    );
    mounts.extend([
        json!({
        "source":home,"target":"/home/node","readOnly":false}
        ),
        json!({
        "source":output_directory,"target":output_directory,"readOnly":false}
        ),
    ]);
    let empty = directory.join("empty");
    private_dir(&empty).await?;
    for workspace in &workspaces {
        for relative in [".codex", ".agents/skills"] {
            let target = Path::new(text(workspace, "path")).join(relative);
            if tokio::fs::try_exists(&target).await? {
                mounts.push(json!({
                "source":empty,"target":target,"readOnly":true}
                ));
            }
        }
    }
    Ok(json!({
    "projectRoot":root,"cwd":cwd,"output":output,"workspaces":workspaces,"isolated":true,"mounts":mounts,"skills":skills,"backend":if microvm {"firecracker"} else {"local"}}
    ))
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
    mut prepared: Value,
    config: &Config,
    github: Option<&str>,
) -> Result<Value> {
    if uses_vm(run, config) && prepared["backend"] != "firecracker" {
        // One-time migration of saved container/shared conversations. Keep the old
        // checkout intact and seed the new private disk with its uncommitted files.
        let directory = config.data_dir.join("runs").join(text(run, "id"));
        let old_home = directory.join(if prepared["isolated"] == true {
            "home/.codex"
        } else {
            "codex"
        });
        let generation = format!("microvm-{}", &crate::config::id()[..8]);
        let mut migrated = prepare(run, config, github, Some(&old_home), Some(&generation)).await?;
        for old in prepared["workspaces"].as_array().into_iter().flatten() {
            if migrated["workspaces"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["projectId"] == old["projectId"])
            {
                continue;
            }
            let project = run_projects(run)
                .into_iter()
                .find(|p| p["id"] == old["projectId"])
                .ok_or_else(|| Error::bad("Saved project is no longer authorized."))?;
            let entry = project_seed(
                run,
                &project,
                config,
                Path::new(text(&migrated, "projectRoot")),
            )
            .await?;
            migrated["mounts"].as_array_mut().unwrap().push(json!({"source":entry["path"],"target":entry["path"],"readOnly":policy(&run["snapshot"]["agent"])["sandbox"]=="read-only"}));
            migrated["workspaces"].as_array_mut().unwrap().push(entry);
        }
        for old in prepared["workspaces"].as_array().into_iter().flatten() {
            if let Some(new) = migrated["workspaces"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|new| new["projectId"] == old["projectId"])
            {
                let source = workspace(
                    Path::new(text(old, "path")),
                    &[directory.clone(), PathBuf::from(text(old, "path"))],
                )
                .await?;
                // Replace the initial clone's working files so tracked deletions
                // remain deleted. A prior independent clone also keeps its Git history.
                let target_root = Path::new(text(new, "path"));
                let copy_git = tokio::fs::symlink_metadata(source.join(".git"))
                    .await
                    .is_ok_and(|m| m.is_dir());
                if !copy_git && source.join(".git").exists() {
                    // A linked worktree's .git file points outside the guest. Its
                    // branch and commits must become independent before import.
                    let head = git(args(&["-C", path(&source)?, "rev-parse", "HEAD"]), 10).await?;
                    let branch = git(
                        args(&["-C", path(&source)?, "symbolic-ref", "--short", "HEAD"]),
                        10,
                    )
                    .await
                    .unwrap_or_else(|_| format!("feat/recovered-{}", &text(run, "id")[..8]));
                    git(
                        args(&["-C", path(target_root)?, "fetch", path(&source)?, &head]),
                        120,
                    )
                    .await?;
                    git(
                        args(&["-C", path(target_root)?, "checkout", "-B", &branch, &head]),
                        30,
                    )
                    .await?;
                }
                let mut targets = tokio::fs::read_dir(target_root).await?;
                while let Some(entry) = targets.next_entry().await? {
                    if entry.file_name() == ".git" && !copy_git {
                        continue;
                    }
                    if entry.file_type().await?.is_dir() {
                        tokio::fs::remove_dir_all(entry.path()).await?;
                    } else {
                        tokio::fs::remove_file(entry.path()).await?;
                    }
                }
                let mut entries = tokio::fs::read_dir(source).await?;
                while let Some(entry) = entries.next_entry().await? {
                    if entry.file_name() == ".git" && !copy_git {
                        continue;
                    }
                    let target = Path::new(text(new, "path")).join(entry.file_name());
                    if target.exists() {
                        if target.is_dir() {
                            tokio::fs::remove_dir_all(&target).await?;
                        } else {
                            tokio::fs::remove_file(&target).await?;
                        }
                    }
                    copy_tree(&entry.path(), &target, false).await?;
                }
            }
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
        return Ok(migrated);
    }
    if prepared["backend"] == "firecracker" && !prepared["projectRoot"].is_string() {
        let cwd = Path::new(text(&prepared, "cwd"));
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
        let directory = config.data_dir.join("runs").join(text(run, "id"));
        if !project_root.starts_with(&directory) || project_root == directory {
            return Err(Error::bad("Invalid saved project root."));
        }
        prepared["projectRoot"] = json!(project_root);
    }
    if prepared["projectRoot"].is_string() {
        let project_root = PathBuf::from(text(&prepared, "projectRoot"));
        for entry in run["workspaces"].as_array().into_iter().flatten() {
            if prepared["workspaces"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["projectId"] == entry["projectId"])
            {
                continue;
            }
            let project_id = text(entry, "projectId");
            crate::validation::uuid(project_id)?;
            if Path::new(text(entry, "path")) != project_root.join(project_id)
                || !crate::project_workspaces::catalog(run)
                    .iter()
                    .any(|p| p["id"] == project_id)
            {
                return Err(Error::bad("Invalid saved project workspace."));
            }
            prepared["workspaces"]
                .as_array_mut()
                .unwrap()
                .push(entry.clone());
            prepared["mounts"].as_array_mut().unwrap().push(json!({"source":entry["path"],"target":entry["path"],"readOnly":policy(&run["snapshot"]["agent"])["sandbox"]=="read-only"}));
        }
    }
    if prepared["isolated"] != (uses_vm(run, config) || isolated(&run["snapshot"]["agent"])) {
        return Err(Error::new(
            409,
            "Execution isolation changed; this run cannot be resumed.",
        ));
    }
    if prepared["isolated"] == true && !uses_vm(run, config) {
        return Err(Error::new(503, "The isolated runner is not configured."));
    }
    let projects = run_projects(run);
    for project in &projects {
        if prepared["backend"] == "firecracker"
            && !prepared["workspaces"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|w| w["projectId"] == project["id"])
        {
            continue;
        }
        if workspace(Path::new(text(project, "path")), &config.workspace_roots).await?
            != Path::new(text(project, "path"))
        {
            return Err(Error::new(
                409,
                "A project moved outside its permitted location.",
            ));
        }
    }
    let root = config.data_dir.join("runs").join(text(run, "id"));
    let mut allowed = vec![root.clone()];
    let workspaces = prepared["workspaces"]
        .as_array()
        .ok_or_else(|| Error::bad("Invalid saved execution workspace."))?;
    for w in workspaces {
        if w["kind"] == "direct" {
            let p = projects
                .iter()
                .find(|p| p["id"] == w["projectId"])
                .ok_or_else(|| {
                    Error::new(
                        409,
                        "A saved workspace is no longer in the agent’s project scope.",
                    )
                })?;
            allowed.push(PathBuf::from(text(p, "path")));
        }
    }
    let mut paths = vec![
        PathBuf::from(text(&prepared, "cwd")),
        Path::new(text(&prepared, "output"))
            .parent()
            .ok_or_else(|| Error::bad("Invalid output directory."))?
            .to_owned(),
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
            return Err(Error::new(
                409,
                "A saved workspace changed location. Working files were preserved.",
            ));
        }
    }
    if prepared["isolated"] == true {
        github_home(
            &root.join("home"),
            config,
            policy(&run["snapshot"]["agent"])["github"] == true,
            github,
        )
        .await?;
    }
    Ok(prepared)
}
