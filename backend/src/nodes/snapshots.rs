//! Content-addressed disk manifests. Capture callers must provide an immutable disk.
use crate::{
    error::{Error, Result},
    validation::text,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{future::Future, path::Path};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
pub const BLOCK: u64 = 4 * 1024 * 1024;
/// The baseline manifest with the written blocks re-hashed from `copy`, which holds
/// exactly those blocks at their original offsets.
pub async fn update(copy: &Path, baseline: &Value, written: &[u64]) -> Result<Value> {
    validate(baseline)?;
    let (copy, mut manifest, written) = (copy.to_owned(), baseline.clone(), written.to_owned());
    tokio::task::spawn_blocking(move || -> Result<Value> {
        use std::os::unix::fs::FileExt;
        let file = std::fs::File::open(&copy)?;
        let size = manifest["size"].as_u64().unwrap_or(0);
        let blocks = manifest["blocks"].as_array_mut().unwrap();
        let mut buffer = vec![0; BLOCK as usize];
        for &index in &written {
            let offset = index * BLOCK;
            let length = (size.saturating_sub(offset)).min(BLOCK) as usize;
            let entry = blocks
                .get_mut(index as usize)
                .ok_or_else(|| Error::bad("Written block outside the disk."))?;
            file.read_exact_at(&mut buffer[..length], offset)?;
            entry["hash"] = if buffer[..length].iter().all(|b| *b == 0) {
                Value::Null
            } else {
                hex::encode(Sha256::digest(&buffer[..length])).into()
            };
        }
        Ok(manifest)
    })
    .await
    .map_err(Error::internal)?
}
/// Next offset at or after `offset` that may hold data. Holes read as zeros, so blocks
/// entirely inside one need neither reading nor hashing.
fn next_data(file: &std::fs::File, offset: u64) -> std::io::Result<u64> {
    use std::os::fd::AsRawFd;
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
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || -> Result<Value> {
        use std::os::unix::fs::{FileExt, OpenOptionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let size = file.metadata()?.len();
        let mut blocks = Vec::new();
        let mut buffer = vec![0; BLOCK as usize];
        let (mut offset, mut read, mut data) = (0, 0u64, next_data(&file, 0)?);
        while offset < size {
            let length = (size - offset).min(BLOCK) as usize;
            if data < offset {
                data = next_data(&file, offset)?;
            }
            let hash = if data >= offset + length as u64 {
                Value::Null
            } else {
                file.read_exact_at(&mut buffer[..length], offset)?;
                read += length as u64;
                if buffer[..length].iter().all(|b| *b == 0) {
                    Value::Null
                } else {
                    hex::encode(Sha256::digest(&buffer[..length])).into()
                }
            };
            blocks.push(json!({"offset":offset,"size":length,"hash":hash}));
            offset += length as u64;
        }
        Ok(json!({"version":1,"size":size,"blockSize":BLOCK,"blocks":blocks,"localBytesRead":read}))
    })
    .await
    .map_err(Error::internal)?
}
pub fn validate(manifest: &Value) -> Result<()> {
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
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub async fn block(path: &Path, manifest: &Value, hash: &str) -> Result<Vec<u8>> {
    block_where(path, manifest, hash, |_| true).await
}
/// Serves a block from a capture directory. An incremental capture holds only the
/// blocks written since its baseline, listed in `present.json`; the others are
/// already stored by the master.
pub async fn served(directory: &Path, hash: &str) -> Result<Vec<u8>> {
    let manifest: Value =
        serde_json::from_slice(&tokio::fs::read(directory.join("manifest.json")).await?)?;
    if manifest["onDemand"] == true {
        let run = tokio::fs::read_to_string(directory.join("run")).await?;
        crate::validation::uuid(&run)?;
        let state = directory
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| Error::bad("Invalid snapshot directory."))?;
        let volume = crate::storage::runtime::open(&state.join("disks").join(run))?;
        let generation = manifest["generation"]
            .as_i64()
            .ok_or_else(|| Error::bad("Invalid generation."))?;
        let hash = hash.to_owned();
        return tokio::task::spawn_blocking(move || volume.disk.captured_block(generation, &hash))
            .await
            .map_err(Error::internal)?
            .map_err(Into::into);
    }
    let present = match tokio::fs::read(directory.join("present.json")).await {
        Ok(bytes) => Some(serde_json::from_slice::<std::collections::HashSet<u64>>(
            &bytes,
        )?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    block_where(&directory.join("disk"), &manifest, hash, |offset| {
        present
            .as_ref()
            .is_none_or(|present| present.contains(&offset))
    })
    .await
}
async fn block_where(
    path: &Path,
    manifest: &Value,
    hash: &str,
    available: impl Fn(u64) -> bool,
) -> Result<Vec<u8>> {
    if !valid_hash(hash) {
        return Err(Error::bad("Invalid block digest."));
    }
    let block = manifest["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|b| b["hash"] == hash && b["offset"].as_u64().is_some_and(&available))
        .ok_or_else(|| Error::new(404, "Unknown backup block."))?;
    let size = block["size"]
        .as_u64()
        .filter(|s| *s <= BLOCK)
        .ok_or_else(|| Error::bad("Invalid block size."))?;
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(
        block["offset"]
            .as_u64()
            .ok_or_else(|| Error::bad("Invalid block offset."))?,
    ))
    .await?;
    let mut bytes = vec![0; size as usize];
    file.read_exact(&mut bytes).await?;
    if hex::encode(Sha256::digest(&bytes)) != hash {
        return Err(Error::new(409, "Backup data changed."));
    }
    Ok(bytes)
}
pub async fn restore<F, Fut>(target: &Path, manifest: &Value, mut fetch: F) -> Result<()>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<Vec<u8>>>,
{
    validate(manifest)?;
    if target.exists() {
        return Err(Error::new(409, "Restore cannot replace an existing disk."));
    }
    let directory = target
        .parent()
        .ok_or_else(|| Error::bad("Invalid restore directory."))?;
    crate::skills::private_dir(directory).await?;
    let temporary = tempfile::NamedTempFile::new_in(directory)?;
    let mut file = tokio::fs::File::from_std(temporary.reopen()?);
    file.set_len(manifest["size"].as_u64().unwrap()).await?;
    for block in manifest["blocks"].as_array().unwrap() {
        if block["hash"].is_null() {
            continue;
        }
        let hash = text(block, "hash");
        let bytes = fetch(hash.to_owned()).await?;
        if bytes.len() as u64 != block["size"].as_u64().unwrap()
            || hex::encode(Sha256::digest(&bytes)) != hash
        {
            return Err(Error::bad("Backup block integrity check failed."));
        }
        file.seek(std::io::SeekFrom::Start(block["offset"].as_u64().unwrap()))
            .await?;
        file.write_all(&bytes).await?;
    }
    file.sync_all().await?;
    drop(file);
    temporary
        .persist_noclobber(target)
        .map_err(Error::internal)?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

/// A peer cannot cause an unbounded allocation by lying about a block's size.
pub async fn response_block(response: reqwest::Response) -> Result<Vec<u8>> {
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(Error::internal)?;
        if bytes.len().saturating_add(chunk.len()) > BLOCK as usize {
            return Err(Error::bad("Backup block exceeds its maximum size."));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
