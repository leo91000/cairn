//! Durable rejection of acknowledgements captured before local disks. The old
//! ledger format remains readable across controller updates; no publisher runs.
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    pending: BTreeMap<String, String>,
    rejected: BTreeSet<String>,
}

struct Fences {
    file: PathBuf,
    state: Mutex<State>,
}

fn retained(state: &Path) -> Result<Arc<Fences>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<PathBuf, Weak<Fences>>>> = OnceLock::new();
    let directory = state.join("publication-fences");
    let mut registry = REGISTRY
        .get_or_init(Mutex::default)
        .lock()
        .map_err(Error::internal)?;
    if let Some(fences) = registry.get(&directory).and_then(Weak::upgrade) {
        return Ok(fences);
    }
    std::fs::create_dir_all(&directory)?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    File::open(state)?.sync_all()?;
    let file = directory.join("direct-read-failures.json");
    let persisted = match File::open(&file) {
        Ok(input) => {
            let mut bytes = Vec::new();
            input.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(Error::conflict("Legacy journal fence ledger is too large."));
            }
            let persisted: State = serde_json::from_slice(&bytes)?;
            for (object, hash) in &persisted.pending {
                crate::validation::uuid(object)?;
                if !crate::nodes::snapshots::valid_hash(hash) {
                    return Err(Error::bad("Invalid retained integrity report."));
                }
            }
            for publication in &persisted.rejected {
                crate::validation::uuid(publication)?;
            }
            persisted
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => State::default(),
        Err(error) => return Err(error.into()),
    };
    let fences = Arc::new(Fences {
        file,
        state: Mutex::new(persisted),
    });
    registry.retain(|_, entry| entry.strong_count() > 0);
    registry.insert(directory, Arc::downgrade(&fences));
    Ok(fences)
}

impl Fences {
    fn fence(&self, publication: &str) -> Result<()> {
        let mut state = self.state.lock().map_err(Error::internal)?;
        state.rejected.insert(publication.to_owned());
        let parent = self.file.parent().unwrap();
        let temporary = parent.join(format!(".{}.tmp", crate::config::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(&*state)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(temporary, &self.file)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}

pub(crate) async fn fence(state: &Path, run: &str, publication: &str) -> Result<()> {
    crate::validation::uuid(run)?;
    crate::validation::uuid(publication)?;
    let state = state.to_owned();
    let publication = publication.to_owned();
    tokio::task::spawn_blocking(move || retained(&state)?.fence(&publication))
        .await
        .map_err(Error::internal)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn durable_fences_preserve_old_report_state_across_restart() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("publication-fences");
        std::fs::create_dir(&directory).unwrap();
        let object = crate::config::id();
        let previous = crate::config::id();
        let publication = crate::config::id();
        let hash = format!("b3-{}", "a".repeat(64));
        let file = directory.join("direct-read-failures.json");
        let pending = BTreeMap::from([(object, hash)]);
        std::fs::write(
            &file,
            serde_json::to_vec(&json!({
                "pending": pending,
                "rejected": [previous]
            }))
            .unwrap(),
        )
        .unwrap();

        fence(root.path(), &crate::config::id(), &publication)
            .await
            .unwrap();
        let reloaded = retained(root.path()).unwrap();
        let state = reloaded.state.lock().unwrap();
        assert_eq!(state.pending, pending);
        assert_eq!(state.rejected, BTreeSet::from([previous, publication]));
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
