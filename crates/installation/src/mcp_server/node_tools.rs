//! The node setup and retirement controls shared with the owner UI.
use crate::{
    error::{Error, Result},
    installation_mcp::management_authorization,
    nodes,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) async fn invoke(
    s: &Arc<Service>,
    name: &str,
    args: Value,
    bearer: Option<&str>,
) -> Result<Value> {
    let authorize = management_authorization(bearer);
    let before = authorize.clone();
    s.store.read(move |db| before(db)).await?;

    match name {
        "list_nodes" => {
            let _ = nodes::refresh_local(s).await;
            let nodes = nodes::inventory(s).await?;
            s.store
                .read(move |db| {
                    authorize(db)?;
                    Ok(nodes.into())
                })
                .await
        }
        "list_native_callers" => nodes::native_callers(s, authorize).await,
        "create_node_enrollment" => nodes::invite(s, args, authorize).await,
        "update_node" => {
            nodes::configure_node(s, text(&args, "nodeId"), args["node"].clone(), authorize).await
        }
        "change_native_node_mode" => {
            nodes::native_mode::change(s, text(&args, "nodeId"), args["change"].clone(), authorize)
                .await
        }
        "update_node_storage" => {
            nodes::configure_storage(s, text(&args, "nodeId"), &args["storage"], authorize).await
        }
        "revoke_node" => {
            nodes::revoke(
                s,
                text(&args, "nodeId"),
                json!({
                    "releasePlacements": args["releasePlacements"],
                }),
                authorize,
            )
            .await
        }
        _ => Err(Error::not_found("Unknown node operation")),
    }
}
