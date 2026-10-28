//! Nonpersonal resource consent, metadata and disconnect. Every delayed
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

    pub async fn start(mut self, s: &Service, input: &Value) -> Result<Value> {
        self.target.require_revision(input)?;
        let mut body = input.clone();
        if self.target.is_setup()
            && let Some(body) = body.as_object_mut()
        {
            body.remove("setupRevision");
        }
        self.configuration
            .credentials(s, &self.target, std::slice::from_ref(&self.resource), true)
            .await?;
        self.recheck(s).await?;
        let target = self.target;
        let connection = target.connection();
        let provider = provider(target.manifest(), &self.provider, &self.resource.collection)?;
        let caller = capabilities::Caller::for_management(self.configuration.management.as_ref());
        begin(
            s,
            target.package(),
            &connection,
            self.resource,
            provider,
            Initiator {
                caller,
                account: self.configuration.owner.account_id().to_owned(),
                owner_access: self.configuration.owner.access_id().to_owned(),
                actor: None,
                external: None,
                source_event: String::new(),
                target: Some(target.clone()),
            },
            &body,
        )
        .await
    }

    pub async fn finish(mut self, s: &Service, input: &Value) -> Result<Value> {
        self.configuration
            .credentials(s, &self.target, std::slice::from_ref(&self.resource), true)
            .await?;
        self.recheck(s).await?;
        let state = text(input, "state");
        if state.len() != 43 {
            return Err(expired());
        }
        let key = pending_key(state);
        let value = s.store.kv(&stored_key(&key)).await?.ok_or_else(expired)?;
        let pending: Pending = serde_json::from_value(s.vault.decrypt(&key, &value)?)?;
        let expected_setup = match (pending.setup.as_deref(), self.target.setup_snapshot()) {
            (None, None) => true,
            (Some(saved), Some(current)) => {
                saved.id == current.id && saved.revision == current.revision
            }
            _ => false,
        };
        if pending.account != self.configuration.owner.account_id()
            || pending.owner_access != self.configuration.owner.access_id()
            || pending.instance != self.target.instance()
            || pending.instance_revision != self.target.connection().revision
            || pending.resource != self.resource.id
            || pending.resource_revision != self.resource.revision
            || pending.provider != self.provider
            || pending.actor.is_some()
            || pending.external.is_some()
            || !expected_setup
        {
            return Err(Error::forbidden(
                "This authorization belongs to another owner or resource.",
            ));
        }
        let provider = provider(
            self.target.manifest(),
            &self.provider,
            &self.resource.collection,
        )?;
        let caller = capabilities::Caller::for_management(self.configuration.management.as_ref());
        finish_pending(
            s,
            self.target.package(),
            provider,
            key,
            value,
            pending,
            caller,
        )
        .await
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
                    self.target.setup_snapshot(),
                ))
            })
            .await
    }

    pub async fn revoke(mut self, s: &Service, input: &Value) -> Result<Value> {
        self.target.require_revision(input)?;
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
                self.target.put(db, &resource)?;
                let key = authorization_key(&self.resource.id, &self.provider, None);
                db.delete(&stored_key(&key))
            })
            .await?;
        Ok(json!({"status": "disconnected"}))
    }
}
