//! Reserve announced staging files under the pool gate, then receive without it.
use crate::{
    error::{Error, Result},
    nodes::files::FileAdmission,
};
use std::{
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(crate) struct Storage {
    pub state: PathBuf,
    pub data: PathBuf,
    pub client: reqwest::Client,
    pub origin: String,
}

impl Storage {
    pub fn local() -> Result<Self> {
        Ok(Self {
            state: super::state_dir(),
            data: crate::nodes::node_data_dir(),
            client: crate::nodes::local_client()?,
            origin: "http://localhost".into(),
        })
    }
}

pub(crate) struct Admission {
    pub storage: Storage,
    pub keep: Vec<String>,
    /// Inbox replacement briefly holds a second copy. This sparse, private
    /// file charges that promise to the same logical staging pool.
    pub replacement: Option<std::fs::File>,
}

impl FileAdmission for Admission {
    async fn open(&mut self, target: &Path, size: u64, mode: u32) -> Result<tokio::fs::File> {
        let owner = super::disk::lock_staging(&self.storage.state).await?;
        let required = size
            .checked_mul(if self.replacement.is_some() { 2 } else { 1 })
            .ok_or_else(|| Error::bad("Workspace exceeds node disk budget."))?;
        let available =
            super::disk::staging_budget_from(&self.storage.client, &self.storage.origin).await?;
        if required > available {
            crate::project_archives::reclaim_for_foreground(
                &self.storage.data,
                required - available,
                &self.keep,
            )
            .await?;
            let available =
                super::disk::staging_budget_from(&self.storage.client, &self.storage.origin)
                    .await?;
            if required > available {
                return Err(Error::bad("Workspace exceeds node disk budget."));
            }
        }

        let target = target.to_owned();
        let replacement = self
            .replacement
            .as_ref()
            .map(std::fs::File::try_clone)
            .transpose()?;
        // Keep the gate with the actual metadata mutation if its caller cancels.
        // Logical lengths are charged even for sparse files, before any body IO.
        let file = tokio::task::spawn_blocking(move || -> Result<std::fs::File> {
            let _owner = owner;
            let file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(mode)
                .custom_flags(libc::O_NOFOLLOW)
                .open(target)?;
            file.set_len(size)?;
            if let Some(replacement) = replacement {
                let promised = replacement
                    .metadata()?
                    .len()
                    .checked_add(size)
                    .ok_or_else(|| Error::bad("Workspace exceeds node disk budget."))?;
                replacement.set_len(promised)?;
            }
            Ok(file)
        })
        .await
        .map_err(Error::internal)??;
        Ok(tokio::fs::File::from_std(file))
    }
}
