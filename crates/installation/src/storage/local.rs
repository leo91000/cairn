use crate::{
    error::{Error, Result},
    microvm::host::command,
};
use std::path::{Path, PathBuf};

mod paired;

pub(crate) async fn prepare_layout(
    directory: &Path,
    desired: u64,
    layout: super::layout::Layout,
) -> Result<(PathBuf, super::layout::Layout)> {
    match layout {
        super::layout::Layout::Flat => Ok((prepare(directory, desired).await?, layout)),
        super::layout::Layout::Paired { .. } => paired::prepare(directory, desired, layout).await,
    }
}

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
        return Err(Error::conflict("A retained VM disk cannot be shrunk."));
    }
    if desired > actual {
        // A stopped VM can leave a valid journal requiring replay. resize2fs
        // refuses that image until an offline filesystem check has completed.
        command("e2fsck", &["-pf", disk.to_str().unwrap()]).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    #[tokio::test]
    async fn growing_a_used_disk_replays_its_ext4_journal() {
        let directory = tempfile::tempdir().unwrap();
        let original_size = 128 * 1024 * 1024;
        let disk = prepare(directory.path(), original_size).await.unwrap();
        command(
            "debugfs",
            &["-w", "-R", "feature needs_recovery", disk.to_str().unwrap()],
        )
        .await
        .unwrap();

        let desired = original_size * 2;
        prepare(directory.path(), desired).await.unwrap();
        command("e2fsck", &["-fn", disk.to_str().unwrap()])
            .await
            .unwrap();
        let mut image = tokio::fs::File::open(&disk).await.unwrap();
        image.seek(std::io::SeekFrom::Start(1024)).await.unwrap();
        let mut header = [0; 28];
        image.read_exact(&mut header).await.unwrap();
        let blocks = u32::from_le_bytes(header[4..8].try_into().unwrap()) as u64;
        let block_size = 1024_u64 << u32::from_le_bytes(header[24..28].try_into().unwrap());
        assert_eq!(blocks * block_size, desired);
    }

    #[tokio::test]
    async fn invalid_filesystem_is_rejected_before_extending_the_image() {
        let directory = tempfile::tempdir().unwrap();
        let disk = directory.path().join("data.ext4");
        let original = b"not an ext4 filesystem";
        tokio::fs::write(&disk, original).await.unwrap();

        let error = prepare(directory.path(), 128 * 1024 * 1024)
            .await
            .unwrap_err();
        assert!(error.message.contains("e2fsck"));
        assert_eq!(tokio::fs::read(&disk).await.unwrap(), original);
    }
}
