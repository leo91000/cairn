//! Host execution. Permissions and lifecycle are separate from coding-engine adapters.
pub(crate) mod bases;
#[cfg(test)]
mod bases_tests;
mod bases_warmer;
pub(crate) mod block;
pub(crate) mod disk;
mod home;
mod mode;
pub mod policy;
mod protection;
pub mod releases;
mod service;
mod systemd;
mod watchdog;
mod workspace;

pub(crate) use mode::matches as mode_matches;
pub(crate) use service::native_budget;
pub use service::serve;
pub use watchdog::serve as watchdog;

pub fn enabled() -> bool {
    std::env::var("CAIRN_NODE_BACKEND").is_ok_and(|value| value == "native")
}

/// Metadata transitions must survive power loss before acknowledgement.
pub(crate) async fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> crate::error::Result<()> {
    crate::skills::atomic_write(path, bytes).await?;
    let parent = path
        .parent()
        .ok_or_else(|| crate::error::Error::bad("Native metadata needs a parent directory."))?;
    tokio::fs::File::open(parent).await?.sync_all().await?;
    Ok(())
}

/// Controller and independent guardian share the first durable initiating
/// reason. Later fencing observations must not replace quota measurements.
pub(crate) async fn record_stop_once(
    path: &std::path::Path,
    details: &serde_json::Value,
) -> crate::error::Result<()> {
    use std::io::Write;

    let lock = path.with_extension("lock");
    let owner = loop {
        match crate::file_lock::exclusive(&lock, "Native stop diagnostic is being recorded.") {
            Err(error) if error.is_conflict() => {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            result => break result?,
        }
    };
    let path = path.to_owned();
    let bytes = serde_json::to_vec(details)?;
    // Caller timeouts may detach this task, but cannot release its flock while
    // a write/commit is still in flight. Never replace another durable cause.
    tokio::task::spawn_blocking(move || -> crate::error::Result<()> {
        let _owner = owner;
        if path.try_exists()? {
            return Ok(());
        }
        let parent = path.parent().ok_or_else(|| {
            crate::error::Error::bad("Native stop metadata needs a parent directory.")
        })?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        match file.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
            Err(error) => return Err(error.error.into()),
        }
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })
    .await
    .map_err(crate::error::Error::internal)?
}

pub(crate) fn state_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("RUNNER_STATE_DIR").unwrap_or_else(|_| "/var/lib/cairn-native/state".into()),
    )
}

pub(crate) fn controller_client(path: &std::path::Path) -> crate::error::Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .unix_socket(path.to_owned())
        .build()
        .map_err(crate::error::Error::internal)
}

/// Run before runtime threads start. systemd must resolve stable host paths;
/// only the ublk backend gets a private mount namespace before its device exists.
/// Every journal/control worker must inherit writeback progress.
pub fn prepare_storage_process() -> std::io::Result<()> {
    {
        use std::os::unix::fs::MetadataExt;

        if std::fs::metadata("/proc/self/ns/mnt")?.ino()
            != std::fs::metadata("/proc/1/ns/mnt")?.ino()
        {
            return Err(std::io::Error::other(
                "Native controller must share the system manager's mount namespace.",
            ));
        }
        crate::storage::ublk::prepare_io_threads()
    }
}

#[cfg(test)]
mod stop_tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_stop_recorders_publish_one_complete_immutable_cause() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("stop-reason.json");
        let mut writers = Vec::new();
        for index in 0..16 {
            let path = path.clone();
            writers.push(tokio::spawn(async move {
                record_stop_once(
                    &path,
                    &serde_json::json!({ "initiator": index, "usage": vec![index; 32] }),
                )
                .await
            }));
        }
        for writer in writers {
            writer.await.unwrap().unwrap();
        }
        let first = tokio::fs::read(&path).await.unwrap();
        let record: serde_json::Value = serde_json::from_slice(&first).unwrap();
        let index = record["initiator"].as_u64().unwrap();
        assert!(index < 16);
        assert!(
            record["usage"]
                .as_array()
                .unwrap()
                .iter()
                .all(|value| value.as_u64() == Some(index))
        );
        record_stop_once(&path, &serde_json::json!({ "reason": "later fencing" }))
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), first);
    }
}
