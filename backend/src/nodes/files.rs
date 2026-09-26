//! Streaming private workspace transfer. Never follows symlinks on either host.
use crate::{
    error::{Error, Result},
    microvm::wire,
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, path::Path};
use tokio::io::{AsyncBufRead, AsyncReadExt, AsyncWriteExt};

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
            if kind.is_symlink() {
                let target = tokio::fs::read_link(&path).await?;
                wire::write(
                    writer,
                    &json!({"path":relative,"kind":"link","target":target}),
                )
                .await?;
            } else if kind.is_dir() {
                wire::write(writer,&json!({"path":relative,"kind":"directory","mode":metadata.permissions().mode()&0o777})).await?;
                directories.push(path);
            } else if kind.is_file() {
                let mut file = tokio::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)
                    .await?;
                let size = file.metadata().await?.len();
                wire::write(writer,&json!({"path":relative,"kind":"file","size":size,"mode":metadata.permissions().mode()&0o777})).await?;
                let mut left = size;
                let mut buffer = vec![0; 65536];
                while left > 0 {
                    let limit = left.min(buffer.len() as u64) as usize;
                    let count = file.read(&mut buffer[..limit]).await?;
                    if count == 0 {
                        return Err(Error::new(409, "Workspace changed during transfer."));
                    }
                    wire::write(writer, &json!({"data":STANDARD.encode(&buffer[..count])})).await?;
                    left -= count as u64;
                }
            }
        }
    }
    wire::write(writer, &json!({"complete":true})).await
}
pub async fn receive(
    reader: &mut (impl AsyncBufRead + Unpin),
    root: &Path,
    budget: u64,
) -> Result<()> {
    crate::skills::private_dir(root).await?;
    let mut used = 0u64;
    let mut entries = 0u64;
    let mut modes = Vec::new();
    while let Some(entry) = wire::read(reader).await? {
        if entry["complete"] == true {
            for (path, mode) in modes.into_iter().rev() {
                tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await?;
            }
            return Ok(());
        }
        entries += 1;
        if entries > 1_000_000 {
            return Err(Error::bad("Too many workspace entries."));
        }
        let relative = Path::new(text(&entry, "path"));
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|p| !matches!(p, std::path::Component::Normal(_)))
        {
            return Err(Error::bad("Invalid workspace path."));
        }
        let target = root.join(relative);
        let mut parent = root.to_path_buf();
        let parts = relative.components().collect::<Vec<_>>();
        for part in &parts[..parts.len() - 1] {
            parent.push(part.as_os_str());
            let meta = tokio::fs::symlink_metadata(&parent).await?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(Error::bad("Workspace parent is not a directory."));
            }
        }
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
                let mut file = tokio::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(mode)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&target)
                    .await?;
                let mut left = size;
                while left > 0 {
                    let frame = wire::read(reader)
                        .await?
                        .ok_or_else(|| Error::bad("Incomplete workspace transfer."))?;
                    let bytes = STANDARD
                        .decode(text(&frame, "data"))
                        .map_err(|_| Error::bad("Invalid workspace bytes."))?;
                    if bytes.is_empty() || bytes.len() > 65536 || bytes.len() as u64 > left {
                        return Err(Error::bad("Invalid workspace chunk size."));
                    }
                    file.write_all(&bytes).await?;
                    left -= bytes.len() as u64;
                }
                file.sync_all().await?;
            }
            _ => return Err(Error::bad("Invalid workspace entry.")),
        }
    }
    Err(Error::bad("Incomplete workspace transfer."))
}
