//! Bounded project archives. Extraction never follows repository symlinks and
//! durability is committed once by the caller, rather than once per file.
use crate::error::{Error, Result};
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
    path::{Component, Path},
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

const MAX_ENTRIES: usize = 500_000;

struct BoundedFile {
    file: File,
    position: u64,
}

impl Write for BoundedFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.position.saturating_add(bytes.len() as u64) > super::CACHE_BYTES {
            return Err(std::io::Error::other(
                "Project archive exceeds cache limit.",
            ));
        }
        let written = self.file.write(bytes)?;
        self.position += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Seek for BoundedFile {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.position = self.file.seek(position)?;
        Ok(self.position)
    }
}

fn archive_error(error: impl std::fmt::Display) -> Error {
    Error::bad(format!("Invalid project archive: {error}"))
}

pub(super) fn pack(source: &Path, file: File) -> Result<u64> {
    let mut archive = ZipWriter::new(BoundedFile { file, position: 0 });
    let mut pending = vec![source.to_owned()];
    let mut logical = 0u64;
    let mut count = 0;
    while let Some(directory) = pending.pop() {
        let mut entries = std::fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            count += 1;
            if count > MAX_ENTRIES {
                return Err(Error::too_large("Too many project archive entries."));
            }
            let path = entry.path();
            let relative = path.strip_prefix(source).map_err(Error::internal)?;
            // Same private provider state excluded by execution::workspace.
            if relative.starts_with(".codex") || relative.starts_with(".agents/skills") {
                continue;
            }
            let name = relative
                .to_str()
                .ok_or_else(|| Error::bad("Invalid project path."))?;
            let metadata = std::fs::symlink_metadata(&path)?;
            let options = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(1))
                .unix_permissions(metadata.permissions().mode() & 0o777);
            if metadata.is_symlink() {
                if relative == Path::new(".agents") {
                    continue;
                }
                let target = std::fs::read_link(&path)?;
                let target = target
                    .to_str()
                    .ok_or_else(|| Error::bad("Invalid project symlink."))?;
                logical = logical
                    .checked_add(target.len() as u64)
                    .filter(|size| *size <= super::LOGICAL_BYTES)
                    .ok_or_else(|| {
                        Error::too_large("Project archive exceeds logical size limit.")
                    })?;
                archive
                    .add_symlink(name, target, options)
                    .map_err(archive_error)?;
            } else if metadata.is_dir() {
                archive
                    .add_directory(format!("{name}/"), options)
                    .map_err(archive_error)?;
                pending.push(path);
            } else if metadata.is_file() {
                let mut input = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)?;
                if logical.saturating_add(metadata.len()) > super::LOGICAL_BYTES {
                    return Err(Error::too_large(
                        "Project archive exceeds logical size limit.",
                    ));
                }
                archive.start_file(name, options).map_err(archive_error)?;
                logical = logical
                    .checked_add(std::io::copy(&mut input, &mut archive)?)
                    .ok_or_else(|| Error::too_large("Project archive is too large."))?;
            } else {
                return Err(Error::bad("Unsupported project file."));
            }
        }
    }
    archive.finish().map_err(archive_error)?.file.sync_all()?;
    Ok(logical)
}

pub(crate) fn extract(file: File, target: &Path, budget: u64) -> Result<u64> {
    let mut archive = ZipArchive::new(file).map_err(archive_error)?;
    if archive.len() > MAX_ENTRIES {
        return Err(Error::too_large("Too many project archive entries."));
    }
    std::fs::create_dir(target)?;
    let mut used = 0u64;
    let mut seen = HashSet::new();
    let mut directories = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(archive_error)?;
        let path = entry
            .enclosed_name()
            .ok_or_else(|| Error::bad("Project archive escapes its root."))?;
        if path.as_os_str().is_empty()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || !seen.insert(path.clone())
        {
            return Err(Error::bad("Invalid or duplicate project archive path."));
        }
        let mut parent = target.to_owned();
        for part in path.parent().into_iter().flat_map(Path::components) {
            parent.push(part);
            let metadata = std::fs::symlink_metadata(&parent)?;
            if !metadata.is_dir() || metadata.is_symlink() {
                return Err(Error::bad("Project archive parent is not a directory."));
            }
        }
        used = used
            .checked_add(entry.size())
            .filter(|size| *size <= budget)
            .ok_or_else(|| Error::too_large("Project archive exceeds disk budget."))?;
        let destination = target.join(path);
        let mode = entry.unix_mode().unwrap_or(0o600);
        if entry.is_dir() {
            std::fs::create_dir(&destination)?;
            directories.push((destination, mode & 0o777));
        } else if mode & libc::S_IFMT == libc::S_IFLNK {
            if entry.size() > 4096 {
                return Err(Error::bad("Project archive symlink is too long."));
            }
            let mut link = String::new();
            entry.read_to_string(&mut link)?;
            symlink(link, destination)?;
        } else {
            if mode & libc::S_IFMT != 0 && mode & libc::S_IFMT != libc::S_IFREG {
                return Err(Error::bad("Unsupported project archive entry."));
            }
            let expected = entry.size();
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode & 0o777)
                .custom_flags(libc::O_NOFOLLOW)
                .open(destination)?;
            if std::io::copy(&mut entry.take(expected + 1), &mut file)? != expected {
                return Err(Error::bad("Project archive size changed."));
            }
            file.flush()?;
            file.set_permissions(std::fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    for (directory, mode) in directories.into_iter().rev() {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(used)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_roundtrip_preserves_links_modes_and_excludes_provider_state() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir_all(source.join(".codex")).unwrap();
        std::fs::create_dir_all(source.join(".agents/skills/private")).unwrap();
        std::fs::write(source.join(".codex/auth.json"), "secret").unwrap();
        std::fs::write(source.join(".agents/skills/private/SKILL.md"), "private").unwrap();
        std::fs::write(source.join("script"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(
            source.join("script"),
            std::fs::Permissions::from_mode(0o751),
        )
        .unwrap();
        symlink("script", source.join("link")).unwrap();
        let output = tempfile::tempfile().unwrap();
        let logical = pack(&source, output.try_clone().unwrap()).unwrap();
        let target = root.path().join("target");
        assert_eq!(logical, 16); // Script bytes plus the symlink target.
        assert_eq!(extract(output, &target, logical).unwrap(), logical);
        assert_eq!(
            std::fs::read_link(target.join("link")).unwrap(),
            Path::new("script")
        );
        assert_eq!(
            std::fs::metadata(target.join("script"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
        assert!(!target.join(".codex").exists());
        assert!(!target.join(".agents/skills").exists());
    }

    #[test]
    fn traversal_symlink_parents_duplicates_and_budget_are_rejected() {
        for scenario in ["../escape", "link/escape", "duplicate", "budget"] {
            let root = tempfile::tempdir().unwrap();
            let file = tempfile::tempfile().unwrap();
            let mut writer = ZipWriter::new(file.try_clone().unwrap());
            let options = SimpleFileOptions::default().unix_permissions(0o644);
            if scenario == "link/escape" {
                writer
                    .add_symlink("link", root.path().to_str().unwrap(), options)
                    .unwrap();
            }
            let name = if scenario == "duplicate" || scenario == "budget" {
                "file"
            } else {
                scenario
            };
            writer.start_file(name, options).unwrap();
            writer.write_all(b"payload").unwrap();
            if scenario == "duplicate" {
                // A file and a directory with the same normalized path.
                writer.add_directory("file/", options).unwrap();
            }
            writer.finish().unwrap();
            let budget = if scenario == "budget" { 1 } else { 100 };
            assert!(
                extract(file, &root.path().join("target"), budget).is_err(),
                "{scenario}"
            );
            assert!(!root.path().join("escape").exists());
        }
    }
}
