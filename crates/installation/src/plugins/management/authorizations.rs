//! Resource OAuth consent/metadata/disconnect parity. Personal consent does not inherit
//! an installation owner's identity from an agent's management grant.
use super::Authority;
use crate::{
    error::{Error, Result},
    plugins::{
        oauth::ConfiguredAuthorization,
        resources::{configuration::Configuration, target::Target},
    },
    service::Service,
    validation::text,
};
use serde_json::Value;
use std::sync::Arc;

pub(crate) async fn invoke(s: &Arc<Service>, args: &Value, bearer: Option<&str>) -> Result<Value> {
    let requester = super::requester(s, bearer).await?;
    let authority = bearer.map(|bearer| Authority {
        bearer: bearer.to_owned(),
        requester,
        instance: None,
        shared: None,
        setup: None,
        uses_setup_credentials: false,
    });
    let mut configuration = Configuration::managed(s, authority).await?;
    let plugin = text(args, "pluginId");
    let target = match (args["instanceId"].as_str(), args["draftId"].as_str()) {
        (Some(instance), None) => Target::active(s, plugin, instance).await?,
        (None, Some(draft)) => {
            Target::setup(s, plugin, draft, configuration.owner.account_id()).await?
        }
        _ => return Err(Error::bad("Choose exactly one instanceId or draftId.")),
    };

    if let Some(authority) = &mut configuration.management {
        authority.setup = target.setup_snapshot().cloned().map(Box::new);
        authority.recheck(s).await?;
    }

    let is_setup = target.is_setup();
    let authorization = ConfiguredAuthorization::select(
        s,
        target,
        text(args, "collection"),
        text(args, "resourceId"),
        text(args, "provider"),
        configuration,
    )
    .await?;

    if is_setup && bearer.is_some() && matches!(text(args, "action"), "start" | "finish") {
        return Err(Error::forbidden(
            "Private setup consent belongs to the owner. Use public owner MCP or the owner's plugin page.",
        ));
    }

    match text(args, "action") {
        "start" => authorization.start(s, &args["data"]).await,
        "finish" => authorization.finish(s, &args["data"]).await,
        "status" => authorization.status(s).await,
        "revoke" => authorization.revoke(s, &args["data"]).await,
        _ => Err(Error::bad("Unknown resource authorization action.")),
    }
}
