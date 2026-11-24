use super::*;
use crate::nodes::workspace::{await_preparation, check_preparing};
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

async fn download(
    root: &Path,
    descriptor: &Archive,
    response: reqwest::Response,
    budget: u64,
    stop: &CancellationToken,
) -> Result<std::fs::File> {
    let mut admission = None;
    download_admitted(
        root,
        descriptor,
        response,
        budget,
        stop,
        &mut |target, bytes| {
            if !crate::native::enabled() {
                return Ok(());
            }
            // Capture accounting only after cleanup/pruning, against this download's
            // inode. Old cache removals cannot hide newly allocated payload bytes.
            if admission.is_none() {
                admission = Some(crate::native::ingress::Admission::staging(
                    root,
                    &crate::native::state_dir(),
                    target,
                )?);
            }
            admission.as_mut().unwrap().admit(bytes)
        },
    )
    .await
}

async fn download_admitted(
    root: &Path,
    descriptor: &Archive,
    response: reqwest::Response,
    budget: u64,
    stop: &CancellationToken,
    admit: &mut (impl FnMut(&Path, u64) -> Result<()> + Send),
) -> Result<std::fs::File> {
    check_preparing(stop)?;
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
    admit(temporary.path(), 2 * 5120)?;
    let mut output = tokio::fs::File::from_std(temporary.reopen()?);
    let received = async {
        let mut stream = response.bytes_stream();
        let mut bytes = 0u64;
        let mut hash = blake3::Hasher::new();
        while let Some(chunk) = await_preparation(stop, async {
            tokio::time::timeout(std::time::Duration::from_secs(30), stream.next())
                .await
                .map_err(|_| Error::timeout("Project archive download stalled."))
        })
        .await?
        {
            let chunk =
                chunk.map_err(|_| Error::unavailable("Project archive download interrupted."))?;
            bytes = bytes.saturating_add(chunk.len() as u64);
            if bytes > descriptor.bytes {
                return Err(Error::too_large("Project archive length changed."));
            }
            for bytes in chunk.chunks(65536) {
                check_preparing(stop)?;
                admit(temporary.path(), bytes.len() as u64)?;
                hash.update(bytes);
                output.write_all(bytes).await?;
            }
        }
        if bytes != descriptor.bytes || hash.finalize().to_hex().as_str() != descriptor.hash {
            return Err(Error::bad("Project archive checksum mismatch."));
        }
        Ok::<_, Error>(())
    }
    .await;
    // Drain accepted filesystem writes before releasing the partial and its owner.
    let drained = output.flush().await;
    received?;
    drained?;
    check_preparing(stop)?;
    output.sync_all().await?;
    check_preparing(stop)?;
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
    base_disk_mib: Option<u64>,
    stop: &CancellationToken,
) -> Result<Option<String>> {
    check_preparing(stop)?;
    let run = root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| Error::bad("Missing run identity."))?;
    crate::native::bases::release_pin(root, &session.attempt).await?;
    let retained = crate::native::state_dir()
        .join("native")
        .join(run)
        .join("block-initialized")
        .exists();
    if retained {
        for descriptor in descriptors {
            check_preparing(stop)?;
            descriptor.validate()?;
            crate::skills::private_dir(&root.join("workspace").join(&descriptor.project_id))
                .await?;
        }
        return Ok(None);
    }
    let _disk =
        await_preparation(stop, crate::native::disk::lock(&crate::native::state_dir())).await?;
    if let Some(disk_mib) = base_disk_mib {
        let lookup = crate::native::bases::request(
            "lookup",
            &serde_json::json!({ "archives": descriptors, "diskMiB": disk_mib }),
        )
        .await;
        if let Ok(value) = lookup
            && let Some(key) = value["base"]["key"].as_str()
            && key.len() == 64
            && key.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            tokio::fs::hard_link(
                crate::native::state_dir()
                    .join("native-bases")
                    .join(key)
                    .join("pin"),
                crate::native::bases::pin_path(root, &session.attempt)?,
            )
            .await?;
            crate::native::bases::touch_pin(&crate::native::bases::pin_path(
                root,
                &session.attempt,
            )?);
            for descriptor in descriptors {
                check_preparing(stop)?;
                descriptor.validate()?;
                crate::skills::private_dir(&root.join("workspace").join(&descriptor.project_id))
                    .await?;
            }
            return Ok(Some(key.to_owned()));
        }
    }
    let _guard = await_preparation(stop, async { Ok(lock().lock().await) }).await?;
    let data = crate::nodes::node_data_dir();
    clean_interrupted_downloads(&data).await?;
    let pins = root.join("project-archives");
    crate::skills::private_dir(&pins).await?;
    let mut required = 0u64;
    for descriptor in descriptors {
        check_preparing(stop)?;
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
    let available = await_preparation(stop, budget()).await?;
    let reclaimed = reclaim(&data, required.saturating_sub(available)).await?;
    if required > available.saturating_add(reclaimed) {
        return Err(Error::new(
            507,
            "Project archive preparation exceeds free node disk budget.",
        ));
    }
    for descriptor in descriptors {
        check_preparing(stop)?;
        let target = pins.join(format!("{}.zip", descriptor.project_id));
        if !target.exists() {
            let selection =
                serde_json::json!({ "projectId": descriptor.project_id, "hash": descriptor.hash });
            let response = await_preparation(
                stop,
                crate::nodes::workspace::fetch(
                    &session.client,
                    &session.master,
                    &session.token,
                    &session.attempt,
                    "archive",
                    &selection,
                ),
            )
            .await?;
            let available = await_preparation(stop, budget()).await?;
            download(&data, descriptor, response, available, stop).await?;
            check_preparing(stop)?;
            tokio::fs::hard_link(descriptor.path(&data), target).await?;
        }
        crate::skills::private_dir(&root.join("workspace").join(&descriptor.project_id)).await?;
    }
    check_preparing(stop)?;
    if let Some(disk_mib) = base_disk_mib {
        let _ = crate::native::bases::request(
            "warm",
            &serde_json::json!({ "archives": descriptors, "diskMiB": disk_mib }),
        )
        .await;
    }
    Ok(None)
}

pub(crate) async fn abandon_stage(root: &Path, pressure: bool, attempt: &str) -> Result<()> {
    crate::native::bases::release_pin(root, attempt).await?;
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
        if stop.is_cancelled() {
            return Ok(());
        }
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
        let transfer = stop.child_token();
        let fetch = async {
            let response = await_preparation(&transfer, async {
                client
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
                    .map_err(|_| Error::unavailable("Project archive preload interrupted."))
            })
            .await?;
            download(&root, &descriptor, response, budget().await?, &transfer).await?;
            Ok::<_, Error>(())
        };
        tokio::pin!(fetch);
        tokio::select! {
            () = foreground_waiter(&state), if crate::native::enabled() => {
                transfer.cancel();
                // Keep both mutation guards until accepted writes and any
                // publication already in flight have settled.
                let _ = fetch.await;
                return Ok(());
            }
            result = &mut fetch => {
                if let Err(error) = result
                    && !stop.is_cancelled()
                {
                    tracing::debug!(project_id = %descriptor.project_id, status = error.status, "Project archive preload deferred");
                }
            },
        }
    }
    Ok(())
}

/// OS locks make the foreground priority visible to the controller process as
/// well as the connector. Cancellation releases both the partial file and the
/// exclusive mutation lock; no partially downloaded archive becomes visible.
pub(crate) async fn foreground_waiter(state: &Path) {
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
        // Prefetch deliberately yields to this process-global mutation lock,
        // including mutations of unrelated temporary roots in other tests.
        let _unrelated_cache_mutation = lock().lock().await;
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "project_archives::node::tests::failed_project_background_downloads_child",
                "--ignored",
            ])
            .output()
            .await
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success() && stdout.contains("1 passed; 0 failed;"),
            "{stdout}\n{stderr}"
        );
    }

    #[tokio::test]
    #[ignore = "isolated process-global archive mutation lock child"]
    async fn failed_project_background_downloads_child() {
        use axum::{
            Router, body::Body, extract::Path as RoutePath, http::StatusCode, routing::post,
        };
        let root = tempfile::tempdir().unwrap();
        let payload = b"second project archive";
        let first = Archive {
            project_id: crate::config::id(),
            key: "a".repeat(64),
            revision: "b".repeat(40),
            origin_hash: None,
            hash: blake3::hash(payload).to_hex().to_string(),
            bytes: payload.len() as u64,
            logical_bytes: 100,
        };
        let second = Archive {
            project_id: crate::config::id(),
            ..first.clone()
        };
        let first_id = first.project_id.clone();
        let expected_requests = [first.project_id.clone(), second.project_id.clone()];
        let (requests, mut received_requests) = tokio::sync::mpsc::unbounded_channel();
        let app = Router::new().route(
            "/internal/node-workspace/cache/{project}",
            post(move |RoutePath(project): RoutePath<String>| {
                requests.send(project.clone()).unwrap();
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
        for project in expected_requests {
            assert_eq!(received_requests.try_recv().unwrap(), project);
        }
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
    async fn archive_admission_starts_after_reclaim_on_its_private_file() {
        use axum::{Router, body::Body, routing::post};
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let state = root.path().join("state");
        std::fs::create_dir_all(data.join("project-archives")).unwrap();
        std::fs::create_dir(&state).unwrap();
        let old = data.join("project-archives/old.zip");
        std::fs::write(&old, vec![0; 4 * 1024 * 1024]).unwrap();
        let bytes = vec![42; 1536 * 1024];
        let archive = Archive {
            project_id: crate::config::id(),
            key: "b".repeat(64),
            revision: "a".repeat(40),
            origin_hash: None,
            hash: blake3::hash(&bytes).to_hex().to_string(),
            bytes: bytes.len() as u64,
            logical_bytes: bytes.len() as u64,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route("/", post(move || async move { Body::from(bytes) }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = reqwest::Client::new().post(&url).send().await.unwrap();
        let policy = crate::storage::policy::for_node(
            &serde_json::json!({"reserveMiB":64,"reservePercent":1}),
        )
        .unwrap();
        let mut admission = None;
        let mut target_path = None;
        let result = download_admitted(
            &data,
            &archive,
            response,
            0,
            &CancellationToken::new(),
            &mut |target, bytes| {
                if admission.is_none() {
                    assert!(!old.exists(), "accounting sampled before reclaim");
                    assert!(
                        target.is_file(),
                        "accounting must target the private download inode"
                    );
                    target_path = Some(target.to_owned());
                    admission = Some(crate::native::ingress::Admission::new(
                        &data,
                        &state,
                        target,
                        &data,
                        3 * 1024 * 1024,
                        &policy,
                    )?);
                }
                assert_eq!(target_path.as_deref(), Some(target));
                admission.as_mut().unwrap().admit(bytes)
            },
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert!(archive.path(&data).is_file());
        server.abort();
    }

    #[tokio::test]
    async fn cancelled_stalled_archive_drains_its_partial_and_releases_storage() {
        use axum::{
            Router,
            body::{Body, Bytes},
            routing::post,
        };

        let root = tempfile::tempdir().unwrap();
        let archive = Archive {
            project_id: crate::config::id(),
            key: "b".repeat(64),
            revision: "a".repeat(40),
            origin_hash: None,
            hash: blake3::hash(b"partrest").to_hex().to_string(),
            bytes: 8,
            logical_bytes: 8,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route(
            "/",
            post(|| async {
                let first = futures_util::stream::once(async {
                    Ok::<_, std::io::Error>(Bytes::from_static(b"part"))
                });
                Body::from_stream(first.chain(futures_util::stream::pending()))
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = reqwest::Client::new().post(&url).send().await.unwrap();
        let stop = CancellationToken::new();
        let state = root.path().to_owned();
        let streamed_archive = archive.clone();
        let worker_stop = stop.clone();
        let (writing, wrote) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let _lease = crate::native::disk::lock(&state).await.unwrap();
            let mut writing = Some(writing);
            download_admitted(
                &state,
                &streamed_archive,
                response,
                CACHE_BYTES,
                &worker_stop,
                &mut |_, bytes| {
                    if bytes == 4
                        && let Some(writing) = writing.take()
                    {
                        let _ = writing.send(());
                    }
                    Ok(())
                },
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), wrote)
            .await
            .unwrap()
            .unwrap();
        stop.cancel();
        let error = tokio::time::timeout(std::time::Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.message, "Node workspace preparation stopped.");
        assert_eq!(
            std::fs::read_dir(root.path().join("project-archives"))
                .unwrap()
                .count(),
            0
        );
        assert!(!archive.path(root.path()).exists());
        let _new_owner = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            crate::native::disk::lock(root.path()),
        )
        .await
        .unwrap()
        .unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn archive_admission_refusal_never_publishes_the_partial_download() {
        use axum::{Router, body::Body, routing::post};
        let root = tempfile::tempdir().unwrap();
        let bytes = vec![42; 128 * 1024];
        let archive = Archive {
            project_id: crate::config::id(),
            key: "b".repeat(64),
            revision: "a".repeat(40),
            origin_hash: None,
            hash: blake3::hash(&bytes).to_hex().to_string(),
            bytes: bytes.len() as u64,
            logical_bytes: bytes.len() as u64,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route("/", post(move || async move { Body::from(bytes) }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = reqwest::Client::new().post(&url).send().await.unwrap();
        let mut payload = 0;
        let result = download_admitted(
            root.path(),
            &archive,
            response,
            CACHE_BYTES,
            &CancellationToken::new(),
            &mut |_, bytes| {
                if bytes != 2 * 5120 {
                    payload += bytes;
                    if payload > 65536 {
                        return Err(Error::new(507, "fixture reserve increased"));
                    }
                }
                Ok(())
            },
        )
        .await;
        assert_eq!(result.unwrap_err().status, 507);
        assert!(!archive.path(root.path()).exists());
        assert_eq!(
            std::fs::read_dir(root.path().join("project-archives"))
                .unwrap()
                .count(),
            0
        );
        server.abort();
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
            origin_hash: None,
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
            let result = download(
                root.path(),
                &archive,
                response,
                CACHE_BYTES,
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result.is_ok(), path == "valid");
            assert_eq!(archive.path(root.path()).exists(), path == "valid");
        }
        server.abort();
    }
}
