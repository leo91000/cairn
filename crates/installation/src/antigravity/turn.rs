use super::{
    hook::{Mode, Policy},
    side::Sides,
};
use crate::{
    accounts::broker,
    config::Config,
    error::{Error, Result},
    skills::{atomic_write, private_dir},
    validation::text,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

const LIMIT: usize = 2_000_000;
const LINE_LIMIT: usize = 16_000_000;
const STOPPED: &str = "Antigravity execution stopped.";
/// A Google account's access token outlives no more than one hour of work; `agy` cannot
/// renew it without the refresh token the manager keeps.
const RENEWALS: usize = 48;
const RENEWED: &str = "Your access was renewed after the previous step failed with an expired token. Continue the interrupted request. Preserve completed work and verify external effects before repeating an action.";

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    message_id: String,
    /// Cairn messages sent to the native session, in order.
    submissions: Vec<String>,
    delivered: HashSet<String>,
    previously_delivered: HashSet<String>,
    operational_notices: HashSet<String>,
    complete: bool,
    answer: String,
    items: Vec<Value>,
}

pub(super) async fn emit(events: &mpsc::Sender<Value>, value: Value) -> Result<()> {
    events
        .send(value)
        .await
        .map_err(|_| Error::unavailable("Antigravity output stopped."))
}

impl Receipt {
    async fn read(path: &Path, id: &str) -> Option<Self> {
        let file = tokio::fs::File::open(path).await.ok()?;
        let bytes = crate::process::read_bounded(file, 8_000_000).await.ok()?;
        let value = serde_json::from_slice::<Self>(&bytes).ok()?;
        if value.message_id == id {
            return Some(value);
        }
        // Keep only the previous turn's acknowledgements: a new turn never replays old output.
        let previously_delivered = value
            .previously_delivered
            .union(&value.delivered)
            .cloned()
            .collect();
        Some(Self {
            message_id: id.into(),
            previously_delivered,
            ..Self::default()
        })
    }

    async fn save(&self, path: &Path) -> Result<()> {
        atomic_write(path, &serde_json::to_vec(self)?).await
    }

    async fn replay(&self, plan: &Value, events: &mpsc::Sender<Value>) -> Result<()> {
        for id in &self.delivered {
            emit(events, json!({ "type": "chat.delivered", "messageId": id })).await?;
        }
        atomic_write(Path::new(text(plan, "output")), self.answer.as_bytes()).await?;
        for item in &self.items {
            emit(events, json!({ "type": "item.completed", "item": item })).await?;
        }
        emit(events, json!({ "type": "turn.completed" })).await
    }
}

/// The text `agy` receives for a Cairn message. Its stream-json input accepts text
/// blocks only: attachments are named by their local paths, which its file tools read.
fn input(body: &str, attachments: &Value, inbox: &Path) -> String {
    crate::attachments::input(body, attachments, inbox)
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| part["type"] == "text")
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The Cairn conversation item for one native step, or `None` for steps users never see.
pub(super) fn step_item(id: &str, step: &Value, text_so_far: &str) -> Option<Value> {
    let status = match text(step, "state") {
        "DONE" => "completed",
        "ERROR" | "CANCELED" | "CANCELLED" => "failed",
        _ => "in_progress",
    };
    Some(match text(step, "step_type") {
        "agent_response" => json!({
            "id": id,
            "type": "agent_message",
            "text": text_so_far,
        }),
        "tool" => {
            let info = &step["tool_info"];
            let name = step["tool_name"]
                .as_str()
                .or(info["name"].as_str())
                .unwrap_or_default();
            let parameters = &info["parameters"];
            if name == "run_command" {
                json!({
                    "id": id,
                    "type": "command_execution",
                    "command": parameters["CommandLine"],
                    "aggregated_output": info["output"].as_str().or(info["error"]["message"].as_str()).unwrap_or_default(),
                    "status": status,
                })
            } else if name == "call_mcp_tool" {
                json!({
                    "id": id,
                    "type": "mcp_tool_call",
                    "server": parameters["ServerName"],
                    "tool": parameters["ToolName"],
                    "arguments": parameters["Arguments"],
                    "result": info["output"],
                    "error": info["error"]["message"],
                    "status": status,
                })
            } else {
                json!({
                    "id": id,
                    "type": "mcp_tool_call",
                    "server": "Antigravity",
                    "tool": name,
                    "arguments": parameters,
                    "result": info["output"],
                    "error": info["error"]["message"],
                    "status": status,
                })
            }
        }
        _ => return None,
    })
}

/// `agy` reports provider failures on stderr as `AGY_ERROR: {json}`.
pub(super) fn native_error(error: Option<&Value>, message: &str) -> Error {
    let code = error.and_then(|e| e["error_code"].as_u64());
    let status = error.map(|e| text(e, "status")).unwrap_or_default();
    let lower = message.to_ascii_lowercase();
    let text = if code == Some(429)
        || status == "RESOURCE_EXHAUSTED"
        || lower.contains("resource_exhausted")
    {
        "Usage limit reached for Antigravity. Check this account's quota or connect another account."
    } else if matches!(code, Some(401 | 403))
        || status == "UNAUTHENTICATED"
        || status == "PERMISSION_DENIED"
        || lower.contains("authentication")
    {
        "Antigravity authentication failed. Replace this account's credentials in Connections."
    } else if lower.contains("invalid model selection") {
        "This Antigravity account does not offer the selected model or reasoning level. Choose another model."
    } else {
        "Antigravity could not complete this response. Check the model, account credentials, and available capacity."
    };
    Error::bad_gateway(text)
}

pub(super) fn parse_agy_error(line: &str) -> Option<Value> {
    serde_json::from_str(line.strip_prefix("AGY_ERROR:")?.trim()).ok()
}

/// Launch arguments shared by turns and side questions.
pub(super) fn args(plan: &Value, credentials: &Value, mode: Mode) -> Result<Vec<String>> {
    let mut settings = json!({
        "disableSlashCommands": true,
        "permissions": {
            "deny": match mode {
                Mode::Turn => json!([]),
                // Native rules double the hook: no shell, writes or URL actions.
                Mode::Plan | Mode::Side => json!([
                    "command(*)",
                    "write_file(*)",
                    "read_url(*)",
                    "execute_url(*)",
                    "download_url(*)",
                ]),
            },
        },
    });
    // Native settings from the agent's configuration only add restrictions.
    let native = &plan["antigravitySettings"];
    if let Some(sandbox) = native["enableTerminalSandbox"].as_bool() {
        settings["enableTerminalSandbox"] = sandbox.into();
    }
    // Only deny rules: ask rules do not restrict `agy` 1.5.14 under
    // --dangerously-skip-permissions, and validation refuses them.
    let added = native["permissions"]["deny"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if added.iter().any(|rule| !rule.is_string()) {
        return Err(Error::bad("Invalid Antigravity native permissions."));
    }
    let mut rules = settings["permissions"]["deny"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    rules.extend(added);
    settings["permissions"]["deny"] = rules.into();
    if credentials["kind"] == super::GEMINI {
        settings["modelProvider"] = super::GEMINI.into();
    }
    let model = text(plan, "antigravityModel");
    if !super::valid_slug(model) {
        return Err(Error::bad("Choose an Antigravity model."));
    }
    let mut args = vec![
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--settings".into(),
        settings.to_string(),
        "--model".into(),
        model.into(),
        "--dangerously-skip-permissions".into(),
        "--disable-slash-commands".into(),
    ];
    if mode == Mode::Plan {
        // Guides the model only; the hook and deny rules are the boundary.
        args.extend(["--mode".into(), "plan".into()]);
    }
    Ok(args)
}

pub(super) fn conversation_id(value: &str) -> Result<&str> {
    crate::validation::uuid(value)
        .map(|()| value)
        .map_err(|_| Error::bad_gateway("Antigravity returned an invalid conversation."))
}

/// Writes the per-launch native configuration into the private state.
pub(super) async fn configure(home: &Path, plan: &Value, policy: &Policy) -> Result<()> {
    let config = home.join(".gemini/config");
    private_dir(&config).await?;
    let mcp = if policy.mode == Mode::Side || !plan["antigravityMcp"].is_object() {
        json!({ "mcpServers": {} })
    } else {
        plan["antigravityMcp"].clone()
    };
    atomic_write(&config.join("mcp_config.json"), &serde_json::to_vec(&mcp)?).await?;
    atomic_write(
        &config.join("hooks.json"),
        &serde_json::to_vec(&super::hook::hooks_config(home)?)?,
    )
    .await?;
    policy.save(home).await?;
    // Settings an earlier YOLO turn may have written never outlive it: each launch
    // passes its own with --settings.
    let native = home.join(".gemini/antigravity-cli");
    private_dir(&native).await?;
    atomic_write(&native.join("settings.json"), b"{}").await?;
    // Cairn's instructions are the user rules of every launch.
    atomic_write(
        &home.join(".gemini/GEMINI.md"),
        text(plan, "instructions").as_bytes(),
    )
    .await
}

pub(super) fn policy(plan: &Value, mode: Mode) -> Policy {
    Policy {
        mode,
        mcp: serde_json::from_value(plan["antigravityPolicyMcp"].clone()).unwrap_or_default(),
    }
}

pub(super) async fn spawn(
    config: &Config,
    home: &Path,
    cwd: &Path,
    args: &[String],
    credentials: &Value,
) -> Result<Child> {
    let env = super::environment(config, home, Some(credentials));
    super::check_version(config, &env).await?;
    let mut command = crate::process::command(&config.antigravity_bin, args, &env, Some(cwd));
    command.stdin(Stdio::piped()).process_group(0);
    command
        .spawn()
        .map_err(|_| Error::unavailable("Unable to start the Antigravity CLI."))
}

/// Stops a native process tree when its owner ends, including when a cancelled
/// execution future is dropped: commands `agy` started must not outlive the turn.
/// `agy` 1.5.14 runs each command in its own session and leaves it running when it
/// is interrupted, so its own process group is not enough. Once `agy` has exited its
/// commands belong to init, so the groups seen while it ran are remembered.
pub(super) struct Group {
    root: Option<i32>,
    seen: Vec<i32>,
}

impl Group {
    pub(super) fn of(child: &Child) -> Self {
        Self {
            root: child.id().map(|pid| pid as i32),
            seen: Vec::new(),
        }
    }

    /// Records the groups of the commands `agy` is running now.
    pub(super) fn refresh(&mut self) {
        let Some(root) = self.root else { return };
        for group in tree_groups(root, &processes()) {
            if group != root && !self.seen.contains(&group) {
                self.seen.push(group);
            }
        }
    }

    /// Kills `agy` and every command it started; call before reaping it.
    pub(super) fn stop(&mut self) {
        if let Some(root) = self.root {
            self.refresh();
            kill_group(root);
        }
        self.finish();
    }

    /// `agy` exited and was reaped: its identifier must not be signalled again.
    pub(super) fn exited(&mut self) {
        self.root = None;
    }

    /// After `agy` exited: kills the commands it left behind. The tree is gone, and
    /// the reaped identifier is never signalled again.
    pub(super) fn finish(&mut self) {
        self.root = None;
        for group in std::mem::take(&mut self.seen) {
            kill_group(group);
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Each process's parent and process group, from `/proc`.
fn processes() -> Vec<(i32, i32, i32)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let pid: i32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            // Fields after the command name: state, parent, process group.
            let mut fields = stat.rsplit_once(')')?.1.split_whitespace().skip(1);
            let parent = fields.next()?.parse().ok()?;
            let group = fields.next()?.parse().ok()?;
            Some((pid, parent, group))
        })
        .collect()
}

/// The process groups of `root` and every live descendant, root first.
fn tree_groups(root: i32, processes: &[(i32, i32, i32)]) -> Vec<i32> {
    let mut members = vec![root];
    let mut index = 0;
    while index < members.len() {
        let parent = members[index];
        let children = processes
            .iter()
            .filter(|(pid, ppid, _)| *ppid == parent && !members.contains(pid))
            .map(|(pid, _, _)| *pid)
            .collect::<Vec<_>>();
        members.extend(children);
        index += 1;
    }
    let mut groups = vec![root];
    for member in &members {
        if let Some((_, _, group)) = processes.iter().find(|(pid, _, _)| pid == member)
            && *group > 1
            && !groups.contains(group)
        {
            groups.push(*group);
        }
    }
    groups
}

fn kill_group(group: i32) {
    // SAFETY: getpgrp has no preconditions. Never signal Cairn's own group or init's.
    if group <= 1 || group == unsafe { libc::getpgrp() } {
        return;
    }
    // SAFETY: signals only groups led by this execution's processes.
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
}

pub(super) async fn read_line(
    reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
) -> Result<Option<String>> {
    let mut line = Vec::new();
    let read = reader
        .take(LINE_LIMIT as u64 + 1)
        .read_until(b'\n', &mut line)
        .await?;
    if read == 0 {
        return Ok(None);
    }
    if line.len() > LINE_LIMIT {
        return Err(Error::bad_gateway(
            "Antigravity output exceeded the supported limit.",
        ));
    }
    Ok(Some(String::from_utf8_lossy(&line).trim_end().to_owned()))
}

struct Turn<'a> {
    plan: &'a Value,
    events: mpsc::Sender<Value>,
    stdin: ChildStdin,
    receipt: Receipt,
    receipt_path: PathBuf,
    /// Cairn messages sent in this process, awaiting their native user step.
    unconfirmed: VecDeque<String>,
    /// Messages whose native turn has not produced its result yet.
    unanswered: VecDeque<String>,
    submitted: HashSet<String>,
    items: HashMap<i64, (String, String)>,
    started: HashSet<i64>,
    finished: HashSet<i64>,
    nonce: String,
    conversation: String,
    last_answer: Option<String>,
    sides: Sides,
    /// This launch uses a Google account's access token, renewed by relaunching.
    google: bool,
    renew: bool,
    /// Messages a failed launch never recorded, sent again after renewal.
    resend: Vec<String>,
    group: Group,
}

enum Outcome {
    Complete,
    /// The access token expired: relaunch the same conversation with a new one.
    Renew,
}

fn expired_token(error: Option<&Value>, message: &str) -> bool {
    let reported = error.map(|e| text(e, "short_error")).unwrap_or_default();
    [reported, message]
        .iter()
        .any(|text| text.contains("token expired"))
}

impl Turn<'_> {
    async fn deliver(&mut self, id: &str) -> Result<()> {
        if self.receipt.operational_notices.contains(id) {
            return Ok(());
        }
        if self.receipt.delivered.insert(id.into()) {
            self.receipt.save(&self.receipt_path).await?;
            emit(
                &self.events,
                json!({ "type": "chat.delivered", "messageId": id }),
            )
            .await?;
        }
        Ok(())
    }

    async fn submit(&mut self, id: &str, body: &str, attachments: &Value) -> Result<()> {
        let content = input(
            body,
            attachments,
            Path::new(text(self.plan, "inputDirectory")),
        );
        self.receipt.submissions.push(id.into());
        self.receipt.save(&self.receipt_path).await?;
        let mut line = serde_json::to_vec(&json!({
            "event": "user",
            "message": { "content": content },
        }))?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .await
            .map_err(|_| Error::bad_gateway("Antigravity stopped receiving messages."))?;
        self.stdin.flush().await?;
        self.submitted.insert(id.into());
        self.unconfirmed.push_back(id.into());
        self.unanswered.push_back(id.into());
        Ok(())
    }

    async fn poll_inbox(&mut self) -> Result<()> {
        let directory = Path::new(text(self.plan, "inputDirectory"));
        let Some(bytes) = crate::chat_inbox::read(directory, LIMIT).await? else {
            return Ok(());
        };
        let Ok(messages) = serde_json::from_slice::<Vec<Value>>(&bytes) else {
            return Ok(());
        };
        for message in messages {
            let id = text(&message, "id");
            if message["operationalNotice"] == true {
                self.receipt.operational_notices.insert(id.to_owned());
            }
            if id.is_empty()
                || !crate::chat_inbox::joins_turn(&message, self.plan)
                || self.submitted.contains(id)
                || self.receipt.delivered.contains(id)
                || self.receipt.previously_delivered.contains(id)
            {
                continue;
            }
            if message["sideQuestionCancelled"] == true {
                self.sides.withdraw(text(&message, "sideQuestionId"));
                self.submitted.insert(id.into());
                continue;
            }
            if message["sideQuestion"].is_object() {
                self.sides.ask(&message["sideQuestion"], &self.conversation);
                self.submitted.insert(id.into());
                continue;
            }
            if message["questionCancelled"] == true {
                self.submitted.insert(id.into());
                continue;
            }
            self.submit(id, text(&message, "text"), &message["attachments"])
                .await?;
        }
        Ok(())
    }

    fn item_id(&self, index: i64) -> String {
        format!("agy-{}-{index}", self.nonce)
    }

    async fn on_step(&mut self, step: &Value) -> Result<()> {
        let index = step["step_index"].as_i64().unwrap_or(-1);
        match text(step, "step_type") {
            "user_input" if step["state"] == "DONE" => {
                if let Some(id) = self.unconfirmed.pop_front() {
                    self.deliver(&id).await?;
                }
                return Ok(());
            }
            "error_message" => return Ok(()),
            _ => {}
        }
        if self.finished.contains(&index) {
            return Ok(());
        }
        let id = self.item_id(index);
        let entry = self
            .items
            .entry(index)
            .or_insert_with(|| (id.clone(), String::new()));
        entry.1.push_str(text(step, "text_delta"));
        if entry.1.len() > 8_000_000 {
            return Err(Error::bad_gateway(
                "Antigravity response exceeded the supported limit.",
            ));
        }
        let content = entry.1.clone();
        let Some(item) = step_item(&id, step, &content) else {
            return Ok(());
        };
        let done = matches!(
            text(step, "state"),
            "DONE" | "ERROR" | "CANCELED" | "CANCELLED"
        );
        if step["step_type"] == "agent_response" && content.is_empty() && done {
            // A response step that only carried tool calls has nothing to show.
            self.finished.insert(index);
            return Ok(());
        }
        let started = self.started.insert(index);
        if started {
            emit(
                &self.events,
                json!({ "type": "item.started", "item": item }),
            )
            .await?;
        }
        if done {
            self.finished.insert(index);
            emit(
                &self.events,
                json!({ "type": "item.completed", "item": item }),
            )
            .await?;
            if step["step_type"] == "agent_response" {
                self.last_answer = Some(id);
            }
        } else if !started {
            emit(
                &self.events,
                json!({ "type": "item.updated", "item": item }),
            )
            .await?;
        }
        Ok(())
    }

    /// One native turn finished. Answers whether every submitted message is answered.
    async fn on_result(&mut self, result: &Value, stderr_error: Option<&Value>) -> Result<bool> {
        let Some(id) = self.unanswered.pop_front() else {
            return Err(Error::bad_gateway(
                "Antigravity answered a message Cairn did not send.",
            ));
        };
        let renew = self.google
            && text(result, "status") != "SUCCESS"
            && expired_token(stderr_error, text(result, "error"));
        // A turn that never reached its user step was still consumed by this result,
        // unless it failed before reaching the model: a renewed launch sends it again.
        if let Some(position) = self.unconfirmed.iter().position(|pending| *pending == id) {
            self.unconfirmed.remove(position);
            if renew {
                self.resend.push(id.clone());
            } else {
                self.deliver(&id).await?;
            }
        }
        match text(result, "status") {
            "SUCCESS" => {}
            "WAITING" => {
                return Err(Error::bad_gateway(
                    "Antigravity stopped to wait for an approval Cairn cannot give. Working files were preserved.",
                ));
            }
            _ if renew => {
                self.renew = true;
                return Ok(false);
            }
            _ => return Err(native_error(stderr_error, text(result, "error"))),
        }
        if !self.unanswered.is_empty() {
            return Ok(false);
        }
        self.receipt.answer = text(result, "response").trim_end().to_owned();
        self.receipt.items = self
            .last_answer
            .as_ref()
            .map(|id| {
                vec![json!({
                    "id": id,
                    "type": "agent_message",
                    "text": self.receipt.answer,
                })]
            })
            .unwrap_or_default();
        self.receipt.save(&self.receipt_path).await?;
        Ok(true)
    }

    async fn complete(&mut self) -> Result<()> {
        self.sides.expire().await?;
        self.receipt.complete = true;
        self.receipt.save(&self.receipt_path).await?;
        self.receipt.replay(self.plan, &self.events).await
    }
}

pub async fn run(
    config: &Config,
    plan: Value,
    events: mpsc::Sender<Value>,
    cancel: CancellationToken,
) -> Result<()> {
    tokio::select! {
        result = execute(config, plan, events, cancel.clone()) => result,
        () = cancel.cancelled() => Err(Error::bad_gateway(STOPPED)),
    }
}

pub(super) async fn credentials(config: &Config) -> Result<(PathBuf, Value)> {
    let home = std::env::var_os("CAIRN_ANTIGRAVITY_HOME")
        .map_or_else(|| config.home.join(".antigravity"), PathBuf::from);
    let credentials = broker::request(
        &broker::socket(&home),
        &broker::Request::default(),
        Duration::from_secs(10),
    )
    .await?;
    if !matches!(
        text(&credentials, "kind"),
        super::GEMINI | super::GOOGLE_CLOUD | super::GOOGLE
    ) {
        return Err(Error::bad(
            "Reconnect this Antigravity account, or replace its credentials, in Connections.",
        ));
    }
    Ok((home, credentials))
}

async fn execute(
    config: &Config,
    plan: Value,
    events: mpsc::Sender<Value>,
    cancel: CancellationToken,
) -> Result<()> {
    if plan["sandbox"] != "yolo" && plan["planning"] != true {
        return Err(Error::bad(
            "Antigravity currently requires an isolated YOLO execution environment.",
        ));
    }
    let initial = text(&plan["execution"], "messageId");
    let receipt_path = Path::new(text(&plan, "output")).with_extension("antigravity-receipt.json");
    let saved = Receipt::read(&receipt_path, initial).await;
    if let Some(receipt) = saved
        .as_ref()
        .filter(|r| r.complete && r.delivered.contains(initial))
    {
        return receipt.replay(&plan, &events).await;
    }
    let mode = if plan["planning"] == true {
        Mode::Plan
    } else {
        Mode::Turn
    };
    let mut receipt = saved.unwrap_or_else(|| Receipt {
        message_id: initial.into(),
        ..Receipt::default()
    });
    let mut conversation = match plan["sessionId"].as_str().filter(|id| !id.is_empty()) {
        Some(id) => conversation_id(id)?.to_owned(),
        None => String::new(),
    };
    let mut message = crate::chats::execution_text(&plan);
    if mode == Mode::Plan {
        message = format!(
            "Shell commands, file changes and subagents are unavailable during this plan turn; read and search files instead.\n\n{message}"
        );
    }
    if plan["execution"]["recovery"] == true {
        message = format!(
            "Continue the interrupted request. Preserve completed work and verify external effects before repeating an action.\n{message}"
        );
    }
    let mut attachments = plan["execution"]["attachments"].clone();
    for renewal in 0.. {
        let (home, credentials) = credentials(config).await?;
        super::prepare_native_home(config, &home, Some(&credentials)).await?;
        configure(&home, &plan, &policy(&plan, mode)).await?;
        let mut launch = args(&plan, &credentials, mode)?;
        if !conversation.is_empty() {
            launch.extend(["--conversation".into(), conversation.clone()]);
        }
        launch.push("-p=".into());
        let cwd = PathBuf::from(text(&plan, "cwd"));
        let mut child = tokio::select! {
            result = spawn(config, &home, &cwd, &launch, &credentials) => result?,
            () = cancel.cancelled() => return Err(Error::bad_gateway(STOPPED)),
        };
        let group = Group::of(&child);
        let stdin = child.stdin.take().unwrap();
        // A line read is not cancellation safe: a separate task keeps partial lines
        // while the turn's loop also polls the inbox and timers.
        let (line_tx, mut lines) = mpsc::channel::<Result<String>>(64);
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let reader = tokio::spawn(async move {
            loop {
                let line = read_line(&mut stdout).await.transpose();
                let Some(line) = line else { break };
                let failed = line.is_err();
                if line_tx.send(line).await.is_err() || failed {
                    break;
                }
            }
        });
        let stderr = child.stderr.take().unwrap();
        // The last provider error `agy` reported, which explains a failed result.
        let (error_tx, mut error_rx) = mpsc::channel::<Value>(8);
        let drain = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            while let Ok(Some(line)) = read_line(&mut reader).await {
                if let Some(error) = parse_agy_error(&line) {
                    let _ = error_tx.try_send(error);
                }
            }
        });
        let mut turn = Turn {
            plan: &plan,
            events: events.clone(),
            stdin,
            submitted: receipt.delivered.iter().cloned().collect(),
            receipt: std::mem::take(&mut receipt),
            receipt_path: receipt_path.clone(),
            unconfirmed: VecDeque::new(),
            unanswered: VecDeque::new(),
            items: HashMap::new(),
            started: HashSet::new(),
            finished: HashSet::new(),
            nonce: crate::config::id()[..8].to_owned(),
            conversation: conversation.clone(),
            last_answer: None,
            sides: Sides::new(config, &home, &plan, &credentials, events.clone()),
            google: credentials["kind"] == super::GOOGLE,
            renew: false,
            resend: Vec::new(),
            group,
        };
        // Messages acknowledged before an interruption are already in the native history.
        let outcome = match turn.submit(initial, &message, &attachments).await {
            Ok(()) => drive(&mut turn, &mut child, &mut lines, &mut error_rx, &cancel).await,
            Err(error) => Err(error),
        };
        turn.sides.stop();
        receipt = std::mem::take(&mut turn.receipt);
        conversation = std::mem::take(&mut turn.conversation);
        let resend = std::mem::take(&mut turn.resend);
        let Turn {
            stdin, mut group, ..
        } = turn;
        if !matches!(outcome, Ok(Outcome::Complete)) || cancel.is_cancelled() {
            // Stop the tree while `agy` is still the parent of its commands.
            group.stop();
        } else {
            group.refresh();
        }
        // stdin is closed: `agy` exits after its last turn.
        drop(stdin);
        if tokio::time::timeout(Duration::from_secs(30), child.wait())
            .await
            .is_err()
        {
            group.stop();
            let _ = child.wait().await;
        }
        group.finish();
        drain.abort();
        reader.abort();
        match outcome? {
            Outcome::Complete => return Ok(()),
            Outcome::Renew if renewal < RENEWALS && !conversation.is_empty() => {
                // The first message reaches the model with its own text when the
                // failed launch never recorded it; later ones come back from the inbox.
                if !resend.iter().any(|id| id == initial) {
                    message = RENEWED.into();
                    attachments = json!([]);
                }
            }
            Outcome::Renew => {
                return Err(Error::bad_gateway(
                    "Antigravity could not continue with a renewed Google account token. Working files were preserved.",
                ));
            }
        }
    }
    unreachable!("the renewal loop returns")
}

async fn drive(
    turn: &mut Turn<'_>,
    child: &mut Child,
    lines: &mut mpsc::Receiver<Result<String>>,
    errors: &mut mpsc::Receiver<Value>,
    cancel: &CancellationToken,
) -> Result<Outcome> {
    let mut last_error = None;
    let mut timer = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            () = cancel.cancelled() => return Err(Error::bad_gateway(STOPPED)),
            _ = timer.tick() => {
                turn.group.refresh();
                turn.poll_inbox().await?;
                turn.sides.settle().await?;
            }
            Some(error) = errors.recv() => last_error = Some(error),
            line = lines.recv() => {
                let Some(line) = line.transpose()? else {
                    let status = child.wait().await?;
                    turn.group.exited();
                    while let Ok(error) = errors.try_recv() { last_error = Some(error); }
                    return Err(match &last_error {
                        Some(error) => native_error(Some(error), text(error, "short_error")),
                        None => Error::bad_gateway(format!("Antigravity stopped unexpectedly ({status}).")),
                    });
                };
                let Ok(event) = serde_json::from_str::<Value>(&line) else { continue };
                match text(&event, "event") {
                    "init" => {
                        let id = conversation_id(text(&event, "conversation_id"))?.to_owned();
                        if !turn.conversation.is_empty() && turn.conversation != id {
                            return Err(Error::conflict("The saved Antigravity conversation is unavailable. Working files were preserved."));
                        }
                        turn.conversation = id.clone();
                        emit(&turn.events, json!({ "type": "thread.started", "thread_id": id })).await?;
                        emit(&turn.events, json!({ "type": "turn.started" })).await?;
                        emit(&turn.events, json!({ "type": "chat.side.available" })).await?;
                    }
                    "step_update" => turn.on_step(&event["step_update"]).await?,
                    "result" => {
                        while let Ok(error) = errors.try_recv() { last_error = Some(error); }
                        if turn.on_result(&event["result"], last_error.as_ref()).await? {
                            turn.poll_inbox().await?;
                            if turn.unanswered.is_empty() {
                                turn.complete().await?;
                                return Ok(Outcome::Complete);
                            }
                        }
                        if turn.renew {
                            return Ok(Outcome::Renew);
                        }
                        last_error = None;
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_steps_become_conversation_items() {
        let command = step_item(
            "a",
            &json!({
                "state": "DONE",
                "step_type": "tool",
                "tool_name": "run_command",
                "tool_info": { "name": "run_command", "parameters": { "CommandLine": "ls" }, "output": "file" },
            }),
            "",
        )
        .unwrap();
        assert_eq!(command["type"], "command_execution");
        assert_eq!(command["command"], "ls");
        assert_eq!(command["status"], "completed");

        let mcp = step_item(
            "b",
            &json!({
                "state": "ERROR",
                "step_type": "tool",
                "tool_name": "call_mcp_tool",
                "tool_info": {
                    "parameters": { "ServerName": "cairn_workspace", "ToolName": "list_nodes", "Arguments": {} },
                    "error": { "message": "denied" },
                },
            }),
            "",
        )
        .unwrap();
        assert_eq!(mcp["server"], "cairn_workspace");
        assert_eq!(mcp["tool"], "list_nodes");
        assert_eq!(mcp["status"], "failed");
        assert_eq!(mcp["error"], "denied");

        assert!(step_item("c", &json!({ "step_type": "checkpoint" }), "").is_none());
    }

    #[test]
    fn process_trees_include_commands_in_their_own_sessions() {
        // agy 10 runs a shell 20 in its own group, which starts 21 in that group;
        // 30 belongs to another tree.
        let processes = [
            (10, 1, 10),
            (20, 10, 20),
            (21, 20, 20),
            (30, 1, 30),
            (31, 30, 31),
        ];
        assert_eq!(tree_groups(10, &processes), [10, 20]);
        assert_eq!(tree_groups(30, &processes), [30, 31]);
    }

    #[test]
    fn only_an_expired_access_token_is_renewed() {
        let reported = parse_agy_error(
            r#"AGY_ERROR: {"short_error":"request failed: oauth2: token expired and refresh token is not set"}"#,
        );
        assert!(expired_token(reported.as_ref(), ""));
        assert!(expired_token(
            None,
            "oauth2: token expired and refresh token is not set"
        ));
        assert!(!expired_token(None, "Error 500"));
    }

    #[test]
    fn provider_errors_map_to_exhaustion_and_authentication() {
        let quota =
            parse_agy_error(r#"AGY_ERROR: {"status":"RESOURCE_EXHAUSTED","error_code":429}"#);
        let error = native_error(quota.as_ref(), "");
        assert!(crate::run_output::exhausted_message(&error.message));
        let auth = parse_agy_error(r#"AGY_ERROR: {"error_code":401}"#);
        assert!(
            native_error(auth.as_ref(), "")
                .message
                .contains("authentication")
        );
        assert!(parse_agy_error("error: other").is_none());
    }

    #[test]
    fn plan_turns_deny_native_shell_writes_and_urls_beside_the_hook() {
        let plan = json!({ "antigravityModel": "gemini-4-neon-high" });
        let key = json!({ "kind": "gemini" });
        let planned = args(&plan, &key, Mode::Plan).unwrap();
        let settings: Value = serde_json::from_str(&planned[5]).unwrap();
        assert_eq!(settings["modelProvider"], "gemini");
        assert!(
            settings["permissions"]["deny"]
                .as_array()
                .unwrap()
                .contains(&json!("command(*)"))
        );
        assert!(planned.windows(2).any(|w| w == ["--mode", "plan"]));
        let ordinary = args(&plan, &json!({ "kind": "google-cloud" }), Mode::Turn).unwrap();
        let settings: Value = serde_json::from_str(&ordinary[5]).unwrap();
        assert!(settings.get("modelProvider").is_none());
        assert_eq!(settings["permissions"]["deny"], json!([]));
        assert!(!ordinary.iter().any(|a| a == "--mode"));
        let configured = json!({
            "antigravityModel": "gemini-4-neon-high",
            "antigravitySettings": {
                "enableTerminalSandbox": true,
                "permissions": { "deny": ["command(rm)"], "ask": ["write_file(/etc)"] },
            },
        });
        let restricted = args(&configured, &key, Mode::Plan).unwrap();
        let settings: Value = serde_json::from_str(&restricted[5]).unwrap();
        assert_eq!(settings["enableTerminalSandbox"], true);
        let deny = settings["permissions"]["deny"].as_array().unwrap();
        assert!(deny.contains(&json!("command(*)")) && deny.contains(&json!("command(rm)")));
        // An ask rule would not restrict anything: it never reaches the native settings.
        assert!(settings["permissions"].get("ask").is_none());
        assert!(args(&json!({ "antigravityModel": "bad slug" }), &key, Mode::Turn).is_err());
    }
}
