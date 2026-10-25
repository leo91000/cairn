//! The owner MCP and installation tool share inert configuration operations.
//! A management bearer never becomes a grant on a saved private credential.
use super::Authority;
use crate::{
    error::{Error, Result},
    plugins::{oauth, setup},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) async fn invoke(s: &Arc<Service>, args: &Value, bearer: Option<&str>) -> Result<Value> {
    let requester = super::requester(s, bearer).await?;
    let plugin = text(args, "pluginId");
    let action = text(args, "action");
    let data = args.get("data").cloned().unwrap_or_else(|| json!({}));
    if data.get("agentIds").is_some() {
        return Err(Error::forbidden(
            "Grant plugin instances from the owner's agent editor.",
        ));
    }
    let mut authority = bearer.map(|bearer| Authority {
        bearer: bearer.to_owned(),
        requester,
        instance: None,
        shared: None,
        setup: None,
        uses_setup_credentials: matches!(action, "save" | "form" | "activate"),
    });
    if let Some(authority) = &authority {
        authority.recheck(s).await?;
    }
    // Public MCP is owner-authenticated by its relay. An agent's administrative
    // grant permits inert metadata, not impersonation for private consent.
    let owner = crate::relay::task_author_policy(s)
        .await?
        .ok_or_else(|| Error::unavailable("Claim this installation first."))?
        .owner;
    let account = owner.account_id.as_str();
    let draft = args["draftId"].as_str();
    let required_draft = || draft.ok_or_else(|| Error::bad("Choose a draftId."));
    if let Some(authority) = &mut authority {
        if let Some(draft) = draft {
            authority.setup = Some(Box::new(setup::owned(s, plugin, draft, account).await?));
        }
        if data.get("credentialFlow").is_some()
            || matches!(action, "signin" | "signin_status" | "signin_cancel")
        {
            return Err(Error::forbidden(
                "Private setup consent belongs to the owner. Use public owner MCP or the owner's plugin page.",
            ));
        }
        authority.recheck(s).await?;
    }
    let result = match action {
        "list" => setup::list(s, plugin, account, args["cursor"].as_str()).await?,
        "get" => setup::get(s, plugin, required_draft()?, account).await?,
        "save" => return setup::save_managed(s, plugin, account, &data, draft, authority).await,
        "delete" => {
            return setup::remove_managed(s, plugin, required_draft()?, account, &data, authority)
                .await;
        }
        "form" => {
            setup::form_managed(
                s,
                plugin,
                required_draft()?,
                account,
                &data,
                authority.clone(),
            )
            .await?
        }
        "activate" => {
            return setup::activate_managed(
                s,
                plugin,
                required_draft()?,
                account,
                &data,
                authority,
            )
            .await;
        }
        "signin" => {
            oauth::credential::start_setup(
                s,
                plugin,
                required_draft()?,
                account,
                &data,
                &format!("cairn-account:{account}"),
            )
            .await?
        }
        "signin_status" => s.plugins.oauth_flows.status(
            plugin,
            text(args, "flowId"),
            &format!("cairn-account:{account}"),
        )?,
        "signin_cancel" => s.plugins.oauth_flows.cancel(
            plugin,
            text(args, "flowId"),
            &format!("cairn-account:{account}"),
        )?,
        _ => return Err(Error::bad("Unknown setup management action.")),
    };
    if let Some(authority) = &authority {
        authority.recheck(s).await?;
    }
    Ok(result)
}
