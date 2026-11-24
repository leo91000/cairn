//! Root-private cache of immutable skill seeds. Attempt-specific configuration
//! and credentials always use the existing authenticated transfer.
use crate::{error::Result, nodes::files};
use std::{
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use tokio::io::BufReader;

pub(super) const KEY: &str = "x-cairn-seed-skills-key";
pub(super) const HIT: &str = "x-cairn-seed-skills-hit";
pub(super) const NEGOTIATE: &str = "nativeSkillCache";
const MAX_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const CACHE_WRITE_MARGIN: u64 = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 1024;
const MAX_CACHE_ENTRIES: usize = 8;

pub(super) fn directory() -> PathBuf {
    crate::nodes::node_data_dir().join("native-seed-skills")
}

pub(super) fn valid(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub(super) fn offered(cache: &Path) -> Vec<String> {
    std::fs::read_dir(cache)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            (valid(&name) && entry.file_type().ok()?.is_file()).then_some(name)
        })
        .take(MAX_CACHE_ENTRIES)
        .collect()
}

/// Count file names, modes and exact contents in deterministic traversal order.
/// Unusual entries and larger bundles continue to use the streaming protocol.
pub(super) fn key(home: &Path) -> Option<String> {
    fn real_directory(path: &Path) -> Option<()> {
        std::fs::symlink_metadata(path).ok()?.is_dir().then_some(())
    }

    real_directory(home)?;
    let parent = home.join(".agents");
    real_directory(&parent)?;
    let root = parent.join("skills");
    real_directory(&root)?;
    let mut hash = blake3::Hasher::new();
    let mut pending = vec![root.clone()];
    let (mut entries, mut bytes) = (0, 0u64);
    while let Some(path) = pending.pop() {
        entries += 1;
        if entries > MAX_ENTRIES {
            return None;
        }
        let name = path
            .strip_prefix(&root)
            .ok()?
            .as_os_str()
            .as_encoded_bytes();
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name);
        let mode = if name.is_empty() {
            0o700
        } else {
            metadata.mode() & 0o777
        };
        hash.update(&mode.to_le_bytes());
        if metadata.is_dir() {
            let mut children = Vec::new();
            for child in std::fs::read_dir(path).ok()? {
                if children.len() + pending.len() >= MAX_ENTRIES {
                    return None;
                }
                children.push(child.ok()?.path());
            }
            children.sort();
            pending.extend(children.into_iter().rev());
        } else if metadata.is_file() {
            bytes = bytes.checked_add(metadata.len())?;
            if bytes > MAX_BYTES {
                return None;
            }
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                .ok()?;
            let mut contents = Vec::new();
            file.take(metadata.len() + 1)
                .read_to_end(&mut contents)
                .ok()?;
            if contents.len() as u64 != metadata.len() {
                return None;
            }
            hash.update(&metadata.len().to_le_bytes());
            hash.update(&contents);
        } else {
            return None;
        }
    }
    (entries > 1).then(|| hash.finalize().to_hex().to_string())
}

pub(super) async fn install(
    cache: &Path,
    digest: &str,
    home: &Path,
    admit: &mut (impl FnMut(u64) -> Result<()> + Send),
) -> Result<()> {
    let file = tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(cache.join(digest))
        .await?;
    for path in [home.to_owned(), home.join(".agents")] {
        if !tokio::fs::symlink_metadata(path).await?.is_dir() {
            return Err(crate::error::Error::bad(
                "Native skill seed parent is not a directory.",
            ));
        }
    }
    let target = home.join(".agents/skills");
    if tokio::fs::symlink_metadata(&target).await.is_ok() {
        return Err(crate::error::Error::bad(
            "Native skill seed was sent twice.",
        ));
    }
    files::receive_admitted(
        &mut BufReader::new(file),
        &target,
        MAX_BYTES,
        MAX_BYTES,
        |_| std::future::ready(Ok(0)),
        files::Durability::StagedTree,
        admit,
    )
    .await?;
    if key(home).as_deref() != Some(digest) {
        return Err(crate::error::Error::conflict(
            "Cached native skills do not match the authorized seed.",
        ));
    }
    Ok(())
}

/// The shared disk guard serializes cache publications with staging admission.
pub(super) async fn remember(
    cache: &Path,
    digest: &str,
    home: &Path,
    admit: &mut (impl FnMut(u64) -> Result<()> + Send),
) -> Result<()> {
    if cache.join(digest).is_file() || key(home).as_deref() != Some(digest) {
        return Ok(());
    }
    let mut bytes = Vec::new();
    files::send(&home.join(".agents/skills"), &mut bytes).await?;
    let staging = cache
        .parent()
        .ok_or_else(|| crate::error::Error::bad("Missing seed cache parent."))?;
    match std::fs::symlink_metadata(cache) {
        Ok(metadata)
            if !metadata.is_dir() || metadata.dev() != std::fs::metadata(staging)?.dev() =>
        {
            return Err(crate::error::Error::conflict(
                "Native seed cache must share the staging filesystem.",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    admit((bytes.len() as u64).saturating_add(10_240))?;
    crate::skills::private_dir(cache).await?;
    let mut candidates = std::fs::read_dir(cache)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            if !valid(&name) || !entry.file_type().ok()?.is_file() {
                return None;
            }
            Some((entry.metadata().ok()?.modified().ok()?, entry.path()))
        })
        .collect::<Vec<_>>();
    candidates.sort();
    let evict = candidates
        .len()
        .saturating_add(1)
        .saturating_sub(MAX_CACHE_ENTRIES);
    for (_, path) in candidates.into_iter().take(evict) {
        std::fs::remove_file(path)?;
    }
    // The enclosing seed barrier covers the staging filesystem. A torn cache
    // is rejected by digest on reuse and rebuilt from the next base stream.
    let mut file = tempfile::NamedTempFile::new_in(cache)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(cache.join(digest))
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn only_the_authorized_skill_tree_is_reused() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let target = fixture.path().join("target");
        let cache = fixture.path().join("cache");
        let skills = source.join(".agents/skills");
        std::fs::create_dir_all(skills.join("sample")).unwrap();
        std::fs::create_dir_all(target.join(".agents")).unwrap();
        std::fs::write(
            source.join("generated-config"),
            b"private per-attempt configuration",
        )
        .unwrap();
        let file = skills.join("sample/SKILL.md");
        std::fs::write(&file, b"shared skill").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
        let digest = key(&source).unwrap();
        remember(&cache, &digest, &source, &mut |_| Ok(()))
            .await
            .unwrap();
        assert_eq!(offered(&cache), vec![digest.clone()]);
        install(&cache, &digest, &target, &mut |_| Ok(()))
            .await
            .unwrap();
        assert_eq!(key(&target).as_deref(), Some(digest.as_str()));
        assert!(!target.join("generated-config").exists());

        std::fs::write(&file, b"new skill version").unwrap();
        assert_ne!(key(&source).as_deref(), Some(digest.as_str()));
        std::fs::remove_dir_all(target.join(".agents/skills")).unwrap();
        let mut wrong_tree = Vec::new();
        files::send(&skills, &mut wrong_tree).await.unwrap();
        std::fs::write(cache.join(&digest), wrong_tree).unwrap();
        assert!(
            install(&cache, &digest, &target, &mut |_| Ok(()))
                .await
                .unwrap_err()
                .is_conflict()
        );
    }
}
