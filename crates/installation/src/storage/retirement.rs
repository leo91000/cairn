//! Durable idempotence for detached-environment deletion. Receipts live outside
//! the disk and survive new executions, controller restarts and later retirements.
use crate::{
    error::{Error, Result},
    file_lock::Guard,
    skills::{atomic_write, private_dir},
    validation::uuid,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(crate) struct Retirement {
    id: String,
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
        let root = state.join("disk-retirements").join(run);
        private_dir(&root).await?;
        let owner = crate::file_lock::exclusive(
            &root.join("lock"),
            "This conversation's disk retirement is still in progress.",
        )?;
        Ok(Some(Self {
            id: id.to_owned(),
            receipt: root.join(id),
            _owner: owner,
        }))
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
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
