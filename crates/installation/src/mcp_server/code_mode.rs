//! A compact MCP surface over the installation's typed operations.
mod worker;

pub use worker::run as worker;

use super::{ToolResult, catalog, operations, resource_metadata};
use crate::{
    error::{Error, Result},
    service::Service,
    validation::{parse, text},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::Semaphore,
};

const MAX_FRAME: usize = 8 * 1024 * 1024;
const MAX_RESULT: usize = 1024 * 1024;
const MAX_CALLS: usize = 64;
const DEADLINE: Duration = Duration::from_secs(30);

/// Execution identity stays in the manager and never enters the JavaScript worker.
#[derive(Clone)]
pub(crate) enum Authorization {
    Public(Vec<String>, Option<String>),
    Installation(String),
}

impl Authorization {
    async fn operations(&self, s: &Service) -> Result<Vec<Value>> {
        match self {
            Self::Public(..) => Ok(operations()),
            Self::Installation(token) => crate::installation_mcp::operations(s, token).await,
        }
    }

    async fn invoke(&self, s: &Arc<Service>, name: &str, args: Value) -> Result<Value> {
        match self {
            Self::Public(scopes, credential) => {
                super::operation(s, scopes, credential.as_deref(), name, args).await
            }
            Self::Installation(token) => {
                crate::installation_mcp::operation(s, token, name, args).await
            }
        }
    }
}

/// Bounds worker resources independently from agent execution slots.
pub struct CodeMode {
    slots: Semaphore,
    executable: Mutex<Option<PathBuf>>,
}

impl Default for CodeMode {
    fn default() -> Self {
        Self {
            slots: Semaphore::new(8),
            executable: Mutex::new(None),
        }
    }
}

impl CodeMode {
    /// Tests run from a harness binary; production uses its current executable.
    #[doc(hidden)]
    pub fn use_worker(&self, executable: PathBuf) {
        *self.executable.lock().unwrap() = Some(executable);
    }

    async fn execute(
        &self,
        s: &Arc<Service>,
        auth: &Authorization,
        code: &str,
        data: Value,
        denied_scope: &mut Option<String>,
    ) -> Result<Value> {
        let _slot = self
            .slots
            .try_acquire()
            .map_err(|_| Error::too_many_requests("Code workers are busy. Retry shortly."))?;
        let executable = self
            .executable
            .lock()
            .unwrap()
            .clone()
            .map_or_else(std::env::current_exe, Ok)?;
        let directory = tempfile::tempdir()?;
        let mut child = Command::new(executable)
            .arg("mcp-code-worker")
            .env_clear()
            .current_dir(directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| Error::internal("Code worker input unavailable."))?;
        let mut output = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| Error::internal("Code worker output unavailable."))?,
        );
        let names = operations()
            .iter()
            .map(|op| text(op, "name").to_owned())
            .collect();

        let execution = async {
            write(
                &mut input,
                &Frame::Start {
                    code: code.to_owned(),
                    input: data,
                    names,
                },
            )
            .await?;
            let mut calls = 0;
            loop {
                match read(&mut output).await? {
                    Frame::Call { name, args } => {
                        calls += 1;
                        if calls > MAX_CALLS {
                            return Err(Error::too_many_requests(
                                "Code operation limit exceeded (64 calls).",
                            ));
                        }
                        let reply = match auth.invoke(s, &name, args).await {
                            Ok(value) => Frame::Reply { value },
                            Err(error) => {
                                if matches!(auth, Authorization::Public(..))
                                    && matches!(error.status, 401 | 403)
                                {
                                    *denied_scope = operations()
                                        .iter()
                                        .find(|op| op["name"] == name)
                                        .and_then(|op| {
                                            op["_meta"]["securitySchemes"][0]["scopes"][0]
                                                .as_str()
                                                .map(str::to_owned)
                                        });
                                }
                                Frame::Denied {
                                    message: error.message,
                                    status: error.status,
                                }
                            }
                        };
                        write(&mut input, &reply).await?;
                    }
                    Frame::Done { value } => {
                        if serde_json::to_vec(&value)?.len() > MAX_RESULT {
                            return Err(Error::too_large(
                                "Code result exceeds 1 MiB. Filter results or read bounded pages.",
                            ));
                        }
                        return Ok(value);
                    }
                    Frame::Failure { message } => return Err(Error::bad(message)),
                    _ => return Err(Error::bad_gateway("Invalid code worker response.")),
                }
            }
        };
        let result = tokio::time::timeout(DEADLINE, execution)
            .await
            .unwrap_or_else(|_| Err(Error::timeout("Code deadline exceeded (30 seconds).")));
        // Normal completion, failure and timeout all reap the worker. Dropping a
        // cancelled HTTP request also kills it through kill_on_drop.
        let _ = child.kill().await;
        let _ = child.wait().await;
        result
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Frame {
    Start {
        code: String,
        input: Value,
        names: Vec<String>,
    },
    Call {
        name: String,
        args: Value,
    },
    Reply {
        value: Value,
    },
    Denied {
        message: String,
        status: u16,
    },
    Done {
        value: Value,
    },
    Failure {
        message: String,
    },
}

async fn write(input: &mut tokio::process::ChildStdin, frame: &Frame) -> Result<()> {
    let mut bytes = serde_json::to_vec(frame)?;
    if bytes.len() > MAX_FRAME {
        return Err(Error::too_large(
            "Code operation data exceeds 8 MiB. Use bounded pages.",
        ));
    }
    bytes.push(b'\n');
    input.write_all(&bytes).await?;
    Ok(())
}

async fn read(output: &mut BufReader<tokio::process::ChildStdout>) -> Result<Frame> {
    let mut bytes = Vec::new();
    (&mut *output)
        .take(MAX_FRAME as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .await?;
    if bytes.len() > MAX_FRAME || bytes.last() != Some(&b'\n') {
        return Err(Error::bad_gateway(
            "Code worker stopped or exceeded its resource limits.",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::bad_gateway("Invalid code worker frame."))
}

fn search(tools: Vec<Value>, args: &Value) -> Value {
    let words: Vec<_> = text(args, "query")
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let selected = args["names"].as_array().unwrap();
    let matches: Vec<_> = tools
        .into_iter()
        .filter(|tool| {
            let content =
                format!("{} {}", text(tool, "name"), text(tool, "description")).to_lowercase();
            (selected.is_empty() || selected.contains(&tool["name"]))
                && words.iter().all(|word| content.contains(word))
        })
        .collect();

    let offset = args["offset"].as_u64().unwrap() as usize;
    let limit = args["limit"].as_u64().unwrap() as usize;
    let page: Vec<_> = matches
        .iter()
        .skip(offset)
        .take(limit)
        .map(|tool| {
            if args["detail"] == "schema" {
                return tool.clone();
            }

            let mut result = json!({
                "name": tool["name"],
                "description": tool["description"],
                "annotations": tool["annotations"],
            });
            if let Some(meta) = tool.get("_meta") {
                result["_meta"] = meta.clone();
            }
            result
        })
        .collect();

    let next = offset + page.len();
    json!({
        "operations": page,
        "nextOffset": (next < matches.len()).then_some(next),
        "hasMore": next < matches.len(),
    })
}

pub(crate) async fn call(
    s: &Arc<Service>,
    auth: Authorization,
    name: &str,
    args: Value,
) -> Result<Value> {
    if !catalog().iter().any(|tool| tool["name"] == name) {
        return Err(Error::not_found(
            "Unknown tool. Discover operations with search and call them through execute.",
        ));
    }
    let mut denied_scope = None;
    let operation = async {
        let args = parse(&format!("mcp:{name}"), args)?;
        match name {
            "search" => Ok(search(auth.operations(s).await?, &args)),
            "execute" => {
                s.code_mode
                    .execute(
                        s,
                        &auth,
                        text(&args, "code"),
                        args["input"].clone(),
                        &mut denied_scope,
                    )
                    .await
            }
            _ => unreachable!(),
        }
    }
    .await;
    let mut result = match operation {
        Ok(value) => ToolResult::success(value.to_string(), json!({ "result": value })),
        Err(error) => ToolResult::failure(&error.message),
    };
    if result.is_error
        && let Some(scope) = denied_scope
    {
        result.meta = Some(json!({
            "mcp/www_authenticate": format!(
                "Bearer error=\"insufficient_scope\", scope=\"{scope}\", resource_metadata=\"{}\"", resource_metadata(s).await
            ),
        }));
    }
    Ok(serde_json::to_value(result)?)
}
