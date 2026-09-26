//! Bounded transfer of stopped VM archives without a shared host filesystem.
use crate::{
    error::{Error, Result},
    service::Service,
};
use axum::{
    body::Body,
    extract::Request,
    response::{IntoResponse, Response},
};
use std::{path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
const CHUNK: usize = 256 * 1024;
const MAX: u64 = 1_099_511_627_776;

pub async fn controller(data: &Path, request: Request) -> Result<Response> {
    let parts = request
        .uri()
        .path()
        .trim_start_matches('/')
        .split('/')
        .collect::<Vec<_>>();
    let ["archive-transfers", transfer, offset] = parts.as_slice() else {
        return Err(Error::bad("Invalid archive transfer."));
    };
    crate::validation::uuid(transfer)?;
    let directory = data.join("archive-transfers").join(transfer);
    if request.method() == "DELETE" && *offset == "discard" {
        match tokio::fs::remove_dir_all(directory).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        };
        return Ok(axum::Json(serde_json::json!({"ok":true})).into_response());
    }
    let offset = offset
        .parse::<u64>()
        .ok()
        .filter(|v| *v <= MAX && *v % CHUNK as u64 == 0)
        .ok_or_else(|| Error::bad("Invalid archive offset."))?;
    crate::skills::private_dir(&directory).await?;
    let file = directory.join("workspace.tar.gz");
    if request.method() == "GET" {
        let mut file = tokio::fs::File::open(file).await?;
        if offset > file.metadata().await?.len() {
            return Err(Error::bad("Archive offset exceeds file size."));
        }
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut bytes = vec![0; CHUNK];
        let mut count = 0;
        while count < CHUNK {
            let n = file.read(&mut bytes[count..]).await?;
            if n == 0 {
                break;
            }
            count += n;
        }
        bytes.truncate(count);
        return Ok(([("content-length", count.to_string())], Body::from(bytes)).into_response());
    }
    if request.method() != "POST" {
        return Err(Error::new(405, "Method not allowed."));
    }
    let bytes = axum::body::to_bytes(request.into_body(), CHUNK)
        .await
        .map_err(|_| Error::bad("Archive chunk too large."))?;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(file)
        .await?;
    let size = file.metadata().await?.len();
    if offset != size || offset.saturating_add(bytes.len() as u64) > MAX {
        return Err(Error::new(409, "Archive upload is out of order."));
    }
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    file.write_all(&bytes).await?;
    file.sync_all().await?;
    Ok(axum::Json(serde_json::json!({"received":bytes.len()})).into_response())
}

pub async fn transfer(
    s: &Service,
    base: &str,
    credential: &str,
    staging: &Path,
    upload: bool,
) -> Result<()> {
    let transfer = staging
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| Error::bad("Invalid archive transfer."))?;
    crate::validation::uuid(transfer)?;
    let path = staging.join("workspace.tar.gz");
    let mut file = if upload {
        tokio::fs::File::open(path).await?
    } else {
        tokio::fs::File::create(path).await?
    };
    let mut offset = 0u64;
    loop {
        let url = format!("{base}/archive-transfers/{transfer}/{offset}");
        let bytes = if upload {
            let mut bytes = vec![0; CHUNK];
            let mut count = 0;
            while count < CHUNK {
                let n = file.read(&mut bytes[count..]).await?;
                if n == 0 {
                    break;
                }
                count += n;
            }
            bytes.truncate(count);
            let response = s
                .http
                .post(url)
                .bearer_auth(credential)
                .body(bytes.clone())
                .timeout(Duration::from_secs(60))
                .send()
                .await
                .map_err(|_| Error::new(503, "Archive upload interrupted."))?;
            if !response.status().is_success() {
                return Err(Error::new(503, "Archive upload rejected."));
            }
            bytes
        } else {
            let response = s
                .http
                .get(url)
                .bearer_auth(credential)
                .timeout(Duration::from_secs(60))
                .send()
                .await
                .map_err(|_| Error::new(503, "Archive download interrupted."))?;
            if !response.status().is_success() {
                return Err(Error::new(503, "Archive download rejected."));
            }
            let bytes = super::snapshots::response_block(response).await?;
            if bytes.len() > CHUNK {
                return Err(Error::bad("Archive chunk too large."));
            }
            file.write_all(&bytes).await?;
            bytes
        };
        offset += bytes.len() as u64;
        if offset > MAX {
            return Err(Error::bad("Archive exceeds disk limit."));
        }
        if bytes.len() < CHUNK {
            break;
        }
    }
    if !upload {
        file.sync_all().await?;
    }
    Ok(())
}
