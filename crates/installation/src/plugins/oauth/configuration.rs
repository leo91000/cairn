//! Nonpersonal resource authorization metadata and disconnect. Every delayed
//! operation retains configuration authority and one exact target/resource snapshot.
use super::*;
use crate::plugins::resources::{configuration::Configuration, target::Target};

pub(in crate::plugins) struct ConfiguredAuthorization {
    target: Arc<Target>,
    resource: resources::Resource,
    provider: String,
    configuration: Configuration,
}

impl ConfiguredAuthorization {
    pub async fn select(
        s: &Service,
        target: Arc<Target>,
        collection: &str,
        resource: &str,
        name: &str,
        configuration: Configuration,
    ) -> Result<Self> {
        if target.is_setup() {
            return Err(Error::bad(
                "Select an active instance for resource authorization management.",
            ));
        }
        if provider(target.manifest(), name, collection)?
            .personal
            .is_some()
        {
            return Err(Error::forbidden(
                "Personal authorization belongs to its requester. Manage it from that requester's account.",
            ));
        }
        configuration.recheck(s).await?;
        let resource = resources::get_for(s, target.clone(), collection, resource).await?;
        Ok(Self {
            target,
            resource,
            provider: name.to_owned(),
            configuration,
        })
    }

    fn current(&self, db: &Db<'_>) -> Result<Value> {
        self.configuration.current(db)?;
        self.target.current(db)?;
        let resource = self
            .target
            .get(db, &self.resource.id)?
            .ok_or_else(expired)?;
        if resource["revision"] != self.resource.revision
            || resource["connectionId"] != self.resource.connection_id
            || resource["collection"] != self.resource.collection
        {
            return Err(expired());
        }
        Ok(resource)
    }

    async fn recheck(&self, s: &Service) -> Result<()> {
        self.configuration.recheck(s).await?;
        self.target.recheck_package(s).await
    }

    pub async fn status(self, s: &Service) -> Result<Value> {
        self.recheck(s).await?;
        let vault = s.vault.clone();
        s.store
            .read(move |db| {
                self.current(db)?;
                let key = authorization_key(&self.resource.id, &self.provider, None);
                let Some(saved) = vault.get_in(db, &key)? else {
                    return Ok(json!({"status": "disconnected"}));
                };
                let saved: Authorization = serde_json::from_value(saved)?;
                Ok(status_summary(
                    &saved,
                    &self.target.connection(),
                    &self.resource,
                    &self.target.package().digest,
                    Some(self.configuration.owner.access_id()),
                    None,
                ))
            })
            .await
    }

    pub async fn revoke(mut self, s: &Service) -> Result<Value> {
        // Disconnecting a private authorization needs the existing instance grant.
        // Installation management cannot manufacture that grant for its own agent.
        self.configuration
            .credentials(s, &self.target, std::slice::from_ref(&self.resource), true)
            .await?;
        self.recheck(s).await?;
        s.store
            .transaction(move |db| {
                let mut resource = self.current(db)?;
                // Retire an in-flight consent even if its pending state was consumed.
                resource["revision"] = json!(id());
                resource["updatedAt"] = json!(now());
                db.put(resources::KIND, &resource)?;
                let key = authorization_key(&self.resource.id, &self.provider, None);
                db.delete(&stored_key(&key))
            })
            .await?;
        Ok(json!({"status": "disconnected"}))
    }
}
