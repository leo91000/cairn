use super::*;

/// Only the release tested against the native side-question protocol is enabled.
/// Other releases must be qualified before extending this gate (ADR-0078).
pub(super) fn answers_side_questions(version: &str) -> bool {
    version == "2.1.308"
}

/// Prefix of the context usage requests this adapter sends Claude Code.
pub(super) const CONTEXT_REQUEST: &str = "cairn-context-";

/// How long a finished execution waits for the context report requested at a compaction
/// boundary. Claude Code answers it locally, after the command's result.
pub(super) const CONTEXT_REPORT_WAIT: Duration = Duration::from_secs(5);

pub(super) const DENIED: &str =
    "This tool is outside the agent's access policy. Ask the user in your response.";

pub(super) const PLAN_DENIED: &str = "This is a plan turn: Cairn allows reading, searching and the plan file only. Include this step in your plan; the user starts implementation explicitly.";

pub(super) const PLAN_RECORDED: &str = "The plan was shown to the user. Stop here and summarize it briefly: the user starts implementation explicitly in a later turn.";

pub(super) const PLAN_MISSING: &str = "No plan reached the user: write your plan to the plan file named in the plan-mode instructions, then call ExitPlanMode again. The user starts implementation explicitly in a later turn.";

pub(super) const SANDBOX_UNAVAILABLE: &str = "Plan turns need Claude Code's sandbox, which is unavailable on this node (it requires bubblewrap and socat). Nothing was changed.";

pub(super) const SHELL_ENVIRONMENT_ERROR: &str = "Claude Code's session environment contains NUL bytes, so shell commands cannot start. Repair the affected .sh file in the Claude session-env directory; the next turn starts a fresh process. The conversation and workspace have been preserved.";

pub(super) async fn validate_session_environment(directory: &Path, session: &str) -> Result<()> {
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
pub(super) fn shell_environment_failed(item: &Value, block: &Value) -> bool {
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

pub(super) async fn send(stdin: &mut ChildStdin, value: Value) -> Result<()> {
    stdin.write_all(format!("{value}\n").as_bytes()).await?;
    Ok(())
}

pub(super) async fn cancel_side_request(stdin: &mut ChildStdin, request: &str) {
    let cancel = json!({
        "type": "control_cancel_request",
        "request_id": request,
    });
    // UUID-based cancellation frames are below PIPE_BUF, so a single poll either
    // writes the whole frame or writes nothing. Side work never waits for control
    // readiness; unsent cancellations rely on native completion or CLI shutdown.
    let _ = send(stdin, cancel).now_or_never();
}

pub(super) async fn emit(events: &mpsc::Sender<Value>, value: Value) -> Result<()> {
    events
        .send(value)
        .await
        .map_err(|_| Error::unavailable("Claude output stopped."))
}

/// An execution that compacts the native context instead of delivering a message.
pub(super) fn compacting(plan: &Value) -> bool {
    plan["execution"]["operation"] == "compact"
}

pub(super) fn delivered(id: &str) -> Value {
    json!({ "type": "chat.delivered", "messageId": id })
}

pub(super) fn control_response(request_id: &Value, response: &Value) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        },
    })
}

pub(super) fn image_type(path: &Path) -> &'static str {
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
pub(super) async fn input(message: &Value, directory: &Path) -> Result<Value> {
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

pub(super) fn strings(value: &Value) -> impl Iterator<Item = &str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

pub(super) fn settings(plan: &Value) -> Value {
    let mut settings = plan["claudeSettings"].clone();
    if !settings.is_object() {
        settings = json!({});
    }
    if !settings["attribution"].is_object() {
        settings["attribution"] = json!({});
    }
    settings["attribution"]["commit"] = "".into();
    let planning = plan["planning"] == true;
    if plan["sandbox"] == "yolo" && !planning {
        return settings;
    }
    let deny_write = if plan["sandbox"] == "read-only" {
        plan["writableRoots"].clone()
    } else {
        json!([])
    };
    // A plan turn's shell reaches no network: commands such as `git push` or `gh`
    // would otherwise act on external services.
    let domains = if planning { json!([]) } else { json!(["*"]) };
    settings["sandbox"] = json!({
        "enabled": true,
        "failIfUnavailable": true,
        "autoAllowBashIfSandboxed": true,
        "allowUnsandboxedCommands": false,
        "network": { "allowedDomains": domains },
        "filesystem": { "allowWrite": plan["writableRoots"], "denyWrite": deny_write },
    });
    if planning {
        // Claude Code 2.1.308 otherwise lets its auto-mode classifier approve plan-mode
        // tool calls instead of asking Cairn, including a command's own `allowed_domains`,
        // which then reach the network. Cairn's callback decides, and no command can widen
        // the empty allowlist.
        settings["useAutoModeDuringPlan"] = false.into();
        settings["permissions"] = json!({ "disableAutoMode": "disable" });
        settings["sandbox"]["network"]["strictAllowlist"] = true.into();
    }
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
    let planning = plan["planning"] == true;
    if planning {
        args.extend(["--permission-mode".into(), "plan".into()]);
    } else if plan["sandbox"] == "yolo" {
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
    if planning {
        // Claude Code writes its plan file under its own state directory, so the edit
        // tools stay available; these rules deny every workspace root before any
        // permission mode or callback applies. The callback denies other paths.
        denied.extend(strings(&plan["writableRoots"]).map(|root| format!("Edit(/{root}/**)")));
    } else if plan["sandbox"] == "read-only" {
        denied.extend(["Write", "Edit", "NotebookEdit"].map(str::to_owned));
    }
    if !denied.is_empty() {
        args.extend(["--disallowedTools".into(), denied.join(",")]);
    }
    args
}

/// The chat item a Claude content block shows as. `planning` is a Cairn plan turn, whose
/// boundary Cairn enforces.
pub fn item(block: &Value, message: &str, index: usize, planning: bool) -> Option<Value> {
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
        // Claude Code fills `plan` from the plan file the model wrote; without one there is
        // no proposal to show. Only a Cairn plan turn proposes a plan nothing has acted on:
        // in an ordinary turn Claude's own plan mode may implement right after, so the
        // call shows as an ordinary tool call.
        "tool_use"
            if planning
                && block["name"] == "ExitPlanMode"
                && !proposed_plan(&block["input"]).is_empty() =>
        {
            json!({
                "id": id,
                "type": "proposed_plan",
                "text": block["input"]["plan"],
                "status": "in_progress",
            })
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

/// The plan an `ExitPlanMode` call proposes: Claude Code reads it from the plan file the
/// model wrote and adds it to the call, with `planFilePath`.
pub(super) fn proposed_plan(input: &Value) -> &str {
    text(input, "plan").trim()
}

/// A subagent the agent delegates work to, followed until it reports back.
pub(super) fn subagent(id: &str, input: &Value) -> Value {
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
pub(super) fn subagent_report(block: &Value, outcome: &Value) -> String {
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
pub(super) struct Receipt {
    pub(super) message_id: String,
    pub(super) delivered: Vec<String>,
    pub(super) text: String,
    pub(super) item_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) proposals: Vec<Value>,
}

impl Receipt {
    pub(super) async fn read(path: &Path, message_id: &str) -> Option<Self> {
        let bytes = tokio::fs::read(path).await.ok()?;
        let receipt = serde_json::from_slice::<Self>(&bytes).ok()?;
        let completes =
            receipt.message_id == message_id && receipt.delivered.iter().any(|id| id == message_id);
        completes.then_some(receipt)
    }

    /// Replays a completed request without starting Claude Code.
    pub(super) async fn replay(self, plan: &Value, events: &mpsc::Sender<Value>) -> Result<()> {
        for id in &self.delivered {
            emit(events, delivered(id)).await?;
        }
        atomic_write(Path::new(text(plan, "output")), self.text.as_bytes()).await?;
        for item in self.proposals {
            emit(events, json!({ "type": "item.completed", "item": item })).await?;
        }
        let item = json!({ "id": self.item_id, "type": "agent_message", "text": self.text });
        emit(events, json!({ "type": "item.completed", "item": item })).await?;
        emit(events, json!({ "type": "turn.completed" })).await
    }
}

/// The subscription usage limit Claude Code reports in a `rate_limit_event`.
#[derive(Debug, PartialEq)]
pub(super) struct UsageLimit {
    pub(super) status: &'static str,
    pub(super) window: Option<String>,
    /// Milliseconds since the epoch.
    pub(super) resets_at: Option<i64>,
    pub(super) extra_usage: Option<&'static str>,
    pub(super) using_extra_usage: bool,
}

impl UsageLimit {
    pub(super) fn from(info: &Value) -> Option<Self> {
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
    pub(super) fn blocks(&self) -> bool {
        self.status == "rejected"
            && !matches!(self.extra_usage, Some("allowed" | "allowed_warning"))
    }

    pub(super) fn event(&self) -> Value {
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
