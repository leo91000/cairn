use crate::{
    error::{Error, Result},
    microvm::host::command,
};
use std::path::{Path, PathBuf};

/// Prepare or grow a retained local ext4 disk while preserving legacy write tracking.
pub async fn prepare(disk_dir: &Path, desired: u64) -> Result<PathBuf> {
    let disk = disk_dir.join("data.ext4");
    if !disk.exists() {
        crate::nodes::tracking::invalidate(disk_dir).await?;
        let file = tokio::fs::File::create(disk_dir.join("data.partial")).await?;
        file.set_len(desired).await?;
        drop(file);
        command(
            "mkfs.ext4",
            &["-q", "-F", disk_dir.join("data.partial").to_str().unwrap()],
        )
        .await?;
        tokio::fs::rename(disk_dir.join("data.partial"), &disk).await?;
    }
    let actual = tokio::fs::metadata(&disk).await?.len();
    if desired < actual {
        return Err(Error::new(409, "A retained VM disk cannot be shrunk."));
    }
    if desired > actual {
        // resize2fs writes outside the guest's write tracking.
        crate::nodes::tracking::invalidate(disk_dir).await?;
        tokio::fs::OpenOptions::new()
            .write(true)
            .open(&disk)
            .await?
            .set_len(desired)
            .await?;
        command(
            "resize2fs",
            &[disk
                .to_str()
                .ok_or_else(|| Error::bad("Invalid disk path."))?],
        )
        .await?;
    }
    Ok(disk)
}

use crate::nodes::snapshots::BLOCK;
/// Copies the listed blocks of the paused disk to a sparse file of the same size.
pub async fn copy_blocks(source: &Path, target: &Path, blocks: &[u64]) -> Result<u64> {
    let (source, target, blocks) = (source.to_owned(), target.to_owned(), blocks.to_owned());
    tokio::task::spawn_blocking(move || -> Result<u64> {
        use super::{Disk, LocalDisk};
        use std::os::unix::fs::FileExt;
        let input = LocalDisk::open(&source, false)?;
        let size = input.size();
        let output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        output.set_len(size)?;
        let (mut buffer, mut read) = (vec![0; BLOCK as usize], 0);
        for block in blocks {
            let offset = block * BLOCK;
            let length = size.saturating_sub(offset).min(BLOCK) as usize;
            input.read_at(offset, &mut buffer[..length])?;
            output.write_all_at(&buffer[..length], offset)?;
            read += length as u64;
        }
        output.sync_all()?;
        Ok(read)
    })
    .await
    .map_err(Error::internal)?
}
