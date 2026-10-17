//! What the owner sees and does for one instance beyond its form: the package's
//! declarative view, its actions and its test. Each runs in a one-shot worker with the
//! instance's own capabilities.
use super::{
    connections, registry,
    scope::{self, Scope},
};
use crate::{
    error::{Error, Result},
    service::Service,
    validation::text,
};
use cairn_plugin_api::{validate_action_result, validate_view};
use serde_json::{Value, json};

fn action_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
}

fn fits(value: &Value, max: usize) -> bool {
    serde_json::to_string(value).is_ok_and(|json| json.len() <= max)
}

/// `{ view }`: the instance's panel, or `null` when the package declares none.
pub async fn view(s: &Service, plugin_id: &str, connection_id: &str) -> Result<Value> {
    let connection = connections::get(s, plugin_id, connection_id).await?;
    let package = registry::active(s, plugin_id).await?;
    if !package.manifest.instance_view {
        return Ok(json!({ "view": null }));
    }
    let scope = Scope::Instance(Box::new(connection));
    let payload = json!({ "instance": scope.instances()[0] });
    let view = scope::invoke(s, &package, "view", payload, &scope).await?;
    validate_view(&view).map_err(Error::bad_gateway)?;
    Ok(json!({ "view": view }))
}

/// Runs a button or form action from the instance's view. `values` holds a form's
/// fields; the package checks them like any other input.
pub async fn action(
    s: &Service,
    plugin_id: &str,
    connection_id: &str,
    input: &Value,
) -> Result<Value> {
    let connection = connections::get(s, plugin_id, connection_id).await?;
    let package = registry::active(s, plugin_id).await?;
    let name = text(input, "action");
    if !package.manifest.instance_view || !action_name(name) {
        return Err(Error::bad("Unknown plugin action."));
    }
    let payload = input.get("payload").cloned().unwrap_or(Value::Null);
    let values = input.get("values").cloned().unwrap_or(json!({}));
    if !fits(&payload, 4096) || !values.is_object() || !fits(&values, 16_384) {
        return Err(Error::bad("The action input is too large or invalid."));
    }
    let scope = Scope::Instance(Box::new(connection));
    let event = json!({
        "action": name,
        "payload": payload,
        "values": values,
        "instance": scope.instances()[0],
    });
    let result = scope::invoke(s, &package, "action", event, &scope).await?;
    validate_action_result(&result).map_err(Error::bad_gateway)?;
    Ok(if result.is_null() { json!({}) } else { result })
}

/// `{ ok, message }` from the package's `test` handler.
pub async fn test(s: &Service, plugin_id: &str, connection_id: &str) -> Result<Value> {
    let connection = connections::get(s, plugin_id, connection_id).await?;
    let package = registry::active(s, plugin_id).await?;
    if !package.manifest.instance_test {
        return Err(Error::not_found("This plugin has no connection test."));
    }
    let scope = Scope::Instance(Box::new(connection));
    let payload = json!({ "instance": scope.instances()[0] });
    let result = scope::invoke(s, &package, "test", payload, &scope).await?;
    let message = result["message"]
        .as_str()
        .map(|message| message.chars().take(300).collect::<String>())
        .unwrap_or_default();
    if result["ok"] == false {
        return Err(Error::bad_gateway(if message.is_empty() {
            "The test failed.".to_owned()
        } else {
            message
        }));
    }
    Ok(json!({ "ok": true, "message": message }))
}
