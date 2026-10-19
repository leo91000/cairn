//! Stopped file checkpoints reuse the authenticated S3 block transfer machinery.
//! The archive is a versioned file stream, never an ext4 disk or a live directory scan.
use super::atomic_write;
use crate::{
    error::{Error, Result},
    nodes::{files, snapshots},
    skills::private_dir,
    validation::text,
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tokio::io::{AsyncReadExt, BufReader};

pub const KIND: &str = "native-files-v1";

pub fn directory(state: &Path, run: &str) -> Result<PathBuf> {
    crate::validation::uuid(run)?;
    Ok(state.join("native").join(run))
}

/// A new execution invalidates restore idempotence after acquiring exclusive
/// ownership. Retrying an old backup after later work must really restore it.
pub async fn execution_ownership(state: &Path, run: &str) -> Result<crate::file_lock::Guard> {
    let root = directory(state, run)?;
    private_dir(&root).await?;
    let owner = crate::file_lock::exclusive(
        &root.join("owner.lock"),
        "Native workspace is still active.",
    )?;
    if root.join("restore.pending.json").exists() {
        return Err(Error::conflict(
            "Native restore must finish before execution resumes.",
        ));
    }
    match tokio::fs::remove_file(root.join("restored.json")).await {
        Ok(()) => tokio::fs::File::open(&root).await?.sync_all().await?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(owner)
}

/// Credentials and runtime configuration are regenerated for every attempt.
/// Only coding-engine session state, user tools and workspace files are durable.
fn included(path: &Path) -> bool {
    let value = path.to_string_lossy();
    if value == "workspaces"
        || value.starts_with("workspaces/")
        || value == "output"
        || value.starts_with("output/")
    {
        return true;
    }
    if value == "home" {
        return true;
    }
    let Some(relative) = value.strip_prefix("home/") else {
        return false;
    };
    if relative.ends_with(".sock") || relative == ".cache" || relative.starts_with(".cache/") {
        return false;
    }
    if relative == ".agents/skills" || relative.starts_with(".agents/skills/") {
        return false;
    }
    if relative.starts_with(".opencode/data/") || relative.starts_with(".opencode/state/") {
        return !relative.ends_with("/auth.json") && !relative.ends_with("/credentials.json");
    }
    for provider in [
        ".codex",
        ".claude",
        ".opencode",
        ".config/opencode",
        ".local/share/opencode",
    ] {
        if relative == provider {
            return true;
        }
        if let Some(child) = relative.strip_prefix(&format!("{provider}/")) {
            if provider == ".codex" && child.starts_with("state_") && child.contains(".sqlite") {
                return true;
            }
            return [
                "sessions",
                "archived_sessions",
                "projects",
                "todos",
                "storage",
                "session",
                "session_index.jsonl",
                "data",
                "state",
            ]
            .iter()
            .any(|allowed| child == *allowed || child.starts_with(&format!("{allowed}/")));
        }
    }
    !matches!(
        relative,
        ".gitconfig" | ".netrc" | ".npmrc" | ".git-credentials" | ".claude.json"
    ) && ![".ssh", ".aws", ".config/gh", ".config/gcloud"]
        .iter()
        .any(|private| relative == *private || relative.starts_with(&format!("{private}/")))
}

pub async fn copy(source: &Path, target: &Path, budget: u64) -> Result<()> {
    let (mut writer, reader) = tokio::io::duplex(131072);
    let source = source.to_owned();
    let send = tokio::spawn(async move { files::send(&source, &mut writer).await });
    let result = files::receive(&mut BufReader::new(reader), target, budget).await;
    if result.is_err() {
        send.abort();
    }
    result?;
    send.await.map_err(Error::internal)??;
    sync_directories(target).await
}

/// Files received through nodes::files are already synced; sync each directory
/// from leaves upward before a journal can name this prepared tree.
pub async fn sync_directories(root: &Path) -> Result<()> {
    let mut pending = vec![root.to_owned()];
    let mut directories = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = tokio::fs::symlink_metadata(&path).await?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let mut entries = tokio::fs::read_dir(&path).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                pending.push(entry.path());
            }
        }
        directories.push(path);
    }
    for directory in directories.into_iter().rev() {
        tokio::fs::File::open(directory).await?.sync_all().await?;
    }
    Ok(())
}

/// Call only while holding the conversation's physical lock and after its cgroup is empty.
pub async fn capture(state: &Path, run: &str, attempt: &str, budget: u64) -> Result<Value> {
    let root = directory(state, run)?;
    let snapshots_root = state.join("snapshots");
    private_dir(&snapshots_root).await?;
    let id = crate::config::id();
    let temporary = tempfile::tempdir_in(&snapshots_root)?;
    let mut archive = tokio::fs::File::create(temporary.path().join("disk")).await?;
    let started = std::time::Instant::now();
    // Bound encoded bytes too: the transfer includes framing and base64 payloads.
    let source = root.join("data");
    let logical = bytes(&source).await?;
    if logical > budget {
        return Err(Error::bad(
            "Native files exceed their disk budget; local work is retained.",
        ));
    }
    let maximum = budget.saturating_mul(2).saturating_add(16 * 1024 * 1024);
    let needed = logical.saturating_mul(2).saturating_add(16 * 1024 * 1024);
    let path = std::ffi::CString::new(snapshots_root.as_os_str().as_encoded_bytes())
        .map_err(Error::internal)?;
    let mut capacity = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), capacity.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let capacity = unsafe { capacity.assume_init() };
    let free = capacity.f_bavail.saturating_mul(capacity.f_frsize);
    if free < needed.saturating_add(256 * 1024 * 1024) {
        return Err(Error::unavailable(
            "Insufficient free space for a native checkpoint; local work is retained.",
        ));
    }
    let (mut writer, reader) = tokio::io::duplex(131072);
    let send =
        tokio::spawn(async move { files::send_filtered(&source, &mut writer, included).await });
    let written = tokio::io::copy(&mut reader.take(maximum + 1), &mut archive).await;
    if !matches!(written, Ok(count) if count <= maximum) {
        send.abort();
        return Err(Error::bad(
            "Native checkpoint write failed or exceeded its disk budget.",
        ));
    }
    send.await.map_err(Error::internal)??;
    archive.sync_all().await?;
    drop(archive);
    let mut manifest = snapshots::index(&temporary.path().join("disk")).await?;
    manifest["kind"] = KIND.into();
    manifest["runtime"] = match tokio::fs::read(root.join("runtime.json")).await {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(_) => {
            json!({
                "backend": "native",
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "runtimeId": "development",
            })
        }
    };
    manifest["attempt"] = attempt.into();
    manifest["capturedAt"] = crate::config::now().into();
    manifest["indexMs"] = (started.elapsed().as_millis() as u64).into();
    manifest["incremental"] = false.into();
    atomic_write(
        &temporary.path().join("manifest.json"),
        &serde_json::to_vec(&manifest)?,
    )
    .await?;
    atomic_write(&temporary.path().join("run"), run.as_bytes()).await?;
    let destination = snapshots_root.join(&id);
    tokio::fs::rename(temporary.path(), destination).await?;
    tokio::fs::File::open(&snapshots_root)
        .await?
        .sync_all()
        .await?;
    let snapshot = json!({ "id": id, "manifest": manifest });
    atomic_write(
        &root.join("completed.json"),
        &serde_json::to_vec(&snapshot)?,
    )
    .await?;
    Ok(snapshot)
}

/// Collect only archives whose root-owned ownership marker names this conversation.
pub async fn collect_archives(state: &Path, run: &str, keep: Option<&str>) -> Result<()> {
    let parent = state.join("snapshots");
    if !parent.exists() {
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(parent).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_dir()
            && keep.is_none_or(|keep| entry.file_name().to_string_lossy() != keep)
            && tokio::fs::read_to_string(entry.path().join("run"))
                .await
                .is_ok_and(|owner| owner == run)
        {
            tokio::fs::remove_dir_all(entry.path()).await?;
        }
    }
    Ok(())
}

pub async fn retained_bytes(state: &Path, run: &str) -> Result<u64> {
    let mut retained = bytes(&directory(state, run)?).await.unwrap_or(0);
    let parent = state.join("snapshots");
    if !parent.exists() {
        return Ok(retained);
    }
    let mut entries = tokio::fs::read_dir(parent).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_dir()
            && tokio::fs::read_to_string(entry.path().join("run"))
                .await
                .is_ok_and(|owner| owner == run)
        {
            retained = retained.saturating_add(bytes(&entry.path()).await?);
        }
    }
    Ok(retained)
}

pub async fn completed(state: &Path, run: &str) -> Result<Value> {
    let root = directory(state, run)?;
    if root.join("checkpoint-error").exists() {
        return Err(Error::unavailable(
            "Native checkpoint failed; local work is retained on its node.",
        ));
    }
    let snapshot: Value =
        serde_json::from_slice(&tokio::fs::read(root.join("completed.json")).await?)?;
    if !state.join("snapshots").join(text(&snapshot, "id")).exists() {
        return Err(Error::new(
            412,
            "Completed native checkpoint has already been collected.",
        ));
    }
    Ok(snapshot)
}

pub async fn restore(state: &Path, run: &str, request: &Value, budget: u64) -> Result<Value> {
    let manifest = &request["manifest"];
    if manifest["kind"] != KIND
        || manifest["runtime"]["os"] != std::env::consts::OS
        || manifest["runtime"]["arch"] != std::env::consts::ARCH
    {
        return Err(Error::conflict(
            "Native checkpoints require the same backend, OS and architecture.",
        ));
    }
    snapshots::validate(manifest)?;
    let root = directory(state, run)?;
    private_dir(&root).await?;
    let _owner =
        crate::file_lock::exclusive(&root.join("owner.lock"), "Native workspace is active.")?;
    crate::validation::uuid(text(request, "backupId"))?;
    if root.join("restore.pending.json").exists() {
        let pending: Value =
            serde_json::from_slice(&tokio::fs::read(root.join("restore.pending.json")).await?)?;
        if pending["backupId"] != request["backupId"] {
            return Err(Error::conflict(
                "Another native restore needs to finish first.",
            ));
        }
        return finish_restore(&root, &pending).await;
    }
    if let Ok(saved) = tokio::fs::read(root.join("restored.json")).await {
        let restored: Value = serde_json::from_slice(&saved)?;
        if restored["backupId"] == request["backupId"] && root.join("data").exists() {
            return Ok(json!({ "ready": true }));
        }
    }
    let temporary = tempfile::Builder::new()
        .prefix("restore-")
        .tempdir_in(&root)?;
    let master = crate::nodes::connector::disk_master(text(request, "master"))?;
    let grant = text(request, "grant").to_owned();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(Error::internal)?;
    let archive = temporary.path().join("archive");
    snapshots::restore(&archive, manifest, |hash| {
        let (http, master, grant) = (http.clone(), master.clone(), grant.clone());
        async move {
            let response = http
                .get(
                    master
                        .join(&format!("internal/node-restore/{hash}"))
                        .map_err(Error::internal)?,
                )
                .bearer_auth(grant)
                .send()
                .await
                .map_err(Error::internal)?;
            if !response.status().is_success() {
                return Err(Error::forbidden("Native restore grant expired."));
            }
            snapshots::response_block(response).await
        }
    })
    .await?;
    let extracted = temporary.path().join("data");
    let file = tokio::fs::File::open(archive).await?;
    files::receive(&mut BufReader::new(file), &extracted, budget).await?;
    for entry in ["home", "workspaces", "output"] {
        let path = extracted.join(entry);
        if tokio::fs::symlink_metadata(&path)
            .await
            .is_ok_and(|metadata| !metadata.is_dir() || metadata.file_type().is_symlink())
        {
            return Err(Error::bad("Invalid native checkpoint root."));
        }
    }
    sync_directories(&extracted).await?;
    atomic_write(
        &extracted.join(".cairn-seed-complete"),
        text(request, "backupId").as_bytes(),
    )
    .await?;
    tokio::fs::File::open(temporary.path())
        .await?
        .sync_all()
        .await?;
    let pending = json!({
        "backupId": request["backupId"],
        "runtime": manifest["runtime"],
        "attempt": manifest["attempt"],
        "stage": temporary.path().file_name().unwrap().to_string_lossy(),
        "old": format!("stale-{}", crate::config::id()),
    });
    atomic_write(
        &root.join("restore.pending.json"),
        &serde_json::to_vec(&pending)?,
    )
    .await?;
    // After the durable intent, a restart must retain staging until it finishes
    // the same restore. Existing local work is moved aside, never overwritten.
    let _retained = temporary.keep();
    finish_restore(&root, &pending).await
}

async fn finish_restore(root: &Path, pending: &Value) -> Result<Value> {
    let component = |field: &str, prefix: &str| -> Result<PathBuf> {
        let name = text(pending, field);
        if !name.starts_with(prefix)
            || Path::new(name).components().count() != 1
            || name.contains(['/', '\\'])
        {
            return Err(Error::bad("Invalid native restore journal."));
        }
        Ok(root.join(name))
    };
    let stage = component("stage", "restore-")?;
    let old = component("old", "stale-")?;
    let data = root.join("data");
    if stage.join("data").exists() {
        if data.exists() {
            if old.exists() {
                return Err(Error::conflict(
                    "Native restore has ambiguous local ownership.",
                ));
            }
            tokio::fs::rename(&data, &old).await?;
            tokio::fs::File::open(root).await?.sync_all().await?;
        }
        tokio::fs::rename(stage.join("data"), &data).await?;
        tokio::fs::File::open(root).await?.sync_all().await?;
    }
    let installed = tokio::fs::read_to_string(data.join(".cairn-seed-complete"))
        .await
        .unwrap_or_default();
    if installed != text(pending, "backupId") {
        return Err(Error::conflict(
            "Native restore is incomplete; the previous local data is retained.",
        ));
    }
    atomic_write(
        &root.join("runtime.json"),
        &serde_json::to_vec(&pending["runtime"])?,
    )
    .await?;
    atomic_write(&root.join("attempt"), text(pending, "attempt").as_bytes()).await?;
    atomic_write(&root.join("initialized"), b"1").await?;
    for entry in ["completed.json", "checkpoint-error", "published.json"] {
        match tokio::fs::remove_file(root.join(entry)).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    atomic_write(
        &root.join("restored.json"),
        &serde_json::to_vec(&json!({ "backupId": pending["backupId"] }))?,
    )
    .await?;
    tokio::fs::remove_file(root.join("restore.pending.json")).await?;
    tokio::fs::File::open(root).await?.sync_all().await?;
    if stage.exists() {
        tokio::fs::remove_dir_all(stage).await?;
    }
    Ok(json!({ "ready": true }))
}

/// No symlink traversal, device reads or directory deletion through workload paths.
pub async fn own_tree(root: &Path, uid: u32, gid: u32) -> Result<()> {
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        let metadata = tokio::fs::symlink_metadata(&path).await?;
        std::os::unix::fs::lchown(&path, Some(uid), Some(gid))?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            let mut entries = tokio::fs::read_dir(&path).await?;
            while let Some(entry) = entries.next_entry().await? {
                pending.push(entry.path());
            }
            tokio::fs::set_permissions(
                &path,
                std::fs::Permissions::from_mode(metadata.permissions().mode() | 0o700),
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn bytes(root: &Path) -> Result<u64> {
    let root = root.to_owned();
    tokio::task::spawn_blocking(move || {
        use std::{
            ffi::CString,
            os::{
                fd::{AsRawFd, FromRawFd},
                unix::{ffi::OsStrExt, fs::OpenOptionsExt},
            },
        };
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(root)?;
        let entries = std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?;
        let mut stack = vec![(directory, entries)];
        let mut total = 0u64;
        while let Some((directory, entries)) = stack.last_mut() {
            let Some(entry) = entries.next() else {
                stack.pop();
                continue;
            };
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(Error::from(error)),
            };
            let name = CString::new(entry.file_name().as_bytes()).map_err(Error::internal)?;
            // O_PATH never opens a device or FIFO. Each directory descriptor pins
            // its ancestry, so renames and symlink swaps cannot escape this tree.
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::NotFound {
                    continue;
                }
                return Err(error.into());
            }
            let file = unsafe { std::fs::File::from_raw_fd(fd) };
            let metadata = file.metadata()?;
            if metadata.is_dir() {
                let entries = std::fs::read_dir(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
                stack.push((file, entries));
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
        Ok(total)
    })
    .await
    .map_err(Error::internal)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_checkpoint_restores_files_modes_and_session_state_without_managed_credentials()
    {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let root = directory(state.path(), &run).unwrap();
        for path in [
            "data/workspaces/game",
            "data/home/.codex/sessions",
            "data/home/.config/gh",
            "data/output",
            "data/tmp",
        ] {
            private_dir(&root.join(path)).await.unwrap();
        }
        tokio::fs::write(root.join("data/workspaces/game/main.rs"), b"fn main() {}\n")
            .await
            .unwrap();
        tokio::fs::set_permissions(
            root.join("data/workspaces/game/main.rs"),
            std::fs::Permissions::from_mode(0o640),
        )
        .await
        .unwrap();
        tokio::fs::write(
            root.join("data/home/.codex/sessions/state.json"),
            b"session-data",
        )
        .await
        .unwrap();
        tokio::fs::write(
            root.join("data/home/.codex/auth.json"),
            b"synthetic-private-credential",
        )
        .await
        .unwrap();
        tokio::fs::write(
            root.join("data/home/.codex/config.toml"),
            b"synthetic-mcp-token",
        )
        .await
        .unwrap();
        tokio::fs::write(
            root.join("data/home/.config/gh/hosts.yml"),
            b"synthetic-github-token",
        )
        .await
        .unwrap();
        tokio::fs::write(root.join("data/tmp/transient"), b"not-persisted")
            .await
            .unwrap();
        let point = capture(state.path(), &run, &crate::config::id(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(point["manifest"]["kind"], KIND);
        let snapshot = state.path().join("snapshots").join(text(&point, "id"));
        let captured = snapshot.clone();
        let app = axum::Router::new().route(
            "/internal/node-restore/{hash}",
            axum::routing::get(
                move |axum::extract::Path(hash): axum::extract::Path<String>| {
                    let captured = captured.clone();
                    async move { snapshots::served(&captured, &hash).await.unwrap() }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let destination = tempfile::tempdir().unwrap();
        let restored_run = crate::config::id();
        let request = json!({ "manifest": point["manifest"], "master": origin, "grant": "synthetic-scoped-grant", "backupId": crate::config::id() });
        restore(destination.path(), &restored_run, &request, 1024 * 1024)
            .await
            .unwrap();
        let restored = directory(destination.path(), &restored_run)
            .unwrap()
            .join("data");
        assert_eq!(
            tokio::fs::read(restored.join("workspaces/game/main.rs"))
                .await
                .unwrap(),
            b"fn main() {}\n"
        );
        assert_eq!(
            tokio::fs::metadata(restored.join("workspaces/game/main.rs"))
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(
            tokio::fs::read(restored.join("home/.codex/sessions/state.json"))
                .await
                .unwrap(),
            b"session-data"
        );
        for private in [
            "home/.codex/auth.json",
            "home/.codex/config.toml",
            "home/.config/gh/hosts.yml",
            "tmp/transient",
        ] {
            assert!(
                !restored.join(private).exists(),
                "managed credentials and temporary files must not be durable: {private}"
            );
        }
        restore(destination.path(), &restored_run, &request, 1024 * 1024)
            .await
            .unwrap();
        let owner = execution_ownership(destination.path(), &restored_run)
            .await
            .unwrap();
        tokio::fs::write(
            restored.join("workspaces/game/main.rs"),
            b"changes made after restoring",
        )
        .await
        .unwrap();
        drop(owner);
        restore(destination.path(), &restored_run, &request, 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(restored.join("workspaces/game/main.rs"))
                .await
                .unwrap(),
            b"fn main() {}\n"
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_failed_native_capture_never_reuses_an_older_completed_checkpoint() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let root = directory(state.path(), &run).unwrap();
        private_dir(&root.join("data/workspaces")).await.unwrap();
        capture(state.path(), &run, &crate::config::id(), 1024)
            .await
            .unwrap();
        assert!(completed(state.path(), &run).await.is_ok());
        atomic_write(&root.join("checkpoint-error"), b"Synthetic disk failure")
            .await
            .unwrap();
        assert!(completed(state.path(), &run).await.is_err());
        assert!(root.join("data/workspaces").exists());
    }

    #[tokio::test]
    #[ignore = "local reproducible storage measurement; run alone with --nocapture"]
    async fn measure_native_stopped_storage() {
        use crate::{storage::block_codec::BlockCodec, store::Store, vault::Vault};
        use rand::{RngCore, SeedableRng};
        use std::{os::fd::AsRawFd, time::Instant};
        fn cpu_ms() -> f64 {
            let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
            assert_eq!(
                unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
                0
            );
            let usage = unsafe { usage.assume_init() };
            (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.0
                + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.0
        }
        fn evict(path: &Path) {
            let file = std::fs::File::open(path).unwrap();
            assert_eq!(
                unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) },
                0
            );
        }
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let data = directory(state.path(), &run).unwrap().join("data");
        private_dir(&data.join("workspaces/game")).await.unwrap();
        for index in 0..128 {
            tokio::fs::write(
                data.join(format!("workspaces/game/source-{index}.txt")),
                vec![b'x'; 4096],
            )
            .await
            .unwrap();
        }
        let mut payload = vec![0; 8 * 1024 * 1024];
        rand::rngs::StdRng::seed_from_u64(42).fill_bytes(&mut payload);
        tokio::fs::write(data.join("workspaces/game/assets.bin"), &payload)
            .await
            .unwrap();
        let logical = bytes(&data).await.unwrap();
        let reference = state.path().join("reference.files");
        let (start, cpu) = (Instant::now(), cpu_ms());
        let mut file = tokio::fs::File::create(&reference).await.unwrap();
        files::send(&data, &mut file).await.unwrap();
        file.sync_all().await.unwrap();
        drop(file);
        let reference_write =
            json!({ "wallMs": start.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu_ms() - cpu });
        let (start, cpu) = (Instant::now(), cpu_ms());
        let captured = capture(state.path(), &run, &crate::config::id(), 32 * 1024 * 1024)
            .await
            .unwrap();
        let native_write =
            json!({ "wallMs": start.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu_ms() - cpu });
        let snapshot = state.path().join("snapshots").join(text(&captured, "id"));
        let disk = snapshot.join("disk");
        let mut reads = Vec::new();
        for (name, path) in [("reference", &reference), ("native", &disk)] {
            for cold in [true, false] {
                if cold {
                    evict(path);
                }
                let target = tempfile::tempdir_in(state.path()).unwrap();
                let (start, cpu) = (Instant::now(), cpu_ms());
                let selected = if name == "native" {
                    let restored = target.path().join("archive");
                    snapshots::restore(&restored, &captured["manifest"], |hash| {
                        let snapshot = snapshot.clone();
                        async move { snapshots::served(&snapshot, &hash).await }
                    })
                    .await
                    .unwrap();
                    restored
                } else {
                    path.to_path_buf()
                };
                files::receive(
                    &mut BufReader::new(tokio::fs::File::open(selected).await.unwrap()),
                    &target.path().join("files"),
                    32 * 1024 * 1024,
                )
                .await
                .unwrap();
                reads.push(json!({ "mode": name, "coldFileHint": cold, "wallMs": start.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu_ms() - cpu }));
            }
        }
        let vault = Vault::new(Store::open(state.path()).unwrap(), state.path()).unwrap();
        let codec = BlockCodec::default();
        let bytes = tokio::fs::read(&disk).await.unwrap();
        let (start, cpu) = (Instant::now(), cpu_ms());
        let mut encoded_bytes = 0;
        for block in bytes.chunks(snapshots::BLOCK as usize) {
            encoded_bytes += codec
                .encode(
                    vault.clone(),
                    "synthetic-native-benchmark".into(),
                    block.to_vec(),
                )
                .await
                .unwrap()
                .len();
        }
        println!(
            "{}",
            json!({ "workload": "128x4KiB text + 8MiB seeded random (42)", "logicalBytes": logical,
            "referenceArchiveBytes": tokio::fs::metadata(reference).await.unwrap().len(),
            "nativeArchiveBytes": bytes.len(), "nativeManifestBytes": serde_json::to_vec(&captured["manifest"]).unwrap().len(),
            "s3EncodedBlockBytes": encoded_bytes, "referenceWrite": reference_write, "nativeWrite": native_write,
            "reads": reads, "codec": { "wallMs": start.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu_ms() - cpu } })
        );
    }

    #[tokio::test]
    async fn interrupted_restore_completes_without_discarding_previous_local_work() {
        let state = tempfile::tempdir().unwrap();
        let root = state.path();
        private_dir(&root.join("data")).await.unwrap();
        tokio::fs::write(root.join("data/local"), b"unsynchronized old work")
            .await
            .unwrap();
        private_dir(&root.join("restore-test/data")).await.unwrap();
        tokio::fs::write(root.join("restore-test/data/.cairn-seed-complete"), b"1")
            .await
            .unwrap();
        tokio::fs::write(root.join("restore-test/data/restored"), b"published work")
            .await
            .unwrap();
        let intent = json!({ "backupId": crate::config::id(), "stage": "restore-test", "old": "stale-test", "runtime": {}, "attempt": crate::config::id() });
        atomic_write(
            &root.join("restore.pending.json"),
            &serde_json::to_vec(&intent).unwrap(),
        )
        .await
        .unwrap();
        atomic_write(
            &root.join("restore-test/data/.cairn-seed-complete"),
            text(&intent, "backupId").as_bytes(),
        )
        .await
        .unwrap();
        // Simulate power loss after preserving the old directory, before installing staging.
        tokio::fs::rename(root.join("data"), root.join("stale-test"))
            .await
            .unwrap();
        finish_restore(root, &intent).await.unwrap();
        assert_eq!(
            tokio::fs::read(root.join("stale-test/local"))
                .await
                .unwrap(),
            b"unsynchronized old work"
        );
        assert_eq!(
            tokio::fs::read(root.join("data/restored")).await.unwrap(),
            b"published work"
        );
        assert!(!root.join("restore.pending.json").exists());
        assert!(!root.join("restore-test").exists());
    }

    #[tokio::test]
    async fn native_disk_accounting_ignores_links_and_never_opens_fifos() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        tokio::fs::write(outside.path().join("secret"), vec![0; 4096])
            .await
            .unwrap();
        tokio::fs::write(root.path().join("owned"), b"12345")
            .await
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        let pipe = std::ffi::CString::new(root.path().join("pipe").as_os_str().as_encoded_bytes())
            .unwrap();
        assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
        let count = tokio::time::timeout(std::time::Duration::from_secs(2), bytes(root.path()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, 5);
    }

    #[tokio::test]
    async fn incompatible_native_restore_leaves_existing_files_untouched() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let root = directory(state.path(), &run).unwrap();
        private_dir(&root.join("data")).await.unwrap();
        tokio::fs::write(root.join("data/keep"), b"local work")
            .await
            .unwrap();
        for manifest in [
            json!({ "kind": "flat-ext4-v1" }),
            json!({ "kind": KIND, "runtime": { "os": "windows", "arch": "x86_64" } }),
        ] {
            assert!(
                restore(state.path(), &run, &json!({ "manifest": manifest }), 1024)
                    .await
                    .is_err()
            );
            assert_eq!(
                tokio::fs::read(root.join("data/keep")).await.unwrap(),
                b"local work"
            );
        }
    }
}
