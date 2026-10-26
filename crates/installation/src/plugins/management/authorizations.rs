//! Resource OAuth metadata/disconnect parity. Personal consent does not inherit
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
    let configuration = Configuration::managed(s, authority).await?;
    let target = Target::active(s, text(args, "pluginId"), text(args, "instanceId")).await?;
    let authorization = ConfiguredAuthorization::select(
        s,
        target,
        text(args, "collection"),
        text(args, "resourceId"),
        text(args, "provider"),
        configuration,
    )
    .await?;
    match text(args, "action") {
        "status" => authorization.status(s).await,
        "revoke" => authorization.revoke(s).await,
        _ => Err(Error::bad("Unknown resource authorization action.")),
    }
}
