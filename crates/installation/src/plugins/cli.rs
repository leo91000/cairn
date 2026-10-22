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
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(190))
        .build()
        .map_err(Error::internal)?;
    if matches!(
        args.get(1).map(String::as_str),
        Some("--upload" | "--download" | "--delete-file")
    ) {
        return transfer(&client, &bearer, args).await;
    }
    let value = request(&client, &bearer, &input).await?;
    let output = match text(&value, "output") {
        "secret" => text(&value, "value").to_owned(),
        "text" => format!("{}\n", text(&value, "value")),
        _ => format!("{}\n", serde_json::to_string(&value["value"])?),
    };
    std::io::stdout().lock().write_all(output.as_bytes())?;
    Ok(())
}

async fn request(client: &reqwest::Client, bearer: &str, input: &Value) -> Result<Value> {
    let response = client
        .post(format!("{}/plugin-command", crate::microvm::mcp::ORIGIN))
        .bearer_auth(bearer)
        .json(input)
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
    Ok(value)
}

/// Transfer directly between the conversation VM and the installation. Every
/// chunk uses the same run grant and account ownership gate as plugin tools.
async fn transfer(client: &reqwest::Client, bearer: &str, args: &[String]) -> Result<()> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt};
    let usage = "Use cairn plugin PLUGIN --upload INSTANCE PATH, --download INSTANCE HANDLE PATH, or --delete-file INSTANCE HANDLE.";
    let account = args.get(2).ok_or_else(|| Error::bad(usage))?;
    let name = args.get(3).ok_or_else(|| Error::bad(usage))?;
    let call = |mut file: Value| {
        file["instance"] = json!(account);
        let input = json!({ "plugin": args[0], "file": file });
        async move {
            request(client, bearer, &input)
                .await
                .map(|v| v["value"].clone())
        }
    };
    if args[1] == "--delete-file" {
        if args.len() != 4 {
            return Err(Error::bad(usage));
        }
        let value = call(json!({ "method": "delete", "id": name })).await?;
        println!("{value}");
        return Ok(());
    }
    if args[1] == "--upload" {
        if args.len() != 4 {
            return Err(Error::bad(usage));
        }
        let mut source = std::fs::File::open(name)?;
        let metadata = source.metadata()?;
        if !metadata.is_file() || metadata.len() > super::files::MAX_BYTES as u64 {
            return Err(Error::too_large("Upload a regular file of at most 40 MB."));
        }
        let filename = std::path::Path::new(name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("attachment");
        let media = mime_guess::from_path(name)
            .first_or_octet_stream()
            .to_string();
        let mut file =
            call(json!({ "method": "create", "name": filename, "mediaType": media })).await?;
        let upload = async {
            let mut offset = 0;
            let mut buffer = vec![0; 65_536];
            loop {
                let size = source.read(&mut buffer)?;
                if size == 0 { break; }
                if offset + size > super::files::MAX_BYTES { return Err(Error::too_large("File grew beyond 40 MB.")); }
                let result = call(json!({ "method": "append", "id": file["id"], "offset": offset, "data": URL_SAFE_NO_PAD.encode(&buffer[..size]), "encoding": "base64url" })).await?;
                offset += size;
                if result["size"].as_u64() != Some(offset as u64) { return Err(Error::bad_gateway("Invalid transfer offset.")); }
            }
            Ok::<_, Error>(offset)
        }.await;
        match upload {
            Ok(size) => {
                file["size"] = json!(size);
                println!("{file}");
            }
            Err(error) => {
                let _ = call(json!({ "method": "delete", "id": file["id"] })).await;
                return Err(error);
            }
        }
        return Ok(());
    }
    if args.len() != 5 {
        return Err(Error::bad(usage));
    }
    let mut destination = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&args[4])?;
    let download = async {
        let mut offset = 0;
        loop {
            let part = call(json!({ "method": "read", "id": name, "offset": offset })).await?;
            let bytes = URL_SAFE_NO_PAD
                .decode(text(&part, "data"))
                .map_err(|_| Error::bad_gateway("Invalid file data."))?;
            destination.write_all(&bytes)?;
            match part["nextOffset"].as_u64() {
                Some(next) if next == offset + bytes.len() as u64 && next > offset => offset = next,
                Some(_) => return Err(Error::bad_gateway("Invalid transfer offset.")),
                None => break,
            }
        }
        destination.flush()?;
        Ok::<_, Error>(())
    }
    .await;
    if download.is_err() {
        drop(destination);
        let _ = std::fs::remove_file(&args[4]);
    }
    download
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
