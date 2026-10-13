//! Run-scoped secret access over the private VM channel. stdout belongs to the pipe.
use crate::{
    error::{Error, Result},
    validation::text,
};
use serde_json::{Value, json};
use std::io::Write;

pub fn arguments(args: &[String]) -> Result<Value> {
    if args.first().map(String::as_str) != Some("onepassword") {
        return Err(Error::bad(
            "Use cairn plugin onepassword accounts|vaults|items|fields|read [--account ID] [--vault ID] [--item ID] [op://reference].",
        ));
    }
    let operation = args
        .get(1)
        .ok_or_else(|| Error::bad("Missing 1Password operation."))?;
    if !["accounts", "vaults", "items", "fields", "read"].contains(&operation.as_str()) {
        return Err(Error::bad("Unknown 1Password operation."));
    }
    let mut input = json!({
        "operation": operation
    });
    let mut index = 2;
    while index < args.len() {
        let key = match args[index].as_str() {
            "--account" => "accountId",
            "--vault" => "vault",
            "--item" => "item",
            reference if reference.starts_with("op://") && operation == "read" => {
                if input.get("reference").is_some() {
                    return Err(Error::bad("Only one secret reference is allowed."));
                }
                input["reference"] = reference.into();
                index += 1;
                continue;
            }
            _ => return Err(Error::bad("Unknown plugin command argument.")),
        };
        if input.get(key).is_some() {
            return Err(Error::bad("Duplicate plugin command argument."));
        }
        index += 1;
        input[key] = args
            .get(index)
            .ok_or_else(|| Error::bad("Missing plugin command argument."))?
            .clone()
            .into();
        index += 1;
    }
    Ok(json!({
        "plugin": "onepassword",
        "arguments": input
    }))
}

pub async fn run(args: &[String]) -> Result<()> {
    let input = arguments(args)?;
    let bearer = std::env::var("CAIRN_MCP_RUN_TOKEN").map_err(|_| {
        Error::unauthorized("Run this command inside an active Cairn conversation.")
    })?;
    // Always use the run-local channel, never an arbitrary user-provided URL.
    let response = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(40))
        .build()
        .map_err(Error::internal)?
        .post(format!("{}/plugin-command", crate::microvm::mcp::ORIGIN))
        .bearer_auth(bearer)
        .json(&input)
        .send()
        .await
        .map_err(|_| Error::unavailable("Plugin channel unavailable."))?;
    if !response.status().is_success() {
        return Err(Error::bad_gateway(
            "Plugin command failed. Check the active run, account grant and secret reference.",
        ));
    }
    let value: Value = response
        .json()
        .await
        .map_err(|_| Error::bad_gateway("Invalid plugin response."))?;
    let output = if input["arguments"]["operation"] == "read" {
        text(&value, "value").to_owned()
    } else {
        format!("{}\n", serde_json::to_string(&value)?)
    };
    std::io::stdout().lock().write_all(output.as_bytes())?;
    Ok(())
}
