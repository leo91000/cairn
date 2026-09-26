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
pub async fn index(path: &Path) -> Result<Value> {
    let mut file = tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .await?;
    let size = file.metadata().await?.len();
    let mut blocks = Vec::new();
    let mut offset = 0;
    let mut buffer = vec![0; BLOCK as usize];
    while offset < size {
        let length = (size - offset).min(BLOCK) as usize;
        file.read_exact(&mut buffer[..length]).await?;
        let hash = if buffer[..length].iter().all(|b| *b == 0) {
            Value::Null
        } else {
            hex::encode(Sha256::digest(&buffer[..length])).into()
        };
        blocks.push(json!({"offset":offset,"size":length,"hash":hash}));
        offset += length as u64;
    }
    Ok(json!({"version":1,"size":size,"blockSize":BLOCK,"blocks":blocks}))
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
    if !valid_hash(hash) {
        return Err(Error::bad("Invalid block digest."));
    }
    let block = manifest["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|b| b["hash"] == hash)
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
