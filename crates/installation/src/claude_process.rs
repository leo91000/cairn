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
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, ChildStdout},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

const STOPPED: &str = "Claude execution stopped.";
const MAX_LINE: usize = 32_000_000;
const MAX_IMAGE: usize = 10_000_000;

const DENIED: &str =
    "This tool is outside the agent's access policy. Ask the user in your response.";

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

/// Whether a failed shell tool could not start because the session environment
/// contains NUL bytes.
fn shell_environment_failed(item: &Value, block: &Value) -> bool {
    let shell_tool = item["type"] == "command_execution" || item["tool"] == "Monitor";
    if block["is_error"] != true || !shell_tool {
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

async fn send(stdin: &mut ChildStdin, value: Value) -> Result<()> {
    stdin.write_all(format!("{value}\n").as_bytes()).await?;
    Ok(())
}

async fn emit(events: &mpsc::Sender<Value>, value: Value) -> Result<()> {
    events
        .send(value)
        .await
        .map_err(|_| Error::unavailable("Claude output stopped."))
}

fn delivered(id: &str) -> Value {
    json!({ "type": "chat.delivered", "messageId": id })
}

fn control_response(request_id: &Value, response: &Value) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        },
    })
}

fn image_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    }
}

/// A chat message as a stream-json user message, with its images inlined.
async fn input(message: &Value, directory: &Path) -> Result<Value> {
    let blocks =
        crate::attachments::input(text(message, "text"), &message["attachments"], directory);
    let mut content = Vec::new();
    for block in blocks.as_array().unwrap() {
        if block["type"] == "text" {
            content.push(json!({ "type": "text", "text": block["text"] }));
        } else if block["type"] == "localImage" {
            let path = Path::new(text(block, "path"));
            let bytes = tokio::fs::read(path).await?;
            if bytes.len() > MAX_IMAGE {
                return Err(Error::bad("Claude image attachment is too large."));
            }
            content.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image_type(path),
                    "data": STANDARD.encode(bytes),
                },
            }));
        }
    }
    Ok(json!({
        "type": "user",
        "uuid": message["id"],
        "message": { "role": "user", "content": content },
        "parent_tool_use_id": null,
    }))
}

fn strings(value: &Value) -> impl Iterator<Item = &str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

fn settings(plan: &Value) -> Value {
    let mut settings = json!({ "attribution": { "commit": "" } });
    if plan["sandbox"] == "yolo" {
        return settings;
    }
    let deny_write = if plan["sandbox"] == "read-only" {
        plan["writableRoots"].clone()
    } else {
        json!([])
    };
    settings["sandbox"] = json!({
        "enabled": true,
        "failIfUnavailable": true,
        "autoAllowBashIfSandboxed": true,
        "allowUnsandboxedCommands": false,
        "network": { "allowedDomains": ["*"] },
        "filesystem": { "allowWrite": plan["writableRoots"], "denyWrite": deny_write },
    });
    settings
}

pub fn args(plan: &Value, instructions: &Path) -> Vec<String> {
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
        "--append-system-prompt-file".into(),
        instructions.to_string_lossy().into_owned(),
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
    if plan["sandbox"] == "yolo" {
        args.push("--dangerously-skip-permissions".into());
    } else {
        args.extend(["--permission-mode".into(), "default".into()]);
    }
    args.extend(["--settings".into(), settings(plan).to_string()]);
    for root in strings(&plan["writableRoots"]) {
        args.extend(["--add-dir".into(), root.into()]);
    }
    let mut denied = strings(&plan["claudeDeniedTools"])
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

/// The chat item a Claude content block shows as.
pub fn item(block: &Value, message: &str, index: usize) -> Option<Value> {
    let id = if block["type"] == "tool_use" {
        text(block, "id").to_owned()
    } else {
        format!("{message}-{index}")
    };
    Some(match text(block, "type") {
        "text" => json!({ "id": id, "type": "agent_message", "text": block["text"] }),
        "thinking" => json!({ "id": id, "type": "reasoning", "text": block["thinking"] }),
        "tool_use" if matches!(text(block, "name"), "Agent" | "Task") => {
            subagent(&id, &block["input"])
        }
        "tool_use" if block["name"] == "TodoWrite" => {
            let mut plan = crate::agent_plan::todos(&id, &block["input"]);
            plan["status"] = "in_progress".into();
            plan
        }
        "tool_use" if block["name"] == "Bash" => json!({
            "id": id,
            "type": "command_execution",
            "command": block["input"]["command"],
            "status": "in_progress",
        }),
        "tool_use" => json!({
            "id": id,
            "type": "mcp_tool_call",
            "server": "Claude Code",
            "tool": text(block, "name"),
            "arguments": block["input"],
            "status": "in_progress",
        }),
        _ => return None,
    })
}

/// A subagent the agent delegates work to, followed until it reports back.
fn subagent(id: &str, input: &Value) -> Value {
    let name = [text(input, "description"), text(input, "name")]
        .into_iter()
        .find(|name| !name.trim().is_empty())
        .unwrap_or("Subagent");
    json!({
        "id": id,
        "type": "subagent",
        "name": name,
        "agent_type": input["subagent_type"],
        "prompt": input["prompt"],
        "model": input["model"],
        "background": input["run_in_background"] == true,
        "status": "in_progress",
    })
}

/// The final report of a subagent's tool result, without Claude Code's hand-back framing.
fn subagent_report(block: &Value, outcome: &Value) -> String {
    let texts = |content: &Value| {
        content
            .as_array()
            .into_iter()
            .flatten()
            .filter(|part| part["type"] == "text")
            .map(|part| text(part, "text"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let report = texts(&outcome["content"]);
    if !report.is_empty() {
        return report;
    }
    match &block["content"] {
        Value::String(content) => content.clone(),
        content => texts(content),
    }
}

/// Saved before a turn completes, so a restart never repeats a finished request.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    message_id: String,
    delivered: Vec<String>,
    text: String,
    item_id: String,
}

impl Receipt {
    async fn read(path: &Path, message_id: &str) -> Option<Self> {
        let bytes = tokio::fs::read(path).await.ok()?;
        let receipt = serde_json::from_slice::<Self>(&bytes).ok()?;
        let completes =
            receipt.message_id == message_id && receipt.delivered.iter().any(|id| id == message_id);
        completes.then_some(receipt)
    }

    /// Replays a completed request without starting Claude Code.
    async fn replay(self, plan: &Value, events: &mpsc::Sender<Value>) -> Result<()> {
        for id in &self.delivered {
            emit(events, delivered(id)).await?;
        }
        atomic_write(Path::new(text(plan, "output")), self.text.as_bytes()).await?;
        let item = json!({ "id": self.item_id, "type": "agent_message", "text": self.text });
        emit(events, json!({ "type": "item.completed", "item": item })).await?;
        emit(events, json!({ "type": "turn.completed" })).await
    }
}

/// An AskUserQuestion permission request waiting for the user's answers.
struct Question {
    request_id: Value,
    input: Value,
}

/// One execution: the messages submitted to Claude Code and what it reported back.
struct Turn<'a> {
    plan: &'a Value,
    events: mpsc::Sender<Value>,
    stdin: ChildStdin,
    inbox: &'a Path,
    receipt: PathBuf,
    /// The durable chat ID of the request.
    initial_id: &'a str,
    /// The ID Claude Code knows the request by.
    wire_id: String,
    submitted: HashSet<String>,
    operational_notices: HashSet<String>,
    delivered: HashSet<String>,
    /// Submitted messages whose turn has not produced a result yet.
    pending: HashSet<String>,
    /// Submitted messages Claude Code has echoed back.
    consumed: HashSet<String>,
    questions: HashMap<String, Question>,
    /// Running tools, by tool use ID.
    tools: HashMap<String, Value>,
    /// Subagents still working, by the ID of the tool use that started them.
    subagents: HashMap<String, Value>,
    /// Streamed blocks of the current message, by index.
    streams: HashMap<usize, Value>,
    /// Content blocks seen per assistant message.
    blocks: HashMap<String, usize>,
    stream_message: String,
    last: String,
    last_id: String,
    /// Background tasks that keep the run open, with their descriptions.
    background: BTreeMap<String, String>,
    awaiting_background: HashSet<String>,
    background_replayed: bool,
    /// The agent finished responding and only its background tasks keep the run open.
    waiting_for_background: bool,
}

impl Turn<'_> {
    async fn emit(&self, value: Value) -> Result<()> {
        emit(&self.events, value).await
    }

    /// The chat ID of a message Claude Code reports by its wire ID.
    fn chat_id<'b>(&'b self, id: &'b str) -> &'b str {
        if id == self.wire_id {
            self.initial_id
        } else {
            id
        }
    }

    async fn deliver(&mut self, id: &str) -> Result<()> {
        if self.operational_notices.contains(id) {
            return Ok(());
        }
        if self.delivered.insert(id.to_owned()) {
            self.emit(delivered(id)).await?;
        }
        Ok(())
    }

    async fn submit_initial(&mut self) -> Result<()> {
        let mut message = crate::chats::execution_text(self.plan);
        if self.plan["execution"]["recovery"] == true {
            message = format!(
                "Continue the interrupted request. Preserve completed work and verify external effects before repeating an action.\n{message}"
            );
        }
        let initial = json!({
            "id": self.wire_id,
            "text": message,
            "attachments": self.plan["execution"]["attachments"],
        });
        let initial = input(&initial, self.inbox).await?;
        send(&mut self.stdin, initial).await
    }

    /// Submits messages the user added to the inbox, or answers to pending questions.
    async fn poll_inbox(&mut self) -> Result<()> {
        let Some(bytes) =
            crate::chat_inbox::read(self.inbox, crate::chat_inbox::ADAPTER_LIMIT).await?
        else {
            return Ok(());
        };
        let Ok(messages) = serde_json::from_slice::<Vec<Value>>(&bytes) else {
            return Ok(());
        };
        for message in messages {
            let id = text(&message, "id");
            if message["operationalNotice"] == true {
                self.operational_notices.insert(id.to_owned());
            }
            if self.submitted.contains(id) {
                continue;
            }
            if message["questionCancelled"] == true {
                if let Some(question) = self.questions.remove(text(&message, "questionId")) {
                    send(&mut self.stdin, control_response(&question.request_id, &json!({
                        "behavior": "deny",
                        "message": "This question was withdrawn. No answer or authorization was provided.",
                    }))).await?;
                    self.emit(json!({
                        "type": "chat.question.closed",
                        "questionId": message["questionId"],
                    }))
                    .await?;
                }
                self.submitted.insert(id.into());
                continue;
            }
            if let Some(question) = self.questions.remove(text(&message, "questionId")) {
                self.answer(question, &message).await?;
                continue;
            }
            send(&mut self.stdin, input(&message, self.inbox).await?).await?;
            self.submitted.insert(id.into());
            self.pending.insert(id.into());
            self.waiting_for_background = false;
        }
        Ok(())
    }

    async fn answer(&mut self, question: Question, message: &Value) -> Result<()> {
        let id = text(message, "id");
        let mut updated = question.input;
        let mut answers = Map::new();
        for (index, asked) in updated["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let answer = strings(&message["answers"][index.to_string()])
                .collect::<Vec<_>>()
                .join(", ");
            answers.insert(text(asked, "question").to_owned(), answer.into());
        }
        updated["answers"] = answers.into();
        let response = json!({ "behavior": "allow", "updatedInput": updated });
        send(
            &mut self.stdin,
            control_response(&question.request_id, &response),
        )
        .await?;
        self.submitted.insert(id.into());
        self.deliver(id).await?;
        self.emit(json!({
            "type": "chat.question.closed",
            "questionId": message["questionId"],
        }))
        .await
    }

    /// Handles one stream-json line. Answers whether the request is complete.
    async fn handle(&mut self, value: &Value) -> Result<bool> {
        match text(value, "type") {
            "system" if value["subtype"] == "init" => {
                let session = text(value, "session_id");
                crate::validation::uuid(session)?;
                self.emit(json!({ "type": "thread.started", "thread_id": session }))
                    .await?;
            }
            "system" if value["subtype"] == "background_tasks_changed" => {
                self.background_tasks(value).await?;
            }
            "system"
                if matches!(
                    text(value, "subtype"),
                    "task_started" | "task_progress" | "task_notification"
                ) =>
            {
                self.subagent_task(value).await?;
            }
            "user" => self.on_user(value).await?,
            "assistant" => self.on_assistant(value).await?,
            "stream_event" if value["parent_tool_use_id"].is_null() => {
                self.on_stream_event(&value["event"]).await?;
            }
            "control_request" => self.on_control_request(value).await?,
            "result" => return self.on_result(value).await,
            _ => {}
        }
        Ok(false)
    }

    async fn background_tasks(&mut self, value: &Value) -> Result<()> {
        let tasks = value["tasks"].as_array().into_iter().flatten();
        let (ambient, owned): (Vec<_>, Vec<_>) = tasks.partition(|task| task["ambient"] == true);
        let background: BTreeMap<String, String> = owned
            .iter()
            .map(|task| {
                let id = text(task, "task_id").to_owned();
                (id, text(task, "description").to_owned())
            })
            .collect();
        let changed = background != self.background;
        self.background = background;
        self.awaiting_background
            .extend(self.background.keys().cloned());
        for task in ambient {
            self.awaiting_background.remove(text(task, "task_id"));
        }

        if self.waiting_for_background && changed {
            self.announce_waiting().await?;
        }
        Ok(())
    }

    /// Tells clients which background tasks the idle agent waits for. An empty list
    /// means they finished and the agent is about to continue.
    async fn announce_waiting(&mut self) -> Result<()> {
        let tasks: Vec<Value> = self
            .background
            .iter()
            .map(|(id, description)| json!({ "id": id, "description": description }))
            .collect();
        self.waiting_for_background = !tasks.is_empty();
        self.emit(json!({ "type": "turn.waiting", "tasks": tasks }))
            .await
    }

    async fn on_user(&mut self, value: &Value) -> Result<()> {
        let id = self.chat_id(text(value, "uuid")).to_owned();
        if self.submitted.contains(&id) {
            self.consumed.insert(id.clone());
            self.deliver(&id).await?;
        }
        if value["origin"]["kind"] == "task-notification" {
            self.background_replayed = true;
        }
        let blocks = value["message"]["content"].as_array().into_iter().flatten();
        let results = blocks
            .clone()
            .filter(|block| block["type"] == "tool_result")
            .count();
        for block in blocks {
            if block["type"] != "tool_result" {
                continue;
            }
            let id = text(block, "tool_use_id");
            let Some(mut item) = self.tools.remove(id) else {
                continue;
            };
            if item["type"] == "subagent" {
                // Claude Code describes the outcome once per message, beside its only result.
                let outcome = if results == 1 {
                    &value["tool_use_result"]
                } else {
                    &Value::Null
                };
                self.subagent_result(item, block, outcome).await?;
                continue;
            }
            let environment_failed = shell_environment_failed(&item, block);
            let status = if block["is_error"] == true {
                "failed"
            } else {
                "completed"
            };
            item["status"] = status.into();

            if item["tool"] == "TaskStop" && block["is_error"] != true {
                // TaskStop can finish without replaying a task-notification
                // turn. Its successful result settles that notification wait.
                let arguments = &item["arguments"];
                let task = arguments["task_id"]
                    .as_str()
                    .or_else(|| arguments["shell_id"].as_str())
                    .unwrap_or_default();
                self.awaiting_background.remove(task);
            }

            let content = &block["content"];
            // The raw spawn error includes environment exports, which can contain
            // secrets. Persist only the recovery message.
            if environment_failed {
                let output = if item["type"] == "command_execution" {
                    "aggregated_output"
                } else {
                    "result"
                };
                item[output] = SHELL_ENVIRONMENT_ERROR.into();
            } else if item["type"] == "command_execution" {
                item["aggregated_output"] = if content.is_string() {
                    content.clone()
                } else {
                    content.to_string().into()
                };
            } else if item["type"] != "todo_list" {
                item["result"] = content.clone();
            }
            self.emit(json!({ "type": "item.completed", "item": item }))
                .await?;
            if environment_failed {
                return Err(Error::bad_gateway(SHELL_ENVIRONMENT_ERROR));
            }
        }
        Ok(())
    }

    // Claude Code emits one assistant event per content block, all sharing the message id.
    // Count blocks per message so ids match the streamed block index. A subagent's messages name
    // the tool use that started it: they show as its steps, never as the agent's own reply.
    async fn on_assistant(&mut self, value: &Value) -> Result<()> {
        let parent = value["parent_tool_use_id"].as_str();
        if parent.is_none() {
            // The agent responds again, so it no longer only waits for background tasks.
            self.waiting_for_background = false;
        }
        let message = text(&value["message"], "id").to_owned();
        for block in value["message"]["content"].as_array().into_iter().flatten() {
            let next = self.blocks.entry(message.clone()).or_insert(0);
            let index = *next;
            *next += 1;
            let Some(mut item) = item(block, &message, index) else {
                continue;
            };
            if let Some(parent) = parent {
                item["parent_id"] = parent.into();
                if item["type"] == "agent_message" {
                    item["type"] = "subagent_message".into();
                }
            }
            if item["type"] == "agent_message" {
                self.last = text(&item, "text").into();
                self.last_id = text(&item, "id").into();
            }
            if item["type"] == "subagent" {
                self.subagents
                    .insert(text(&item, "id").into(), item.clone());
            }
            let running = block["type"] == "tool_use";
            if running {
                self.tools.insert(text(&item, "id").into(), item.clone());
            }
            // The plan counts once Claude Code accepts it, with the call's result.
            if item["type"] == "todo_list" {
                continue;
            }
            let kind = if running {
                "item.started"
            } else {
                "item.completed"
            };
            self.emit(json!({ "type": kind, "item": item })).await?;
        }
        Ok(())
    }

    /// The tool result of a subagent: its report, or its launch when it keeps working in the
    /// background.
    async fn subagent_result(&mut self, item: Value, block: &Value, outcome: &Value) -> Result<()> {
        let id = text(&item, "id").to_owned();
        let mut item = self.subagents.remove(&id).unwrap_or(item);
        let launched = matches!(
            text(outcome, "status"),
            "async_launched" | "remote_launched"
        );
        if block["is_error"] == true {
            item["status"] = "failed".into();
            item["result"] = subagent_report(block, &Value::Null).into();
        } else if launched {
            item["background"] = true.into();
        } else {
            if item["status"] == "in_progress" {
                item["status"] = "completed".into();
            }
            item["result"] = subagent_report(block, outcome).into();
            for (source, target) in [
                ("totalDurationMs", "duration_ms"),
                ("totalTokens", "tokens"),
                ("totalToolUseCount", "tool_uses"),
            ] {
                if outcome[source].is_u64() {
                    item[target] = outcome[source].clone();
                }
            }
        }
        // A background subagent reports through its task notification. Until then, the tool
        // call is complete but the subagent is still working.
        if launched && item["status"] == "in_progress" {
            self.subagents.insert(id, item.clone());
        }
        self.emit(json!({ "type": "item.completed", "item": item }))
            .await
    }

    /// Progress and completion that Claude Code reports for a subagent while it works.
    async fn subagent_task(&mut self, value: &Value) -> Result<()> {
        let id = text(value, "tool_use_id");
        let Some(item) = self.subagents.get_mut(id) else {
            return Ok(());
        };
        let before = item.clone();
        if !text(value, "task_id").is_empty() {
            item["task_id"] = value["task_id"].clone();
        }
        let usage = &value["usage"];
        match text(value, "subtype") {
            "task_started" => {
                item["background"] = (value["is_backgrounded"] == true).into();
                if item["agent_type"].is_null() {
                    item["agent_type"] = value["subagent_type"].clone();
                }
            }
            "task_progress" => {
                item["activity"] = value["description"].clone();
                item["last_tool"] = value["last_tool_name"].clone();
            }
            _ => {
                item["status"] = match text(value, "status") {
                    "completed" => "completed",
                    "stopped" | "killed" | "cancelled" => "stopped",
                    _ => "failed",
                }
                .into();
                if text(item, "result").is_empty() {
                    item["result"] = value["summary"].clone();
                }
            }
        }
        for (source, target) in [
            ("duration_ms", "duration_ms"),
            ("total_tokens", "tokens"),
            ("tool_uses", "tool_uses"),
        ] {
            if usage[source].is_u64() {
                item[target] = usage[source].clone();
            }
        }
        if *item == before {
            return Ok(());
        }
        let item = item.clone();
        // A foreground subagent's tool result follows its notification and completes it.
        if item["status"] != "in_progress" && !self.tools.contains_key(id) {
            self.subagents.remove(id);
        }
        self.emit(json!({ "type": "item.updated", "item": item }))
            .await
    }

    async fn on_stream_event(&mut self, event: &Value) -> Result<()> {
        let index = event["index"].as_u64().unwrap_or(0) as usize;
        match text(event, "type") {
            "message_start" => {
                self.waiting_for_background = false;
                self.stream_message = text(&event["message"], "id").into();
                self.streams.clear();
            }
            "content_block_start" => {
                let Some(item) = item(&event["content_block"], &self.stream_message, index) else {
                    return Ok(());
                };
                if !["agent_message", "reasoning"].contains(&text(&item, "type")) {
                    return Ok(());
                }
                self.streams.insert(index, item.clone());
                self.emit(json!({ "type": "item.started", "item": item }))
                    .await?;
            }
            "content_block_delta" => {
                let Some(item) = self.streams.get_mut(&index) else {
                    return Ok(());
                };
                let delta = &event["delta"];
                let delta = if delta["type"] == "thinking_delta" {
                    text(delta, "thinking")
                } else {
                    text(delta, "text")
                };
                item["text"] = format!("{}{delta}", text(item, "text")).into();
                let item = item.clone();
                self.emit(json!({ "type": "item.updated", "item": item }))
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn on_control_request(&mut self, value: &Value) -> Result<()> {
        let request = &value["request"];
        let request_id = value["request_id"].clone();
        let can_use_tool = request["subtype"] == "can_use_tool";
        if can_use_tool && request["tool_name"] == "AskUserQuestion" {
            return self.ask(value, request_id).await;
        }
        let allowed =
            can_use_tool && allowed_tool(self.plan, text(request, "tool_name"), &request["input"]);
        let response = if allowed {
            json!({ "behavior": "allow", "updatedInput": request["input"] })
        } else {
            json!({ "behavior": "deny", "message": DENIED })
        };
        send(&mut self.stdin, control_response(&request_id, &response)).await
    }

    async fn ask(&mut self, value: &Value, request_id: Value) -> Result<()> {
        let input = &value["request"]["input"];
        let id = crate::auth::hex_digest(&format!(
            "claude:{}:{}",
            self.initial_id,
            text(value, "request_id")
        ));
        let fields = input["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, question)| {
                let options = question["options"].as_array().cloned().unwrap_or_default();
                json!({ "id": index.to_string(), "title": question["question"], "options": options })
            })
            .collect::<Vec<_>>();
        let fields = crate::validation::parse("questions", fields.into())?;
        self.questions.insert(
            id.clone(),
            Question {
                request_id,
                input: input.clone(),
            },
        );
        self.emit(json!({
            "type": "chat.question",
            "question": { "id": id, "blocking": true, "fields": fields },
        }))
        .await
    }

    /// The messages a result settles. A result belongs to a turn, not one stdin message:
    /// Claude can merge prompts and emit unrelated results while resuming tasks.
    fn settled(&self, value: &Value) -> HashSet<String> {
        if value["user_message_uuids"].is_array() {
            return strings(&value["user_message_uuids"])
                .map(str::to_owned)
                .collect();
        }
        if let Some(id) = value["user_message_uuid"].as_str() {
            return HashSet::from([id.to_owned()]);
        }
        if value["origin"]["kind"] == "task-notification" {
            return HashSet::new();
        }
        self.consumed.clone()
    }

    async fn on_result(&mut self, value: &Value) -> Result<bool> {
        let result = text(value, "result");
        if value["is_error"] == true || value["subtype"] != "success" {
            let message = if result.is_empty() {
                "Claude Code could not complete this turn. Check sign-in, model access, or usage limits."
            } else {
                result
            };
            return Err(Error::bad_gateway(message));
        }
        if !result.is_empty() {
            self.last = result.into();
        }
        for id in self.settled(value) {
            let id = self.chat_id(&id).to_owned();
            if self.pending.remove(&id) {
                self.deliver(&id).await?;
            }
        }
        self.consumed.retain(|id| self.pending.contains(id));
        if self.background_replayed || value["origin"]["kind"] == "task-notification" {
            self.awaiting_background
                .retain(|id| self.background.contains_key(id));
        }
        self.background_replayed = false;
        let waiting = !self.pending.is_empty()
            || !self.background.is_empty()
            || !self.awaiting_background.is_empty();
        if waiting {
            // A result without pending messages means the agent stopped responding
            // and now waits for its background tasks to notify it.
            if self.pending.is_empty() && !self.background.is_empty() {
                self.announce_waiting().await?;
            }
            return Ok(false);
        }
        self.complete().await?;
        Ok(true)
    }

    async fn complete(&self) -> Result<()> {
        let receipt = Receipt {
            message_id: self.initial_id.into(),
            delivered: self.delivered.iter().cloned().collect(),
            text: self.last.clone(),
            item_id: self.last_id.clone(),
        };
        // Save the receipt before completion so restart cannot repeat a finished request.
        atomic_write(&self.receipt, &serde_json::to_vec(&receipt)?).await?;
        atomic_write(Path::new(text(self.plan, "output")), self.last.as_bytes()).await?;
        self.emit(json!({ "type": "chat.question.closed" })).await?;
        self.emit(json!({ "type": "turn.completed" })).await
    }

    async fn execute(
        &mut self,
        stdout: &mut BufReader<ChildStdout>,
        mut auth: Option<&mut crate::accounts::claude::Client>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        self.submit_initial().await?;
        let mut buffer = Vec::new();
        let mut timer = tokio::time::interval(Duration::from_millis(250));
        let mut auth_timer = tokio::time::interval(Duration::from_secs(30));
        auth_timer.tick().await;
        loop {
            // read_until is cancellation safe. Bound allocation using fill_buf below.
            tokio::select! {
                () = cancel.cancelled() => return Err(Error::conflict(STOPPED)),
                _ = auth_timer.tick(), if auth.is_some() => {
                    let client = auth.as_deref_mut().unwrap();
                    tokio::select! {
                        () = cancel.cancelled() => return Err(Error::conflict(STOPPED)),
                        result = client.sync() => result?,
                    }
                }
                _ = timer.tick() => self.poll_inbox().await?,
                bytes = stdout.fill_buf() => {
                    let (count, complete) = take_line(bytes?, &mut buffer)?;
                    stdout.consume(count);
                    if !complete {
                        continue;
                    }
                    let value: Value = serde_json::from_slice(&buffer).map_err(|_| {
                        Error::bad_gateway("Claude Code returned an invalid streaming event.")
                    })?;
                    buffer.clear();
                    if self.handle(&value).await? {
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Appends the next line's bytes to `buffer`. Answers how many bytes were read and whether
/// the line is complete.
fn take_line(bytes: &[u8], buffer: &mut Vec<u8>) -> Result<(usize, bool)> {
    if bytes.is_empty() {
        return Err(Error::bad_gateway(
            "Claude Code disconnected before completing its response. Resume to continue.",
        ));
    }
    let end = bytes.iter().position(|b| *b == b'\n').map(|i| i + 1);
    let count = end.unwrap_or(bytes.len());
    if buffer.len() + count > MAX_LINE {
        return Err(Error::bad_gateway(
            "Claude response exceeded the supported limit.",
        ));
    }
    buffer.extend_from_slice(&bytes[..count]);
    Ok((count, end.is_some()))
}

pub async fn run(
    config: &Config,
    plan: Value,
    events: mpsc::Sender<Value>,
    cancel: CancellationToken,
) -> Result<()> {
    let directory = std::env::var("CLAUDE_CONFIG_DIR")
        .map_or_else(|_| config.home.join(".claude"), PathBuf::from);
    let inbox = Path::new(text(&plan, "inputDirectory"));
    let receipt = Path::new(text(&plan, "output")).with_extension("claude-receipt.json");
    let initial_id = text(&plan["execution"], "messageId");
    if let Some(saved) = Receipt::read(&receipt, initial_id).await {
        return saved.replay(&plan, &events).await;
    }

    // Completed receipts above do not launch Claude and must remain replayable.
    if let Some(session) = plan["sessionId"].as_str() {
        validate_session_environment(&directory, session).await?;
    }
    // Claude acknowledges but does not execute UUIDs already in a resumed session.
    // Keep the durable chat ID while giving each continuation a fresh wire ID.
    let wire_id = if plan["sessionId"].is_string() {
        crate::config::id()
    } else {
        initial_id.to_owned()
    };
    // Native nodes put the writable secure store outside their journal, while
    // the full user/session directory stays durable. VM runs retain their home.
    let credentials = plan["claudeCredentialsDirectory"]
        .as_str()
        .map_or_else(|| directory.clone(), PathBuf::from);
    let mut auth = if plan["claudeManagedAuth"] == true {
        let mut client = crate::accounts::claude::Client::new(&credentials);
        client.sync().await?;
        Some(client)
    } else {
        None
    };
    // Linux limits each argument independently of the total command size. Keep
    // large skill instructions in a private file for this process's lifetime.
    let instructions = tempfile::NamedTempFile::new()?;
    tokio::fs::write(instructions.path(), text(&plan, "instructions")).await?;

    let mut cmd = command(
        &config.claude_bin,
        &args(&plan, instructions.path()),
        &claude::environment(config, &directory),
        Some(Path::new(text(&plan, "cwd"))),
    );
    if auth.is_some() {
        cmd.env("CLAUDE_SECURESTORAGE_CONFIG_DIR", &credentials);
    }
    cmd.stdin(std::process::Stdio::piped()).process_group(0);
    let mut child = cmd.spawn().map_err(|error| {
        Error::unavailable(format!(
            "Unable to start Claude Code: {error}. Check the server installation."
        ))
    })?;
    let pid = child.id().unwrap();
    let stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut stderr = child.stderr.take().unwrap();
    let drain = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
    });
    let mut turn = Turn {
        plan: &plan,
        events,
        stdin,
        inbox,
        receipt,
        initial_id,
        wire_id,
        submitted: HashSet::from([initial_id.to_owned()]),
        operational_notices: HashSet::new(),
        delivered: HashSet::new(),
        pending: HashSet::from([initial_id.to_owned()]),
        consumed: HashSet::new(),
        questions: HashMap::new(),
        tools: HashMap::new(),
        subagents: HashMap::new(),
        streams: HashMap::new(),
        blocks: HashMap::new(),
        stream_message: String::new(),
        last: String::new(),
        last_id: format!("{initial_id}-result"),
        background: BTreeMap::new(),
        awaiting_background: HashSet::new(),
        background_replayed: false,
        waiting_for_background: false,
    };
    let operation = turn.execute(&mut stdout, auth.as_mut(), &cancel).await;
    // Closes Claude Code's stdin.
    drop(turn);
    stop(&mut child, pid).await;
    drain.abort();
    let _ = drain.await;
    operation
}

/// Stops Claude Code's process group, forcefully after three seconds.
async fn stop(child: &mut tokio::process::Child, pid: u32) {
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
}

fn allowed_tool(plan: &Value, name: &str, input: &Value) -> bool {
    if plan["sandbox"] == "yolo" {
        return true;
    }
    let read_only = matches!(
        name,
        "Bash" | "Read" | "Glob" | "Grep" | "WebSearch" | "WebFetch" | "TodoWrite"
    );
    if read_only || name.starts_with("mcp__") {
        return true;
    }
    let writes = matches!(name, "Write" | "Edit" | "NotebookEdit");
    if plan["sandbox"] != "workspace-write" || !writes {
        return false;
    }
    let key = if name == "NotebookEdit" {
        "notebook_path"
    } else {
        "file_path"
    };
    let Some(path) = resolved(Path::new(text(input, key))) else {
        return false;
    };
    strings(&plan["writableRoots"]).any(|root| path.starts_with(root))
}

/// `path` with its symlinks resolved, or its parent's when the file does not exist yet.
fn resolved(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok().or_else(|| {
        let parent = std::fs::canonicalize(path.parent()?).ok()?;
        Some(parent.join(path.file_name().unwrap_or_default()))
    })
}
