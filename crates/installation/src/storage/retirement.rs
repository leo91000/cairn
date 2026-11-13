//! Durable idempotence for detached-environment deletion. Receipts live outside
//! the disk and survive new executions, controller restarts and later retirements.
use crate::{
    error::{Error, Result},
    file_lock::Guard,
    skills::{atomic_write, private_dir},
    validation::uuid,
};
use serde_json::Value;

pub(crate) const PROTOCOL: u64 = 2;
pub(crate) const MAX_ATTEMPTS: usize = 64;

use std::path::{Path, PathBuf};

pub(crate) struct Retirement {
    id: String,
    attempts: Vec<String>,
    receipt: PathBuf,
    _owner: Guard,
}

impl Retirement {
    /// Serialize retries before any side effect (including retained-VM eviction).
    /// The caller also holds its normal disk ownership during deletion/commit.
    pub(crate) async fn begin(state: &Path, run: &str, request: &Value) -> Result<Option<Self>> {
        let Some(id) = request.get("retirementId").filter(|id| !id.is_null()) else {
            if request["permanentlyDeleted"] == true {
                return Ok(None);
            }
            return Err(Error::conflict(
                "Disk deletion needs a retirement identity or permanent conversation deletion.",
            ));
        };
        let id = id
            .as_str()
            .ok_or_else(|| Error::bad("Invalid disk retirement identity."))?;
        uuid(id)?;
        uuid(run)?;
        let attempts = match request.get("attempts") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(values)) if values.len() <= MAX_ATTEMPTS => values
                .iter()
                .map(|value| {
                    let id = value
                        .as_str()
                        .ok_or_else(|| Error::bad("Invalid retired attempt identity."))?;
                    uuid(id)?;
                    Ok(id.to_owned())
                })
                .collect::<Result<Vec<_>>>()?,
            _ => return Err(Error::bad("Invalid retired attempt identities.")),
        };
        let root = state.join("disk-retirements").join(run);
        private_dir(&root).await?;
        let owner = crate::file_lock::exclusive(
            &root.join("lock"),
            "This conversation's disk retirement is still in progress.",
        )?;
        Ok(Some(Self {
            id: id.to_owned(),
            attempts,
            receipt: root.join(id),
            _owner: owner,
        }))
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn attempts(&self) -> &[String] {
        &self.attempts
    }

    pub(crate) async fn completed(&self) -> Result<bool> {
        match tokio::fs::read(&self.receipt).await {
            Ok(bytes) if bytes == self.id.as_bytes() => {
                self.sync().await?;
                Ok(true)
            }
            Ok(_) => Err(Error::conflict("Invalid disk retirement receipt.")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// No acknowledgement may escape until the receipt and its new ancestors
    /// are durable. Never prune receipts: an older detached request may still run.
    pub(crate) async fn commit(&self, disk: &Path) -> Result<()> {
        tokio::fs::File::open(disk).await?.sync_all().await?;
        atomic_write(&self.receipt, self.id.as_bytes()).await?;
        self.sync().await
    }

    async fn sync(&self) -> Result<()> {
        let root = self.receipt.parent().unwrap().to_owned();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            std::fs::File::open(&root)?.sync_all()?;
            std::fs::File::open(root.parent().unwrap())?.sync_all()?;
            std::fs::File::open(root.parent().unwrap().parent().unwrap())?.sync_all()
        })
        .await
        .map_err(Error::internal)??;
        Ok(())
    }
}

/// Captured node-qualified identities repair cleanup interrupted by older code,
/// which could erase its ownership files before erasing private output.
pub(crate) async fn seed_attempts(
    state: &Path,
    data: &Path,
    run: &str,
    attempts: &[String],
) -> Result<()> {
    if attempts.is_empty() {
        return Ok(());
    }
    for id in attempts {
        uuid(id)?;
        let owner = state.join(format!("{id}.run"));
        match tokio::fs::read_to_string(&owner).await {
            Ok(current) if current != run => {
                return Err(Error::conflict(
                    "Retired attempt belongs to another conversation.",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let paths = [
            data.join("runner-plans").join(format!("{id}.json")),
            state.join(format!("{id}.vm.json")),
            state.join("roots").join(format!("{id}.plan.json")),
        ];
        for path in &paths {
            match tokio::fs::read(path).await {
                Ok(bytes) => {
                    let value: Value = serde_json::from_slice(&bytes)?;
                    if value["runId"]
                        .as_str()
                        .is_some_and(|current| current != run)
                    {
                        return Err(Error::conflict(
                            "Retired attempt belongs to another conversation.",
                        ));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let artifacts = [
            owner.clone(),
            paths[0].clone(),
            paths[1].clone(),
            paths[2].clone(),
            state.join(format!("{id}.log")),
            state.join(format!("{id}.terminal")),
            state.join("roots").join(id),
        ];
        let mut present = false;
        for path in artifacts {
            match tokio::fs::symlink_metadata(path).await {
                Ok(_) => {
                    present = true;
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if present {
            atomic_write(&owner, run.as_bytes()).await?;
        }
    }
    tokio::fs::File::open(state).await?.sync_all().await?;
    Ok(())
}
