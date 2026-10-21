//! `exec`: programs a package declares and the owner approved, run on the installation
//! without a shell. Secrets reach the program only through its environment.
use super::capabilities::{self, Context};
use crate::{
    error::{Error, Result},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const MAX_ARGUMENTS: usize = 64;
const MAX_ARGUMENT_BYTES: usize = 8192;
const MAX_ENVIRONMENT: usize = 32;
const MAX_STDIN: usize = 262_144;
const MAX_STDOUT: usize = 1_048_576;
const MAX_STDERR: usize = 16_384;
const MAX_SECONDS: u64 = 30;

fn variable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
        && name
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        // The loader and the private home stay under Cairn's control.
        && !name.starts_with("LD_")
        && !name.starts_with("DYLD_")
        && name != "HOME"
}

fn plain(value: &str, max: usize) -> bool {
    value.len() <= max && !value.contains('\0')
}

async fn read_limited(mut reader: impl AsyncRead + Unpin, limit: usize) -> Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    let mut overflow = false;
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok((bytes, overflow));
        }
        let room = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..read.min(room)]);
        overflow |= read > room;
    }
}

/// `exec.run{ program, args?, env?, stdin?, timeout? }` → `{ status, stdout, stderr }`.
/// `env` values are text or `{ secret = "<setting key>" }` from the instance's secrets.
pub async fn run(s: &Service, context: &Context, args: &Value) -> Result<Value> {
    let manifest = &context.manifest;
    let program = manifest
        .program(text(args, "program"))
        .ok_or_else(|| Error::forbidden("This program is not declared by the plugin."))?;
    let path = super::program_installation::executable(s, manifest, program).await?;
    let arguments = match &args["args"] {
        Value::Null => Vec::new(),
        Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| Error::bad("Program arguments must be text."))?,
        _ => return Err(Error::bad("Program arguments must be a list.")),
    };
    if arguments.len() > MAX_ARGUMENTS
        || !arguments
            .iter()
            .all(|argument| plain(argument, MAX_ARGUMENT_BYTES))
    {
        return Err(Error::bad("Program arguments are too long or invalid."));
    }
    let home = tempfile::tempdir()?;
    let mut command = tokio::process::Command::new(&path);
    command
        .args(&arguments)
        .env_clear()
        .env("HOME", home.path())
        .env("TMPDIR", home.path())
        .current_dir(home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(environment) = args["env"].as_object() {
        if environment.len() > MAX_ENVIRONMENT {
            return Err(Error::bad("Too many program environment variables."));
        }
        let mut secrets = None;
        for (name, value) in environment {
            if !variable(name) {
                return Err(Error::bad("Invalid program environment variable."));
            }
            let value = match value {
                Value::String(value) if plain(value, MAX_ARGUMENT_BYTES) => value.clone(),
                Value::Object(reference) => {
                    let key = reference
                        .get("secret")
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::bad("Invalid program environment variable."))?;
                    if secrets.is_none() {
                        secrets = Some(capabilities::secrets(s, context).await?);
                    }
                    secrets
                        .as_ref()
                        .and_then(|secrets| secrets.get(key))
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::forbidden("This instance has no such secret."))?
                        .to_owned()
                }
                _ => return Err(Error::bad("Invalid program environment variable.")),
            };
            command.env(name, value);
        }
    } else if !args["env"].is_null() {
        return Err(Error::bad("Program environment must be a table."));
    }
    let stdin = args["stdin"].as_str().unwrap_or("").to_owned();
    if stdin.len() > MAX_STDIN {
        return Err(Error::too_large("Program input is too large."));
    }
    let seconds = args["timeout"]
        .as_u64()
        .unwrap_or(MAX_SECONDS)
        .clamp(1, MAX_SECONDS);
    capabilities::authorize_management(s, context).await?;
    let mut child = command.spawn().map_err(|_| {
        Error::unavailable("The program could not start. Is it installed on this server?")
    })?;
    let mut input = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let operation = async {
        let feed = async {
            // A program may exit without reading its input.
            let _ = input.write_all(stdin.as_bytes()).await;
            drop(input);
            Ok::<_, Error>(())
        };
        let ((), (stdout, overflow), (stderr, _), status) = tokio::try_join!(
            feed,
            read_limited(stdout, MAX_STDOUT),
            read_limited(stderr, MAX_STDERR),
            async { child.wait().await.map_err(Error::from) },
        )?;
        Ok::<_, Error>((stdout, overflow, stderr, status))
    };
    let (stdout, overflow, stderr, status) =
        match tokio::time::timeout(Duration::from_secs(seconds), operation).await {
            Ok(result) => result?,
            Err(_) => {
                let _ = child.kill().await;
                return Err(Error::unavailable("The program did not finish in time."));
            }
        };
    if overflow {
        return Err(Error::too_large("The program output is larger than 1 MB."));
    }
    Ok(json!({
        "status": status.code().unwrap_or(-1),
        "stdout": String::from_utf8_lossy(&stdout),
        "stderr": String::from_utf8_lossy(&stderr),
    }))
}

#[cfg(test)]
mod tests {
    use super::variable;

    #[test]
    fn environment_names_exclude_the_loader_and_home() {
        assert!(variable("OP_SERVICE_ACCOUNT_TOKEN"));
        assert!(variable("_X1"));
        for name in [
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "HOME",
            "lower",
            "1X",
            "A-B",
            "",
        ] {
            assert!(!variable(name), "{name}");
        }
    }
}
