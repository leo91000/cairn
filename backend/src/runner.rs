//! Authenticated execution interface shared by chats, scheduled tasks and recovery.
mod broker;
mod client;
mod routes;

pub use client::client;

use crate::{
    error::{Error, Result},
    microvm::{host, pool::Pool},
    skills::{atomic_write, private_dir},
};
use axum::{Router, routing::any};
use broker::Broker;
use std::{os::fd::AsRawFd, path::PathBuf};
use tokio_util::sync::CancellationToken;

pub const CONTROLLER_INTERRUPTED: i32 = 75;

fn router(broker: Broker) -> Router {
    Router::new()
        .fallback(any(routes::handler))
        .with_state(broker)
}

fn env_path(name: &str, default: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| default.into()))
}

/// A controller container restart fences its entire PID namespace. Mark interrupted attempts
/// exited, but retain all guest disks so the manager can launch replacement attempts.
async fn fence_interrupted_attempts(state: &std::path::Path) -> Result<()> {
    let mut files = tokio::fs::read_dir(state).await?;
    while let Some(file) = files.next_entry().await? {
        if file.path().extension().is_some_and(|s| s == "active") {
            atomic_write(&file.path().with_extension("exit"), b"143").await?;
            tokio::fs::remove_file(file.path()).await?;
        }
    }
    Ok(())
}

pub async fn serve(stop: CancellationToken) -> Result<()> {
    let concurrency = crate::config::concurrency()?;
    let data = env_path("DATA_DIR", "/data");
    let state = env_path("RUNNER_STATE_DIR", "/runner-state");
    private_dir(&state).await?;
    let controller = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state.join("controller.lock"))?;
    if unsafe { libc::flock(controller.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::unavailable(
            "Another VM controller owns this storage.",
        ));
    }
    // No snapshot transfer survives a controller restart; durable points live on the master/S3.
    if state.join("snapshots").exists() {
        tokio::fs::remove_dir_all(state.join("snapshots")).await?;
    }
    let image = host::assets(&state).await?;
    fence_interrupted_attempts(&state).await?;
    let pool = Pool::new(state.clone(), image, stop.clone(), concurrency).await?;
    pool.initialize(crate::microvm::budget::cgroup().await?)
        .await?;
    let memory_monitor = tokio::spawn(pool.clone().monitor());
    let broker = Broker::new(data, state, pool, stop.clone());
    let draining = broker.clone();
    let address = std::env::var("RUNNER_BIND").unwrap_or_else(|_| "0.0.0.0:4311".into());
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, router(broker))
        .with_graceful_shutdown(stop.cancelled_owned())
        .await?;
    let attempts = draining
        .active
        .lock()
        .await
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for id in attempts {
        draining.stop(&id).await?;
    }
    draining.pool.drain().await;
    memory_monitor.await.map_err(Error::internal)?;
    drop(controller);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microvm::plan::Plan;
    use axum::body::Body;
    use broker::Attempt;
    use futures_util::StreamExt;
    use serde_json::{Value, json};
    use std::{
        io::{Seek, SeekFrom, Write},
        os::unix::fs::MetadataExt,
        path::Path,
        sync::Arc,
        time::Duration,
    };
    use tokio::sync::watch;
    use tower::ServiceExt;

    const SECRET: &str = "synthetic-runner-secret";

    /// Broker over private `data` and `state` directories under `root`.
    async fn fixture_broker(root: &Path) -> Broker {
        let data = root.join("data");
        let state = root.join("state");
        private_dir(&data).await.unwrap();
        private_dir(&state).await.unwrap();
        std::fs::write(data.join("runner-secret"), SECRET).unwrap();
        let stop = CancellationToken::new();
        let pool = Pool::new(state.clone(), root.into(), stop.clone(), 1)
            .await
            .unwrap();
        Broker::new(data, state, pool, stop)
    }

    /// An attempt that is still starting: it has no guest socket yet.
    fn starting_attempt(run: &str) -> (watch::Sender<bool>, Attempt) {
        let (done, receiver) = watch::channel(false);
        let attempt = Attempt {
            stop: CancellationToken::new(),
            done: receiver,
            socket: Arc::default(),
            plan: Plan::new(json!({
                "runId": run
            })),
            imports: Arc::default(),
            control: Arc::default(),
        };
        (done, attempt)
    }

    fn authorized_post(uri: String, body: Body) -> axum::http::Request<Body> {
        axum::http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("authorization", format!("Bearer {SECRET}"))
            .body(body)
            .unwrap()
    }

    #[tokio::test]
    async fn snapshot_batch_reuses_one_journal_and_releases_the_stopped_disk() {
        use crate::storage::Disk;
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let state = broker.state.clone();
        let run = crate::config::id();
        let snapshot = crate::config::id();
        let directory = state.join("disks").join(&run);
        let block = crate::nodes::snapshots::BLOCK;
        let base = json!({
            "version": 1,
            "size": block + 1024,
            "blockSize": block,
            "blocks": [
                { "offset": 0, "size": block, "hash": null },
                { "offset": block, "size": 1024, "hash": null }
            ]
        });
        let source = json!({
            "master": "http://127.0.0.1:9/",
            "grant": "fixture"
        });
        let disk = crate::storage::runtime::create(&directory.join("lazy"), &base, &source)
            .await
            .unwrap();
        disk.write_at(0, &vec![7; block as usize]).unwrap();
        disk.write_at(block, &[9; 1024]).unwrap();
        let generation = disk.seal().unwrap();
        let mut manifest = disk.capture(generation).unwrap();
        manifest["onDemand"] = true.into();
        manifest["generation"] = generation.into();

        // A later unpublished write must not replace data from the captured generation.
        disk.write_at(0, &[3; 4096]).unwrap();
        drop(disk);
        let captured = state.join("snapshots").join(&snapshot);
        private_dir(&captured).await.unwrap();
        std::fs::write(captured.join("run"), run).unwrap();
        std::fs::write(captured.join("manifest.json"), manifest.to_string()).unwrap();
        let hashes: Vec<_> = manifest["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["hash"].clone())
            .collect();
        let request = || {
            let batch = json!({ "hashes": hashes }).to_string();
            authorized_post(format!("/snapshots/{snapshot}/blocks"), Body::from(batch))
        };
        let app = router(broker);

        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), 200);
        let mut stream = response.into_body().into_data_stream();
        assert_eq!(
            stream.next().await.unwrap().unwrap().as_ref(),
            vec![7; block as usize]
        );
        let opened = crate::storage::runtime::load(&directory).await.unwrap();
        let cancellation = opened.stop.clone();
        let identity = Arc::downgrade(&opened);
        drop(opened);
        assert_eq!(stream.next().await.unwrap().unwrap().as_ref(), &[9; 1024]);
        assert!(Arc::ptr_eq(
            &identity.upgrade().unwrap(),
            &crate::storage::runtime::load(&directory).await.unwrap()
        ));
        assert!(stream.next().await.is_none());
        assert!(cancellation.is_cancelled());
        assert!(identity.upgrade().is_none());
        let reopened = crate::storage::runtime::load(&directory).await.unwrap();
        let mut bytes = [0; 4096];
        reopened.disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [3; 4096]);
        drop(reopened);

        let response = app.clone().oneshot(request()).await.unwrap();
        let mut stream = response.into_body().into_data_stream();
        stream.next().await.unwrap().unwrap();
        let cancellation = crate::storage::runtime::load(&directory)
            .await
            .unwrap()
            .stop
            .clone();
        drop(stream);
        assert!(
            cancellation.is_cancelled(),
            "disconnect must cancel stopped reads"
        );
        assert!(crate::storage::runtime::live(&directory).is_none());

        // The same response must not cancel a mounted VM's shared volume.
        let _owner = crate::file_lock::exclusive(&directory.join("lock"), "busy").unwrap();
        let mounted = crate::storage::runtime::load(&directory).await.unwrap();
        let response = app.oneshot(request()).await.unwrap();
        let mut stream = response.into_body().into_data_stream();
        stream.next().await.unwrap().unwrap();
        drop(stream);
        assert!(!mounted.stop.is_cancelled());
    }

    #[tokio::test]
    async fn wait_does_not_report_completion_until_the_attempt_is_released() {
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let id = crate::config::id();
        let (_done, attempt) = starting_attempt(&crate::config::id());
        broker.active.lock().await.insert(id.clone(), attempt);

        // Execution writes its exit marker before asynchronous attempt cleanup.
        // A caller may resume this workspace as soon as /wait returns its status.
        std::fs::write(broker.state.join(format!("{id}.exit")), "0").unwrap();
        let app = router(broker.clone());
        let response = app
            .oneshot(authorized_post(format!("/runs/{id}/wait"), Body::empty()))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let mut body = response.into_body().into_data_stream();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), body.next())
                .await
                .is_err(),
            "completion must not escape while the previous attempt still owns the workspace"
        );

        broker.active.lock().await.remove(&id);
        let bytes = tokio::time::timeout(Duration::from_secs(2), async {
            let mut bytes = Vec::new();
            while let Some(chunk) = body.next().await {
                bytes.extend_from_slice(&chunk.unwrap());
            }
            bytes
        })
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({ "StatusCode": 0 })
        );
    }

    #[tokio::test]
    async fn snapshot_before_attempt_registration_is_deferred_without_touching_disk() {
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let state = broker.state.clone();
        let id = crate::config::id();
        let response = router(broker)
            .oneshot(authorized_post(
                format!("/runs/{id}/snapshot"),
                Body::from("{}"),
            ))
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            409,
            "registration is not a storage failure"
        );
        assert!(
            std::fs::read_dir(state.join("disks"))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[tokio::test]
    async fn snapshot_during_startup_is_deferred_without_touching_the_disk() {
        let root = tempfile::tempdir().unwrap();
        let broker = fixture_broker(root.path()).await;
        let state = broker.state.clone();
        let id = crate::config::id();
        let run = crate::config::id();
        let (_done, attempt) = starting_attempt(&run);
        broker.active.lock().await.insert(id.clone(), attempt);
        let app = router(broker);

        let response = app
            .oneshot(authorized_post(
                format!("/runs/{id}/snapshot"),
                Body::from("{}"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), 409, "startup is not a storage failure");
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"], "VM is still starting.");
        assert!(!state.join("disks").join(run).exists());
    }

    async fn request(
        app: &Router,
        run: &str,
        action: &str,
        transfer: &str,
        credential: &str,
    ) -> u16 {
        let request = axum::http::Request::builder()
            .method("POST")
            .uri(format!("/disks/{run}/{action}"))
            .header("authorization", format!("Bearer {credential}"))
            .body(Body::from(json!({ "transfer": transfer }).to_string()))
            .unwrap();
        app.clone()
            .oneshot(request)
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    #[tokio::test]
    async fn workspace_disk_deletion_uses_authenticated_http_and_preserves_the_lock() {
        let root = tempfile::TempDir::new().unwrap();
        let data = root.path().join("data");
        let state = root.path().join("state");
        private_dir(&data).await.unwrap();
        private_dir(&state).await.unwrap();
        std::fs::write(data.join("runner-secret"), SECRET).unwrap();
        let stop = CancellationToken::new();
        let pool = Pool::new(
            state.clone(),
            root.path().join("unused-image"),
            stop.clone(),
            1,
        )
        .await
        .unwrap();
        let app = router(Broker::new(data.clone(), state.clone(), pool, stop));
        let run = crate::config::id();
        let failed_attempt = crate::config::id();
        std::fs::write(state.join(format!("{failed_attempt}.run")), &run).unwrap();
        std::fs::write(
            state.join(format!("{failed_attempt}.log")),
            "private agent output",
        )
        .unwrap();
        let transfer = crate::config::id();
        let directory = state.join("disks").join(&run);
        private_dir(&directory).await.unwrap();
        let mut file = std::fs::File::create(directory.join("data.ext4")).unwrap();
        file.seek(SeekFrom::Start(8 * 1024 * 1024)).unwrap();
        file.write_all(b"native session and unpublished work")
            .unwrap();
        file.sync_all().unwrap();
        assert_eq!(request(&app, &run, "delete", &transfer, "wrong").await, 401);
        std::fs::write(directory.join("stale-older.ext4"), "older copy").unwrap();
        assert_eq!(request(&app, &run, "prune", &transfer, SECRET).await, 200);
        assert!(!directory.join("stale-older.ext4").exists());
        assert!(directory.join("data.ext4").exists());
        assert_eq!(request(&app, &run, "export", &transfer, SECRET).await, 405);
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert_eq!(request(&app, &run, "delete", &transfer, SECRET).await, 409);
        let inode = lock.metadata().unwrap().ino();
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
        drop(lock);
        assert_eq!(request(&app, &run, "delete", &transfer, SECRET).await, 200);
        assert!(!directory.join("data.ext4").exists());
        assert!(!state.join(format!("{failed_attempt}.log")).exists());
        assert!(state.join(format!("{failed_attempt}.stopped")).exists());
        assert_eq!(
            std::fs::metadata(directory.join("lock")).unwrap().ino(),
            inode
        );
        assert_eq!(request(&app, &run, "import", &transfer, SECRET).await, 405);
        assert!(!directory.join("data.ext4").exists());
    }
}
