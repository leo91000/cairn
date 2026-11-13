//! Only the replacement copy is outside the task allowance. Node accounting
//! always includes it. Paths and their bounded credit change with the base swap.
use super::{Disk, LazyDisk, failure};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        RwLock, RwLockWriteGuard,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Default)]
struct State {
    paths: HashMap<PathBuf, (u64, u64)>,
    retained: HashSet<PathBuf>,
    limit: u64,
    logical_bytes: u64,
    logical_limit: u64,
}

#[derive(Default)]
pub(super) struct Scratch {
    state: RwLock<State>,
    revision: AtomicU64,
}

pub(super) struct Commit<'a> {
    state: RwLockWriteGuard<'a, State>,
    revision: &'a AtomicU64,
    copied: u64,
}

impl Scratch {
    pub(super) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    pub(super) fn track_block(
        &self,
        name: &OsStr,
        file: &std::fs::File,
        bytes: u64,
    ) -> io::Result<()> {
        let metadata = file.metadata()?;
        let mut state = self.state.write().map_err(failure)?;
        let logical = state
            .logical_bytes
            .checked_add(bytes)
            .filter(|bytes| *bytes <= state.logical_limit)
            .ok_or_else(|| failure("Consolidation copy exceeds the existing disk"))?;
        state.logical_bytes = logical;
        state.paths.insert(
            Path::new("cache").join(name),
            (metadata.dev(), metadata.ino()),
        );
        Ok(())
    }

    pub(super) fn persist(
        &self,
        directory: &Path,
        file: tempfile::NamedTempFile,
        hash: &str,
    ) -> io::Result<()> {
        let mut state = self.state.write().map_err(failure)?;
        let temporary = Path::new("cache").join(file.path().file_name().unwrap());
        let metadata = file.as_file().metadata()?;
        file.persist(directory.join("cache").join(hash))
            .map_err(failure)?;
        state.paths.remove(&temporary);
        let target = Path::new("cache").join(hash);
        if !state.retained.contains(&target) {
            state.paths.insert(target, (metadata.dev(), metadata.ino()));
        }
        Ok(())
    }

    pub(super) fn commit(&self, directory: &Path) -> io::Result<Commit<'_>> {
        let state = self.state.write().map_err(failure)?;
        let mut copied = 0u64;
        for path in state.paths.keys() {
            match std::fs::symlink_metadata(directory.join(path)) {
                Ok(metadata) if metadata.is_file() => {
                    copied = copied.saturating_add(metadata.blocks().saturating_mul(512));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let copied = copied.min(state.limit);
        Ok(Commit {
            state,
            revision: &self.revision,
            copied,
        })
    }
}

impl Commit<'_> {
    /// The new base counts immediately. Only its still-present replaced files
    /// retain a credit, never larger than the physical replacement copy.
    pub(super) fn retire(&mut self, directory: &Path, paths: impl IntoIterator<Item = PathBuf>) {
        self.state.paths = paths
            .into_iter()
            .filter_map(|path| {
                let metadata = std::fs::symlink_metadata(directory.join(&path)).ok()?;
                metadata
                    .is_file()
                    .then(|| (path, (metadata.dev(), metadata.ino())))
            })
            .collect();
        self.state.limit = self.copied;
        self.state.retained.clear();
        self.revision.fetch_add(1, Ordering::Release);
    }
}

impl LazyDisk {
    pub(crate) fn backing_revision(&self) -> u64 {
        self.scratch.revision()
    }

    pub(crate) fn retained_backing(&self, environment: &Path) -> io::Result<(u64, u64)> {
        let scratch = self.scratch.state.read().map_err(failure)?;
        let revision = self.scratch.revision();
        let mut remaining = scratch.limit;
        let bytes = crate::microvm::budget::allocated_with(environment, &mut |path, metadata| {
            let bytes = metadata.blocks().saturating_mul(512);
            let internal = path.strip_prefix(&self.directory).is_ok_and(|relative| {
                scratch.paths.get(relative) == Some(&(metadata.dev(), metadata.ino()))
            });
            let excluded = if internal { remaining.min(bytes) } else { 0 };
            remaining -= excluded;
            bytes - excluded
        })?;
        Ok((bytes, revision))
    }

    /// A failed attempt cannot accumulate another copy on its next retry.
    /// Keep captured legacy snapshots; their files then count as retained data.
    pub(super) fn prepare_consolidation_scratch(&self) -> io::Result<()> {
        let _publication = self.publication.write().map_err(failure)?;
        let base = self.base.lock().map_err(failure)?.clone();
        let captured = {
            let db = self.db.lock().map_err(failure)?;
            let mut query = db
                .prepare("SELECT manifest FROM sealed WHERE manifest IS NOT NULL")
                .map_err(failure)?;
            query
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(failure)?
                .map(|row| serde_json::from_str::<Value>(&row.map_err(failure)?).map_err(failure))
                .collect::<io::Result<Vec<_>>>()?
        };
        let needed: HashSet<_> = std::iter::once(base.as_ref())
            .chain(&captured)
            .flat_map(|manifest| manifest["blocks"].as_array().unwrap())
            .filter_map(|block| block["hash"].as_str())
            .map(|hash| Path::new("cache").join(hash))
            .collect();
        let _admission = super::super::cache::admission()?;
        let mut scratch = self.scratch.state.write().map_err(failure)?;
        for (path, identity) in &scratch.paths {
            if needed.contains(path) {
                continue;
            }
            let target = self.directory.join(path);
            match std::fs::symlink_metadata(&target) {
                Ok(metadata) if (metadata.dev(), metadata.ino()) != *identity => continue,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
            match std::fs::remove_file(target) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        scratch.paths.clear();
        scratch.retained = needed;
        // Logical copied data is bounded by the existing geometry. Account its
        // actual file allocations (including extent metadata) to the node.
        scratch.logical_limit = self.size();
        scratch.logical_bytes = 0;
        scratch.limit = u64::MAX;
        self.scratch.revision.fetch_add(1, Ordering::Release);
        Ok(())
    }

    pub(super) fn retire_consolidation_blocks(
        &self,
        needed: &HashSet<&str>,
        reclaimed: bool,
    ) -> io::Result<u64> {
        let admission = super::super::cache::admission()?;
        let mut scratch = self.scratch.state.write().map_err(failure)?;
        let removed = super::super::cache::retire_admitted(
            &self.directory.join("cache"),
            needed,
            &admission,
        )?;
        if reclaimed {
            scratch.paths.clear();
            scratch.limit = 0;
            self.scratch.revision.fetch_add(1, Ordering::Release);
        }
        Ok(removed)
    }
}
