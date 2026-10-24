//! Public MCP and installation-tool parity for generic plugin account setup.
//! Agent grants remain exclusive to the owner's agent editor, as for other plugins.
use super::{connections, device_flow, forms, oauth, panels, registry};
use crate::{
    error::{Error, Result},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) mod setup;

/// Management access does not replace a saved instance's separate agent grant, for
/// the instance it edits and for the one whose shared setup it reuses.
#[derive(Clone)]
pub struct Authority {
    bearer: String,
    requester: Option<super::requesters::RunRequester>,
    instance: Option<Box<connections::Connection>>,
    shared: Option<Box<connections::Connection>>,
    setup: Option<Box<super::setup::SetupDraft>>,
    uses_setup_credentials: bool,
}

impl Authority {
    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        if let Some(requester) = &self.requester {
            super::requesters::recheck(s, requester).await?;
        }
        let expected = self.clone();
        s.store.read(move |db| expected.authorize(db)).await
    }

    pub(crate) fn authorize(&self, db: &crate::store::Db<'_>) -> Result<()> {
        let run = crate::installation_mcp::authorize_in(db, &self.bearer)?;
        if let Some(requester) = &self.requester {
            super::requesters::recheck_in(db, requester)?;
        }
        let agent = text(&run["snapshot"]["agent"], "id");
        for expected in [&self.instance, &self.shared].into_iter().flatten() {
            let current = db
                .get(connections::KIND, &expected.id)?
                .ok_or_else(|| Error::forbidden("This plugin instance was removed."))?;
            let current: connections::Connection = serde_json::from_value(current)?;
            if current.revision != expected.revision || !current.grants(agent) {
                return Err(Error::forbidden(
                    "Installation management does not grant access to this plugin account. Ask the owner to grant it on the agent's page.",
                ));
            }
        }
        if let Some(expected) = &self.setup {
            expected.recheck_in(db)?;
            if self.uses_setup_credentials
                && (expected.credential || !expected.secret_keys.is_empty())
            {
                return Err(Error::forbidden(
                    "Installation management does not grant this setup's saved credentials. Finish private setup from the owner's plugin page.",
                ));
            }
        }
        Ok(())
    }
}

async fn requester(
    s: &Service,
    bearer: Option<&str>,
) -> Result<Option<super::requesters::RunRequester>> {
    match bearer {
        Some(bearer) => {
            let run = crate::installation_mcp::authorize(s, bearer).await?;
            super::requesters::for_run(s, &run).await
        }
        None => Ok(None),
    }
}

pub(crate) async fn invoke(
    s: &Arc<Service>,
    name: &str,
    args: &Value,
    bearer: Option<&str>,
) -> Result<Value> {
    let requester = requester(s, bearer).await?;
    if name == "list_plugins" {
        let mut catalog = registry::catalog(s).await?;
        catalog["connections"] = json!(connections::overview(s).await?);
        if let Some(requester) = &requester {
            super::requesters::recheck(s, requester).await?;
        }
        return Ok(catalog);
    }
    let plugin = text(args, "pluginId");
    let action = text(args, "action");
    let data = &args["data"];
    if data.get("agentIds").is_some() {
        return Err(Error::forbidden(
            "Grant plugin instances from the owner's agent editor.",
        ));
    }
    let instance = args["instanceId"].as_str();
    if data.get("instance").is_some() {
        return Err(Error::bad(
            "Select saved accounts with instanceId, not data.instance.",
        ));
    }
    let uses_saved_credentials = matches!(
        action,
        "signin" | "form" | "save" | "test" | "view" | "action"
    );
    let saved_instance = match instance.filter(|_| uses_saved_credentials && bearer.is_some()) {
        Some(instance) => Some(Box::new(connections::get(s, plugin, instance).await?)),
        None => None,
    };
    // Reusing another instance's shared setup uses its saved secrets.
    let shared_instance = match connections::shared_source(data)?
        .filter(|_| matches!(action, "signin" | "form" | "save") && bearer.is_some())
    {
        Some(source) => Some(Box::new(connections::get(s, plugin, source).await?)),
        None => None,
    };
    let authority = bearer.map(|bearer| Authority {
        bearer: bearer.to_owned(),
        requester,
        instance: saved_instance,
        shared: shared_instance,
        setup: None,
        uses_setup_credentials: false,
    });
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
    let required_instance = || instance.ok_or_else(|| Error::bad("Choose an instanceId."));
    // Stopping or removing an instance uses no package credentials and remains
    // available while its package is unavailable or awaiting approval.
    match action {
        "enabled" => {
            return connections::set_enabled_authorized(
                s,
                plugin,
                required_instance()?,
                data,
                authorize,
            )
            .await;
        }
        "delete" => {
            return connections::remove_authorized(s, plugin, required_instance()?, authorize)
                .await;
        }
        _ => {}
    }
    let package = registry::active(s, plugin).await?;
    let is_oauth = package
        .manifest
        .credential
        .as_ref()
        .is_some_and(|c| c.oauth.is_some());
    let owner_binding =
        if is_oauth && matches!(action, "signin" | "signin_status" | "signin_cancel") {
            let policy = crate::relay::task_author_policy(s)
                .await?
                .ok_or_else(|| Error::unavailable("Claim this installation first."))?;
            format!("cairn-account:{}", policy.owner.account_id)
        } else {
            String::new()
        };
    let result = match action {
        "setup" => {
            let mut result = json!({
                "manifest": package.manifest,
                "setup": package.manifest.setup(),
            });
            if is_oauth {
                result["callbackUrl"] = json!(oauth::callback_url(s).await?);
            }
            if let Some((origin, installation)) =
                crate::relay::beacon_address(&s.config.data_dir).await?
            {
                result["managementUrl"] = json!(format!(
                    "{origin}/installations/{installation}/plugins/{plugin}"
                ));
            }
            result
        }
        "signin" => {
            if is_oauth {
                let mut input = if data.is_object() {
                    data.clone()
                } else {
                    json!({})
                };
                if let Some(instance) = instance {
                    input["instance"] = json!(instance);
                }
                oauth::credential::start(s, plugin, &input, &owner_binding).await?
            } else {
                device_flow::start(s, plugin).await?
            }
        }
        "signin_status" => {
            if is_oauth {
                s.plugins
                    .oauth_flows
                    .status(plugin, text(args, "flowId"), &owner_binding)?
            } else {
                device_flow::status(s, plugin, text(args, "flowId")).await?
            }
        }
        "signin_cancel" => {
            if is_oauth {
                s.plugins
                    .oauth_flows
                    .cancel(plugin, text(args, "flowId"), &owner_binding)?
            } else {
                device_flow::cancel(s, plugin, text(args, "flowId"))
            }
        }
        "form" => {
            let mut input = if data.is_object() {
                data.clone()
            } else {
                json!({})
            };
            if let Some(instance) = instance {
                input["instance"] = json!(instance);
            }
            forms::form_managed(s, plugin, &input, authority.clone()).await?
        }
        "save" => {
            return connections::save_authorized(s, plugin, data, instance, authorize, authority)
                .await;
        }
        "disconnect" => return oauth::disconnect(s, plugin, required_instance()?, authorize).await,
        "test" => panels::test_managed(s, plugin, required_instance()?, authority.clone()).await?,
        "view" => panels::view_managed(s, plugin, required_instance()?, authority.clone()).await?,
        "action" => {
            panels::action_managed(s, plugin, required_instance()?, data, authority.clone()).await?
        }
        _ => return Err(Error::bad("Unknown plugin management action.")),
    };
    if let Some(authority) = &authority {
        authority.recheck(s).await?;
    }
    s.store.read(move |db| authorize(db)).await?;
    Ok(result)
}
