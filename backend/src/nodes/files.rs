//! Streaming private workspace transfer. Never follows symlinks on either host.
use crate::{
    error::{Error, Result},
    microvm::wire,
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
};
use tokio::io::{AsyncBufRead, AsyncReadExt, AsyncWriteExt};

const CHUNK: usize = 65536;
const MAX_ENTRIES: u64 = 1_000_000;

/// The frame announcing one workspace entry. File contents follow as `data` frames.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Entry<'a> {
    Link { path: &'a str, target: PathBuf },
    Directory { path: &'a str, mode: u32 },
    File { path: &'a str, size: u64, mode: u32 },
}

async fn write_frame(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    frame: &impl Serialize,
) -> Result<()> {
    wire::write(writer, &serde_json::to_value(frame)?).await
}

pub async fn send(root: &Path, writer: &mut (impl tokio::io::AsyncWrite + Unpin)) -> Result<()> {
    let root = tokio::fs::canonicalize(root).await?;
    let mut directories = vec![root.clone()];
    while let Some(directory) = directories.pop() {
        let mut entries = tokio::fs::read_dir(&directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let relative = path.strip_prefix(&root).map_err(Error::internal)?;
            let relative = relative
                .to_str()
                .ok_or_else(|| Error::bad("Workspace path is not UTF-8."))?;
            if relative.ends_with(".codex/auth.json") {
                continue;
            }
            let metadata = tokio::fs::symlink_metadata(&path).await?;
            let kind = metadata.file_type();
            let mode = metadata.permissions().mode() & 0o777;
            if kind.is_symlink() {
                let target = tokio::fs::read_link(&path).await?;
                let link = Entry::Link {
                    path: relative,
                    target,
                };
                write_frame(writer, &link).await?;
            } else if kind.is_dir() {
                let directory = Entry::Directory {
                    path: relative,
                    mode,
                };
                write_frame(writer, &directory).await?;
                directories.push(path);
            } else if kind.is_file() {
                send_file(writer, &path, relative, mode).await?;
            }
        }
    }
    wire::write(writer, &json!({ "complete": true })).await
}

async fn send_file(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    path: &Path,
    relative: &str,
    mode: u32,
) -> Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .await?;
    let size = file.metadata().await?.len();
    let header = Entry::File {
        path: relative,
        size,
        mode,
    };
    write_frame(writer, &header).await?;
    let mut left = size;
    let mut buffer = vec![0; CHUNK];
    while left > 0 {
        let limit = left.min(buffer.len() as u64) as usize;
        let count = file.read(&mut buffer[..limit]).await?;
        if count == 0 {
            return Err(Error::conflict("Workspace changed during transfer."));
        }
        let data = STANDARD.encode(&buffer[..count]);
        wire::write(writer, &json!({ "data": data })).await?;
        left -= count as u64;
    }
    Ok(())
}

/// The entry's path, relative and free of `..`, root or prefix components.
fn relative_path(entry: &Value) -> Result<&Path> {
    let relative = Path::new(text(entry, "path"));
    let escapes = relative
        .components()
        .any(|p| !matches!(p, Component::Normal(_)));
    if relative.as_os_str().is_empty() || escapes {
        return Err(Error::bad("Invalid workspace path."));
    }
    Ok(relative)
}

/// Every ancestor of `relative` below `root` must be a real directory, never a symlink.
async fn check_parents(root: &Path, relative: &Path) -> Result<()> {
    let mut parent = root.to_path_buf();
    let parts = relative.components().collect::<Vec<_>>();
    for part in &parts[..parts.len() - 1] {
        parent.push(part.as_os_str());
        let meta = tokio::fs::symlink_metadata(&parent).await?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(Error::bad("Workspace parent is not a directory."));
        }
    }
    Ok(())
}

pub async fn receive(
    reader: &mut (impl AsyncBufRead + Unpin),
    root: &Path,
    budget: u64,
) -> Result<()> {
    crate::skills::private_dir(root).await?;
    let mut used = 0u64;
    let mut entries = 0u64;
    // Directory modes apply last so a read-only directory can still receive its entries.
    let mut modes = Vec::new();
    while let Some(entry) = wire::read(reader).await? {
        if entry["complete"] == true {
            for (path, mode) in modes.into_iter().rev() {
                tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await?;
            }
            return Ok(());
        }
        entries += 1;
        if entries > MAX_ENTRIES {
            return Err(Error::bad("Too many workspace entries."));
        }
        let relative = relative_path(&entry)?;
        check_parents(root, relative).await?;
        let target = root.join(relative);
        let mode = entry["mode"].as_u64().unwrap_or(0o600) as u32 & 0o777;
        match text(&entry, "kind") {
            "directory" => {
                tokio::fs::create_dir(&target).await?;
                modes.push((target, mode));
            }
            "link" => {
                tokio::fs::symlink(text(&entry, "target"), target).await?;
            }
            "file" => {
                let size = entry["size"]
                    .as_u64()
                    .ok_or_else(|| Error::bad("Missing workspace size."))?;
                used = used
                    .checked_add(size)
                    .filter(|v| *v <= budget)
                    .ok_or_else(|| Error::bad("Workspace exceeds node disk budget."))?;
                receive_file(reader, &target, size, mode).await?;
            }
            _ => return Err(Error::bad("Invalid workspace entry.")),
        }
    }
    Err(Error::bad("Incomplete workspace transfer."))
}

async fn receive_file(
    reader: &mut (impl AsyncBufRead + Unpin),
    target: &Path,
    size: u64,
    mode: u32,
) -> Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(target)
        .await?;
    let mut left = size;
    while left > 0 {
        let frame = wire::read(reader)
            .await?
            .ok_or_else(|| Error::bad("Incomplete workspace transfer."))?;
        let bytes = STANDARD
            .decode(text(&frame, "data"))
            .map_err(|_| Error::bad("Invalid workspace bytes."))?;
        if bytes.is_empty() || bytes.len() > CHUNK || bytes.len() as u64 > left {
            return Err(Error::bad("Invalid workspace chunk size."));
        }
        file.write_all(&bytes).await?;
        left -= bytes.len() as u64;
    }
    file.sync_all().await?;
    Ok(())
}
