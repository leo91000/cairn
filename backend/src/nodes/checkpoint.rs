//! Serialize captures of a journal-backed conversation disk.
use crate::{
    error::{Error, Result},
    skills::private_dir,
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub async fn capture(
    state: &Path,
    run: &str,
    socket: Option<PathBuf>,
    control: Arc<Mutex<()>>,
    stop: CancellationToken,
    attempt: &str,
    consistency: crate::storage::checkpoint::Consistency,
) -> Result<Value> {
    // A stopped disk has no physical owner. Running captures retain ownership
    // even when their crash-consistent boundary needs no guest control.
    let _stopped = if socket.is_none() {
        Some(crate::storage::environment::lock(state, run, "VM disk is still active.").await?)
    } else {
        None
    };
    let target = match socket {
        Some(socket) => crate::storage::checkpoint::Target::Running {
            socket,
            attempt: attempt.to_owned(),
            consistency,
        },
        None => crate::storage::checkpoint::Target::Stopped,
    };
    capture_target(state, run, target, control, stop).await
}

/// The pool holds physical ownership throughout capture, but its resume barrier
/// ends at the durable seal. Later writes cannot change the captured prefix.
pub(crate) async fn capture_paused(
    state: &Path,
    run: &str,
    owner: &crate::storage::environment::OwnerLease,
    stop: CancellationToken,
    barrier: tokio::sync::OwnedMutexGuard<()>,
) -> Result<Value> {
    if owner.directory != crate::storage::environment::directory(state, run)? {
        return Err(Error::conflict("Paused disk ownership changed."));
    }
    capture_target_with_barrier(
        state,
        run,
        crate::storage::checkpoint::Target::Paused,
        Arc::default(),
        stop,
        Some(barrier),
    )
    .await
}

async fn capture_target(
    state: &Path,
    run: &str,
    target: crate::storage::checkpoint::Target,
    control: Arc<Mutex<()>>,
    stop: CancellationToken,
) -> Result<Value> {
    capture_target_with_barrier(state, run, target, control, stop, None).await
}

async fn capture_target_with_barrier(
    state: &Path,
    run: &str,
    target: crate::storage::checkpoint::Target,
    control: Arc<Mutex<()>>,
    stop: CancellationToken,
    barrier: Option<tokio::sync::OwnedMutexGuard<()>>,
) -> Result<Value> {
    let _capture = prepare_capture(state, run).await?;
    let sealed = crate::storage::checkpoint::seal_target(state, run, target, control, stop).await?;
    // Later writes enter a new generation. Retained ownership protects the disk,
    // but the next turn no longer waits for hashing or S3 block reads.
    drop(barrier);
    sealed.finish(state, run).await
}

pub async fn capture_completed(state: &Path, run: &str, stop: CancellationToken) -> Result<Value> {
    let _capture = prepare_capture(state, run).await?;
    crate::storage::checkpoint::capture_completed(state, run, stop).await
}

async fn prepare_capture(state: &Path, run: &str) -> Result<crate::file_lock::Guard> {
    crate::validation::uuid(run)?;
    let disk = crate::storage::environment::directory(state, run)?;
    let capture = crate::file_lock::exclusive(
        &disk.join("snapshot.lock"),
        "A snapshot is already in progress.",
    )?;
    if !crate::storage::runtime::exists(&disk) {
        return Err(Error::conflict("Conversation has no S3-backed journal."));
    }
    let snapshots = state.join("snapshots");
    private_dir(&snapshots).await?;
    let mut entries = tokio::fs::read_dir(&snapshots).await?;
    // Earlier captures of this run are superseded by the new one.
    while let Some(entry) = entries.next_entry().await? {
        let owned = entry.file_type().await?.is_dir()
            && tokio::fs::read_to_string(entry.path().join("run"))
                .await
                .is_ok_and(|owner| owner == run);
        if owned {
            tokio::fs::remove_dir_all(entry.path()).await?;
        }
    }
    Ok(capture)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Disk;
    use serde_json::json;

    #[tokio::test]
    async fn retained_resume_can_write_while_manifest_reconstruction_waits_for_a_block() {
        check_writes_during_reconstruction(true).await;
    }

    #[tokio::test]
    async fn periodic_capture_keeps_guest_running_and_fences_the_captured_prefix() {
        check_writes_during_reconstruction(false).await;
    }

    async fn check_writes_during_reconstruction(retained: bool) {
        let root = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let owner = crate::storage::environment::ownership(root.path(), &run, "busy")
            .await
            .unwrap();
        let block = vec![7u8; 4096];
        let started = Arc::new(tokio::sync::Notify::new());
        let release = CancellationToken::new();
        let server = axum::Router::new().route(
            "/internal/node-restore/{hash}",
            axum::routing::get({
                let started = started.clone();
                let release = release.clone();
                let block = block.clone();
                move || {
                    let (started, release, block) =
                        (started.clone(), release.clone(), block.clone());
                    async move {
                        started.notify_one();
                        release.cancelled().await;
                        block
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
        let manifest = json!({
            "version": 1, "size": 4096, "blockSize": crate::nodes::snapshots::BLOCK,
            "blocks": [{ "offset": 0, "size": 4096, "hash": crate::storage::digest::block(&block) }],
        });
        let source = json!({ "master": format!("http://{address}/"), "grant": "retained-test", "policy": { "reserveMiB": 64, "reservePercent": 1 } });
        let disk =
            crate::storage::runtime::create(&owner.directory.join("lazy"), &manifest, &source)
                .await
                .unwrap();
        disk.write_at(0, b"old").unwrap();
        std::fs::write(owner.directory.join("runtime.json"), b"{}").unwrap();
        drop(disk);
        let volume = crate::storage::runtime::load(&owner.directory)
            .await
            .unwrap();
        let barrier = Arc::new(Mutex::new(()));
        let capture = {
            let (state, run, owner) = (root.path().to_owned(), run.clone(), owner.clone());
            let barrier = barrier.clone();
            tokio::spawn(async move {
                if retained {
                    let guard = barrier.lock_owned().await;
                    return capture_paused(&state, &run, &owner, CancellationToken::new(), guard)
                        .await;
                }
                // There is deliberately no guest control socket or VM API.
                // Periodic capture must not freeze or pause the live guest.
                capture(
                    &state,
                    &run,
                    Some(state.join("absent-guest.sock")),
                    barrier,
                    CancellationToken::new(),
                    &crate::config::id(),
                    crate::storage::checkpoint::Consistency::Crash,
                )
                .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        let resume =
            tokio::time::timeout(std::time::Duration::from_millis(250), barrier.lock()).await;
        assert!(
            resume.is_ok(),
            "The next turn must not wait for remote block reconstruction"
        );
        volume.disk.write_at(0, b"new").unwrap();
        assert!(
            !volume.stop.is_cancelled(),
            "Mounted remote reads remain usable"
        );
        assert!(
            crate::storage::environment::ownership(root.path(), &run, "busy")
                .await
                .is_err(),
            "Capture must not release the live disk's physical ownership"
        );
        release.cancel();
        let snapshot = capture.await.unwrap().unwrap();
        assert_eq!(snapshot["manifest"]["consistency"], "crash");
        assert_eq!(snapshot["manifest"]["pauseMs"], 0);
        let generation = snapshot["manifest"]["generation"].as_i64().unwrap();
        let hash = snapshot["manifest"]["blocks"][0]["hash"]
            .as_str()
            .unwrap()
            .to_owned();
        tokio::task::spawn_blocking(move || {
            assert_eq!(
                &volume.disk.captured_block(generation, &hash).unwrap()[..3],
                b"old"
            );
            volume
                .disk
                .commit_published(generation, &crate::config::id())
                .unwrap();
            let mut bytes = [0; 3];
            volume.disk.read_at(0, &mut bytes).unwrap();
            assert_eq!(&bytes, b"new");
            assert!(
                volume.disk.accounting().unwrap()["dirtyBytes"]
                    .as_u64()
                    .unwrap()
                    > 0
            );
        })
        .await
        .unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn retained_capture_preserves_mounted_reads_and_ownership() {
        let root = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let owner = crate::storage::environment::ownership(root.path(), &run, "busy")
            .await
            .unwrap();
        let manifest = json!({
            "version": 1,
            "size": 4096,
            "blockSize": crate::nodes::snapshots::BLOCK,
            "blocks": [{ "offset": 0, "size": 4096, "hash": null }],
        });
        let source = json!({
            "master": "http://127.0.0.1:9/",
            "grant": "synthetic-retained-owner",
            "policy": { "reserveMiB": 64, "reservePercent": 1 },
        });
        let disk =
            crate::storage::runtime::create(&owner.directory.join("lazy"), &manifest, &source)
                .await
                .unwrap();
        disk.write_at(0, b"retained durable data").unwrap();
        std::fs::write(
            owner.directory.join("runtime.json"),
            b"{\"runtimeId\":\"fixture\"}",
        )
        .unwrap();
        drop(disk);
        let volume = crate::storage::runtime::load(&owner.directory)
            .await
            .unwrap();
        let disk = volume.disk.clone();
        let cancellation = volume.stop.clone();
        for _ in 0..2 {
            assert!(volume.inspect().await.unwrap()["waitingFor"].is_null());
            let snapshot = capture_paused(
                root.path(),
                &run,
                &owner,
                CancellationToken::new(),
                Arc::new(Mutex::new(())).lock_owned().await,
            )
            .await
            .unwrap();
            assert_eq!(snapshot["manifest"]["consistency"], "crash");
            assert!(snapshot["manifest"]["generation"].as_i64().unwrap() > 0);
            assert!(
                !cancellation.is_cancelled(),
                "Publishing a paused guest never cancels its mounted source"
            );
            assert!(Arc::ptr_eq(
                &volume,
                &crate::storage::runtime::load(&owner.directory)
                    .await
                    .unwrap()
            ));
            assert!(
                crate::storage::environment::ownership(root.path(), &run, "busy")
                    .await
                    .is_err()
            );
            let mut bytes = [0; 21];
            disk.read_at(0, &mut bytes).unwrap();
            assert_eq!(&bytes, b"retained durable data");
        }
    }
}
