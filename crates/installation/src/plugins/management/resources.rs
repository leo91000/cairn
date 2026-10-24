//! Resource configuration shares the UI's validation and transactions. Management
//! access permits metadata changes; it supplies neither private credentials nor grants.
use super::Authority;
use crate::{
    error::{Error, Result},
    plugins::resources::{self, configuration::Configuration, target::Target},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
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
    let collection = text(args, "collection");
    let resource = args["resourceId"].as_str();
    let required_resource = || resource.ok_or_else(|| Error::bad("Choose a resourceId."));
    let result = match text(args, "action") {
        "form" => {
            let mut input = args.get("data").cloned().unwrap_or_else(|| json!({}));
            if input.get("resource").is_some() {
                return Err(Error::bad(
                    "Select saved resources with resourceId, not data.resource.",
                ));
            }
            if let Some(resource) = resource {
                input["resource"] = json!(resource);
            }
            resources::forms::preview_configured(
                s,
                target,
                collection,
                &input,
                configuration.clone(),
            )
            .await?
        }
        "list" => resources::list_for(s, target, collection, args["cursor"].as_str()).await?,
        "get" => serde_json::to_value(
            resources::get_for(s, target, collection, required_resource()?).await?,
        )?,
        "save" => {
            return resources::save_configured(
                s,
                target,
                collection,
                &args["data"],
                resource,
                configuration,
            )
            .await;
        }
        "delete" => {
            return resources::remove_configured(
                s,
                target,
                collection,
                required_resource()?,
                &args["data"],
                Some(configuration),
            )
            .await;
        }
        _ => return Err(Error::bad("Unknown resource management action.")),
    };
    configuration.recheck(s).await?;
    Ok(result)
}
