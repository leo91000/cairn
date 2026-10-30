//! Adapter for the unmodified Claude Code streaming CLI.
use crate::{
    claude,
    config::Config,
    context_usage::ContextUsage,
    error::{Error, Result},
    process::command,
    skills::atomic_write,
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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

/// Whether this Claude Code release was qualified to answer side questions with every tool
/// denied before it runs (ADR-0078). Earlier or unknown releases never receive them.
fn answers_side_questions(version: &str) -> bool {
    // Build metadata does not change a release; a pre-release precedes it, so only one of a
    // later release qualifies.
    let release = version.split('+').next().unwrap_or_default();
    let (core, pre_release) = match release.split_once('-') {
        Some((core, _)) => (core, true),
        None => (release, false),
    };
    let core = core
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>();
    let qualified = vec![2, 1, 308];
    core.is_some_and(|core| {
        if pre_release {
            core > qualified
        } else {
            core >= qualified
        }
    })
}

/// Prefix of the context usage requests this adapter sends Claude Code.
const CONTEXT_REQUEST: &str = "cairn-context-";

/// How long a finished execution waits for the context report requested at a compaction
/// boundary. Claude Code answers it locally, after the command's result.
const CONTEXT_REPORT_WAIT: Duration = Duration::from_secs(5);

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

/// An execution that compacts the native context instead of delivering a message.
fn compacting(plan: &Value) -> bool {
    plan["execution"]["operation"] == "compact"
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
    denied.push("AskUserQuestion".into());
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

/// The subscription usage limit Claude Code reports in a `rate_limit_event`.
#[derive(Debug, PartialEq)]
struct UsageLimit {
    status: &'static str,
    window: Option<String>,
    /// Milliseconds since the epoch.
    resets_at: Option<i64>,
    extra_usage: Option<&'static str>,
    using_extra_usage: bool,
}

impl UsageLimit {
    fn from(info: &Value) -> Option<Self> {
        let status = |value: &Value| match value.as_str()? {
            "allowed" => Some("allowed"),
            "allowed_warning" => Some("allowed_warning"),
            "rejected" => Some("rejected"),
            _ => None,
        };
        let window = text(info, "rateLimitType");
        let window = (!window.is_empty()
            && window.len() <= 64
            && window
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_'))
        .then(|| window.to_owned());
        // Claude Code reports seconds; accept milliseconds from a future CLI too.
        let resets_at = info["resetsAt"]
            .as_f64()
            .filter(|reset| reset.is_finite() && *reset > 0.0)
            .map(|reset| if reset < 1e12 { reset * 1000.0 } else { reset } as i64);
        Some(Self {
            status: status(&info["status"])?,
            window,
            resets_at,
            extra_usage: status(&info["overageStatus"]),
            using_extra_usage: info["isUsingOverage"] == true || info["overageInUse"] == true,
        })
    }

    /// Requests stop until the window resets: extra usage cannot take over.
    fn blocks(&self) -> bool {
        self.status == "rejected"
            && !matches!(self.extra_usage, Some("allowed" | "allowed_warning"))
    }

    fn event(&self) -> Value {
        json!({
            "type": "provider.limit",
            "status": self.status,
            "window": self.window,
            "resetsAt": self.resets_at,
            "blocked": self.blocks(),
            "usingExtraUsage": self.using_extra_usage,
        })
    }
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
    /// Running tools, by tool use ID.
    tools: HashMap<String, Value>,
    /// Subagents still working, by the ID of the tool use that started them.
    subagents: HashMap<String, Value>,
    /// Subagents that reported back, by the same ID, until a new run resumes them.
    settled: HashMap<String, Value>,
    /// The ID of each subagent, by the task ID Claude Code keeps across its runs.
    tasks: HashMap<String, String>,
    /// The usage limit Claude Code last reported, unless requests run normally.
    limit: Option<UsageLimit>,
    /// Subagents by the tool use that resumed them, such as a SendMessage call: before 2.1.292,
    /// a resumed subagent's messages may name that call rather than an agent ID.
    resumed_by: HashMap<String, String>,
    /// Every tool use of this execution, by tool name, including those that already returned.
    called: HashMap<String, String>,
    /// Whether a response streamed since the usage limit was reported: requests went through.
    answered: bool,
    /// The error Claude Code gave the agent's latest message: `rate_limit` when the usage limit
    /// refused its request.
    api_error: String,
    /// Tool uses whose progress notice was reported: Claude Code repeats it while the tool runs.
    tool_notices: HashSet<String>,
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
    /// Context usage requests sent to Claude Code.
    context_requests: u64,
    /// Requests up to this number were sent before the latest measured response, so their
    /// reports describe an older context.
    context_measured: u64,
    /// The report requested at a compaction boundary, until it answers or a response measures
    /// the compacted context.
    context_pending: Option<u64>,
    /// The execution finished and only waits for [`Self::context_pending`], until this time.
    finishing: Option<tokio::time::Instant>,
    /// The model's context window, from Claude Code's latest context usage report.
    context_window: Option<u64>,
    /// The latest context usage shown, so unchanged reports are not repeated.
    context: Option<ContextUsage>,
    /// The compaction Claude Code is running, by the ID of its chat item.
    compaction: Option<String>,
    /// Compactions Claude Code reported during this execution.
    compactions: usize,
    /// Compactions Claude Code started or reported during this execution, each with its own
    /// item: a resumed or retried compaction never reuses an earlier attempt's divider.
    compaction_attempts: usize,
    /// Identifies this launch's compaction items.
    attempt: String,
    /// Side questions Claude Code is answering, by control request ID.
    sides: HashMap<String, String>,
    /// This Claude Code release announced side questions; it takes none otherwise.
    side_available: bool,
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
        if compacting(self.plan) {
            // Claude Code runs its own `/compact` command locally: it is never sent to the
            // model as a user request, and a restarted attempt simply compacts again.
            let command = json!({ "id": self.wire_id, "text": "/compact", "attachments": [] });
            let command = input(&command, self.inbox).await?;
            return send(&mut self.stdin, command).await;
        }
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

    /// Asks Claude Code how full its context is. Answering does not call the model.
    async fn request_context(&mut self) -> Result<()> {
        self.context_requests += 1;
        let request = json!({
            "type": "control_request",
            "request_id": format!("{CONTEXT_REQUEST}{}", self.context_requests),
            "request": { "subtype": "get_context_usage", "detail": "summary" },
        });
        send(&mut self.stdin, request).await
    }

    async fn show_context(&mut self, usage: Option<ContextUsage>) -> Result<()> {
        let Some(usage) = usage else {
            return Ok(());
        };
        if self.context.as_ref() == Some(&usage) {
            return Ok(());
        }
        let event = usage.event();
        self.context = Some(usage);
        self.emit(event).await
    }

    /// Claude Code's answer to a side question or a context usage request. Answers whether the
    /// execution is now complete.
    async fn on_control_response(&mut self, value: &Value) -> Result<bool> {
        let response = &value["response"];
        if let Some(question) = self.sides.remove(text(response, "request_id")) {
            self.on_side_answer(&question, response).await?;
            return Ok(false);
        }
        let Some(number) = text(response, "request_id")
            .strip_prefix(CONTEXT_REQUEST)
            .and_then(|number| number.parse::<u64>().ok())
        else {
            return Ok(false);
        };

        if response["subtype"] == "success" {
            let report = &response["response"];
            if let Some(window) = report["maxTokens"].as_u64().filter(|window| *window > 0) {
                self.context_window = Some(window);
            }
            // A response measured after this request was sent is more recent than its report.
            if number > self.context_measured {
                self.show_context(ContextUsage::claude_report(report))
                    .await?;
            }
        }
        if self.context_pending == Some(number) {
            self.context_pending = None;
        }
        self.finish_after_report().await
    }

    /// Completes a finished execution once the compaction's context report answered, or once
    /// waiting for it took too long. The run then keeps the last usage it showed.
    async fn finish_after_report(&mut self) -> Result<bool> {
        let Some(deadline) = self.finishing else {
            return Ok(false);
        };
        if self.context_pending.is_some() && tokio::time::Instant::now() < deadline {
            return Ok(false);
        }
        self.complete().await?;
        Ok(true)
    }

    /// A new compaction item's ID, unique to this attempt.
    fn compaction_id(&mut self) -> String {
        self.compaction_attempts += 1;
        format!(
            "{}-compaction-{}-{}",
            self.initial_id, self.attempt, self.compaction_attempts
        )
    }

    /// `status` while Claude Code compacts, and the `compact_boundary` that ends it.
    async fn on_compaction(&mut self, value: &Value) -> Result<()> {
        if value["subtype"] == "status" {
            if value["status"] == "compacting" && self.compaction.is_none() {
                let id = self.compaction_id();
                self.compaction = Some(id.clone());
                let item =
                    json!({ "id": id, "type": "context_compaction", "status": "in_progress" });
                self.emit(json!({ "type": "item.started", "item": item }))
                    .await?;
            }
            let failed = value["compact_result"]
                .as_str()
                .is_some_and(|result| result != "success");
            if failed && let Some(id) = self.compaction.take() {
                let item = json!({ "id": id, "type": "context_compaction", "status": "failed" });
                self.emit(json!({ "type": "item.completed", "item": item }))
                    .await?;
            }
            return Ok(());
        }

        self.compactions += 1;
        let id = match self.compaction.take() {
            Some(id) => id,
            None => self.compaction_id(),
        };
        let metadata = &value["compact_metadata"];
        let mut item = json!({
            "id": id,
            "type": "context_compaction",
            "status": "completed",
        });
        if matches!(text(metadata, "trigger"), "manual" | "auto") {
            item["trigger"] = metadata["trigger"].clone();
        }
        for (source, target) in [("pre_tokens", "pre_tokens"), ("post_tokens", "post_tokens")] {
            if metadata[source].is_u64() {
                item[target] = metadata[source].clone();
            }
        }
        self.emit(json!({ "type": "item.completed", "item": item }))
            .await?;
        // The next response measures the compacted context; until then show Claude Code's estimate.
        self.request_context().await?;
        self.context_pending = Some(self.context_requests);
        Ok(())
    }

    /// Submits messages the user added to the inbox, or answers to pending questions.
    async fn poll_inbox(&mut self) -> Result<()> {
        // Messages sent while compacting stay queued for the next turn.
        if compacting(self.plan) {
            return Ok(());
        }
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
            let side =
                message["sideQuestion"].is_object() || message["sideQuestionCancelled"] == true;
            if side && !self.side_available {
                continue;
            }
            if message["sideQuestionCancelled"] == true {
                self.withdraw_side(text(&message, "sideQuestionId")).await?;
                self.submitted.insert(id.into());
                continue;
            }
            if message["sideQuestion"].is_object() {
                self.ask_side(&message["sideQuestion"]).await?;
                self.submitted.insert(id.into());
                continue;
            }
            if message["questionCancelled"] == true {
                self.submitted.insert(id.into());
                continue;
            }
            send(&mut self.stdin, input(&message, self.inbox).await?).await?;
            self.submitted.insert(id.into());
            self.pending.insert(id.into());
            self.waiting_for_background = false;
        }
        Ok(())
    }

    /// Claude Code's native side question: answered from the live conversation,
    /// including the running turn, with every tool denied and nothing written to
    /// the session transcript. The main turn continues meanwhile.
    async fn ask_side(&mut self, side: &Value) -> Result<()> {
        let question = text(side, "id").to_owned();
        let request_id = format!("side-question-{question}");
        let request = json!({
            "type": "control_request",
            "request_id": request_id,
            "request": {
                "subtype": "side_question",
                "question": side["text"],
            },
        });
        send(&mut self.stdin, request).await?;
        self.sides.insert(request_id, question);
        Ok(())
    }

    /// Withdraws the side questions Claude Code is still answering when the reply ends:
    /// they never delay it, and can be asked again during the next one.
    async fn withdraw_sides(&mut self) -> Result<()> {
        for (request, question) in std::mem::take(&mut self.sides) {
            let cancel = json!({ "type": "control_cancel_request", "request_id": request });
            send(&mut self.stdin, cancel).await?;
            self.side_failed(&question, crate::chats::side_questions::EXPIRED)
                .await?;
        }
        Ok(())
    }

    async fn withdraw_side(&mut self, question: &str) -> Result<()> {
        let request = self
            .sides
            .iter()
            .find(|(_, id)| *id == question)
            .map(|(request, _)| request.clone());
        if let Some(request) = request {
            self.sides.remove(&request);
            let cancel = json!({ "type": "control_cancel_request", "request_id": request });
            send(&mut self.stdin, cancel).await?;
        }
        Ok(())
    }

    /// The reply to a side question. Claude reports a missing or synthetic answer
    /// (its own fallback text) when it could not answer.
    async fn on_side_answer(&mut self, question: &str, response: &Value) -> Result<()> {
        let answer = &response["response"];
        let answered = response["subtype"] == "success"
            && answer["synthetic"] != true
            && !text(answer, "response").trim().is_empty();
        if answered {
            return self
                .emit(json!({
                    "type": "chat.side.answered",
                    "sideQuestionId": question,
                    "text": answer["response"],
                }))
                .await;
        }
        // A synthetic answer is Claude Code's own explanation, such as a tool it refused.
        let explanation = text(answer, "response")
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')');
        let error = if response["subtype"] == "error" && !text(response, "error").is_empty() {
            text(response, "error")
        } else if answer["synthetic"] == true && !explanation.is_empty() {
            explanation
        } else {
            "Claude Code could not answer this side question."
        };
        self.side_failed(question, error).await
    }

    async fn side_failed(&self, question: &str, error: &str) -> Result<()> {
        self.emit(json!({
            "type": "chat.side.failed",
            "sideQuestionId": question,
            "error": error,
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
                let release = text(value, "claude_code_version");
                self.side_available = answers_side_questions(release);
                if self.side_available {
                    self.emit(json!({ "type": "chat.side.available" })).await?;
                } else {
                    // Says why, instead of leaving the reply "still starting".
                    self.emit(json!({ "type": "chat.side.unavailable", "release": release }))
                        .await?;
                }
            }
            "system" if value["subtype"] == "background_tasks_changed" => {
                self.background_tasks(value).await?;
            }
            "system"
                if value["subtype"] == "compact_boundary"
                    || (value["subtype"] == "status"
                        && (value["status"] == "compacting"
                            || value["compact_result"].is_string())) =>
            {
                self.on_compaction(value).await?;
            }
            "system"
                if matches!(
                    text(value, "subtype"),
                    "task_started" | "task_progress" | "task_notification"
                ) =>
            {
                self.subagent_task(value).await?;
            }
            "system" if value["subtype"] == "api_retry" => self.engine_retry(value).await?,
            "system" if value["subtype"] == "informational" => self.engine_notice(value).await?,
            // Opt-in (CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS) and not enabled: completion stays
            // correlated with results, and Cairn answers permission requests itself.
            "system" if value["subtype"] == "session_state_changed" => {}
            "rate_limit_event" => self.rate_limit(&value["rate_limit_info"]).await?,
            "user" => self.on_user(value).await?,
            "assistant" => self.on_assistant(value).await?,
            "stream_event" if value["parent_tool_use_id"].is_null() => {
                self.on_stream_event(&value["event"]).await?;
            }
            "control_request" => self.on_control_request(value).await?,
            "control_response" => return self.on_control_response(value).await,
            "result" => return self.on_result(value).await,
            _ => {}
        }
        Ok(false)
    }

    /// Claude Code retries a failed API request itself. The turn goes on: this is not a failure,
    /// and nothing for Cairn to retry or wait for.
    async fn engine_retry(&self, value: &Value) -> Result<()> {
        let delay = value["retry_delay_ms"].as_u64().unwrap_or(0);
        let reason = text(value, "error");
        let reason = if !reason.is_empty()
            && reason.len() <= 64
            && reason
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        {
            reason
        } else {
            "unknown"
        };
        let mut retry = json!({
            "type": "engine.retry",
            "attempt": value["attempt"].as_u64(),
            "maxRetries": value["max_retries"].as_u64(),
            "delayMs": delay,
            "retryAt": crate::config::now().saturating_add_unsigned(delay),
            "status": value["error_status"].as_u64(),
            "reason": reason,
        });
        let no_response = &value["no_response"];
        if no_response.is_object() {
            retry["noResponse"] = json!({
                "waitedMs": no_response["waited_ms"].as_u64(),
                "retryWaitMs": no_response["retry_wait_ms"].as_u64(),
            });
        }
        self.emit(retry).await
    }

    /// A warning or notice Claude Code raised during the turn.
    async fn engine_notice(&mut self, value: &Value) -> Result<()> {
        let message = text(value, "content");
        if message.trim().is_empty() {
            return Ok(());
        }
        let level = match text(value, "level") {
            level @ ("info" | "notice" | "suggestion" | "warning") => level,
            _ => "notice",
        };
        // Informational lines naming a tool use report its progress: the first one says it is
        // running. A warning, a suggestion or a notice that stops the turn is always reported.
        let tool_use = text(value, "tool_use_id");
        let progress = matches!(level, "info" | "notice") && value["prevent_continuation"] != true;
        if progress && !tool_use.is_empty() && !self.tool_notices.insert(tool_use.to_owned()) {
            return Ok(());
        }
        // The worker cuts a long notice once it is redacted.
        let mut notice = json!({ "type": "engine.notice", "level": level, "message": message });
        if value["prevent_continuation"] == true {
            notice["stopsTurn"] = true.into();
        }
        self.emit(notice).await
    }

    /// The subscription's usage-limit state. Only changes are shown: a warning, a limit that
    /// stops requests, or a return to normal after either.
    async fn rate_limit(&mut self, info: &Value) -> Result<()> {
        let Some(limit) = UsageLimit::from(info) else {
            return Ok(());
        };
        if limit.status == "allowed" && self.limit.is_none() {
            return Ok(());
        }
        if self.limit.as_ref() == Some(&limit) {
            // While the limit refuses requests, Claude Code re-emits it (at most about every 30
            // seconds per window): a refusal after an answered request. One within that time
            // falls back to recognizing Claude's wording.
            if limit.blocks() {
                self.answered = false;
            }
            return Ok(());
        }
        self.emit(limit.event()).await?;
        self.limit = (limit.status != "allowed").then_some(limit);
        self.answered = false;
        Ok(())
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

    /// The subagent a message comes from, or None for the agent's own. Claude Code names the
    /// tool use that started it and, from 2.1.292, its agent ID (absent on the agent's own
    /// messages): the task ID it keeps when it is resumed, so a resumed subagent's steps stay
    /// with it.
    fn parent(&self, value: &Value) -> Option<String> {
        let agent = text(value, "agent_id");
        if let Some(id) = self.tasks.get(agent) {
            return Some(id.clone());
        }
        let tool_use = value["parent_tool_use_id"].as_str();
        if let Some(id) = tool_use.and_then(|tool_use| self.resumed_by.get(tool_use)) {
            return Some(id.clone());
        }
        tool_use
            .or_else(|| (!agent.is_empty()).then_some(agent))
            .map(str::to_owned)
    }

    // Claude Code emits one assistant event per content block, all sharing the message id.
    // Count blocks per message so ids match the streamed block index. A subagent's messages
    // show as its steps, never as the agent's own reply.
    async fn on_assistant(&mut self, value: &Value) -> Result<()> {
        let parent = self.parent(value);
        if parent.is_none() {
            // The agent responds again, so it no longer only waits for background tasks.
            self.waiting_for_background = false;
            self.api_error = text(value, "error").to_owned();
            // Subagents have their own context. Local command replies are not model requests.
            let response = &value["message"];
            let usage = (response["model"] != "<synthetic>")
                .then(|| ContextUsage::claude_response(&response["usage"], self.context_window))
                .flatten();
            if let Some(usage) = usage {
                // Reports requested before this response describe the context it replaced.
                self.context_measured = self.context_requests;
                self.context_pending = None;
                self.show_context(Some(usage)).await?;
            }
        }
        let message = text(&value["message"], "id").to_owned();
        for block in value["message"]["content"].as_array().into_iter().flatten() {
            let next = self.blocks.entry(message.clone()).or_insert(0);
            let index = *next;
            *next += 1;
            let Some(mut item) = item(block, &message, index) else {
                continue;
            };
            if let Some(parent) = &parent {
                item["parent_id"] = parent.as_str().into();
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
                self.called
                    .insert(text(&item, "id").into(), text(block, "name").into());
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
        if let Some(agent) = outcome["agentId"]
            .as_str()
            .filter(|agent| !agent.is_empty())
        {
            self.tasks.insert(agent.to_owned(), id.clone());
            if item["task_id"].is_null() {
                item["task_id"] = agent.into();
            }
        }
        // The call that started the subagent can return after a later run resumed it: that run
        // reports for itself, whether it still works or already reported.
        if item["resumed"] == true {
            if item["status"] == "in_progress" {
                self.subagents.insert(id, item);
            } else {
                self.settled.insert(id, item);
            }
            return Ok(());
        }
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
        } else {
            self.settled.insert(id, item.clone());
        }
        self.emit(json!({ "type": "item.completed", "item": item }))
            .await
    }

    /// Progress and completion that Claude Code reports for a subagent while it works. A resumed
    /// subagent keeps its task ID, while its frames may name another tool use, or none.
    async fn subagent_task(&mut self, value: &Value) -> Result<()> {
        let task = text(value, "task_id");
        let tool_use = text(value, "tool_use_id");
        let id = if self.subagents.contains_key(tool_use) || self.settled.contains_key(tool_use) {
            tool_use.to_owned()
        } else if let Some(id) = self.tasks.get(task) {
            id.clone()
        } else {
            return self.subagent_resumed(value).await;
        };
        if !task.is_empty() {
            self.tasks.insert(task.to_owned(), id.clone());
        }
        if !tool_use.is_empty() && tool_use != id {
            self.resumed_by.insert(tool_use.to_owned(), id.clone());
        }
        let started = value["subtype"] == "task_started";
        if !self.subagents.contains_key(&id) {
            // Only a new run reopens a subagent that reported back: other frames, and another
            // start of the run that reported, are late and leave it settled.
            let run = text(value, "run_id");
            let reopens = started
                && self.settled.get(&id).is_some_and(|item| {
                    let reported = text(item, "run_id");
                    run.is_empty() || reported.is_empty() || run > reported
                });
            if !reopens {
                return Ok(());
            }
            let item = self.settled.remove(&id).unwrap();
            self.subagents.insert(id.clone(), item);
        }
        let item = self.subagents.get_mut(&id).unwrap();

        // Claude Code promises that a task's run IDs sort, by plain string comparison, in the
        // order its runs opened: a frame naming an earlier run is late.
        let run = text(value, "run_id");
        let current = text(item, "run_id").to_owned();
        if !run.is_empty() && !current.is_empty() && run < current.as_str() {
            return Ok(());
        }
        let before = item.clone();
        if !task.is_empty() {
            item["task_id"] = task.into();
        }
        if !run.is_empty() {
            item["run_id"] = run.into();
        }
        let usage = &value["usage"];
        match text(value, "subtype") {
            "task_started" => {
                // A start naming another run is a resumption, even before the earlier run's tool
                // result; another start of the current run is not. Frames without run IDs
                // (before 2.1.292) only start again once the subagent finished.
                let resumed = if !current.is_empty() && !run.is_empty() {
                    run != current
                } else {
                    before["status"] != "in_progress"
                };
                if resumed {
                    item["status"] = "in_progress".into();
                    item["resumed"] = true.into();
                    // Its new run reports again; until then the earlier report stays shown.
                    item["activity"] = Value::Null;
                    item["result"] = Value::Null;
                }
                item["background"] = (value["is_backgrounded"] == true).into();
                if item["agent_type"].is_null() {
                    item["agent_type"] = value["subagent_type"].clone();
                }
                if !text(value, "parent_task_id").is_empty() {
                    item["parent_task_id"] = value["parent_task_id"].clone();
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
        if item["status"] != "in_progress" && !self.tools.contains_key(&id) {
            self.subagents.remove(&id);
            self.settled.insert(id, item.clone());
        }
        self.emit(json!({ "type": "item.updated", "item": item }))
            .await
    }

    /// A subagent an earlier execution started, resumed in this one: by a SendMessage call, or
    /// by Claude Code itself. It keeps its task ID, but this execution never saw the tool use that
    /// started it, so it shows as a new row of this execution marked resumed. Its run names the
    /// SendMessage call or, when Claude Code resumed it, the earlier execution's call that
    /// started it. Another tool use of this execution, such as a workflow's, starts its own
    /// agents; a start naming no call is a new agent, such as a forked slash command. A call this
    /// execution never made only names a resumed subagent the agent itself started: a nested
    /// agent may start from work this execution does not see.
    async fn subagent_resumed(&mut self, value: &Value) -> Result<()> {
        let task = text(value, "task_id");
        let tool_use = text(value, "tool_use_id");
        let top_level = text(value, "parent_task_id").is_empty()
            && value["spawn_depth"].as_u64().is_none_or(|depth| depth <= 1);
        let resumes = match self.called.get(tool_use) {
            Some(tool) => tool == "SendMessage",
            None => !tool_use.is_empty() && top_level,
        };
        // Housekeeping agents Claude Code starts itself stay out of the transcript.
        if value["subtype"] != "task_started"
            || value["task_type"] != "local_agent"
            || value["skip_transcript"] == true
            || value["ambient"] == true
            || task.is_empty()
            || !resumes
        {
            return Ok(());
        }
        let input = json!({
            "description": value["description"],
            "subagent_type": value["subagent_type"],
            "prompt": value["prompt"],
        });
        // Clients key rows by run and item ID, and a run's later executions may resume the same
        // subagent again: each row gets its own ID, named after the subagent's run when known.
        let row = match text(value, "run_id") {
            "" => format!("{task}:{}", crate::config::id()),
            run => format!("{task}:{run}"),
        };
        let mut item = subagent(&row, &input);
        item["resumed"] = true.into();
        item["task_id"] = task.into();
        item["background"] = (value["is_backgrounded"] == true).into();
        for key in ["run_id", "parent_task_id"] {
            if !text(value, key).is_empty() {
                item[key] = value[key].clone();
            }
        }
        if let Some(parent) = self.tasks.get(text(value, "parent_task_id")) {
            item["parent_id"] = parent.as_str().into();
        }
        self.tasks.insert(task.to_owned(), row.clone());
        if !tool_use.is_empty() {
            self.resumed_by.insert(tool_use.to_owned(), row.clone());
        }
        self.subagents.insert(row, item.clone());
        self.emit(json!({ "type": "item.started", "item": item }))
            .await
    }

    async fn on_stream_event(&mut self, event: &Value) -> Result<()> {
        let index = event["index"].as_u64().unwrap_or(0) as usize;
        match text(event, "type") {
            "message_start" => {
                self.waiting_for_background = false;
                self.answered = true;
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
            return send(
                &mut self.stdin,
                control_response(
                    &request_id,
                    &json!({
                        "behavior": "deny",
                        "message": crate::chats::questions::NATIVE_DENIED,
                    }),
                ),
            )
            .await;
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
            // Claude Code said the usage limit stops requests, and refused the agent's request
            // for it: report it as such rather than leaving its wording to be recognized. A limit
            // it answered after (a limit on another model's window, or one a subagent hit), or
            // whose reset passed, does not explain the failure.
            let now = crate::config::now();
            if let Some(limit) = self.limit.as_ref().filter(|limit| {
                limit.blocks()
                    && self.api_error == "rate_limit"
                    && !self.answered
                    && limit.resets_at.is_none_or(|reset| reset > now)
            }) {
                self.emit(json!({
                    "type": "turn.failed",
                    "error": {
                        "message": message,
                        "code": "usage_limit_reached",
                        "window": limit.window,
                        "resetsAt": limit.resets_at,
                    },
                }))
                .await?;
            }
            return Err(Error::bad_gateway(message));
        }
        if compacting(self.plan) && self.compactions == 0 {
            // Claude Code answers `/compact` successfully even when it had nothing to compact.
            let message = if self.last.is_empty() {
                "Claude Code finished without compacting the conversation."
            } else {
                self.last.as_str()
            };
            return Err(Error::bad_gateway(message));
        }
        if !result.is_empty() && !compacting(self.plan) {
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
        self.withdraw_sides().await?;
        if self.context_pending.is_some() {
            // Claude Code answers the compaction's context request after its result: wait
            // briefly for it so the run shows the compacted context.
            self.finishing = Some(tokio::time::Instant::now() + CONTEXT_REPORT_WAIT);
            return Ok(false);
        }
        self.complete().await?;
        Ok(true)
    }

    async fn complete(&self) -> Result<()> {
        // A compaction has no reply to replay: a restart compacts again instead.
        if !compacting(self.plan) {
            let receipt = Receipt {
                message_id: self.initial_id.into(),
                delivered: self.delivered.iter().cloned().collect(),
                text: self.last.clone(),
                item_id: self.last_id.clone(),
            };
            // Save the receipt before completion so restart cannot repeat a finished request.
            atomic_write(&self.receipt, &serde_json::to_vec(&receipt)?).await?;
        }
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
        self.request_context().await?;
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
                _ = timer.tick() => {
                    if self.finish_after_report().await? {
                        return Ok(());
                    }
                    // A finished execution no longer takes messages.
                    if self.finishing.is_none() {
                        self.poll_inbox().await?;
                    }
                }
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

    if compacting(&plan) && !plan["sessionId"].is_string() {
        return Err(Error::conflict(
            "This conversation has no saved Claude Code session to compact.",
        ));
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
        tools: HashMap::new(),
        subagents: HashMap::new(),
        settled: HashMap::new(),
        tasks: HashMap::new(),
        limit: None,
        resumed_by: HashMap::new(),
        called: HashMap::new(),
        answered: false,
        api_error: String::new(),
        tool_notices: HashSet::new(),
        streams: HashMap::new(),
        blocks: HashMap::new(),
        stream_message: String::new(),
        last: String::new(),
        last_id: format!("{initial_id}-result"),
        background: BTreeMap::new(),
        awaiting_background: HashSet::new(),
        background_replayed: false,
        waiting_for_background: false,
        context_requests: 0,
        context_measured: 0,
        context_pending: None,
        finishing: None,
        context_window: None,
        context: None,
        compaction: None,
        compactions: 0,
        compaction_attempts: 0,
        attempt: crate::config::id(),
        sides: HashMap::new(),
        side_available: false,
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

#[cfg(test)]
mod side_question_release_tests {
    use super::answers_side_questions;

    #[test]
    fn only_releases_from_the_qualified_one_answer_side_questions() {
        for release in [
            "2.1.308",
            "2.1.310",
            "2.2.0",
            "2.1.310-beta.1",
            "3.0.0+build",
        ] {
            assert!(answers_side_questions(release), "{release}");
        }
        for release in [
            "2.1.280",
            "2.1.307",
            "2.1.307-rc",
            "2.1.308-rc.1",
            "2.1",
            "",
            "next",
        ] {
            assert!(!answers_side_questions(release), "{release}");
        }
    }
}
