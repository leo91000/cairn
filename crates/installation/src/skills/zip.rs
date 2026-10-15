//! Minimal ZIP reading and writing for skill archives: stored and deflated
//! entries, Unix modes, no encryption and no ZIP64.
use crate::error::{Error, Result};
use flate2::{Compression, Crc, read::DeflateDecoder, write::DeflateEncoder};
use std::io::{Read, Write};

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
const UNIX: u16 = 3;
const MAX_ENTRIES: usize = 10_000;

/// A file listed in an archive's central directory.
pub struct Entry {
    pub path: String,
    pub size: u64,
    /// Known only for archives made on Unix.
    pub executable: Option<bool>,
    method: u16,
    compressed: u64,
    crc: u32,
    offset: usize,
}

fn invalid() -> Error {
    Error::bad("This file is not a readable ZIP archive.")
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16> {
    let slice = bytes.get(at..at + 2).ok_or_else(invalid)?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    let slice = bytes.get(at..at + 4).ok_or_else(invalid)?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// Regular files of the archive, without directory entries. Paths that could
/// escape the extraction folder are rejected.
pub fn entries(bytes: &[u8]) -> Result<Vec<Entry>> {
    let search = bytes.len().saturating_sub(22 + 65_535);
    let end = (search..bytes.len().saturating_sub(21))
        .rev()
        .find(|&at| u32_at(bytes, at).is_ok_and(|signature| signature == END))
        .ok_or_else(invalid)?;
    let count = usize::from(u16_at(bytes, end + 10)?);
    let directory = u32_at(bytes, end + 16)? as usize;
    if count == 0xffff || directory == 0xffff_ffff {
        return Err(Error::bad("ZIP64 archives are not supported."));
    }
    if count > MAX_ENTRIES {
        return Err(Error::bad("This archive has too many entries."));
    }

    let mut entries = Vec::new();
    let mut at = directory;
    for _ in 0..count {
        if u32_at(bytes, at)? != CENTRAL {
            return Err(invalid());
        }
        let made_by = u16_at(bytes, at + 4)?;
        let flags = u16_at(bytes, at + 8)?;
        let method = u16_at(bytes, at + 10)?;
        let crc = u32_at(bytes, at + 16)?;
        let compressed = u64::from(u32_at(bytes, at + 20)?);
        let size = u64::from(u32_at(bytes, at + 24)?);
        let name_length = usize::from(u16_at(bytes, at + 28)?);
        let extra_length = usize::from(u16_at(bytes, at + 30)?);
        let comment_length = usize::from(u16_at(bytes, at + 32)?);
        let attributes = u32_at(bytes, at + 38)?;
        let offset = u32_at(bytes, at + 42)? as usize;
        let name = bytes
            .get(at + 46..at + 46 + name_length)
            .ok_or_else(invalid)?;
        at += 46 + name_length + extra_length + comment_length;

        let path = std::str::from_utf8(name)
            .map_err(|_| Error::bad("Archive file names must be UTF-8."))?
            .replace('\\', "/");
        if path.ends_with('/') {
            continue;
        }
        if path.starts_with('/')
            || path
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(Error::bad(format!("Archive path is not allowed: {path}")));
        }
        if flags & 1 != 0 {
            return Err(Error::bad("Encrypted archives are not supported."));
        }
        if compressed == 0xffff_ffff || size == 0xffff_ffff {
            return Err(Error::bad("ZIP64 archives are not supported."));
        }
        let mode = attributes >> 16;
        let unix = made_by >> 8 == UNIX;
        // 0o120000: symbolic link; such entries hold a link target, not content.
        if unix && mode & 0o170_000 == 0o120_000 {
            return Err(Error::bad(format!(
                "Archives must not contain symbolic links: {path}"
            )));
        }
        entries.push(Entry {
            path,
            size,
            executable: unix.then_some(mode & 0o111 != 0),
            method,
            compressed,
            crc,
            offset,
        });
    }
    Ok(entries)
}

/// Decompresses one entry, refusing output beyond its declared size.
pub fn extract(bytes: &[u8], entry: &Entry) -> Result<Vec<u8>> {
    if u32_at(bytes, entry.offset)? != LOCAL {
        return Err(invalid());
    }
    let name_length = usize::from(u16_at(bytes, entry.offset + 26)?);
    let extra_length = usize::from(u16_at(bytes, entry.offset + 28)?);
    let start = entry.offset + 30 + name_length + extra_length;
    let data = bytes
        .get(start..start + entry.compressed as usize)
        .ok_or_else(invalid)?;

    let mut output = Vec::with_capacity(entry.size.min(16 << 20) as usize);
    match entry.method {
        0 => output.extend_from_slice(data),
        8 => {
            DeflateDecoder::new(data)
                .take(entry.size + 1)
                .read_to_end(&mut output)
                .map_err(|_| invalid())?;
        }
        _ => {
            return Err(Error::bad(format!(
                "Unsupported compression in archive entry {}.",
                entry.path
            )));
        }
    }
    if output.len() as u64 != entry.size {
        return Err(invalid());
    }
    let mut crc = Crc::new();
    crc.update(&output);
    if crc.sum() != entry.crc {
        return Err(Error::bad(format!(
            "Archive entry {} is corrupted.",
            entry.path
        )));
    }
    Ok(output)
}

/// A file to write into an archive.
pub struct File<'a> {
    pub path: &'a str,
    pub bytes: &'a [u8],
    pub executable: bool,
}

/// Builds an archive with Unix modes, so scripts stay executable when unpacked.
pub fn write(files: &[File]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut directory = Vec::new();
    for file in files {
        let mut crc = Crc::new();
        crc.update(file.bytes);
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(file.bytes)?;
        let deflated = encoder.finish()?;
        let (method, data) = if deflated.len() < file.bytes.len() {
            (8u16, deflated.as_slice())
        } else {
            (0u16, file.bytes)
        };
        let (size, compressed, offset) = (
            u32::try_from(file.bytes.len()),
            u32::try_from(data.len()),
            u32::try_from(output.len()),
        );
        let (Ok(size), Ok(compressed), Ok(offset)) = (size, compressed, offset) else {
            return Err(Error::too_large("This skill is too large to export."));
        };
        let name = file.path.as_bytes();
        let name_length = u16::try_from(name.len()).map_err(|_| invalid())?;
        let mode: u32 = if file.executable {
            0o100_755
        } else {
            0o100_644
        };

        // Fixed DOS timestamp (1980-01-01) keeps archives of one revision identical.
        let header = |signature: u32| {
            let mut header = signature.to_le_bytes().to_vec();
            if signature == CENTRAL {
                header.extend((UNIX << 8 | 20).to_le_bytes());
            }
            header.extend(20u16.to_le_bytes());
            header.extend(0x0800u16.to_le_bytes());
            header.extend(method.to_le_bytes());
            header.extend(0u16.to_le_bytes());
            header.extend(0x0021u16.to_le_bytes());
            header.extend(crc.sum().to_le_bytes());
            header.extend(compressed.to_le_bytes());
            header.extend(size.to_le_bytes());
            header.extend(name_length.to_le_bytes());
            header.extend(0u16.to_le_bytes());
            header
        };

        output.extend(header(LOCAL));
        output.extend_from_slice(name);
        output.extend_from_slice(data);

        directory.extend(header(CENTRAL));
        directory.extend(0u16.to_le_bytes());
        directory.extend(0u16.to_le_bytes());
        directory.extend(0u16.to_le_bytes());
        directory.extend((mode << 16).to_le_bytes());
        directory.extend(offset.to_le_bytes());
        directory.extend_from_slice(name);
    }

    let count = u16::try_from(files.len())
        .map_err(|_| Error::too_large("This skill has too many files to export."))?;
    let start = u32::try_from(output.len())
        .map_err(|_| Error::too_large("This skill is too large to export."))?;
    let length = directory.len() as u32;
    output.extend(directory);
    output.extend(END.to_le_bytes());
    output.extend(0u16.to_le_bytes());
    output.extend(0u16.to_le_bytes());
    output.extend(count.to_le_bytes());
    output.extend(count.to_le_bytes());
    output.extend(length.to_le_bytes());
    output.extend(start.to_le_bytes());
    output.extend(0u16.to_le_bytes());
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_modes_and_compression() {
        let text = "repeat ".repeat(500);
        let archive = write(&[
            File {
                path: "review/SKILL.md",
                bytes: text.as_bytes(),
                executable: false,
            },
            File {
                path: "review/scripts/check.sh",
                bytes: b"#!/bin/sh\n",
                executable: true,
            },
        ])
        .unwrap();
        assert!(archive.len() < text.len());

        let listed = entries(&archive).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].path, "review/SKILL.md");
        assert_eq!(listed[0].executable, Some(false));
        assert_eq!(listed[1].executable, Some(true));
        assert_eq!(extract(&archive, &listed[0]).unwrap(), text.as_bytes());
        assert_eq!(extract(&archive, &listed[1]).unwrap(), b"#!/bin/sh\n");
    }

    #[test]
    fn rejects_escaping_paths_and_corruption() {
        let archive = write(&[File {
            path: "../evil",
            bytes: b"x",
            executable: false,
        }])
        .unwrap();
        assert!(entries(&archive).is_err());

        let mut archive = write(&[File {
            path: "ok.txt",
            bytes: b"hello",
            executable: false,
        }])
        .unwrap();
        // The stored payload follows the 30-byte header and the 6-byte name.
        archive[36] ^= 0xff;
        let listed = entries(&archive).unwrap();
        assert!(extract(&archive, &listed[0]).is_err());
        assert!(entries(b"not a zip").is_err());
    }

    #[test]
    fn refuses_output_beyond_the_declared_size() {
        let mut archive = write(&[File {
            path: "bomb.txt",
            bytes: &[b'a'; 4096],
            executable: false,
        }])
        .unwrap();
        let central = archive
            .windows(4)
            .position(|window| window == CENTRAL.to_le_bytes())
            .unwrap();
        // Declare a smaller uncompressed size than the deflated stream produces.
        archive[central + 24..central + 28].copy_from_slice(&16u32.to_le_bytes());
        let listed = entries(&archive).unwrap();
        assert!(extract(&archive, &listed[0]).is_err());
    }
}
