//! File rules shared by guest export and durable delivery. No guest path is opened on the host.
use crate::error::{Error, Result};
use std::{
    ffi::CString,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::{Component, Path},
};

pub const MAX_FILE: u64 = 512 * 1024 * 1024;

pub fn open_export(path: &Path, root: &Path) -> Result<std::fs::File> {
    if !path.is_absolute() || !(path.starts_with(root) || path.starts_with("/tmp")) {
        return Err(Error::bad("Publish files from the run workspace or /tmp."));
    }
    // Walk with directory descriptors: neither parent symlinks nor concurrent renames
    // can redirect this read to a different tree. Nonblocking prevents FIFO/device hangs.
    let mut fd: OwnedFd = std::fs::File::open("/")?.into();
    let components: Vec<_> = path.components().collect();
    for (index, component) in components.iter().enumerate().skip(1) {
        let Component::Normal(name) = component else {
            return Err(Error::bad("Invalid artifact path."));
        };
        let name =
            CString::new(name.as_bytes()).map_err(|_| Error::bad("Invalid artifact path."))?;
        let directory = index + 1 < components.len();
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if directory { libc::O_DIRECTORY } else { 0 };
        let next = unsafe { libc::openat(fd.as_raw_fd(), name.as_ptr(), flags) };
        if next < 0 {
            return Err(Error::bad("Artifact is unavailable or contains a symlink."));
        }
        fd = unsafe { OwnedFd::from_raw_fd(next) };
    }
    let file = std::fs::File::from(fd);
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_FILE {
        return Err(Error::bad("Publish a regular file of at most 512 MB."));
    }
    Ok(file)
}

pub async fn snapshot(path: &Path, root: &Path) -> Result<(tempfile::NamedTempFile, u64)> {
    use std::os::unix::fs::MetadataExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let source = open_export(path, root)?;
    let before = source.metadata()?;
    let temporary = tempfile::NamedTempFile::new()?;
    let mut output = tokio::fs::File::from_std(temporary.reopen()?);
    let mut source = tokio::fs::File::from_std(source);
    let size = tokio::io::copy(&mut (&mut source).take(MAX_FILE + 1), &mut output).await?;
    let after = source.metadata().await?;
    if size > MAX_FILE
        || size != before.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(Error::bad(
            "File changed while publishing. Finish writing it and retry.",
        ));
    }
    output.flush().await?;
    Ok((temporary, size))
}

/// Text extensions previewed as source. Markup such as HTML stays `text/plain`:
/// viewers render it only inside a sandbox, never as an active document.
const CODE_EXTENSIONS: &[&str] = &[
    "txt",
    "log",
    "csv",
    "tsv",
    "json",
    "jsonl",
    "ndjson",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "xml",
    "html",
    "htm",
    "css",
    "scss",
    "less",
    "js",
    "mjs",
    "cjs",
    "jsx",
    "ts",
    "mts",
    "cts",
    "tsx",
    "vue",
    "svelte",
    "rs",
    "py",
    "rb",
    "php",
    "go",
    "java",
    "kt",
    "kts",
    "swift",
    "c",
    "h",
    "cc",
    "cpp",
    "hpp",
    "cs",
    "lua",
    "sh",
    "bash",
    "zsh",
    "fish",
    "sql",
    "graphql",
    "proto",
    "tf",
    "diff",
    "patch",
    "dockerfile",
    "makefile",
    "gradle",
];

pub fn classify(name: &str, bytes: &[u8]) -> (&'static str, &'static str) {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return ("image", "image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return ("image", "image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return ("image", "image/gif");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return ("image", "image/webp");
    }
    if bytes.starts_with(b"%PDF-") {
        return ("pdf", "application/pdf");
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_else(|| name.strip_prefix('.').unwrap_or(name))
        .to_lowercase();
    if bytes.starts_with(b"BM") && extension == "bmp" {
        return ("image", "image/bmp");
    }
    if bytes.get(4..8) == Some(b"ftyp") && matches!(bytes.get(8..12), Some(b"avif" | b"avis")) {
        return ("image", "image/avif");
    }
    if bytes.get(4..8) == Some(b"ftyp") {
        return if extension == "m4a" {
            ("audio", "audio/mp4")
        } else {
            ("video", "video/mp4")
        };
    }
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return ("video", "video/webm");
    }
    if bytes.starts_with(b"OggS") {
        return ("audio", "audio/ogg");
    }
    if bytes.starts_with(b"ID3")
        || (bytes.first() == Some(&0xff) && bytes.get(1).is_some_and(|b| b & 0xe0 == 0xe0))
    {
        return ("audio", "audio/mpeg");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        return ("audio", "audio/wav");
    }
    if bytes.starts_with(b"fLaC") {
        return ("audio", "audio/flac");
    }
    if !bytes.contains(&0)
        && match std::str::from_utf8(bytes) {
            Ok(_) => true,
            Err(error) => error.error_len().is_none(),
        }
    {
        if ["md", "markdown"].contains(&extension.as_str()) {
            return ("markdown", "text/plain; charset=utf-8");
        }
        // Served with a sandbox CSP and as an attachment; viewers only load it as an image.
        if extension == "svg" && String::from_utf8_lossy(bytes).contains("<svg") {
            return ("image", "image/svg+xml");
        }
        if CODE_EXTENSIONS.contains(&extension.as_str()) {
            return ("code", "text/plain; charset=utf-8");
        }
    }
    ("file", "application/octet-stream")
}

pub fn range(header: Option<&str>, size: u64) -> Result<Option<(u64, u64)>> {
    let Some(header) = header else {
        return Ok(None);
    };
    let invalid = || Error::new(416, "Requested range is unavailable.");
    let range = header.strip_prefix("bytes=").ok_or_else(invalid)?;
    let (start, end) = range.split_once('-').ok_or_else(invalid)?;
    if size == 0 {
        return Err(invalid());
    }
    if start.is_empty() {
        let suffix: u64 = end.parse().map_err(|_| invalid())?;
        if suffix == 0 {
            return Err(invalid());
        }
        return Ok(Some((size.saturating_sub(suffix), size - 1)));
    }
    let start: u64 = start.parse().map_err(|_| invalid())?;
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().map_err(|_| invalid())?.min(size - 1)
    };
    if start >= size || end < start {
        return Err(invalid());
    }
    Ok(Some((start, end)))
}
