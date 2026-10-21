//! Copy durable user files between native data disks and VM overlay disks.
//! Read ext4 in userspace: an agent-controlled filesystem is never mounted on
//! the manager. A fresh image is installed only after every selected file copies.
use crate::{
    error::{Error, Result},
    validation::text,
};
use ext4_view::{Ext4, Ext4Read, FileType};
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::{Seek, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};

struct Reader {
    file: File,
    offset: u64,
    size: u64,
}

impl Ext4Read for Reader {
    fn read(
        &mut self,
        at: u64,
        bytes: &mut [u8],
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if at
            .checked_add(bytes.len() as u64)
            .is_none_or(|end| end > self.size)
        {
            return Err(std::io::Error::other("Filesystem read exceeds its disk view").into());
        }
        self.file.read_exact_at(bytes, self.offset + at)?;
        Ok(())
    }
}

fn filesystem(path: &Path, offset: u64, size: u64) -> Result<Ext4> {
    Ext4::load(Box::new(Reader {
        file: File::open(path)?,
        offset,
        size,
    }))
    .map_err(|error| Error::bad(format!("Cannot read the saved ext4 filesystem: {error}")))
}

fn relative(path: &Path) -> Result<&Path> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(Error::bad("Invalid saved environment path."));
    }
    path.strip_prefix("/").map_err(Error::internal)
}

/// Create only actual directories, never following a symlink from copied data.
fn directory(root: &Path, path: &Path) -> Result<()> {
    let suffix = path.strip_prefix(root).map_err(Error::internal)?;
    let mut current = root.to_owned();
    for part in suffix.components() {
        let Component::Normal(name) = part else {
            return Err(Error::bad("Invalid migration directory."));
        };
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(Error::conflict(
                    "Saved data overlaps a migration directory.",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn require_space(path: &Path, reserve: u64, additional: u64) -> Result<()> {
    let (_, free) = super::policy::space(path)?;
    if free < reserve.saturating_add(additional) {
        return Err(Error::new(
            507,
            "Insufficient temporary migration space; original environment retained.",
        ));
    }
    Ok(())
}

struct Copy {
    root: PathBuf,
    remaining: u64,
    entries: u64,
    minimum_free: u64,
}

impl Copy {
    fn tree(&mut self, fs: &Ext4, source: &Path, destination: &Path) -> Result<()> {
        let mut pending = vec![(source.to_owned(), destination.to_owned(), 0)];
        while let Some((source, target, depth)) = pending.pop() {
            require_space(&self.root, self.minimum_free, 64 * 1024 * 1024)?;
            self.entries += 1;
            if depth > 256 || self.entries > 1_000_000 {
                return Err(Error::bad(
                    "Saved environment exceeds migration traversal limits.",
                ));
            }
            let metadata = fs
                .symlink_metadata(source.as_path())
                .map_err(Error::internal)?;
            directory(
                &self.root,
                target
                    .parent()
                    .ok_or_else(|| Error::bad("Invalid migration target."))?,
            )?;
            match metadata.file_type() {
                FileType::Directory => {
                    directory(&self.root, &target)?;
                    std::fs::set_permissions(
                        &target,
                        std::fs::Permissions::from_mode(u32::from(metadata.mode() & 0o777) | 0o700),
                    )?;
                    for entry in fs.read_dir(source.as_path()).map_err(Error::internal)? {
                        let entry = entry.map_err(Error::internal)?;
                        let name = entry.file_name();
                        if name == "." || name == ".." {
                            continue;
                        }
                        let name = std::ffi::OsStr::from_bytes(name.as_ref());
                        pending.push((source.join(name), target.join(name), depth + 1));
                    }
                }
                FileType::Regular => {
                    let mut source = fs.open(source.as_path()).map_err(Error::internal)?;
                    let mut output = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&target)?;
                    let mut buffer = vec![0; 128 * 1024];
                    loop {
                        let count = source.read_bytes(&mut buffer).map_err(Error::internal)?;
                        if count == 0 {
                            break;
                        }
                        // Keep sparse emulator images and caches sparse on the
                        // manager. Dense extraction can exceed source block use.
                        require_space(&self.root, self.minimum_free, 64 * 1024 * 1024)?;
                        for page in buffer[..count].chunks(4096) {
                            if page.iter().all(|byte| *byte == 0) {
                                output.seek(std::io::SeekFrom::Current(page.len() as i64))?;
                            } else {
                                // Apparent lengths can exceed the filesystem
                                // size for sparse files. Bound bytes written,
                                // including repeated hard links, instead of holes.
                                self.remaining = self
                                    .remaining
                                    .checked_sub(page.len() as u64)
                                    .ok_or_else(|| {
                                        Error::new(
                                            507,
                                            "Saved data exceeds the migration disk budget.",
                                        )
                                    })?;
                                output.write_all(page)?;
                            }
                        }
                    }
                    output.set_len(metadata.len())?;
                    output.sync_all()?;
                    output.set_permissions(std::fs::Permissions::from_mode(
                        u32::from(metadata.mode() & 0o777) | 0o600,
                    ))?;
                }
                FileType::Symlink => {
                    let link = fs.read_link(source.as_path()).map_err(Error::internal)?;
                    std::os::unix::fs::symlink(
                        Path::new(std::ffi::OsStr::from_bytes(link.as_ref())),
                        target,
                    )?;
                }
                // Overlay whiteouts and runtime IPC are not portable user files.
                FileType::CharacterDevice | FileType::Socket | FileType::Fifo => {}
                FileType::BlockDevice => {
                    return Err(Error::bad("Saved environment contains a device file."));
                }
            }
        }
        Ok(())
    }
}

/// Produce a flat ext4 disk for the other backend. The source stays read-only.
/// Workspaces, private home and outputs move; host/guest operating-system files
/// and processes are deliberately rebuilt by the destination runtime.
pub(crate) async fn convert(
    image: &Path,
    manifest: &Value,
    prepared: &Value,
    backend: &str,
    staging: &Path,
    minimum_free: u64,
) -> Result<PathBuf> {
    let source_native = manifest["kind"] == "native-ext4-v1";
    if !matches!(backend, "native" | "firecracker") || source_native == (backend == "native") {
        return Err(Error::bad("Choose the other execution backend."));
    }
    let (image, manifest, prepared, staging, backend) = (
        image.to_owned(),
        manifest.clone(),
        prepared.clone(),
        staging.to_owned(),
        backend.to_owned(),
    );
    let tree = staging.join("tree");
    std::fs::create_dir(&tree)?;
    let copy_root = tree.clone();
    let size = manifest["size"]
        .as_u64()
        .ok_or_else(|| Error::bad("Missing saved disk size."))?;
    tokio::task::spawn_blocking(move || -> Result<()> {
        let layout = super::layout::Layout::from_manifest(&manifest)?;
        let system_size = layout.system_bytes(size);
        let system = filesystem(&image, 0, system_size)?;
        let paired = match layout {
            super::layout::Layout::Flat => None,
            super::layout::Layout::Paired { system_bytes } => {
                Some(filesystem(&image, system_bytes, size - system_bytes)?)
            }
        };
        let native_source = manifest["kind"] == "native-ext4-v1";
        let mut copy = Copy {
            root: copy_root.clone(),
            remaining: size,
            entries: 0,
            minimum_free,
        };
        for (name, guest) in [
            ("home", Path::new("/home/node")),
            ("workspaces", Path::new(text(&prepared, "projectRoot"))),
            (
                "output",
                Path::new(text(&prepared, "output"))
                    .parent()
                    .ok_or_else(|| Error::bad("Missing saved output directory."))?,
            ),
        ] {
            let guest_relative = relative(guest)?;
            let (source, fs) = if native_source {
                (Path::new("/").join(name), &system)
            } else if let Some(paired) = paired.as_ref().filter(|_| guest.starts_with("/data")) {
                (
                    Path::new("/").join(guest.strip_prefix("/data").unwrap()),
                    paired,
                )
            } else {
                (Path::new("/upper").join(guest_relative), &system)
            };
            let target = if backend == "native" {
                copy_root.join(name)
            } else {
                copy_root.join("upper").join(guest_relative)
            };
            if !fs.exists(source.as_path()).map_err(Error::internal)? {
                return Err(Error::conflict(format!(
                    "Saved {name} directory is unavailable; original disk retained."
                )));
            }
            // These roots become host bind sources on native nodes. Following
            // a saved root symlink could bind a host path instead of user data.
            if !fs
                .symlink_metadata(source.as_path())
                .map_err(Error::internal)?
                .is_dir()
            {
                return Err(Error::conflict(format!(
                    "Saved {name} root must be a directory, not a symlink or file; original disk retained."
                )));
            }
            copy.tree(fs, &source, &target)?;
        }
        if !native_source && let Some(paired) = paired {
            for (source, relative) in [
                ("home-cache", ".cache"),
                ("home-share", ".local/share"),
                ("android", ".android"),
            ] {
                let source = Path::new("/.cairn-system").join(source);
                if paired.exists(source.as_path()).map_err(Error::internal)? {
                    copy.tree(&paired, &source, &copy_root.join("home").join(relative))?;
                }
            }
        }
        if backend == "firecracker" {
            let initialized = copy_root.join("upper/var/lib/cairn");
            directory(&copy_root, &initialized)?;
            std::fs::write(initialized.join("initialized"), b"1")?;
        }
        Ok(())
    })
    .await
    .map_err(Error::internal)??;
    // Reserve the full logical destination size before mkfs. Its allocation
    // cannot exceed this bound, even when filesystem metadata/journal creation
    // writes more than the source's nonzero block estimate.
    require_space(
        &staging,
        minimum_free,
        size.saturating_add(64 * 1024 * 1024),
    )?;
    let target = staging.join("converted.ext4");
    File::create(&target)?.set_len(size)?;
    // The manager is unprivileged and may have any UID. fakeroot changes only
    // metadata observed by mkfs, including symlink ownership, without following
    // copied links or requiring host chown privileges. Directory population also
    // works with the e2fsprogs 1.47.0 shipped in the installation image.
    crate::microvm::host::command(
        "fakeroot",
        &[
            "--",
            "sh",
            "-c",
            "chown -hR 1000:1000 -- \"$1\" && exec mkfs.ext4 -q -F -d \"$1\" \"$2\"",
            "cairn-backend-migration",
            tree.to_str()
                .ok_or_else(|| Error::bad("Invalid staging path."))?,
            target.to_str().unwrap(),
        ],
    )
    .await?;
    Ok(target)
}

#[cfg(test)]
mod tests;
