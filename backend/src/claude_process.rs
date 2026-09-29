//! Adapter for the unmodified Claude Code streaming CLI.
use crate::{
    claude,
    config::Config,
    error::{Error, Result},
    process::command,
    skills::atomic_write,
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

const SHELL_ENVIRONMENT_ERROR: &str = "Claude Code's session environment contains NUL bytes, so shell commands cannot start. Repair the affected .sh file in the Claude session-env directory, then resume this conversation to start a fresh process. The conversation and workspace have been preserved.";

async fn validate_session_environment(directory: &Path, session: &str) -> Result<()> {
    crate::validation::uuid(session)?;
    let environment = directory.join("session-env").join(session);
    let mut entries = match tokio::fs::read_dir(&environment).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "sh") {
            continue;
        }
        // Never execute or rewrite saved shell code while checking it. Scan in
        // chunks so an unexpectedly large generated file cannot exhaust memory.
        let mut file = tokio::fs::File::open(&path).await?;
        let mut buffer = [0; 8192];
        loop {
            let count = file.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            if buffer[..count].contains(&0) {
                return Err(Error::bad_gateway(format!(
                    "{SHELL_ENVIRONMENT_ERROR} Affected file: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn shell_environment_failed(item: &Value, block: &Value) -> bool {
    if block["is_error"] != true
        || (item["type"] != "command_execution" && item["tool"] != "Monitor")
    {
        return false;
    }
    let matches = |message: &str| {
        message.starts_with("The argument 'args[1]' must be a string without null bytes.")
            && (message.contains("/shell-snapshots/") || message.contains("/session-env/"))
    };
    match &block["content"] {
        Value::String(message) => matches(message),
        Value::Array(blocks) => blocks
            .iter()
            .any(|block| block["type"] == "text" && matches(text(block, "text"))),
        _ => false,
    }
}

async fn send(stdin: &mut tokio::process::ChildStdin, value: Value) -> Result<()> {
    stdin.write_all(format!("{value}\n").as_bytes()).await?;
    Ok(())
}

async fn emit(events: &mpsc::Sender<Value>, value: Value) -> Result<()> {
    events
        .send(value)
        .await
        .map_err(|_| Error::unavailable("Claude output stopped."))
}

async fn input(message: &Value, directory: &Path) -> Result<Value> {
    let blocks =
        crate::attachments::input(text(message, "text"), &message["attachments"], directory);
    let mut content = Vec::new();
    for block in blocks.as_array().unwrap() {
        if block["type"] == "text" {
            content.push(json!({
                "type": "text",
                "text": block["text"],
            }));
        } else if block["type"] == "localImage" {
            let path = Path::new(text(block, "path"));
            let bytes = tokio::fs::read(path).await?;
            if bytes.len() > 10_000_000 {
                return Err(Error::bad("Claude image attachment is too large."));
            }
            let mime = match path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str()
            {
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => "image/png",
            };
            content.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": mime,
                    "data": STANDARD.encode(bytes),
                },
            }));
        }
    }
    Ok(json!({
        "type": "user",
        "uuid": message["id"],
        "message": {
            "role": "user",
            "content": content,
        },
        "parent_tool_use_id": null,
    }))
}

pub fn args(plan: &Value) -> Vec<String> {
    let mut args = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--replay-user-messages",
        "--permission-prompt-tool",
        "stdio",
        "--strict-mcp-config",
        "--setting-sources",
        "",
        "--system-prompt-snapshot",
        "off",
    ]
    .map(str::to_owned)
    .to_vec();
    args.extend([
        "--append-system-prompt".into(),
        text(plan, "instructions").into(),
        "--mcp-config".into(),
        plan["claudeMcps"].to_string(),
    ]);
    if !text(plan, "model").is_empty() {
        args.extend(["--model".into(), text(plan, "model").into()]);
    }
    if !text(plan, "reasoning").is_empty() {
        args.extend(["--effort".into(), text(plan, "reasoning").into()]);
    }
    if let Some(session) = plan["sessionId"].as_str() {
        args.extend(["--resume".into(), session.into()]);
    }
    let mut settings = json!({
        "attribution": {
            "commit": "",
        },
    });
    if plan["sandbox"] == "yolo" {
        args.push("--dangerously-skip-permissions".into());
    } else {
        settings["sandbox"] = json!({
            "enabled": true,
            "failIfUnavailable": true,
            "autoAllowBashIfSandboxed": true,
            "allowUnsandboxedCommands": false,
            "network": {
                "allowedDomains": ["*"],
            },
            "filesystem": {
                "allowWrite": plan["writableRoots"],
                "denyWrite": if plan["sandbox"] == "read-only" {
                    plan["writableRoots"].clone()
                } else {
                    json!([])
                },
            },
        });
        args.extend(["--permission-mode".into(), "default".into()]);
    }
    args.extend(["--settings".into(), settings.to_string()]);
    for root in plan["writableRoots"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        args.extend(["--add-dir".into(), root.into()]);
    }
    let mut denied = plan["claudeDeniedTools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if plan["sandbox"] == "read-only" {
        denied.extend(["Write", "Edit", "NotebookEdit"].map(str::to_owned));
    }
    if !denied.is_empty() {
        args.extend(["--disallowedTools".into(), denied.join(",")]);
    }

    args
}

pub fn item(block: &Value, message: &str, index: usize) -> Option<Value> {
    let id = if block["type"] == "tool_use" {
        text(block, "id").to_owned()
    } else {
        format!("{message}-{index}")
    };
    Some(match text(block, "type") {
        "text" => json!({
            "id": id,
            "type": "agent_message",
            "text": block["text"],
        }),
        "thinking" => json!({
            "id": id,
            "type": "reasoning",
            "text": block["thinking"],
        }),
        "tool_use" => {
            let name = text(block, "name");
            let input = &block["input"];
            if name == "Bash" {
                json!({
                    "id": id,
                    "type": "command_execution",
                    "command": input["command"],
                    "status": "in_progress",
                })
            } else {
                json!({
                    "id": id,
                    "type": "mcp_tool_call",
                    "server": "Claude Code",
                    "tool": name,
                    "arguments": input,
                    "status": "in_progress",
                })
            }
        }
        _ => return None,
    })
}

pub async fn run(
    config: &Config,
    plan: Value,
    events: mpsc::Sender<Value>,
    cancel: CancellationToken,
) -> Result<()> {
    let directory = std::env::var("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| config.home.join(".claude"));
    let inbox = Path::new(text(&plan, "inputDirectory"));
    let receipt = Path::new(text(&plan, "output")).with_extension("claude-receipt.json");
    let initial_id = text(&plan["execution"], "messageId");
    // Claude acknowledges but does not execute UUIDs already in a resumed session.
    // Keep the durable chat ID while giving each continuation a fresh wire ID.
    let wire_id = if plan["sessionId"].is_string() {
        crate::config::id()
    } else {
        initial_id.to_owned()
    };
    if let Ok(bytes) = tokio::fs::read(&receipt).await
        && let Ok(saved) = serde_json::from_slice::<Value>(&bytes)
        && saved["messageId"] == initial_id
        // Older adapters could save an unrelated startup/background result before
        // Claude consumed this request. Such a receipt must never complete it.
        && saved["delivered"].as_array().is_some_and(|ids| ids.iter().any(|id| id == initial_id))
    {
        for id in saved["delivered"].as_array().into_iter().flatten() {
            emit(
                &events,
                json!({
                    "type": "chat.delivered",
                    "messageId": id,
                }),
            )
            .await?;
        }
        atomic_write(
            Path::new(text(&plan, "output")),
            text(&saved, "text").as_bytes(),
        )
        .await?;
        emit(
            &events,
            json!({
                "type": "item.completed",
                "item": {
                    "id": saved["itemId"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("{initial_id}-recovered")),
                    "type": "agent_message",
                    "text": saved["text"],
                },
            }),
        )
        .await?;
        return emit(
            &events,
            json!({
                "type": "turn.completed",
            }),
        )
        .await;
    }
    // Completed receipts above do not launch Claude and must remain replayable.
    if let Some(session) = plan["sessionId"].as_str() {
        validate_session_environment(&directory, session).await?;
    }
    // Managed runs keep access-only credentials from their broker next to their session.
    let mut auth = if plan["claudeManagedAuth"] == true {
        let mut client = crate::accounts::claude::Client::new(&directory);
        client.sync().await?;
        Some(client)
    } else {
        None
    };
    let mut cmd = command(
        &config.claude_bin,
        &args(&plan),
        &claude::environment(config, &directory),
        Some(Path::new(text(&plan, "cwd"))),
    );
    if auth.is_some() {
        cmd.env("CLAUDE_SECURESTORAGE_CONFIG_DIR", &directory);
    }
    cmd.stdin(std::process::Stdio::piped()).process_group(0);
    let mut child = cmd.spawn().map_err(|_| {
        Error::unavailable("Unable to start Claude Code. Check the server installation.")
    })?;
    let pid = child.id().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut stderr = child.stderr.take().unwrap();
    let drain = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
    });
    let operation = async {
        let mut initial = json!({
            "id": wire_id,
            "text": crate::chats::execution_text(&plan),
            "attachments": plan["execution"]["attachments"]
        });
        if plan["execution"]["recovery"] == true {
            initial["text"] = format!("Continue the interrupted request. Preserve completed work and verify external \
            effects before repeating an action.\n{}",text(&initial,"text")).into();
        }
        send(&mut stdin, input(&initial, inbox).await?).await?;
        let mut submitted = HashSet::from([initial_id.to_owned()]);
        let mut delivered = HashSet::<String>::new();
        let mut questions = HashMap::<String, (Value, Value)>::new();
        let mut tools = HashMap::<String, Value>::new();
        let mut streams = HashMap::<usize, Value>::new();
        let mut blocks = HashMap::<String, usize>::new();
        let mut stream_message = String::new();
        let mut last = String::new();
        let mut last_id = format!("{initial_id}-result");
        let mut pending = HashSet::from([initial_id.to_owned()]);
        let mut consumed = HashSet::<String>::new();
        let mut buffer = Vec::new();
        let mut background = HashSet::<String>::new();
        let mut awaiting_background = HashSet::<String>::new();
        let mut background_replayed = false;
        let mut timer = tokio::time::interval(Duration::from_millis(250));
        let mut auth_timer = tokio::time::interval(Duration::from_secs(30));
        auth_timer.tick().await;
        loop {
            // read_until is cancellation safe. Bound allocation using fill_buf below.
            tokio::select! {
                _ = cancel.cancelled() => return Err(Error::conflict("Claude execution stopped.")),
                _ = auth_timer.tick(),
                if auth.is_some() => {
                    tokio::select! {
                        _ = cancel.cancelled()=>return Err(Error::conflict("Claude execution stopped.")),
                        result = auth.as_mut().unwrap().sync()=>result?,
                    }
                },
                _ = timer.tick() => {
                    if let Ok(bytes) = tokio::fs::read(inbox.join("messages.json")).await {
                        if bytes.len() > 2_000_000 {
                            return Err(Error::bad("Chat inbox is too large."));
                        }
                        if let Ok(messages) = serde_json::from_slice::<Vec<Value>>(&bytes) {
                            for message in messages {
                                let id = text(&message, "id");
                                if submitted.contains(id) {
                                    continue;
                                }
                                if let Some((request, tool_input)) = questions.remove(text(&message, "questionId"))
                                {
                                    let mut updated = tool_input;
                                    let mut answers = json!({});
                                    for (index, q) in updated["questions"]
                                        .as_array()
                                        .into_iter()
                                        .flatten()
                                        .enumerate()
                                    {
                                        answers[text(q, "question")] = message["answers"][index.to_string()]
                                            .as_array()
                                            .into_iter()
                                            .flatten()
                                            .filter_map(Value::as_str)
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                            .into();
                                    }
                                    updated["answers"] = answers;
                                    send(&mut stdin, json!({
                        "type": "control_response",
                        "response": {
                            "subtype": "success",
                            "request_id": request,
                            "response": {"behavior": "allow","updatedInput": updated}
                        }
                    })).await?;
                                    submitted.insert(id.into());
                                    delivered.insert(id.into());
                                    emit(&events, json!({"type": "chat.delivered","messageId": id})).await?;
                                    emit(&events, json!({"type": "chat.question.closed","questionId": message["questionId"]})).await?;
                                } else {
                                    send(&mut stdin, input(&message, inbox).await?).await?;
                                    submitted.insert(id.into());
                                    pending.insert(id.into());
                                }
                            }
                        }
                    }
                },
                bytes = stdout.fill_buf() => {
                    let bytes = bytes?;
                    if bytes.is_empty() {
                        return Err(Error::bad_gateway("Claude Code disconnected before completing its response. Resume \
                                    to continue.",
                        ));
                    }
                    let end = bytes.iter().position(|b| *b == b'\n').map(|i| i + 1);
                    let count = end.unwrap_or(bytes.len());
                    if buffer.len() + count > 32_000_000 {
                        return Err(Error::bad_gateway("Claude response exceeded the supported limit.",
                        ));
                    }
                    buffer.extend_from_slice(&bytes[..count]);
                    stdout.consume(count);
                    if end.is_none() {
                        continue;
                    }
                    let value: Value = serde_json::from_slice(&buffer)
                        .map_err(|_| Error::bad_gateway("Claude Code returned an invalid streaming event."))?;
                    buffer.clear();
                    match text(&value, "type") {
                        "system" if value["subtype"] == "init" => {
                            let session = text(&value, "session_id");
                            crate::validation::uuid(session)?;
                            emit(&events, json!({"type": "thread.started","thread_id": session})).await?;
                        }
                        "system" if value["subtype"] == "background_tasks_changed" => {
                            background = value["tasks"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter(|task| task["ambient"] != true)
                                .map(|task| text(task, "task_id").to_owned())
                                .collect();
                            awaiting_background.extend(background.iter().cloned());
                            for task in value["tasks"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter(|task| task["ambient"] == true)
                            {
                                awaiting_background.remove(text(task, "task_id"));
                            }
                        }
                        "user" => {
                            let id = text(&value, "uuid");
                            let id = if id == wire_id { initial_id } else { id };
                            if submitted.contains(id) {
                                consumed.insert(id.into());
                                if delivered.insert(id.into()) {
                                    emit(&events, json!({"type": "chat.delivered","messageId": id})).await?;
                                }
                            }
                            if value["origin"]["kind"] == "task-notification" {
                                background_replayed = true;
                            }
                            for block in value["message"]["content"].as_array().into_iter().flatten() {
                                if block["type"] == "tool_result"
                                    && let Some(mut item) = tools.remove(text(block, "tool_use_id"))
                                {
                                    let environment_failed = shell_environment_failed(&item, block);
                                    item["status"] = if block["is_error"] == true {
                                        "failed"
                                    } else {
                                        "completed"
                                    }
                                    .into();
                                    // The raw spawn error includes environment exports, which
                                    // can contain secrets. Persist only the recovery message.
                                    if environment_failed {
                                        let output = if item["type"] == "command_execution" {
                                            "aggregated_output"
                                        } else {
                                            "result"
                                        };
                                        item[output] = SHELL_ENVIRONMENT_ERROR.into();
                                    } else if item["type"] == "command_execution" {
                                        item["aggregated_output"] = if block["content"].is_string() {
                                            block["content"].clone()
                                        } else {
                                            block["content"].to_string().into()
                                        };
                                    } else {
                                        item["result"] = block["content"].clone();
                                    }
                                    emit(&events, json!({"type": "item.completed","item": item})).await?;
                                    if environment_failed {
                                        return Err(Error::bad_gateway(SHELL_ENVIRONMENT_ERROR));
                                    }
                                }
                            }
                        }
                        // Claude Code emits one assistant event per content block, all sharing the
                        // message id. Count blocks per message so ids match the streamed block index.
                        "assistant" => {
                            let message = text(&value["message"], "id").to_owned();
                            for block in value["message"]["content"].as_array().into_iter().flatten() {
                                let next = blocks.entry(message.clone()).or_insert(0);
                                let index = *next;
                                *next += 1;
                                if let Some(item) = item(block, &message, index) {
                                    if item["type"] == "agent_message" {
                                        last = text(&item, "text").into();
                                        last_id = text(&item, "id").into();
                                    }
                                    let running = block["type"] == "tool_use";
                                    if running {
                                        tools.insert(text(&item, "id").into(), item.clone());
                                    }
                                    emit(&events, json!({"type": if running{"item.started"}else{"item.completed"},"item": item})).await?;
                                }
                            }
                        }
                        "stream_event" if value["parent_tool_use_id"].is_null() => {
                            let e = &value["event"];
                            let index = e["index"].as_u64().unwrap_or(0) as usize;
                            match text(e, "type") {
                                "message_start" => {
                                    stream_message = text(&e["message"], "id").into();
                                    streams.clear();
                                }
                                "content_block_start" => {
                                    if let Some(item) = item(&e["content_block"], &stream_message, index)
                                        && ["agent_message", "reasoning"].contains(&text(&item, "type"))
                                    {
                                        streams.insert(index, item.clone());
                                        emit(&events, json!({"type": "item.started","item": item})).await?;
                                    }
                                }
                                "content_block_delta" => {
                                    if let Some(item) = streams.get_mut(&index) {
                                        let delta = if e["delta"]["type"] == "thinking_delta" {
                                            text(&e["delta"], "thinking")
                                        } else {
                                            text(&e["delta"], "text")
                                        };
                                        item["text"] = format!("{}{delta}",text(item,"text")).into();
                                        emit(&events, json!({"type": "item.updated","item": item})).await?;
                                    }
                                }
                                _ => {}
                            }
                        }
                        "control_request" => {
                            let req = &value["request"];
                            let request = value["request_id"].clone();
                            if req["subtype"] == "can_use_tool" && req["tool_name"] == "AskUserQuestion" {
                                let qid = crate::auth::hex_digest(&format!("claude:{initial_id}:{}",text(&value,"request_id")));
                                let fields = req["input"]["questions"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .enumerate()
                                    .map(|(i, q)| json!({
                        "id": i.to_string(),
                        "title": q["question"],
                        "options": q["options"].as_array().cloned().unwrap_or_default()
                    }))
                                    .collect::<Vec<_>>();
                                let fields = crate::validation::parse("questions", fields.into())?;
                                questions.insert(qid.clone(), (request, req["input"].clone()));
                                emit(&events, json!({"type": "chat.question","question": {"id": qid,"blocking": true,"fields": fields}})).await?;
                            } else {
                                let allowed = req["subtype"] == "can_use_tool"
                                    && allowed_tool(&plan, text(req, "tool_name"), &req["input"]);
                                send(&mut stdin, json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "success",
                        "request_id": request,
                        "response": if allowed{json!({"behavior": "allow","updatedInput": req["input"]})}else{json!({
                            "behavior": "deny",
                            "message": "This tool is outside the agent's access policy. Ask the user in \
                                your response."
                        })}
                    }
                })).await?;
                            }
                        }
                        "result" => {
                            if value["is_error"] == true || value["subtype"] != "success" {
                                return Err(Error::bad_gateway(if text(&value, "result").is_empty() {
                                        "Claude Code could not complete this turn. Check sign-in, model access, or usage \
                                            limits."
                                    } else {
                                        text(&value, "result")
                                    },
                                ));
                            }
                            if !text(&value, "result").is_empty() {
                                last = text(&value, "result").into();
                            }
                            // A result belongs to a turn, not one stdin message. Claude can
                            // merge prompts and emit unrelated results while resuming tasks.
                            let ids = if value["user_message_uuids"].is_array() {
                                value["user_message_uuids"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned)
                                    .collect::<HashSet<_>>()
                            } else if let Some(id) = value["user_message_uuid"].as_str() {
                                HashSet::from([id.to_owned()])
                            } else if value["origin"]["kind"] == "task-notification" {
                                HashSet::new()
                            } else {
                                consumed.clone()
                            };
                            for id in ids {
                                let id = if id == wire_id {
                                    initial_id.to_owned()
                                } else {
                                    id
                                };
                                if pending.remove(&id) && delivered.insert(id.clone()) {
                                    emit(&events, json!({"type": "chat.delivered","messageId": id})).await?;
                                }
                            }
                            consumed.retain(|id| pending.contains(id));
                            if background_replayed || value["origin"]["kind"] == "task-notification" {
                                awaiting_background.retain(|id| background.contains(id));
                            }
                            background_replayed = false;
                            if !pending.is_empty() || !background.is_empty() || !awaiting_background.is_empty() {
                                continue;
                            }
                            // Save the receipt before completion so restart cannot repeat a finished request.
                            atomic_write(&receipt, &serde_json::to_vec(&json!({
                    "messageId": initial_id,
                    "delivered": delivered,
                    "text": last,
                    "itemId": last_id
                }))?).await?;
                            atomic_write(Path::new(text(&plan, "output")), last.as_bytes()).await?;
                            emit(&events, json!({"type": "chat.question.closed"})).await?;
                            emit(&events, json!({"type": "turn.completed"})).await?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    .await;
    drop(stdin);
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    if tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .is_err()
    {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        let _ = child.wait().await;
    }
    drain.abort();
    let _ = drain.await;
    operation
}

fn allowed_tool(plan: &Value, name: &str, input: &Value) -> bool {
    if plan["sandbox"] == "yolo" {
        return true;
    }
    if matches!(
        name,
        "Bash" | "Read" | "Glob" | "Grep" | "WebSearch" | "WebFetch" | "TodoWrite"
    ) || name.starts_with("mcp__")
    {
        return true;
    }
    if plan["sandbox"] == "workspace-write" && matches!(name, "Write" | "Edit" | "NotebookEdit") {
        let path = Path::new(text(
            input,
            if name == "NotebookEdit" {
                "notebook_path"
            } else {
                "file_path"
            },
        ));
        if let Ok(path) = std::fs::canonicalize(path).or_else(|_| {
            path.parent()
                .and_then(|p| std::fs::canonicalize(p).ok())
                .map(|p| p.join(path.file_name().unwrap_or_default()))
                .ok_or(std::io::Error::from(std::io::ErrorKind::NotFound))
        }) {
            return plan["writableRoots"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|root| path.starts_with(root));
        }
    }
    false
}
