//! Route metadata uses the same validation and transactions as the owner UI.
//! Installation management grants no saved credentials or agent access.
use super::Authority;
use crate::{
    error::{Error, Result},
    plugins::{resources::configuration::Configuration, routes},
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
    let configuration = Configuration::managed(s, authority).await?;
    let plugin = text(args, "pluginId");
    let instance = text(args, "instanceId");
    let collection = text(args, "collection");
    let resource = text(args, "resourceId");
    let route = args["routeId"].as_str();
    let data = &args["data"];
    match text(args, "action") {
        "eligibility" => {
            let query: std::collections::HashMap<String, String> =
                serde_json::from_value(args.get("data").cloned().unwrap_or_else(|| json!({})))
                    .map_err(|_| Error::bad("Choose valid projectId and accessMode strings."))?;
            if query
                .keys()
                .any(|key| !matches!(key.as_str(), "projectId" | "accessMode"))
            {
                return Err(Error::bad(
                    "Eligibility accepts only projectId and accessMode.",
                ));
            }
            let result = routes::eligibility_configured(
                s,
                plugin,
                instance,
                collection,
                resource,
                &query,
                configuration.clone(),
            )
            .await?;
            configuration.recheck(s).await?;
            Ok(result)
        }
        "list" => {
            let result = routes::list_configured(
                s,
                plugin,
                instance,
                collection,
                resource,
                args["cursor"].as_str(),
                configuration.clone(),
            )
            .await?;
            configuration.recheck(s).await?;
            Ok(result)
        }
        "requests" => {
            let result = routes::requests_configured(
                s,
                plugin,
                instance,
                collection,
                resource,
                route.ok_or_else(|| Error::bad("Choose a routeId."))?,
                configuration.clone(),
            )
            .await?;
            configuration.recheck(s).await?;
            Ok(result)
        }
        "people" => {
            routes::people_configured(
                s,
                plugin,
                instance,
                collection,
                resource,
                route.ok_or_else(|| Error::bad("Choose a routeId."))?,
                args["userId"]
                    .as_str()
                    .ok_or_else(|| Error::bad("Choose a userId."))?,
                data,
                configuration,
            )
            .await
        }
        "save" => {
            routes::save_configured(
                s,
                plugin,
                instance,
                collection,
                resource,
                data,
                route,
                configuration,
            )
            .await
        }
        "delete" => {
            routes::remove_configured(
                s,
                plugin,
                instance,
                collection,
                resource,
                route.ok_or_else(|| Error::bad("Choose a routeId."))?,
                data,
                configuration,
            )
            .await
        }
        _ => Err(Error::bad("Unknown route management action.")),
    }
}
