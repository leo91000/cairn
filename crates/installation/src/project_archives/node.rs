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

/// Receive into an already charged private inode. Its owner retains all
/// reservations through error drainage, including after caller cancellation.
async fn download_reserved(
    descriptor: &Archive,
    response: reqwest::Response,
    temporary: &tempfile::NamedTempFile,
    stop: &CancellationToken,
) -> Result<()> {
    if !response.status().is_success() {
        return Err(Error::unavailable("Project archive download rejected."));
    }
    let mut output = tokio::fs::File::from_std(temporary.reopen()?);
    let mut stream = Box::pin(
        response
            .bytes_stream()
            .take_until(stop.clone().cancelled_owned()),
    );
    let result = async {
        let mut bytes = 0u64;
        let mut hash = blake3::Hasher::new();
        while let Some(chunk) =
            tokio::time::timeout(std::time::Duration::from_secs(30), stream.next())
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
        Ok(())
    }
    .await;
    let drained = output.flush().await.map_err(Error::from);
    result.and(drained)
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
) -> Result<Option<String>> {
    stage_with_storage(
        session,
        root,
        descriptors,
        base_disk_mib,
        crate::native::staging::Storage::local()?,
    )
    .await
}

async fn stage_with_storage(
    session: &crate::nodes::executor::Session,
    root: &Path,
    descriptors: &[Archive],
    base_disk_mib: Option<u64>,
    storage: crate::native::staging::Storage,
) -> Result<Option<String>> {
    let stop = CancellationToken::new();
    let _cancel = stop.clone().drop_guard();
    let (session, root, descriptors) = (session.clone(), root.to_owned(), descriptors.to_vec());
    tokio::spawn(async move {
        let mut pin = crate::native::bases::PendingPin::new(&root, &session.attempt)?;
        let result = stage_owned(
            &session,
            &root,
            &descriptors,
            base_disk_mib,
            storage,
            stop.clone(),
        )
        .await;
        if result.is_ok() && !stop.is_cancelled() {
            pin.keep();
        }
        result
    })
    .await
    .map_err(Error::internal)?
}

async fn stage_owned(
    session: &crate::nodes::executor::Session,
    root: &Path,
    descriptors: &[Archive],
    base_disk_mib: Option<u64>,
    storage: crate::native::staging::Storage,
    stop: CancellationToken,
) -> Result<Option<String>> {
    let run = root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| Error::bad("Missing run identity."))?;
    crate::native::bases::release_pin(root, &session.attempt).await?;
    let retained = storage
        .state
        .clone()
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
        return Ok(None);
    }
    let _disk = crate::native::disk::lock_staging(&storage.state).await?;
    if let Some(disk_mib) = base_disk_mib {
        let lookup = crate::native::bases::request(
            "lookup",
            &serde_json::json!({ "archives": descriptors, "diskMiB": disk_mib }),
        )
        .await;
        if stop.is_cancelled() {
            return Err(Error::unavailable("Project archive preparation cancelled."));
        }
        if let Ok(value) = lookup
            && let Some(key) = value["base"]["key"].as_str()
            && key.len() == 64
            && key.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            tokio::fs::hard_link(
                storage
                    .state
                    .clone()
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
                descriptor.validate()?;
                crate::skills::private_dir(&root.join("workspace").join(&descriptor.project_id))
                    .await?;
            }
            return Ok(Some(key.to_owned()));
        }
    }
    let guard = lock().lock().await;
    let data = storage.data.clone();
    clean_interrupted_downloads(&data).await?;
    let pins = root.join("project-archives");
    crate::skills::private_dir(&pins).await?;
    let mut required = 0u64;
    let mut logical = 0u64;
    let mut missing = Vec::new();
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
            missing.push(descriptor.clone());
            required = required
                .checked_add(descriptor.bytes)
                .ok_or_else(|| Error::too_large("Project archives exceed disk budget."))?;
        }
        logical = logical
            .checked_add(descriptor.logical_bytes)
            .ok_or_else(|| Error::too_large("Project archives exceed disk budget."))?;
        required = required
            .checked_add(descriptor.logical_bytes)
            .ok_or_else(|| Error::too_large("Project archives exceed disk budget."))?;
    }
    // Current inputs are pinned before reclamation. Reserve the final filesystem
    // plus missing encoded objects; warm objects are already charged to the pool.
    let available =
        crate::native::disk::staging_budget_from(&storage.client, &storage.origin).await?;
    let reclaimed = reclaim(&data, required.saturating_sub(available)).await?;
    if required > available.saturating_add(reclaimed) {
        return Err(Error::new(
            507,
            "Project archive preparation exceeds free node disk budget.",
        ));
    }
    // Every promised byte is represented by a sparse private file before
    // releasing the pool gate. Cache cleanup never traverses run staging.
    let parent = root
        .parent()
        .ok_or_else(|| Error::bad("Missing project seed parent."))?;
    let receiving = tempfile::TempDir::new_in(parent)?;
    let promise = tempfile::NamedTempFile::new_in(receiving.path())?;
    promise.as_file().set_len(logical)?;
    let mut incoming = Vec::new();
    for descriptor in missing {
        let temporary = tempfile::NamedTempFile::new_in(receiving.path())?;
        temporary.as_file().set_len(descriptor.bytes)?;
        incoming.push((descriptor, temporary));
    }
    drop(guard);
    drop(_disk);

    for (descriptor, temporary) in &incoming {
        let selection =
            serde_json::json!({ "projectId": descriptor.project_id, "hash": descriptor.hash });
        let response = tokio::select! {
            biased;
            () = stop.cancelled() => return Err(Error::unavailable("Project archive preparation cancelled.")),
            result = crate::nodes::workspace::fetch(&session.client, &session.master, &session.token,
                &session.attempt, "archive", &selection) => result?,
        };
        download_reserved(descriptor, response, temporary, &stop).await?;
    }

    let _disk = crate::native::disk::lock_staging(&storage.state).await?;
    let _guard = lock().lock().await;
    if stop.is_cancelled() {
        return Err(Error::unavailable("Project archive preparation cancelled."));
    }
    for (descriptor, temporary) in incoming {
        let cached = match cached(&data, &descriptor).await? {
            Some(file) => {
                let (hash, bytes) = fingerprint(file).await?;
                hash == descriptor.hash && bytes == descriptor.bytes
            }
            None => false,
        };
        if !cached {
            prune(&data, descriptor.bytes).await?;
            tokio::fs::rename(temporary.path(), descriptor.path(&data)).await?;
            tokio::fs::File::open(data.join("project-archives"))
                .await?
                .sync_all()
                .await?;
        }
        tokio::fs::hard_link(
            descriptor.path(&data),
            pins.join(format!("{}.zip", descriptor.project_id)),
        )
        .await?;
    }
    for descriptor in descriptors {
        crate::skills::private_dir(&root.join("workspace").join(&descriptor.project_id)).await?;
    }
    drop(promise);
    drop(_guard);
    drop(_disk);
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
    async fn foreground_archive_network_wait_releases_pool_gate_but_retains_byte_promises() {
        use crate::native::{disk, staging::Storage};
        use axum::{
            Router,
            body::{Body, Bytes},
            routing::{get, post},
        };
        for cancel in [false, true] {
            let fixture = tempfile::tempdir().unwrap();
            let (data, state) = (fixture.path().join("data"), fixture.path().join("state"));
            crate::skills::private_dir(&data).await.unwrap();
            crate::skills::private_dir(&state).await.unwrap();
            let root = data.join("runs").join(crate::config::id());
            crate::skills::private_dir(&root).await.unwrap();
            let payload = vec![37; disk::MIB as usize];
            let descriptor = Archive {
                project_id: crate::config::id(),
                key: "a".repeat(64),
                revision: "b".repeat(40),
                origin_hash: None,
                hash: blake3::hash(&payload).to_hex().to_string(),
                bytes: disk::MIB,
                logical_bytes: disk::MIB,
            };
            let (entered, mut observed) = tokio::sync::mpsc::unbounded_channel();
            let (release, released) = tokio::sync::watch::channel(false);
            let budget = {
                let (data, state) = (data.clone(), state.clone());
                move || {
                    let (data, state) = (data.clone(), state.clone());
                    async move {
                        axum::Json(
                            serde_json::json!({ "availableBytes": disk::available(8 * disk::MIB, disk::used(&data, &state).await.unwrap()) }),
                        )
                    }
                }
            };
            let app = Router::new().route("/staging-budget", get(budget)).route(
                "/internal/node-workspace/{attempt}/archive",
                post(move || {
                    let (entered, released, payload) =
                        (entered.clone(), released.clone(), payload.clone());
                    async move {
                        let stream = futures_util::stream::unfold(
                            (0, released, payload, entered),
                            |(part, mut released, payload, entered)| async move {
                                match part {
                                    0 => Some((
                                        Ok::<_, std::io::Error>(Bytes::copy_from_slice(
                                            &payload[..65536],
                                        )),
                                        (1, released, payload, entered),
                                    )),
                                    1 => {
                                        entered.send(()).unwrap();
                                        released.wait_for(|value| *value).await.unwrap();
                                        Some((
                                            Ok(Bytes::copy_from_slice(&payload[65536..])),
                                            (2, released, payload, entered),
                                        ))
                                    }
                                    _ => None,
                                }
                            },
                        );
                        Body::from_stream(stream)
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let storage = Storage {
                state: state.clone(),
                data: data.clone(),
                client: reqwest::Client::new(),
                origin: origin.clone(),
            };
            let session = crate::nodes::executor::Session {
                client: reqwest::Client::new(),
                master: origin.parse().unwrap(),
                token: "synthetic-node".into(),
                attempt: crate::config::id(),
            };
            let (stage_root, archive) = (root.clone(), descriptor.clone());
            let preparing = tokio::spawn(async move {
                stage_with_storage(&session, &stage_root, &[archive], None, storage).await
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), observed.recv())
                .await
                .unwrap()
                .unwrap();
            let charged = disk::used(&data, &state).await.unwrap() >= 2 * disk::MIB;
            let acquired =
                tokio::time::timeout(std::time::Duration::from_secs(1), disk::lock(&state)).await;
            let free = matches!(&acquired, Ok(Ok(_)));
            if free {
                // Global cache reclamation cannot mistake active private files
                // for interrupted cache downloads or spend their reservations.
                reclaim_for_foreground(&data, u64::MAX, &[]).await.unwrap();
            }
            drop(acquired);
            if cancel {
                preparing.abort();
                let _ = preparing.await;
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        if disk::used(&data, &state).await.unwrap() < disk::MIB {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                assert!(!descriptor.path(&data).exists());
            } else {
                release.send(true).unwrap();
                preparing.await.unwrap().unwrap();
                assert_eq!(
                    tokio::fs::read(descriptor.path(&data)).await.unwrap(),
                    vec![37; disk::MIB as usize]
                );
                assert_eq!(
                    tokio::fs::read(
                        root.join("project-archives")
                            .join(format!("{}.zip", descriptor.project_id))
                    )
                    .await
                    .unwrap(),
                    vec![37; disk::MIB as usize]
                );
                assert!(disk::used(&data, &state).await.unwrap() < 2 * disk::MIB);
            }
            server.abort();
            assert!(free, "a foreground archive transfer retained the pool gate");
            assert!(
                charged,
                "the live encoded and expanded promises must stay charged"
            );
        }
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
            let result = download(root.path(), &archive, response, CACHE_BYTES).await;
            assert_eq!(result.is_ok(), path == "valid");
            assert_eq!(archive.path(root.path()).exists(), path == "valid");
        }
        server.abort();
    }
}
