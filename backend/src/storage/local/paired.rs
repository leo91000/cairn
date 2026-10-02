//! Offline preparation only. Both filesystem images are staged before installation.
use crate::{
    error::{Error, Result},
    storage::layout::Layout,
};
use std::{
    fs::File,
    io,
    os::{fd::AsRawFd, unix::fs::FileExt},
    path::{Path, PathBuf},
};

fn copy_region(
    source: &File,
    start: u64,
    target: &File,
    destination: u64,
    size: u64,
) -> io::Result<()> {
    let mut position = start;
    let end = start
        .checked_add(size)
        .ok_or_else(|| io::Error::other("Invalid filesystem extent"))?;
    let mut buffer = vec![0; 1024 * 1024];
    while position < end {
        let data =
            unsafe { libc::lseek(source.as_raw_fd(), position as libc::off_t, libc::SEEK_DATA) };
        let next = if data >= 0 {
            data as u64
        } else {
            match io::Error::last_os_error().raw_os_error() {
                Some(libc::ENXIO) => break,
                Some(libc::EINVAL) => position,
                _ => return Err(io::Error::last_os_error()),
            }
        };
        if next >= end {
            break;
        }
        position = next;
        let length = (end - position).min(buffer.len() as u64) as usize;
        source.read_exact_at(&mut buffer[..length], position)?;
        if buffer[..length].iter().any(|byte| *byte != 0) {
            target.write_all_at(&buffer[..length], destination + position - start)?;
        }
        position += length as u64;
    }
    target.sync_all()
}

pub(super) async fn prepare(
    directory: &Path,
    desired: u64,
    previous: Layout,
) -> Result<(PathBuf, Layout)> {
    let raw = directory.join("data.ext4");
    let next = previous.grown(desired)?;
    next.validate(desired)?;
    let previous_size = match tokio::fs::metadata(&raw).await {
        Ok(metadata) => Some(metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(size) = previous_size {
        previous.validate(size)?;
        if desired < size {
            return Err(Error::conflict("A retained VM disk cannot be shrunk."));
        }
        if size == desired {
            return Ok((raw, previous));
        }
    }
    let staging = tempfile::Builder::new()
        .prefix("paired-")
        .tempdir_in(directory)?;
    let system = staging.path().join("system");
    let workspace = staging.path().join("workspace");
    tokio::fs::create_dir(&system).await?;
    tokio::fs::create_dir(&workspace).await?;
    if let Some(size) = previous_size {
        let source = raw.clone();
        let system_file = system.join("data.ext4");
        let workspace_file = workspace.join("data.ext4");
        tokio::task::spawn_blocking(move || -> io::Result<()> {
            let source = File::open(source)?;
            let first = File::create(system_file)?;
            let second = File::create(workspace_file)?;
            let boundary = previous.system_bytes(size);
            first.set_len(boundary)?;
            second.set_len(size - boundary)?;
            copy_region(&source, 0, &first, 0, boundary)?;
            copy_region(&source, boundary, &second, 0, size - boundary)
        })
        .await
        .map_err(Error::internal)??;
    }
    let boundary = next.system_bytes(desired);
    let system = super::prepare(&system, boundary).await?;
    let workspace = super::prepare(&workspace, desired - boundary).await?;
    let combined = staging.path().join("data.ext4");
    let target = combined.clone();
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        let target = File::create(target)?;
        target.set_len(desired)?;
        copy_region(&File::open(system)?, 0, &target, 0, boundary)?;
        copy_region(
            &File::open(workspace)?,
            0,
            &target,
            boundary,
            desired - boundary,
        )
    })
    .await
    .map_err(Error::internal)??;
    tokio::fs::rename(combined, &raw).await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    Ok((raw, next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microvm::host::command;

    #[tokio::test]
    async fn growing_both_filesystems_preserves_files_and_the_original_on_failure() {
        let root = tempfile::tempdir().unwrap();
        let size = 256 * 1024 * 1024;
        let (raw, layout) = prepare(root.path(), size, Layout::paired(size).unwrap())
            .await
            .unwrap();
        let extract = |name: &str, offset, length| {
            let path = root.path().join(name);
            let file = File::create(&path).unwrap();
            file.set_len(length).unwrap();
            copy_region(&File::open(&raw).unwrap(), offset, &file, 0, length).unwrap();
            path
        };
        let first = extract("first", 0, size / 4);
        let second = extract("second", size / 4, size * 3 / 4);
        let marker = root.path().join("marker");
        tokio::fs::write(&marker, b"persistent").await.unwrap();
        for filesystem in [&first, &second] {
            command(
                "debugfs",
                &[
                    "-w",
                    "-R",
                    &format!("write {} /marker", marker.display()),
                    filesystem.to_str().unwrap(),
                ],
            )
            .await
            .unwrap();
        }
        let file = File::options().write(true).open(&raw).unwrap();
        copy_region(&File::open(&first).unwrap(), 0, &file, 0, size / 4).unwrap();
        copy_region(
            &File::open(&second).unwrap(),
            0,
            &file,
            size / 4,
            size * 3 / 4,
        )
        .unwrap();
        let (_, grown) = prepare(root.path(), size * 2, layout).await.unwrap();
        assert_eq!(grown.system_bytes(size * 2), size / 2);
        for (name, offset, length) in [
            ("grown-first", 0, size / 2),
            ("grown-second", size / 2, size * 3 / 2),
        ] {
            let image = extract(name, offset, length);
            command("e2fsck", &["-fn", image.to_str().unwrap()])
                .await
                .unwrap();
            let saved = root.path().join(format!("{name}-saved"));
            command(
                "debugfs",
                &[
                    "-R",
                    &format!("dump /marker {}", saved.display()),
                    image.to_str().unwrap(),
                ],
            )
            .await
            .unwrap();
            assert_eq!(tokio::fs::read(saved).await.unwrap(), b"persistent");
        }
        let mut signature = [0; 2];
        File::open(&raw)
            .unwrap()
            .read_exact_at(&mut signature, size / 2 + 1080)
            .unwrap();
        assert_eq!(signature, [0x53, 0xef]);
        File::options()
            .write(true)
            .open(&raw)
            .unwrap()
            .write_all_at(&[0, 0], size / 2 + 1080)
            .unwrap();
        assert!(prepare(root.path(), size * 4, grown).await.is_err());
        assert_eq!(tokio::fs::metadata(&raw).await.unwrap().len(), size * 2);
    }
}
