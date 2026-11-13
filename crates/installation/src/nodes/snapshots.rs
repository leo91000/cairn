use crate::{
    error::{Error, Result},
    validation::text,
};
use serde::Serialize;
use serde_json::Value;
use std::{
    os::{
        fd::AsRawFd,
        unix::fs::{FileExt, OpenOptionsExt},
    },
    path::Path,
};

pub const BLOCK: u64 = 4 * 1024 * 1024;
const MANIFEST_VERSION: u64 = 1;

/// One fixed-size extent of a disk manifest. Holes and zero blocks have no hash.
#[derive(Serialize)]
pub(crate) struct ManifestBlock {
    pub offset: u64,
    pub size: u64,
    pub hash: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    version: u64,
    size: u64,
    block_size: u64,
    blocks: Vec<ManifestBlock>,
    local_bytes_read: u64,
}

/// Next offset at or after `offset` that may hold data. Holes read as zeros, so blocks
/// entirely inside one need neither reading nor hashing.
fn next_data(file: &std::fs::File, offset: u64) -> std::io::Result<u64> {
    let position = unsafe { libc::lseek(file.as_raw_fd(), offset as libc::off_t, libc::SEEK_DATA) };
    if position >= 0 {
        return Ok(position as u64);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        // Only a hole remains after this offset.
        Some(libc::ENXIO) => Ok(u64::MAX),
        // A filesystem without hole reporting: treat everything as data.
        Some(libc::EINVAL) => Ok(offset),
        _ => Err(error),
    }
}

pub async fn index(path: &Path) -> Result<Value> {
    index_with_stop(path, &tokio_util::sync::CancellationToken::new()).await
}

pub(crate) async fn index_with_stop(
    path: &Path,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<Value> {
    let path = path.to_owned();
    let stop = stop.clone();
    tokio::task::spawn_blocking(move || -> Result<Value> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let size = file.metadata()?.len();
        let mut blocks = Vec::new();
        let mut buffer = vec![0; BLOCK as usize];
        let (mut offset, mut read, mut data) = (0, 0u64, next_data(&file, 0)?);
        while offset < size {
            if stop.is_cancelled() {
                return Err(Error::unavailable("Disk indexing stopped."));
            }
            let length = (size - offset).min(BLOCK) as usize;
            if data < offset {
                data = next_data(&file, offset)?;
            }
            let hash = if data >= offset + length as u64 {
                None
            } else {
                file.read_exact_at(&mut buffer[..length], offset)?;
                read += length as u64;
                let data = &buffer[..length];
                (!data.iter().all(|b| *b == 0)).then(|| crate::storage::digest::block(data))
            };
            blocks.push(ManifestBlock {
                offset,
                size: length as u64,
                hash,
            });
            offset += length as u64;
        }

        let index = Index {
            version: MANIFEST_VERSION,
            size,
            block_size: BLOCK,
            blocks,
            local_bytes_read: read,
        };
        Ok(serde_json::to_value(index)?)
    })
    .await
    .map_err(Error::internal)?
}

pub fn validate(manifest: &Value) -> Result<()> {
    crate::storage::layout::validate(manifest).map_err(|error| Error::bad(error.to_string()))?;
    let blocks = manifest["blocks"]
        .as_array()
        .ok_or_else(|| Error::bad("Missing backup blocks."))?;
    let size = manifest["size"]
        .as_u64()
        .filter(|s| *s > 0 && *s <= 1024 * 1024 * 1024 * 1024)
        .ok_or_else(|| Error::bad("Invalid backup size."))?;
    if manifest["version"] != 1
        || manifest["blockSize"] != BLOCK
        || blocks.len() as u64 != size.div_ceil(BLOCK)
    {
        return Err(Error::bad("Invalid backup manifest."));
    }
    for (i, block) in blocks.iter().enumerate() {
        let offset = i as u64 * BLOCK;
        if block["offset"] != offset
            || block["size"] != (size - offset).min(BLOCK)
            || !(block["hash"].is_null() || valid_hash(text(block, "hash")))
        {
            return Err(Error::bad("Invalid backup extent."));
        }
    }
    Ok(())
}

pub fn valid_hash(hash: &str) -> bool {
    crate::storage::digest::valid(hash)
}
