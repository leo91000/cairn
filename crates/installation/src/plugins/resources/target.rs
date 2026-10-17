//! Explicit configuration targets. Draft records never appear in the live resource
//! namespace, and operational callers can construct only a live connection target.
use super::super::{
    connections::{self, Connection},
    registry,
    setup::SetupDraft,
};
use super::{KIND, current};
use crate::{
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use cairn_plugin_api::Manifest;
use serde_json::Value;
use std::sync::Arc;

pub(super) enum Target {
    Active {
        connection: Box<Connection>,
        approved: registry::ApprovedPackage,
    },
    Setup {
        draft: Box<SetupDraft>,
        approved: registry::ApprovedPackage,
    },
}

impl Target {
    pub(super) async fn active(s: &Service, plugin: &str, instance: &str) -> Result<Arc<Self>> {
        Ok(Arc::new(Self::Active {
            connection: Box::new(connections::get(s, plugin, instance).await?),
            approved: registry::approved(s, plugin).await?,
        }))
    }

    pub(super) async fn setup(
        s: &Service,
        plugin: &str,
        draft: &str,
        account: &str,
    ) -> Result<Arc<Self>> {
        let (draft, approved) =
            super::super::setup::configuration(s, plugin, draft, account).await?;
        Ok(Arc::new(Self::Setup {
            draft: Box::new(draft),
            approved,
        }))
    }

    pub(super) fn instance(&self) -> &str {
        match self {
            Self::Active { connection, .. } => &connection.id,
            Self::Setup { draft, .. } => &draft.id,
        }
    }

    pub(super) fn manifest(&self) -> &Manifest {
        match self {
            Self::Active { approved, .. } | Self::Setup { approved, .. } => {
                &approved.package.manifest
            }
        }
    }

    pub(super) fn is_setup(&self) -> bool {
        matches!(self, Self::Setup { .. })
    }

    pub(super) fn expires_at(&self) -> Option<i64> {
        match self {
            Self::Active { .. } => None,
            Self::Setup { draft, .. } => Some(draft.expires_at),
        }
    }

    pub(super) fn current(&self, db: &Db<'_>) -> Result<()> {
        match self {
            Self::Active {
                connection,
                approved,
            } => {
                approved.recheck_in(db)?;
                current(db, connection)
            }
            Self::Setup { draft, approved } => {
                approved.recheck_in(db)?;
                draft.recheck_in(db)
            }
        }
    }

    fn prefix(&self) -> String {
        setup_prefix(self.instance())
    }

    pub(super) fn get(&self, db: &Db<'_>, resource: &str) -> Result<Option<Value>> {
        match self {
            Self::Active { .. } => db.get(KIND, resource),
            Self::Setup { .. } => db.kv(&format!("{}{resource}", self.prefix())),
        }
    }

    pub(super) fn list(&self, db: &Db<'_>) -> Result<Vec<Value>> {
        Ok(match self {
            Self::Active { .. } => db
                .list(KIND)?
                .into_iter()
                .filter(|value| value["connectionId"] == self.instance())
                .collect(),
            Self::Setup { .. } => db
                .keys(&self.prefix())?
                .into_iter()
                .map(|(_, value)| value)
                .collect(),
        })
    }

    pub(super) fn put(&self, db: &Db<'_>, value: &Value) -> Result<Value> {
        match self {
            Self::Active { .. } => db.put(KIND, value),
            Self::Setup { .. } => {
                db.set(
                    &format!("{}{}", self.prefix(), text(value, "id")),
                    value,
                    self.expires_at(),
                )?;
                Ok(value.clone())
            }
        }
    }

    pub(super) fn remove(&self, db: &Db<'_>, resource: &str) -> Result<()> {
        match self {
            Self::Active { .. } => db.remove(KIND, resource),
            Self::Setup { .. } => db.delete(&format!("{}{resource}", self.prefix())),
        }
    }

    pub(super) fn require_revision(&self, input: &Value) -> Result<()> {
        if let Self::Setup { draft, .. } = self
            && input["setupRevision"] != draft.revision
        {
            return Err(Error::conflict("Setup changed. Reload its resources."));
        }
        Ok(())
    }
}

pub(super) fn setup_prefix(instance: &str) -> String {
    format!("plugin-setup-resource:{instance}:")
}
