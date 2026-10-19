//! Host execution. Permissions and lifecycle are separate from coding-engine adapters.
pub(crate) mod disk;
pub mod policy;
pub mod releases;
mod service;
mod systemd;
mod watchdog;
mod workspace;

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
