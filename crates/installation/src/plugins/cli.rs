//! `cairn plugin <plugin> <command> [arguments…]` inside conversations, over the
//! private VM channel. The installation parses the arguments against the package's
//! declared command. Secret output belongs to the pipe: written exactly, nothing else.
use crate::{
    error::{Error, Result},
    validation::text,
};
use serde_json::{Value, json};
use std::io::Write;

const USAGE: &str = "Use cairn plugin <plugin> <command> [arguments], for example cairn plugin onepassword accounts.";

pub fn arguments(args: &[String]) -> Result<Value> {
    let plugin = args.first().filter(|plugin| {
        !plugin.is_empty()
            && plugin.len() <= 80
            && plugin
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    });
    let (Some(plugin), Some(command)) = (plugin, args.get(1)) else {
        return Err(Error::bad(USAGE));
    };
    Ok(json!({
        "plugin": plugin,
        "command": command,
        "args": &args[2..],
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
        .timeout(std::time::Duration::from_secs(190))
        .build()
        .map_err(Error::internal)?
        .post(format!("{}/plugin-command", crate::microvm::mcp::ORIGIN))
        .bearer_auth(bearer)
        .json(&input)
        .send()
        .await
        .map_err(|_| Error::unavailable("Plugin channel unavailable."))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .map_err(|_| Error::bad_gateway("Invalid plugin response."))?;
    if !status.is_success() {
        // Host messages are written for people and never contain secret values.
        let message = value["error"]
            .as_str()
            .unwrap_or("Plugin command failed. Check the active run and its grants.");
        return Err(Error::bad_gateway(message.to_owned()));
    }
    let output = match text(&value, "output") {
        "secret" => text(&value, "value").to_owned(),
        "text" => format!("{}\n", text(&value, "value")),
        _ => format!("{}\n", serde_json::to_string(&value["value"])?),
    };
    std::io::stdout().lock().write_all(output.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::arguments;
    use serde_json::json;

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn commands_carry_their_words_unparsed() {
        assert_eq!(
            arguments(&words(&[
                "onepassword",
                "read",
                "--account",
                "a1",
                "op://v/i/f"
            ]))
            .unwrap(),
            json!({ "plugin": "onepassword", "command": "read", "args": ["--account", "a1", "op://v/i/f"] })
        );
        for invalid in [
            vec![],
            vec!["onepassword"],
            vec!["../x", "read"],
            vec!["One", "read"],
        ] {
            assert!(arguments(&words(&invalid)).is_err(), "{invalid:?}");
        }
    }
}
