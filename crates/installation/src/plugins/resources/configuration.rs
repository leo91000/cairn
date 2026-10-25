//! Configuration keeps owner lifetime and delegated management separate. Reusing
//! saved credentials requires the existing instance grant, never installation access alone.
use super::{Resource, target::Target};
use crate::{
    error::Result,
    plugins::{connections, management, operations::Owner},
    service::Service,
    store::Db,
};
use std::sync::Arc;

#[derive(Clone)]
pub(in crate::plugins) struct Configuration {
    pub owner: Arc<Owner>,
    pub management: Option<management::Authority>,
}

impl Configuration {
    pub fn owner(owner: Arc<Owner>) -> Self {
        Self {
            owner,
            management: None,
        }
    }

    pub async fn managed(s: &Service, management: Option<management::Authority>) -> Result<Self> {
        let configuration = Self {
            owner: Arc::new(Owner::current(s).await?),
            management,
        };
        configuration.recheck(s).await?;
        Ok(configuration)
    }

    pub fn current(&self, db: &Db<'_>) -> Result<()> {
        if let Some(authority) = &self.management {
            authority.authorize(db)?;
        }
        Ok(())
    }

    pub async fn recheck(&self, s: &Service) -> Result<()> {
        self.owner.recheck(s).await?;
        if let Some(authority) = &self.management {
            authority.recheck(s).await?;
        }
        Ok(())
    }

    /// Bind the grant before decrypting any saved value. A resource operation
    /// grant is deliberately not a grant to arbitrary configuration credentials.
    pub async fn credentials(
        &mut self,
        s: &Service,
        target: &Arc<Target>,
        resources: &[Resource],
        include_root: bool,
    ) -> Result<()> {
        let Some(authority) = &mut self.management else {
            return Ok(());
        };
        let bound = target.clone();
        let expected = authority.clone();
        let root_private = s
            .store
            .read(move |db| {
                expected.authorize(db)?;
                bound.current(db)?;
                let connection = bound.connection();
                Ok(include_root
                    && (!connection.secret_keys.is_empty()
                        || db
                            .kv(&format!(
                                "mcp-secret:{}",
                                connections::credential_key(&connection.id)
                            ))?
                            .is_some()))
            })
            .await?;
        // An OAuth authorization can be saved while a form is awaiting I/O.
        // Bind its potential credential scope now, even before consent exists.
        // Personal OAuth remains unavailable to configuration forms.
        let resource_oauth = include_root
            && resources.iter().any(|resource| {
                target.manifest().oauth.iter().any(|provider| {
                    provider.collection == resource.collection && provider.personal.is_none()
                })
            });
        let private = root_private
            || resource_oauth
            || resources
                .iter()
                .any(|resource| !resource.secret_keys.is_empty());
        authority.configuration_credentials(target, private, include_root)?;
        authority.recheck(s).await
    }
}
