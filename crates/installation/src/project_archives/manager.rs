use super::*;
use crate::validation::text;
use crate::{config::Config, plugins::run_credentials::GitAccess, service::Service};
use axum::{body::Body, response::Response};
use std::{sync::Arc, time::Duration};

async fn latest(config: &Config, project: &Value) -> Result<Option<Archive>> {
    let path = config
        .data_dir
        .join("project-archives")
        .join(format!("{}.json", key(project)));
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let descriptor: Archive = serde_json::from_slice(&bytes)?;
    descriptor.validate()?;
    Ok(Some(descriptor))
}

async fn build(
    config: &Config,
    project: &Value,
    source: &Path,
    revision: &str,
) -> Result<(Archive, std::fs::File)> {
    let _guard = lock().lock().await;
    if let Some(descriptor) = latest(config, project).await?
        && descriptor.revision == revision
        && let Some(file) = cached(&config.data_dir, &descriptor).await?
    {
        return Ok((descriptor, file));
    }
    prune(&config.data_dir, 0).await?;
    let root = config.data_dir.join("project-archives");
    let temp = tempfile::NamedTempFile::new_in(&root)?;
    let source = source.to_owned();
    let output = temp.reopen()?;
    let logical_bytes = tokio::task::spawn_blocking(move || archive::pack(&source, output))
        .await
        .map_err(Error::internal)??;
    let (hash, bytes) = fingerprint(temp.reopen()?).await?;
    let descriptor = Archive {
        project_id: text(project, "id").into(),
        revision: revision.into(),
        key: key(project),
        hash,
        bytes,
        logical_bytes,
    };
    descriptor.validate()?;
    prune(&config.data_dir, bytes).await?;
    tokio::fs::rename(temp.path(), descriptor.path(&config.data_dir)).await?;
    crate::native::atomic_write(
        &root.join(format!("{}.json", descriptor.key)),
        &serde_json::to_vec(&descriptor)?,
    )
    .await?;
    let file = cached(&config.data_dir, &descriptor)
        .await?
        .ok_or_else(|| Error::not_found("Project archive was not installed."))?;
    Ok((descriptor, file))
}

/// Authoritatively resolve the remote on every new checkout. A cache hit never
/// replaces current Git authorization or silently falls back to a stale branch.
pub(crate) async fn checkout(
    source: &Path,
    target: &Path,
    project: &Value,
    config: &Config,
    credentials: &[GitAccess],
) -> Result<bool> {
    if project["sourceMode"] == "local" {
        return Ok(false);
    }
    let Some(revision) =
        crate::project_git::remote_revision(source, project, config, credentials).await?
    else {
        return Ok(false);
    };
    let hit = {
        let _guard = lock().lock().await;
        match latest(config, project).await? {
            Some(descriptor) if descriptor.revision == revision => {
                cached(&config.data_dir, &descriptor).await?
            }
            _ => None,
        }
    };
    if let Some(file) = hit {
        extract(file, target, LOGICAL_BYTES).await?;
        return Ok(true);
    }
    crate::project_git::clone(source, target, project, config, credentials).await?;
    let actual = crate::project_git::revision(target, config).await?;
    if let Err(error) = build(config, project, target, &actual).await {
        tracing::warn!(
            status = error.status,
            "Project archive cache unavailable; using the fresh checkout"
        );
    }
    Ok(true)
}

pub(crate) async fn run_archives(
    s: &Service,
    run: &Value,
    root: &Path,
) -> Result<Option<Vec<Archive>>> {
    let projects = crate::service::run_projects(run);
    let mut archives = Vec::new();
    for entry in run["workspaces"].as_array().into_iter().flatten() {
        let Some(project) = projects
            .iter()
            .find(|p| p["id"] == entry["projectId"] && p["sourceMode"] != "local")
        else {
            return Ok(None);
        };
        let Some(revision) = entry["revision"].as_str() else {
            return Ok(None);
        };
        match build(
            &s.config,
            project,
            &root.join("workspace").join(text(project, "id")),
            revision,
        )
        .await
        {
            Ok((archive, _file)) => archives.push(archive),
            Err(error) => {
                tracing::warn!(
                    status = error.status,
                    "Project archive cache unavailable; using file transfer"
                );
                return Ok(None);
            }
        }
    }
    // A complete advertised set must fit the disposable cache. Otherwise keep
    // the established file-transfer path rather than repeatedly evicting a
    // sibling archive before the node has received it.
    if archives.is_empty()
        || archives.iter().map(|archive| archive.bytes).sum::<u64>() > CACHE_BYTES
    {
        return Ok(None);
    }
    Ok(Some(archives))
}

async fn allowed(s: &Service, node_id: &str, project: &Value) -> Result<bool> {
    let node = s.get("nodes", node_id).await?;
    if node["revoked"] == true || node["accepting"] != true {
        return Ok(false);
    }
    let native = crate::native::policy::node_native(&node);
    if !native {
        return Ok(false);
    }
    let trusted = node["nativeAccounts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let current = s
        .store
        .read(move |db| {
            let authority = db.kv("native-authority")?.unwrap_or_default();
            Ok(authority["observedAt"]
                .as_i64()
                .is_some_and(|at| (0..=60_000).contains(&(crate::config::now() - at)))
                && trusted
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|person| authority["accounts"].get(person).is_some()))
        })
        .await?;
    if !current {
        return Ok(false);
    }
    if native
        && (node["nativeEnabled"] != true
            || node["nativeAccounts"].as_array().is_none_or(Vec::is_empty))
    {
        return Ok(false);
    }
    Ok(s.store.list("agents").await?.iter().any(|agent| {
        let policy = crate::service::policy(agent);
        crate::native::policy::native(agent) == native
            && crate::nodes::agent_allows(agent, node_id)
            && crate::service::allowed(&policy["projects"], text(project, "id"))
    }))
}

pub(crate) async fn catalog(s: &Service, node: &str) -> Result<Vec<Archive>> {
    let mut result = Vec::new();
    for project in s.store.list("projects").await? {
        if project["sourceMode"] == "local" || !allowed(s, node, &project).await? {
            continue;
        }
        if let Some(descriptor) = latest(&s.config, &project).await?
            && cached(&s.config.data_dir, &descriptor).await?.is_some()
        {
            result.push(descriptor);
        }
    }
    Ok(result)
}

async fn response(file: std::fs::File) -> Result<Response> {
    Response::builder()
        .header("content-type", "application/zip")
        .header("content-length", file.metadata()?.len())
        .body(Body::from_stream(tokio_util::io::ReaderStream::new(
            tokio::fs::File::from_std(file),
        )))
        .map_err(Error::internal)
}

pub(crate) async fn serve_cached(
    s: &Service,
    node: &str,
    project_id: &str,
    hash: &str,
) -> Result<Response> {
    let descriptor = catalog(s, node)
        .await?
        .into_iter()
        .find(|a| a.project_id == project_id && a.hash == hash)
        .ok_or_else(|| Error::forbidden("Project archive is not authorized on this node."))?;
    let _guard = lock().lock().await;
    let file = cached(&s.config.data_dir, &descriptor)
        .await?
        .ok_or_else(|| Error::not_found("Project archive was evicted."))?;
    response(file).await
}

pub(crate) async fn serve_run(
    s: &Service,
    run: &Value,
    root: &Path,
    body: &Value,
) -> Result<Response> {
    let projects = crate::service::run_projects(run);
    let project = projects
        .iter()
        .find(|project| project["id"] == body["projectId"])
        .ok_or_else(|| Error::forbidden("Project archive outside run scope."))?;
    let revision = run["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["projectId"] == body["projectId"])
        .and_then(|entry| entry["revision"].as_str())
        .ok_or_else(|| Error::forbidden("Project archive outside run scope."))?;
    // Open the requested archive under the builder's eviction lock. A concurrent
    // cache refresh cannot invalidate this response, even after unlinking it.
    let (descriptor, file) = build(
        &s.config,
        project,
        &root.join("workspace").join(text(project, "id")),
        revision,
    )
    .await?;
    if descriptor.hash != body["hash"] {
        return Err(Error::conflict("Project archive identity changed."));
    }
    response(file).await
}

async fn refresh(s: &Service, project: &Value) -> Result<()> {
    if project["sourceMode"] == "local" {
        return Ok(());
    }
    let agents = s.store.list("agents").await?;
    let Some(agent) = agents.iter().find(|agent| {
        crate::service::allowed(
            &crate::service::policy(agent)["projects"],
            text(project, "id"),
        )
    }) else {
        return Ok(());
    };
    let credentials =
        crate::plugins::run_credentials::fetch(s, Some(text(agent, "id")), project).await?;
    let revision = crate::project_git::remote_revision(
        Path::new(text(project, "path")),
        project,
        &s.config,
        &credentials,
    )
    .await?;
    if let Some(descriptor) = latest(&s.config, project).await?
        && Some(&descriptor.revision) == revision.as_ref()
        && cached(&s.config.data_dir, &descriptor).await?.is_some()
    {
        return Ok(());
    }
    let root = s.config.data_dir.join("project-archives");
    crate::skills::private_dir(&root).await?;
    let temporary = tempfile::tempdir_in(root)?;
    checkout(
        Path::new(text(project, "path")),
        &temporary.path().join("repository"),
        project,
        &s.config,
        &credentials,
    )
    .await?;
    Ok(())
}

pub(crate) async fn maintain(s: Arc<Service>) {
    loop {
        let refresh_all = async {
            for project in s.store.list("projects").await? {
                if let Err(error) = refresh(&s, &project).await {
                    tracing::warn!(
                        status = error.status,
                        "Project archive refresh deferred; foreground fetch remains authoritative"
                    );
                }
            }
            Ok::<_, Error>(())
        };
        tokio::select! {
            () = s.shutdown.cancelled() => return,
            _ = refresh_all => {},
        }
        tokio::select! {
            () = s.shutdown.cancelled() => return,
            () = tokio::time::sleep(Duration::from_secs(60)) => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn projects_exceeding_the_shared_archive_capacity_keep_the_legacy_transfer() {
        let root = tempfile::tempdir().unwrap();
        let config: Config = serde_json::from_value(json!({
            "dataDir": root.path().join("data"), "home": root.path().join("home"),
            "workspaceRoots": [root.path()], "publicUrl": "http://localhost:4310",
            "host": "127.0.0.1", "port": 0, "codexBin": "unused", "concurrency": 1,
            "logger": false, "workerEnabled": false, "runnerUrl": "",
        }))
        .unwrap();
        let service = Service::new(config).await.unwrap();
        let cache = service.config.data_dir.join("project-archives");
        std::fs::create_dir(&cache).unwrap();
        let mut projects = Vec::new();
        let mut workspaces = Vec::new();
        for hash in ["a", "b"] {
            let project =
                json!({ "id": crate::config::id(), "path": root.path(), "sourceMode": "remote" });
            let descriptor = Archive {
                project_id: text(&project, "id").into(),
                key: key(&project),
                revision: "c".repeat(40),
                hash: hash.repeat(64),
                bytes: CACHE_BYTES * 3 / 5,
                logical_bytes: 0,
            };
            // Sparse stand-ins exercise the real catalogue/cache admission seam
            // without allocating multi-gigabyte test payloads.
            std::fs::File::create(descriptor.path(&service.config.data_dir))
                .unwrap()
                .set_len(descriptor.bytes)
                .unwrap();
            std::fs::write(
                cache.join(format!("{}.json", descriptor.key)),
                serde_json::to_vec(&descriptor).unwrap(),
            )
            .unwrap();
            workspaces.push(json!({ "projectId": project["id"], "revision": descriptor.revision }));
            projects.push(project);
        }
        let run = json!({ "snapshot": { "projects": projects }, "workspaces": workspaces });
        assert!(
            run_archives(&service, &run, root.path())
                .await
                .unwrap()
                .is_none()
        );
    }
}
