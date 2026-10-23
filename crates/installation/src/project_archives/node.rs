use super::*;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

async fn download(
    root: &Path,
    descriptor: &Archive,
    response: reqwest::Response,
    budget: u64,
) -> Result<std::fs::File> {
    descriptor.validate()?;
    if !response.status().is_success() {
        return Err(Error::unavailable("Project archive download rejected."));
    }
    let freed = clean_interrupted_downloads(root).await?;
    let budget = budget.saturating_add(freed);
    let reclaimed = reclaim(root, descriptor.bytes.saturating_sub(budget)).await?;
    if descriptor.bytes > budget.saturating_add(reclaimed) {
        return Err(Error::new(
            507,
            "Project archive exceeds free node disk budget.",
        ));
    }
    prune(root, descriptor.bytes).await?;
    let directory = root.join("project-archives");
    let temporary = tempfile::NamedTempFile::new_in(&directory)?;
    let mut output = tokio::fs::File::from_std(temporary.reopen()?);
    let mut stream = response.bytes_stream();
    let mut bytes = 0u64;
    let mut hash = blake3::Hasher::new();
    while let Some(chunk) = tokio::time::timeout(std::time::Duration::from_secs(30), stream.next())
        .await
        .map_err(|_| Error::timeout("Project archive download stalled."))?
    {
        let chunk =
            chunk.map_err(|_| Error::unavailable("Project archive download interrupted."))?;
        bytes = bytes.saturating_add(chunk.len() as u64);
        if bytes > descriptor.bytes {
            return Err(Error::too_large("Project archive length changed."));
        }
        hash.update(&chunk);
        output.write_all(&chunk).await?;
    }
    if bytes != descriptor.bytes || hash.finalize().to_hex().as_str() != descriptor.hash {
        return Err(Error::bad("Project archive checksum mismatch."));
    }
    output.sync_all().await?;
    drop(output);
    tokio::fs::rename(temporary.path(), descriptor.path(root)).await?;
    tokio::fs::File::open(&directory).await?.sync_all().await?;
    Ok(tokio::fs::File::open(descriptor.path(root))
        .await?
        .into_std()
        .await)
}

async fn budget() -> Result<u64> {
    if crate::native::enabled() {
        crate::native::disk::staging_budget().await
    } else {
        Ok(CACHE_BYTES)
    }
}

/// Pins each immutable archive into this attempt's private staging directory.
/// The controller extracts directly to the durable filesystem, without a
/// second node-side expanded project tree.
pub(crate) async fn stage(
    session: &crate::nodes::executor::Session,
    root: &Path,
    descriptors: &[Archive],
) -> Result<()> {
    let run = root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| Error::bad("Missing run identity."))?;
    let retained = crate::native::state_dir()
        .join("native")
        .join(run)
        .join("block-initialized")
        .exists();
    if retained {
        for descriptor in descriptors {
            descriptor.validate()?;
            crate::skills::private_dir(&root.join("workspace").join(&descriptor.project_id))
                .await?;
        }
        return Ok(());
    }
    let _disk = crate::native::disk::lock(&crate::native::state_dir()).await?;
    let _guard = lock().lock().await;
    let data = crate::nodes::node_data_dir();
    clean_interrupted_downloads(&data).await?;
    let pins = root.join("project-archives");
    crate::skills::private_dir(&pins).await?;
    let mut required = 0u64;
    for descriptor in descriptors {
        descriptor.validate()?;
        let cached_file = cached(&data, descriptor).await?;
        let verified = if let Some(file) = cached_file {
            let (hash, bytes) = fingerprint(file).await?;
            hash == descriptor.hash && bytes == descriptor.bytes
        } else {
            false
        };
        if verified {
            tokio::fs::hard_link(
                descriptor.path(&data),
                pins.join(format!("{}.zip", descriptor.project_id)),
            )
            .await?;
        } else {
            // A corrupt cache entry cannot reserve space while its replacement downloads.
            match tokio::fs::remove_file(descriptor.path(&data)).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            required = required
                .checked_add(descriptor.bytes)
                .ok_or_else(|| Error::too_large("Project archives exceed disk budget."))?;
        }
        required = required
            .checked_add(descriptor.logical_bytes)
            .ok_or_else(|| Error::too_large("Project archives exceed disk budget."))?;
    }
    // Current inputs are pinned before reclamation. Reserve the final filesystem
    // plus missing encoded objects; warm objects are already charged to the pool.
    let available = budget().await?;
    let reclaimed = reclaim(&data, required.saturating_sub(available)).await?;
    if required > available.saturating_add(reclaimed) {
        return Err(Error::new(
            507,
            "Project archive preparation exceeds free node disk budget.",
        ));
    }
    for descriptor in descriptors {
        let target = pins.join(format!("{}.zip", descriptor.project_id));
        if !target.exists() {
            let selection =
                serde_json::json!({ "projectId": descriptor.project_id, "hash": descriptor.hash });
            let response = crate::nodes::workspace::fetch(
                &session.client,
                &session.master,
                &session.token,
                &session.attempt,
                "archive",
                &selection,
            )
            .await?;
            download(&data, descriptor, response, budget().await?).await?;
            tokio::fs::hard_link(descriptor.path(&data), target).await?;
        }
        crate::skills::private_dir(&root.join("workspace").join(&descriptor.project_id)).await?;
    }
    Ok(())
}

pub(crate) async fn abandon_stage(root: &Path, pressure: bool) -> Result<()> {
    let _disk = if crate::native::enabled() {
        Some(crate::native::disk::lock(&crate::native::state_dir()).await?)
    } else {
        None
    };
    let _guard = lock().lock().await;
    match tokio::fs::remove_dir_all(root.join("project-archives")).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if pressure {
        reclaim(&crate::nodes::node_data_dir(), u64::MAX).await?;
    }
    Ok(())
}

pub(crate) async fn prefetch(
    client: reqwest::Client,
    master: url::Url,
    token: String,
    descriptors: Vec<Archive>,
    stop: CancellationToken,
) -> Result<()> {
    prefetch_into(
        crate::nodes::node_data_dir(),
        client,
        master,
        token,
        descriptors,
        stop,
    )
    .await
}

async fn prefetch_into(
    root: PathBuf,
    client: reqwest::Client,
    master: url::Url,
    token: String,
    descriptors: Vec<Archive>,
    stop: CancellationToken,
) -> Result<()> {
    for descriptor in descriptors {
        // A best-effort warmer never queues behind active filesystem mutation.
        let state = crate::native::state_dir();
        let _disk = if crate::native::enabled() {
            match crate::file_lock::exclusive(&state.join("storage.lock"), "Native storage busy.") {
                Ok(guard) => Some(guard),
                Err(error) if error.status == 409 => return Ok(()),
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let Some(_guard) = lock().try_lock().ok() else {
            return Ok(());
        };
        clean_interrupted_downloads(&root).await?;
        match cached(&root, &descriptor).await {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(project_id = %descriptor.project_id, status = error.status, "Project archive preload deferred");
                continue;
            }
        }
        let fetch = async {
            let response = client
                .post(
                    master
                        .join(&format!(
                            "internal/node-workspace/cache/{}",
                            descriptor.project_id
                        ))
                        .map_err(Error::internal)?,
                )
                .timeout(std::time::Duration::from_secs(300))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "hash": descriptor.hash }))
                .send()
                .await
                .map_err(|_| Error::unavailable("Project archive preload interrupted."))?;
            download(&root, &descriptor, response, budget().await?).await?;
            Ok::<_, Error>(())
        };
        tokio::select! {
            () = stop.cancelled() => return Ok(()),
            () = foreground_waiter(&state), if crate::native::enabled() => return Ok(()),
            result = fetch => if let Err(error) = result {
                tracing::debug!(project_id = %descriptor.project_id, status = error.status, "Project archive preload deferred");
            },
        }
    }
    Ok(())
}

/// OS locks make the foreground priority visible to the controller process as
/// well as the connector. Cancellation releases both the partial file and the
/// exclusive mutation lock; no partially downloaded archive becomes visible.
async fn foreground_waiter(state: &Path) {
    loop {
        match crate::file_lock::exclusive(
            &state.join("storage-foreground.lock"),
            "Foreground storage operation waiting.",
        ) {
            Ok(probe) => drop(probe),
            Err(_) => return,
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_project_does_not_starve_later_background_downloads() {
        use axum::{
            Router, body::Body, extract::Path as RoutePath, http::StatusCode, routing::post,
        };
        let root = tempfile::tempdir().unwrap();
        let payload = b"second project archive";
        let first = Archive {
            project_id: crate::config::id(),
            key: "a".repeat(64),
            revision: "b".repeat(40),
            hash: blake3::hash(payload).to_hex().to_string(),
            bytes: payload.len() as u64,
            logical_bytes: 100,
        };
        let second = Archive {
            project_id: crate::config::id(),
            ..first.clone()
        };
        let first_id = first.project_id.clone();
        let app = Router::new().route(
            "/internal/node-workspace/cache/{project}",
            post(move |RoutePath(project): RoutePath<String>| {
                let failed = project == first_id;
                async move {
                    if failed {
                        (StatusCode::INSUFFICIENT_STORAGE, Body::empty())
                    } else {
                        (StatusCode::OK, Body::from("second project archive"))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let master = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        prefetch_into(
            root.path().to_owned(),
            reqwest::Client::new(),
            master,
            "fixture".into(),
            vec![first, second.clone()],
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            tokio::fs::read(second.path(root.path())).await.unwrap(),
            payload
        );
        server.abort();
    }

    #[tokio::test]
    async fn foreground_storage_preempts_a_stalled_background_transfer() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().to_owned();
        let storage = crate::file_lock::exclusive(&state.join("storage.lock"), "busy").unwrap();
        let background = tokio::spawn({
            let state = state.clone();
            async move {
                let _storage = storage;
                tokio::select! {
                    () = foreground_waiter(&state) => {},
                    () = std::future::pending::<()>() => {},
                }
            }
        });
        // This is the real foreground lock, not a process-local notification.
        let foreground = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            crate::native::disk::lock(&state),
        )
        .await
        .unwrap()
        .unwrap();
        background.await.unwrap();
        drop(foreground);
    }

    #[tokio::test]
    async fn interrupted_and_corrupt_downloads_never_publish_cache_objects() {
        use axum::{Router, body::Body, routing::post};
        let root = tempfile::tempdir().unwrap();
        let bytes = b"expected contents";
        let archive = Archive {
            project_id: crate::config::id(),
            key: "b".repeat(64),
            revision: "a".repeat(40),
            hash: blake3::hash(bytes).to_hex().to_string(),
            bytes: bytes.len() as u64,
            logical_bytes: 100,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/truncated", post(|| async { Body::from("expected") }))
            .route(
                "/corrupt",
                post(|| async { Body::from("modified contents") }),
            )
            .route("/valid", post(|| async { Body::from("expected contents") }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for path in ["truncated", "corrupt", "valid"] {
            let response = reqwest::Client::new()
                .post(format!("{url}/{path}"))
                .send()
                .await
                .unwrap();
            let result = download(root.path(), &archive, response, CACHE_BYTES).await;
            assert_eq!(result.is_ok(), path == "valid");
            assert_eq!(archive.path(root.path()).exists(), path == "valid");
        }
        server.abort();
    }
}
