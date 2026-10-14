//! Multi-file skill bundles: scanning, revisions and atomic batch changes.
//! See docs/SKILLS.md.
use super::{metadata, private_dir, relative};
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::{
    io::ErrorKind,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tokio::{fs, io::AsyncWriteExt};

pub const FILE_BYTES: u64 = 4 * 1024 * 1024;
pub const BUNDLE_BYTES: u64 = 16 * 1024 * 1024;
pub const FILES: usize = 500;
pub const UPLOAD_BYTES: usize = 5 * 1024 * 1024;
const PATH_BYTES: usize = 240;
const SEGMENTS: usize = 8;
// Bounds walks of directories that were not written through Cairn.
const SCAN_FILES: usize = 10_000;
const SCAN_DEPTH: usize = 32;

pub fn limits() -> Value {
    json!({
        "fileBytes": FILE_BYTES,
        "bundleBytes": BUNDLE_BYTES,
        "files": FILES,
        "uploadBytes": UPLOAD_BYTES,
    })
}

/// A path Cairn may write inside a skill.
pub fn path(value: &str) -> Result<()> {
    relative(value)?;
    let segments = value.split('/').collect::<Vec<_>>();
    if value.len() > PATH_BYTES
        || segments.len() > SEGMENTS
        || segments.iter().any(|segment| segment.starts_with('.'))
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return Err(Error::bad(format!(
            "Use a relative path of at most {SEGMENTS} parts, without hidden names: {value}"
        )));
    }
    Ok(())
}

pub fn is_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

pub fn mode(executable: bool) -> u32 {
    if executable { 0o700 } else { 0o600 }
}

/// A visible regular file of a skill.
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub executable: bool,
}

/// Visible regular files of a skill directory, sorted by path. Names starting
/// with `.` are not part of a skill; symbolic links are refused.
pub async fn scan(directory: &Path) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut queue = vec![(PathBuf::new(), 0)];
    while let Some((relative, depth)) = queue.pop() {
        let mut items = fs::read_dir(directory.join(&relative)).await?;
        while let Some(item) = items.next_entry().await? {
            let name = item.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| Error::bad("Skill file names must be UTF-8."))?;
            if name.starts_with('.') {
                continue;
            }
            let path = relative.join(name);
            let kind = item.file_type().await?;
            if kind.is_symlink() {
                return Err(Error::bad(format!(
                    "Skills must not contain symbolic links: {}",
                    path.display()
                )));
            }
            if kind.is_dir() {
                if depth >= SCAN_DEPTH {
                    return Err(Error::bad("Skill folders are nested too deeply."));
                }
                queue.push((path, depth + 1));
                continue;
            }
            if !kind.is_file() {
                return Err(Error::bad(
                    "Skills can contain only regular files and folders.",
                ));
            }
            let metadata = item.metadata().await?;
            entries.push(Entry {
                path: path.to_string_lossy().into_owned(),
                size: metadata.len(),
                executable: metadata.permissions().mode() & 0o111 != 0,
            });
            if entries.len() > SCAN_FILES {
                return Err(Error::bad("This skill has too many files."));
            }
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// Every visible file of a skill with its content, read once, and the
/// revision of exactly those contents.
pub async fn read_all(directory: &Path) -> Result<(String, Vec<(Entry, Vec<u8>)>)> {
    let entries = scan(directory).await?;
    let mut hasher = blake3::Hasher::new();
    let mut files = Vec::with_capacity(entries.len());
    for mut entry in entries {
        let bytes = fs::read(directory.join(&entry.path)).await?;
        entry.size = bytes.len() as u64;
        hasher.update(entry.path.as_bytes());
        hasher.update(&[0, u8::from(entry.executable)]);
        hasher.update(&entry.size.to_le_bytes());
        hasher.update(&bytes);
        files.push((entry, bytes));
    }
    Ok((hasher.finalize().to_hex().to_string(), files))
}

/// A skill's files with their revision.
pub struct Described {
    pub revision: String,
    pub files: Vec<Value>,
}

pub async fn describe(directory: &Path) -> Result<Described> {
    let (revision, files) = read_all(directory).await?;
    let files = files
        .iter()
        .map(|(entry, bytes)| {
            json!({
                "path": entry.path,
                "size": entry.size,
                "executable": entry.executable,
                "text": is_text(bytes),
            })
        })
        .collect();
    Ok(Described { revision, files })
}

/// One edit of a batch, with its content already resolved.
pub enum Change {
    Write {
        path: String,
        bytes: Vec<u8>,
        executable: Option<bool>,
    },
    Delete {
        path: String,
    },
    Move {
        from: String,
        to: String,
    },
    Mode {
        path: String,
        executable: bool,
    },
}

pub async fn write_file(target: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode(executable))
        .custom_flags(libc::O_NOFOLLOW)
        .open(target)
        .await?;
    file.write_all(bytes).await?;
    file.sync_all().await?;
    drop(file);
    fs::set_permissions(target, std::fs::Permissions::from_mode(mode(executable))).await?;
    Ok(())
}

async fn kind(path: &Path) -> Result<Option<std::fs::Metadata>> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Creates the folders above `file`, refusing a file that already uses one of
/// their names.
pub(super) async fn parents(root: &Path, file: &str) -> Result<()> {
    let mut current = root.to_owned();
    let segments = file.split('/').collect::<Vec<_>>();
    for segment in &segments[..segments.len() - 1] {
        current.push(segment);
        match kind(&current).await? {
            Some(metadata) if metadata.is_dir() => {}
            Some(_) => {
                return Err(Error::bad(format!(
                    "{} is a file, not a folder.",
                    current.strip_prefix(root).unwrap_or(&current).display()
                )));
            }
            None => private_dir(&current).await?,
        }
    }
    Ok(())
}

async fn existing_file(root: &Path, file: &str) -> Result<std::fs::Metadata> {
    match kind(&root.join(file)).await? {
        Some(metadata) if metadata.is_file() => Ok(metadata),
        _ => Err(Error::bad(format!("{file} does not exist."))),
    }
}

async fn apply(root: &Path, change: &Change) -> Result<()> {
    match change {
        Change::Write {
            path: file,
            bytes,
            executable,
        } => {
            path(file)?;
            if bytes.len() as u64 > FILE_BYTES {
                return Err(Error::too_large(format!(
                    "{file} is larger than {} MiB.",
                    FILE_BYTES >> 20
                )));
            }
            parents(root, file).await?;
            let target = root.join(file);
            let current = kind(&target).await?;
            if current.as_ref().is_some_and(|metadata| !metadata.is_file()) {
                return Err(Error::bad(format!("{file} is a folder.")));
            }
            let executable = executable
                .or_else(|| current.map(|metadata| metadata.permissions().mode() & 0o111 != 0))
                .unwrap_or_else(|| bytes.starts_with(b"#!"));
            write_file(&target, bytes, executable).await
        }
        Change::Delete { path: file } => {
            path(file)?;
            existing_file(root, file).await?;
            fs::remove_file(root.join(file)).await?;
            Ok(())
        }
        Change::Move { from, to } => {
            path(from)?;
            path(to)?;
            existing_file(root, from).await?;
            if kind(&root.join(to)).await?.is_some() {
                return Err(Error::bad(format!("{to} already exists.")));
            }
            parents(root, to).await?;
            fs::rename(root.join(from), root.join(to)).await?;
            Ok(())
        }
        Change::Mode {
            path: file,
            executable,
        } => {
            path(file)?;
            existing_file(root, file).await?;
            fs::set_permissions(
                root.join(file),
                std::fs::Permissions::from_mode(mode(*executable)),
            )
            .await?;
            Ok(())
        }
    }
}

/// Removes folders left empty by deletes and moves; returns whether `directory`
/// itself is now empty.
async fn prune(directory: &Path) -> Result<bool> {
    let mut empty = true;
    let mut items = fs::read_dir(directory).await?;
    while let Some(item) = items.next_entry().await? {
        if item.file_type().await?.is_dir() && Box::pin(prune(&item.path())).await? {
            fs::remove_dir(item.path()).await?;
        } else {
            empty = false;
        }
    }
    Ok(empty)
}

/// Checks the limits and `SKILL.md` of a prepared directory.
async fn validate(directory: &Path, skill: &str) -> Result<()> {
    let entries = scan(directory).await?;
    if entries.len() > FILES {
        return Err(Error::too_large(format!(
            "A skill can contain at most {FILES} files."
        )));
    }
    let total = entries.iter().map(|entry| entry.size).sum::<u64>();
    if total > BUNDLE_BYTES {
        return Err(Error::too_large(format!(
            "A skill can contain at most {} MiB.",
            BUNDLE_BYTES >> 20
        )));
    }
    for entry in &entries {
        path(&entry.path)?;
        if entry.size > FILE_BYTES {
            return Err(Error::too_large(format!(
                "{} is larger than {} MiB.",
                entry.path,
                FILE_BYTES >> 20
            )));
        }
    }
    if !entries.iter().any(|entry| entry.path == "SKILL.md") {
        return Err(Error::bad("A skill needs a SKILL.md file."));
    }
    let content = super::small_file(&directory.join("SKILL.md")).await?;
    let parsed = metadata(&content)?;
    if parsed.name != skill {
        return Err(Error::bad(
            "Frontmatter name must match the skill directory.",
        ));
    }
    Ok(())
}

/// Copies a directory tree, preserving file modes and refusing links.
pub async fn copy_dir(source: &Path, target: &Path) -> Result<()> {
    private_dir(target).await?;
    let mut items = fs::read_dir(source).await?;
    while let Some(item) = items.next_entry().await? {
        let kind = item.file_type().await?;
        let destination = target.join(item.file_name());
        if kind.is_dir() {
            Box::pin(copy_dir(&item.path(), &destination)).await?;
        } else if kind.is_file() {
            fs::copy(item.path(), destination).await?;
        } else {
            return Err(Error::bad("Skills must not contain symbolic links."));
        }
    }
    Ok(())
}

/// Atomically exchanges two directories; `false` when the filesystem cannot.
async fn exchange(a: &Path, b: &Path) -> Result<bool> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let (a, b) = (
        CString::new(a.as_os_str().as_bytes()).map_err(|_| Error::bad("Invalid path."))?,
        CString::new(b.as_os_str().as_bytes()).map_err(|_| Error::bad("Invalid path."))?,
    );
    tokio::task::spawn_blocking(move || {
        // SAFETY: both paths are valid NUL-terminated strings for the call.
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                a.as_ptr(),
                libc::AT_FDCWD,
                b.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        };
        if result == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP) => Ok(false),
            _ => Err(error.into()),
        }
    })
    .await
    .map_err(|_| Error::bad("Could not save the skill."))?
}

/// Replaces `target` with `staging`. Readers see either the old or the new
/// skill, never a mix.
async fn swap(staging: &Path, target: &Path, exists: bool) -> Result<()> {
    if !exists {
        return fs::rename(staging, target).await.map_err(|error| {
            if matches!(
                error.kind(),
                ErrorKind::AlreadyExists | ErrorKind::DirectoryNotEmpty
            ) {
                Error::conflict("A skill with this name was created meanwhile.")
            } else {
                error.into()
            }
        });
    }
    if exchange(staging, target).await? {
        fs::remove_dir_all(staging).await?;
        return Ok(());
    }
    let previous = staging.with_file_name(format!(
        ".{}.previous-{}",
        target.file_name().unwrap().to_string_lossy(),
        crate::config::id()
    ));
    fs::rename(target, &previous).await?;
    if let Err(error) = fs::rename(staging, target).await {
        fs::rename(&previous, target).await?;
        return Err(error.into());
    }
    fs::remove_dir_all(previous).await?;
    Ok(())
}

/// Serializes batch saves so that each one checks its base revision against
/// the skill it replaces.
static SAVES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How a batch relates to the existing skill.
pub enum Base<'a> {
    /// The skill must not exist yet.
    Create,
    /// The skill must currently have this revision; changes apply to a copy.
    Edit(&'a str),
    /// Like `Edit`, but the changes describe the whole new skill.
    Replace(&'a str),
}

/// Applies `changes` to a copy of `root/skill`, validates it, then swaps it in.
pub async fn save(root: &Path, skill: &str, base: Base<'_>, changes: &[Change]) -> Result<()> {
    let _save = SAVES.lock().await;
    let directory = root.join(skill);
    let exists = match kind(&directory).await? {
        Some(metadata) if metadata.is_dir() => true,
        Some(_) => return Err(Error::bad("The skill path is not a directory.")),
        None => false,
    };
    match (&base, exists) {
        (Base::Create, true) => {
            return Err(Error::conflict(format!(
                "A skill named {skill} already exists here."
            )));
        }
        (Base::Edit(_) | Base::Replace(_), false) => {
            return Err(Error::not_found("Skill not found."));
        }
        (Base::Edit(revision) | Base::Replace(revision), true) => {
            if describe(&directory).await?.revision != *revision {
                return Err(Error::conflict(
                    "This skill changed since you opened it. Reload it and try again.",
                ));
            }
        }
        (Base::Create, false) => {}
    }

    let staging = root.join(format!(".{skill}.staging-{}", crate::config::id()));
    let result = async {
        if matches!(base, Base::Edit(_)) {
            copy_dir(&directory, &staging).await?;
        } else {
            private_dir(&staging).await?;
        }
        for change in changes {
            apply(&staging, change).await?;
        }
        prune(&staging).await?;
        validate(&staging, skill).await?;
        swap(&staging, &directory, exists).await
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging).await;
    }
    result
}
