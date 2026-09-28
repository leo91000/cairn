use crate::{
    error::{Error, Result},
    microvm::host::command,
};
use std::path::{Path, PathBuf};

/// Format or grow the temporary ext4 image used to build an S3-backed journal.
pub async fn prepare(disk_dir: &Path, desired: u64) -> Result<PathBuf> {
    let disk = disk_dir.join("data.ext4");
    if !disk.exists() {
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
