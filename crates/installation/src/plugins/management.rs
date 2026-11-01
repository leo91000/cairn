//! One typed command for HTTP, public MCP and installation agents. Authority is
//! supplied by the adapter, never by an input or manifest value.
use super::{connections, device_flow, forms, oauth, panels, registry};
use crate::{
    error::{Error, Result},
    service::Service,
    validation::text,
};
use cairn_contracts::{ManagePluginInput, ManagePluginOutput};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub struct Authority {
    bearer: Option<String>,
    preparation: Option<crate::capabilities::plugin_packages::Authority>,
    instance: Option<Box<connections::Connection>>,
    shared: Option<Box<connections::Connection>>,
    snapshot: Option<Arc<super::grant_authorization::Snapshot>>,
    invocation: Option<Arc<super::grant_authorization::Invocation>>,
    partial: bool,
}

impl Authority {
    /// The host's fetch of a run-authorized project does not grant credentials
    /// to the agent. Retain execution, package and instance revocation checks.
    pub(crate) fn host_fetch(
        preparation: crate::capabilities::plugin_packages::Authority,
        instance: connections::Connection,
        package: &registry::Package,
    ) -> Self {
        let expected = instance.clone();
        let package = package.clone();
        let preparation = preparation.with_guard(move |db| {
            connections::check_revision(db, &expected)?;
            if !expected.enabled {
                return Err(Error::forbidden(
                    "The fetch credential instance is disabled.",
                ));
            }
            if !package.builtin {
                let current = db.plugin_package(&package.manifest.id)?;
                if current.active.as_deref() != Some(&package.manifest.version)
                    || current.approvals.get(&package.manifest.version) != Some(&package.digest)
                {
                    return Err(Error::forbidden("The fetch credential package changed."));
                }
            }
            Ok(())
        });
        Self {
            bearer: None,
            preparation: Some(preparation),
            instance: Some(Box::new(instance)),
            shared: None,
            snapshot: None,
            invocation: None,
            partial: false,
        }
    }

    pub(crate) fn resource_read(
        preparation: crate::capabilities::plugin_packages::Authority,
        instance: connections::Connection,
    ) -> Self {
        Self {
            bearer: preparation.bearer.clone(),
            preparation: Some(preparation),
            instance: Some(Box::new(instance)),
            shared: None,
            snapshot: None,
            invocation: None,
            partial: true,
        }
    }

    pub(crate) fn snapshot(&self) -> Option<super::grant_authorization::Snapshot> {
        self.snapshot.as_deref().cloned()
    }

    pub(crate) fn transfer_origin(
        &self,
    ) -> Option<Arc<super::grant_authorization::TransferOrigin>> {
        self.invocation
            .as_ref()
            .map(|invocation| invocation.transfer_origin())
    }

    pub(crate) fn remember_transfer(
        &self,
        origin: Arc<super::grant_authorization::TransferOrigin>,
    ) -> Result<()> {
        if let Some(invocation) = &self.invocation {
            invocation.remember_transfer(origin)?;
        }
        Ok(())
    }

    pub(crate) fn bind_snapshot(mut self, snapshot: super::grant_authorization::Snapshot) -> Self {
        self.snapshot = Some(Arc::new(snapshot));
        self
    }

    pub(crate) fn with_preparation(
        mut self,
        preparation: Option<crate::capabilities::plugin_packages::Authority>,
    ) -> Self {
        self.preparation = preparation;
        self
    }

    pub(crate) fn authorize(&self, db: &crate::store::Db<'_>) -> Result<()> {
        if let Some(preparation) = &self.preparation {
            preparation.authorize(db)?;
        }
        if let Some(snapshot) = &self.snapshot {
            snapshot.authorize(db)?;
        }
        let run = self
            .bearer
            .as_ref()
            .map(|bearer| crate::installation_mcp::authorize_in(db, bearer))
            .transpose()?;
        let agent = match (&self.snapshot, &run) {
            (Some(snapshot), _) => snapshot.agent.as_str(),
            (_, Some(run)) => text(&run["snapshot"]["agent"], "id"),
            _ => return Ok(()),
        };
        for expected in [&self.instance, &self.shared].into_iter().flatten() {
            let current = db
                .plugin_instance(&expected.id)?
                .ok_or_else(|| Error::forbidden("This plugin instance was removed."))?;
            let granted = if self.partial {
                current.enabled && db.has_plugin_grant(agent, &current.id)?
            } else {
                current.grants(agent)
            };
            if current.revision != expected.revision || !granted {
                return Err(Error::forbidden(
                    "Installation management does not grant access to this plugin account. Ask the owner to grant it on the agent's page.",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn agent_action(
        instance: connections::Connection,
        snapshot: super::grant_authorization::Snapshot,
        invocation: Option<Arc<super::grant_authorization::Invocation>>,
    ) -> Self {
        Self {
            bearer: None,
            preparation: None,
            instance: Some(Box::new(instance)),
            shared: None,
            snapshot: Some(Arc::new(snapshot)),
            invocation,
            partial: true,
        }
    }

    /// Nested host preparations retain the complete invocation, including live
    /// resource resolution, rather than just its optional transport authority.
    pub(crate) fn effect_authority(&self) -> crate::capabilities::plugin_packages::Authority {
        let local = self.clone();
        let asynchronous = self.clone();
        crate::capabilities::plugin_packages::Authority::default()
            .with_guard(move |db| local.authorize(db))
            .with_recheck_guard(move |s| {
                let authority = asynchronous.clone();
                Box::pin(async move { authority.recheck(s).await })
            })
    }

    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        if let Some(snapshot) = &self.snapshot {
            Box::pin(snapshot.recheck(s)).await?;
        }
        if let Some(invocation) = &self.invocation {
            Box::pin(invocation.recheck(s)).await?;
        }
        if let Some(preparation) = &self.preparation {
            preparation.recheck(s).await?;
        }
        let authority = self.clone();
        s.store.read(move |db| authority.authorize(db)).await
    }
}

pub(crate) fn decode(input: Value) -> Result<ManagePluginInput> {
    serde_json::from_value(input).map_err(|error| Error::bad(error.to_string()))
}

pub(crate) async fn invoke(
    s: &Arc<Service>,
    input: ManagePluginInput,
    bearer: Option<&str>,
    owner: bool,
    preparation: Option<crate::capabilities::plugin_packages::Authority>,
) -> Result<ManagePluginOutput> {
    use ManagePluginInput as Command;
    use ManagePluginOutput as Output;
    if let Some(bearer) = bearer {
        crate::installation_mcp::authorize(s, bearer).await?;
    }
    // Even an empty submitted selection is forbidden to an agent.
    if matches!(&input, Command::Save { data, .. } if data.agent_ids.is_some()) && bearer.is_some()
    {
        return Err(Error::forbidden(
            "Grant plugin instances from the owner's agent editor.",
        ));
    }
    if matches!(&input, Command::People { .. }) && !owner {
        return Err(Error::forbidden(
            "Only the installation owner can use this command to approve or remove external people.",
        ));
    }
    let (plugin, instance, shared) = match &input {
        Command::Setup { plugin_id }
        | Command::SigninStatus { plugin_id, .. }
        | Command::SigninCancel { plugin_id, .. } => (plugin_id, None, None),
        Command::Save {
            plugin_id,
            instance_id,
            data,
        } => (plugin_id, *instance_id, data.shared_from),
        Command::Form {
            plugin_id,
            instance_id,
            data,
        }
        | Command::Signin {
            plugin_id,
            instance_id,
            data,
        } => {
            if data.instance.is_some() && data.instance != *instance_id {
                return Err(Error::bad("Select saved accounts with instanceId."));
            }
            (plugin_id, *instance_id, data.shared_from)
        }
        Command::People {
            plugin_id,
            instance_id,
            ..
        }
        | Command::Enabled {
            plugin_id,
            instance_id,
            ..
        }
        | Command::Delete {
            plugin_id,
            instance_id,
        }
        | Command::Disconnect {
            plugin_id,
            instance_id,
        }
        | Command::Test {
            plugin_id,
            instance_id,
        }
        | Command::View {
            plugin_id,
            instance_id,
        }
        | Command::Action {
            plugin_id,
            instance_id,
            ..
        } => (plugin_id, Some(*instance_id), None),
    };
    let plugin = plugin.to_owned();
    let instance = instance.map(|id| id.to_string());
    let uses_credentials = matches!(
        &input,
        Command::Signin { .. }
            | Command::Form { .. }
            | Command::Save { .. }
            | Command::Test { .. }
            | Command::View { .. }
            | Command::Action { .. }
    );
    let saved_instance = match instance
        .as_deref()
        .filter(|_| uses_credentials && bearer.is_some())
    {
        Some(instance) => Some(Box::new(connections::get(s, &plugin, instance).await?)),
        None => None,
    };
    let shared_instance = match shared.filter(|_| bearer.is_some()) {
        Some(source) => Some(Box::new(
            connections::get(s, &plugin, &source.to_string()).await?,
        )),
        None => None,
    };
    let authority = (bearer.is_some() || preparation.is_some()).then(|| Authority {
        bearer: bearer.map(str::to_owned),
        preparation,
        instance: saved_instance,
        shared: shared_instance,
        snapshot: None,
        invocation: None,
        partial: false,
    });
    if let Some(preparation) = authority
        .as_ref()
        .and_then(|authority| authority.preparation.as_ref())
    {
        preparation.recheck(s).await?;
    }
    let authorize = {
        let authority = authority.clone();
        move |db: &crate::store::Db<'_>| match &authority {
            Some(authority) => authority.authorize(db),
            None => Ok(()),
        }
    };
    s.store
        .read({
            let authorize = authorize.clone();
            move |db| authorize(db)
        })
        .await?;
    let result = match input {
        Command::People {
            user_id, operation, ..
        } => {
            let value = connections::people(
                s,
                &plugin,
                instance.as_deref().unwrap(),
                &user_id,
                operation,
            )
            .await?;
            Output::People { instance: value }
        }
        Command::Enabled { data, .. } => {
            let value = connections::set_enabled_authorized(
                s,
                &plugin,
                instance.as_deref().unwrap(),
                &data,
                authorize.clone(),
                authority.clone(),
            )
            .await?;
            Output::Enabled { instance: value }
        }
        Command::Delete { .. } => {
            connections::remove_authorized(
                s,
                &plugin,
                instance.as_deref().unwrap(),
                authorize.clone(),
                authority.clone(),
            )
            .await?;
            Output::Deleted { deleted: true }
        }
        command => {
            let package = registry::active(s, &plugin).await?;
            let is_oauth = package
                .manifest
                .credential
                .as_ref()
                .is_some_and(|credential| credential.oauth.is_some());
            let binding = if is_oauth
                && matches!(
                    &command,
                    Command::Signin { .. }
                        | Command::SigninStatus { .. }
                        | Command::SigninCancel { .. }
                ) {
                let policy = crate::relay::task_author_policy(s)
                    .await?
                    .ok_or_else(|| Error::unavailable("Claim this installation first."))?;
                format!("cairn-account:{}", policy.owner.account_id)
            } else {
                String::new()
            };
            match command {
                Command::Setup { .. } => {
                    let callback_url = if is_oauth {
                        Some(oauth::callback_url(s).await?)
                    } else {
                        None
                    };
                    let management_url = crate::relay::beacon_address(&s.config.data_dir)
                        .await?
                        .map(|(origin, installation)| {
                            format!("{origin}/installations/{installation}/plugins/{plugin}")
                        });
                    Output::Setup {
                        setup: package.manifest.setup(),
                        manifest: Box::new(package.manifest),
                        callback_url,
                        management_url,
                    }
                }
                Command::Signin { mut data, .. } => {
                    data.instance = instance
                        .as_deref()
                        .map(|id| uuid::Uuid::parse_str(id).map(cairn_contracts::PluginInstanceId))
                        .transpose()
                        .map_err(|error| Error::bad(error.to_string()))?;
                    let value = if is_oauth {
                        oauth::start(s, &plugin, &serde_json::to_value(data)?, &binding).await?
                    } else {
                        device_flow::start(s, &plugin).await?
                    };
                    Output::Signin {
                        flow: serde_json::from_value(value)?,
                    }
                }
                Command::SigninStatus { flow_id, .. } => {
                    let value = if is_oauth {
                        s.plugins.oauth_flows.status(&plugin, &flow_id, &binding)?
                    } else {
                        device_flow::status(s, &plugin, &flow_id)?
                    };
                    Output::Signin {
                        flow: serde_json::from_value(value)?,
                    }
                }
                Command::SigninCancel { flow_id, .. } => {
                    if is_oauth {
                        s.plugins.oauth_flows.cancel(&plugin, &flow_id, &binding)?;
                    } else {
                        device_flow::cancel(s, &plugin, &flow_id);
                    }
                    Output::SigninCancelled { cancelled: true }
                }
                Command::Form { mut data, .. } => {
                    data.instance = instance
                        .as_deref()
                        .map(|id| uuid::Uuid::parse_str(id).map(cairn_contracts::PluginInstanceId))
                        .transpose()
                        .map_err(|error| Error::bad(error.to_string()))?;
                    let value =
                        Box::pin(forms::form_managed(s, &plugin, &data, authority.clone())).await?;
                    Output::Form { form: value }
                }
                Command::Save { data, .. } => {
                    // Public manage can grant full access on new instances;
                    // installation agents can never grant it.
                    if data.agent_ids.is_some() && bearer.is_some() {
                        return Err(Error::forbidden(
                            "Grant plugin instances from the owner's agent editor.",
                        ));
                    }
                    let value = Box::pin(connections::save_authorized(
                        s,
                        &plugin,
                        data,
                        instance.as_deref(),
                        authorize.clone(),
                        authority.clone(),
                    ))
                    .await?;
                    Output::Saved { instance: value }
                }
                Command::Disconnect { .. } => {
                    let value = oauth::disconnect(
                        s,
                        &plugin,
                        instance.as_deref().unwrap(),
                        authorize.clone(),
                        authority.clone(),
                    )
                    .await?;
                    Output::Disconnected {
                        disconnected: true,
                        revoked: value["revoked"].as_bool(),
                        message: text(&value, "message").into(),
                    }
                }
                Command::Test { .. } => {
                    let value = panels::test_managed(
                        s,
                        &plugin,
                        instance.as_deref().unwrap(),
                        authority.clone(),
                    )
                    .await?;
                    Output::Tested {
                        ok: true,
                        message: value,
                    }
                }
                Command::View { .. } => {
                    let value = panels::view_managed(
                        s,
                        &plugin,
                        instance.as_deref().unwrap(),
                        authority.clone(),
                    )
                    .await?;
                    Output::View {
                        view: value["view"].clone(),
                    }
                }
                Command::Action { data, .. } => {
                    let result = panels::action_managed(
                        s,
                        &plugin,
                        instance.as_deref().unwrap(),
                        &data,
                        authority.clone(),
                    )
                    .await?;
                    Output::Action { result }
                }
                Command::People { .. } | Command::Enabled { .. } | Command::Delete { .. } => {
                    unreachable!()
                }
            }
        }
    };
    if !matches!(
        &result,
        Output::Saved { .. }
            | Output::Enabled { .. }
            | Output::Deleted { .. }
            | Output::Disconnected { .. }
            | Output::People { .. }
    ) {
        s.store.read(move |db| authorize(db)).await?;
    }
    if let Some(preparation) = authority
        .as_ref()
        .and_then(|authority| authority.preparation.as_ref())
    {
        preparation.recheck(s).await?;
    }
    Ok(result)
}

pub(crate) async fn catalog(s: &Service) -> Result<cairn_contracts::PluginCatalog> {
    let mut catalog = registry::catalog(s).await?;
    let mut instances = connections::overview(s).await?;
    for instance in &mut instances {
        let worker = catalog
            .packages
            .iter()
            .find(|package| package.id == instance.plugin_id)
            .and_then(|package| package.manifest.as_ref())
            .map(cairn_contracts::PluginManifest::needs_worker);
        if worker == Some(false) && instance.enabled {
            instance.status = cairn_contracts::PluginStatus {
                state: cairn_contracts::PluginState::Ready,
                message: None,
            };
        }
    }
    catalog.connections = instances;
    Ok(catalog)
}
