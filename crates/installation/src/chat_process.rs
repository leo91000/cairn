//! Adapter for conversations on the Codex app-server.
use crate::{
    config::Config,
    context_usage::ContextUsage,
    error::{Error, Result},
    rpc::{Incoming, Session},
    skills::atomic_write,
    validation::text,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub mod resident;

const STOPPED: &str = "Conversation stopped.";
const MAX_MESSAGE: usize = 5_000_000;

/// A Codex thread item as a chat item: snake_case names, as the conversation events use.
pub fn chat_item(mut item: Value) -> Value {
    let kind = match text(&item, "type") {
        "agentMessage" => "agent_message",
        "commandExecution" => "command_execution",
        "fileChange" => "file_change",
        "mcpToolCall" => "mcp_tool_call",
        "reasoning" | "plan" => "reasoning",
        "webSearch" => "web_search",
        "collabAgentToolCall" => "collab_tool_call",
        "contextCompaction" => "context_compaction",
        kind => kind,
    }
    .to_owned();
    item["type"] = kind.into();
    for (source, target) in [
        ("aggregatedOutput", "aggregated_output"),
        ("exitCode", "exit_code"),
        ("durationMs", "duration_ms"),
    ] {
        if let Some(value) = item.get(source).cloned() {
            item[target] = value;
        }
    }
    if item["text"].is_null()
        && let Some(summary) = item["summary"].as_array()
    {
        item["text"] = summary
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n")
            .into();
    }
    if let Some(changes) = item["changes"].as_array_mut() {
        for change in changes {
            if change["kind"].is_object() {
                change["kind"] = change["kind"]["type"].clone();
            }
        }
    }
    item
}

/// A Codex agent status as a subagent status.
fn subagent_status(status: &str) -> &'static str {
    match status {
        "pendingInit" | "running" => "in_progress",
        "completed" => "completed",
        "interrupted" | "shutdown" => "stopped",
        _ => "failed",
    }
}

/// A short name for a subagent Codex identifies only by its prompt or its path.
fn subagent_name(prompt: &str, path: &str) -> String {
    let path = path.rsplit('/').find(|part| !part.is_empty()).unwrap_or("");
    if !path.is_empty() {
        return path.replace(['_', '-'], " ");
    }
    let line = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let line = line.trim();
    if line.is_empty() {
        return "Subagent".into();
    }
    if line.chars().count() <= 60 {
        return line.into();
    }
    format!("{}…", line.chars().take(59).collect::<String>().trim_end())
}

/// `thread/start` and `thread/resume`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadParams<'a> {
    cwd: &'a Value,
    approval_policy: &'static str,
    sandbox: &'a str,
    developer_instructions: &'a Value,
    config: ThreadConfig<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_turns: Option<bool>,
}

#[derive(Serialize)]
struct ThreadConfig<'a> {
    #[serde(flatten)]
    overrides: &'a Map<String, Value>,
    #[serde(rename = "features.default_mode_request_user_input")]
    request_user_input: bool,
    sandbox_workspace_write: WorkspaceWrite<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_reasoning_effort: Option<&'a str>,
}

#[derive(Serialize)]
struct WorkspaceWrite<'a> {
    network_access: bool,
    writable_roots: &'a Value,
}

/// `turn/start`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TurnParams<'a> {
    thread_id: &'a str,
    input: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_user_message_id: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a Value>,
    /// Kept by the thread for later turns, as `effort` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<&'a str>,
}

/// The plan's model, unless it leaves the choice to Codex.
fn model(plan: &Value) -> Option<&Value> {
    (!text(plan, "model").is_empty()).then(|| &plan["model"])
}

fn inbox(plan: &Value) -> &Path {
    Path::new(text(plan, "inputDirectory"))
}

/// An execution that compacts the native context instead of delivering a message.
fn compacting(plan: &Value) -> bool {
    plan["execution"]["operation"] == "compact"
}

/// Protects against a server that keeps answering with cursors it already gave.
struct Cursors {
    seen: HashSet<String>,
    limit: usize,
}

impl Cursors {
    fn new(limit: usize) -> Self {
        Self {
            seen: HashSet::new(),
            limit,
        }
    }

    fn fresh(&mut self, cursor: &Value) -> bool {
        self.seen.insert(cursor.to_string()) && self.seen.len() <= self.limit
    }
}

// Codex persists client IDs in history. A reserved wire namespace preserves
// operational identity across turns without changing provider message UUIDs.
const OPERATIONAL_ID: &str = "cairn-runtime-disk:";

struct Chat<'a> {
    session: &'a mut Session,
    events: mpsc::Sender<Value>,
    cancel: CancellationToken,
    thread: String,
    /// The model Codex reports for the thread, which a plan without one runs.
    thread_model: String,
    turn: String,
    completed: Option<Value>,
    last_message: String,
    seen: HashSet<String>,
    attempted: HashSet<String>,
    turn_submitted: bool,
    texts: HashMap<String, String>,
    /// Subagents this conversation started, by the ID of the item that started them.
    subagents: HashMap<String, Value>,
    /// The subagent each spawned thread works for.
    children: HashMap<String, String>,
    /// The latest context usage shown, so unchanged reports are not repeated.
    context: Option<ContextUsage>,
    /// Whether Codex reported a completed compaction during this execution.
    compacted: bool,
}

impl Chat<'_> {
    async fn release(&mut self) -> Result<()> {
        // Codex 0.159.3 reloads an idle, unsubscribed thread when resume supplies
        // overrides. A subscribed thread can silently ignore new MCP permissions.
        let response = self
            .session
            .request(
                "thread/unsubscribe",
                json!({
                    "threadId": self.thread,
                }),
            )
            .await?;
        if !matches!(text(&response, "status"), "unsubscribed" | "notLoaded") {
            return Err(Error::unavailable(
                "Codex did not release its conversation.",
            ));
        }
        if self.session.auth.is_some() {
            self.session.request("account/logout", json!({})).await?;
            self.session.auth = None;
        }
        Ok(())
    }

    async fn emit(&self, value: Value) -> Result<()> {
        self.events
            .send(value)
            .await
            .map_err(|_| Error::unavailable("Conversation output stopped."))
    }

    async fn acknowledge(&mut self, id: &str) -> Result<()> {
        if self.seen.insert(id.to_owned()) && !id.starts_with(OPERATIONAL_ID) {
            self.emit(json!({ "type": "chat.delivered", "messageId": id }))
                .await?;
        }
        Ok(())
    }

    async fn item(&mut self, item: Value, kind: &str) -> Result<()> {
        let user_input_result = item["type"] == "functionCallOutput"
            && ["request_user_input", "request_user_input_async"].contains(&text(&item, "name"));
        if user_input_result {
            return Ok(());
        }
        if matches!(
            text(&item, "type"),
            "collabAgentToolCall" | "subAgentActivity"
        ) {
            return self.collaboration(item, kind, None).await;
        }
        if item["type"] == "userMessage" {
            if let Some(id) = item["clientId"].as_str() {
                self.acknowledge(id).await?;
            }
            return Ok(());
        }
        if item["type"] == "agentMessage" && !text(&item, "text").is_empty() {
            self.last_message = text(&item, "text").to_owned();
        }
        let mut item = chat_item(item);
        if item["type"] == "context_compaction" {
            // Codex reports only the lifecycle; the visible transcript is never rewritten.
            let completed = kind == "item.completed";
            self.compacted |= completed;
            item["status"] = if completed {
                "completed"
            } else {
                "in_progress"
            }
            .into();
        }
        self.emit(json!({ "type": kind, "item": item })).await
    }

    /// `thread/tokenUsage/updated`, also replayed when a saved thread is resumed.
    async fn token_usage(&mut self, params: &Value) -> Result<()> {
        let Some(usage) = ContextUsage::codex(&params["tokenUsage"]) else {
            return Ok(());
        };
        if self.context.as_ref() == Some(&usage) {
            return Ok(());
        }
        let event = usage.event();
        self.context = Some(usage);
        self.emit(event).await
    }

    /// Codex's subagent tools as the subagents they start and the statuses they report. Waiting
    /// for or listing subagents only refreshes those statuses.
    async fn collaboration(&mut self, item: Value, kind: &str, parent: Option<&str>) -> Result<()> {
        let tool = text(&item, "tool");
        let spawned = item["type"] == "collabAgentToolCall" && tool == "spawnAgent";
        if spawned {
            // The spawned thread is known once the call completes; the call's ID names the
            // subagent throughout.
            let id = text(&item, "id").to_owned();
            let mut subagent = self.subagents.remove(&id).unwrap_or_else(|| {
                json!({
                    "id": id,
                    "type": "subagent",
                    "name": subagent_name(text(&item, "prompt"), ""),
                    "prompt": item["prompt"],
                    "model": item["model"],
                    "status": "in_progress",
                })
            });
            if let Some(parent) = parent {
                subagent["parent_id"] = parent.into();
            }
            if text(&item, "status") == "failed" {
                subagent["status"] = "failed".into();
            } else if text(&item, "status") == "interrupted" {
                subagent["status"] = "stopped".into();
            }
            for thread in item["receiverThreadIds"].as_array().into_iter().flatten() {
                if let Some(thread) = thread.as_str() {
                    // A lifecycle notification can precede the spawn result. Fold its
                    // provisional row and any steps into the original spawn item.
                    if let Some(previous) = self.children.get(thread).filter(|old| **old != id)
                        && let Some(previous) = self.subagents.remove(previous)
                    {
                        subagent["replaces_id"] = previous["id"].clone();
                        for field in ["name", "model", "result", "status"] {
                            if !previous[field].is_null() {
                                subagent[field] = previous[field].clone();
                            }
                        }
                    }
                    self.children.insert(thread.to_owned(), id.clone());
                    if let Some(state) = item["agentsStates"].get(thread) {
                        let status = subagent_status(text(state, "status"));
                        if subagent["status"] == "in_progress" || status != "in_progress" {
                            subagent["status"] = status.into();
                        }
                        if !text(state, "message").is_empty() {
                            subagent["result"] = state["message"].clone();
                        }
                    }
                }
            }
            self.subagents.insert(id, subagent.clone());
            return self.emit(json!({ "type": kind, "item": subagent })).await;
        }

        if item["type"] == "subAgentActivity" {
            let thread = text(&item, "agentThreadId");
            // A child's message to its parent also reports activity on this thread.
            // It must never register the main conversation as its own subagent.
            if thread == self.thread {
                return Ok(());
            }

            let status = match text(&item, "kind") {
                "completed" => "completed",
                "interrupted" => "stopped",
                _ => "in_progress",
            };
            let path = text(&item, "agentPath");
            if thread.is_empty() || self.children.contains_key(thread) {
                return self
                    .update_subagent(thread, |subagent| {
                        // An interaction can message an already finished agent without
                        // starting another turn; only a started activity reopens it.
                        if text(&item, "kind") == "started"
                            || (text(&item, "kind") != "interacted"
                                && subagent["status"] == "in_progress")
                        {
                            subagent["status"] = status.into();
                        }
                        if !path.is_empty() {
                            subagent["name"] = subagent_name("", path).into();
                        }
                    })
                    .await;
            }
            let id = text(&item, "id").to_owned();
            let mut subagent = json!({
                "id": id,
                "type": "subagent",
                "name": subagent_name("", path),
                "model": item["model"],
                "status": status,
            });
            if let Some(parent) = parent {
                subagent["parent_id"] = parent.into();
            }
            self.children.insert(thread.to_owned(), id.clone());
            self.subagents.insert(id, subagent.clone());
            return self
                .emit(json!({ "type": "item.started", "item": subagent }))
                .await;
        }

        let states = item["agentsStates"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        for (thread, state) in &states {
            let status = subagent_status(text(state, "status"));
            self.update_subagent(thread, |subagent| {
                // Closing a finished subagent does not undo its completion.
                let finished =
                    matches!(text(subagent, "status"), "completed" | "failed" | "stopped");
                if !finished || status != "stopped" {
                    subagent["status"] = status.into();
                }
                if !text(state, "message").is_empty() {
                    subagent["result"] = state["message"].clone();
                }
            })
            .await?;
        }
        if matches!(tool, "wait" | "listAgents") {
            return Ok(());
        }
        let mut item = chat_item(item);
        if let Some(parent) = parent {
            item["parent_id"] = parent.into();
        }
        self.emit(json!({ "type": kind, "item": item })).await
    }

    /// Applies a change to the subagent working in `thread`, and shows it when it changed.
    async fn update_subagent(
        &mut self,
        thread: &str,
        change: impl FnOnce(&mut Value),
    ) -> Result<()> {
        let Some(subagent) = self
            .children
            .get(thread)
            .and_then(|id| self.subagents.get_mut(id))
        else {
            return Ok(());
        };
        let before = subagent.clone();
        change(subagent);
        if before["status"] != "in_progress" && subagent["status"] == "in_progress" {
            subagent["result"] = "".into();
        }
        if *subagent == before {
            return Ok(());
        }
        let subagent = subagent.clone();
        self.emit(json!({ "type": "item.updated", "item": subagent }))
            .await
    }

    /// Activity of a subagent's own thread, shown as its steps. Its messages never become the
    /// conversation's reply, and its turn only settles the subagent.
    async fn subagent_activity(
        &mut self,
        method: &str,
        params: &Value,
        parent: &str,
    ) -> Result<()> {
        match method {
            "item/started" | "item/completed" => {
                let item = &params["item"];
                if item["type"] == "userMessage" {
                    return Ok(());
                }
                let mut item = item.clone();
                item["id"] = format!(
                    "{}:{}:{}",
                    text(params, "threadId"),
                    text(params, "turnId"),
                    text(&item, "id")
                )
                .into();
                if matches!(
                    text(&item, "type"),
                    "collabAgentToolCall" | "subAgentActivity"
                ) {
                    let kind = method.replace('/', ".");
                    return self.collaboration(item, &kind, Some(parent)).await;
                }
                let mut item = chat_item(item);
                if item["type"] == "agent_message" {
                    item["type"] = "subagent_message".into();
                    // Its latest message is its report once its turn completes.
                    let subagent = self.subagents.get_mut(parent);
                    if let Some(subagent) = subagent.filter(|_| method == "item/completed") {
                        subagent["result"] = item["text"].clone();
                    }
                }
                item["parent_id"] = parent.into();
                let kind = method.replace('/', ".");
                self.emit(json!({ "type": kind, "item": item })).await
            }
            "item/agentMessage/delta" => {
                let id = format!(
                    "{}:{}:{}",
                    text(params, "threadId"),
                    text(params, "turnId"),
                    text(params, "itemId")
                );
                let value = self.texts.entry(id.clone()).or_default();
                if value.len() + text(params, "delta").len() > MAX_MESSAGE {
                    return Ok(());
                }
                value.push_str(text(params, "delta"));
                let item = json!({
                    "id": id,
                    "type": "subagent_message",
                    "text": value,
                    "parent_id": parent,
                });
                self.emit(json!({ "type": "item.updated", "item": item }))
                    .await
            }
            "turn/plan/updated" => {
                let id = format!(
                    "{}:{}:plan",
                    text(params, "threadId"),
                    text(params, "turnId")
                );
                let mut plan = crate::agent_plan::codex(&id, params);
                plan["parent_id"] = parent.into();
                self.emit(json!({ "type": "item.updated", "item": plan }))
                    .await
            }
            "turn/started" | "turn/completed" => {
                let status = if method == "turn/started" {
                    "in_progress"
                } else {
                    match text(&params["turn"], "status") {
                        "completed" => "completed",
                        "interrupted" => "stopped",
                        _ => "failed",
                    }
                };
                let thread = text(params, "threadId");
                self.update_subagent(thread, |subagent| subagent["status"] = status.into())
                    .await
            }
            _ => Ok(()),
        }
    }

    async fn message_delta(&mut self, params: &Value) -> Result<()> {
        let delta = text(params, "delta");
        let value = self
            .texts
            .entry(text(params, "itemId").to_owned())
            .or_default();
        if value.len() + delta.len() > MAX_MESSAGE {
            return Err(Error::bad_gateway(
                "Conversation output exceeded the supported limit.",
            ));
        }
        value.push_str(delta);
        let item = json!({ "id": params["itemId"], "type": "agent_message", "text": value });
        self.emit(json!({ "type": "item.updated", "item": item }))
            .await
    }

    async fn incoming(&mut self, incoming: Incoming) -> Result<()> {
        if self.session.handle_auth(&incoming).await? {
            return Ok(());
        }
        let params = incoming.params;
        if let Some(request_id) = incoming.id {
            if incoming.method == "item/tool/requestUserInput" {
                return self
                    .session
                    .rpc
                    .reject_with_message(request_id, crate::chats::questions::NATIVE_DENIED)
                    .await;
            }
            return self.session.rpc.reject(request_id).await;
        }
        let thread = text(&params, "threadId");
        let other_thread = params["threadId"].is_string()
            && !self.thread.is_empty()
            && params["threadId"] != self.thread.as_str();
        if other_thread {
            let Some(parent) = self.children.get(thread).cloned() else {
                return Ok(());
            };
            return self
                .subagent_activity(&incoming.method, &params, &parent)
                .await;
        }
        match incoming.method.as_str() {
            "serverRequest/resolved" => {}
            "turn/started" => {
                self.turn = text(&params["turn"], "id").to_owned();
                self.emit(json!({ "type": "turn.started" })).await?;
            }
            "item/started" | "item/completed" => {
                let kind = incoming.method.replace('/', ".");
                self.item(params["item"].clone(), &kind).await?;
            }
            "item/agentMessage/delta" => self.message_delta(&params).await?,
            "thread/tokenUsage/updated" => self.token_usage(&params).await?,
            // Each update replaces the turn's whole plan.
            "turn/plan/updated" => {
                let id = format!("plan:{}", text(&params, "turnId"));
                let plan = crate::agent_plan::codex(&id, &params);
                self.emit(json!({ "type": "item.updated", "item": plan }))
                    .await?;
            }
            "error" if params["willRetry"] == true && params["turnId"] == self.turn.as_str() => {
                // Codex owns this retry. Report its progress without failing the turn.
                self.emit(json!({
                    "type": "error",
                    "error": params["error"],
                    "willRetry": true,
                }))
                .await?;
            }
            "turn/completed" => self.completed = Some(params["turn"].clone()),
            _ => {}
        }
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let rpc = self.session.rpc.clone();
        let request = rpc.request(method, params);
        tokio::pin!(request);
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => return Err(Error::conflict(STOPPED)),
                result = &mut request => return result,
                incoming = self.session.incoming.recv() => {
                    let Some(incoming) = incoming else {
                        return Err(self.session.rpc.failure().await);
                    };
                    self.incoming(incoming).await?;
                }
            }
        }
    }

    /// The reasoning effort to run with. A resumed thread can retain the previous model's
    /// effort, so the new model's default is resolved explicitly instead of inherited.
    async fn effort(&mut self, plan: &Value) -> String {
        let effort = text(plan, "reasoning");
        if !effort.is_empty() {
            return effort.to_owned();
        }
        let Ok(models) = crate::models::discover(self.session).await else {
            return String::new();
        };
        let chosen = models
            .as_array()
            .into_iter()
            .flatten()
            .find(|m| match model(plan) {
                Some(model) => m["model"] == *model,
                None => m["isDefault"] == true,
            });
        chosen.map_or_else(String::new, |m| {
            text(m, "defaultReasoningEffort").to_owned()
        })
    }

    async fn open_thread(&mut self, plan: &Value, effort: &str, resume: bool) -> Result<Value> {
        let sandbox = if plan["sandbox"] == "yolo" {
            "danger-full-access"
        } else {
            text(plan, "sandbox")
        };
        let empty = Map::new();
        let params = ThreadParams {
            cwd: &plan["cwd"],
            approval_policy: "never",
            sandbox,
            developer_instructions: &plan["instructions"],
            config: ThreadConfig {
                overrides: plan["codexConfig"].as_object().unwrap_or(&empty),
                request_user_input: false,
                sandbox_workspace_write: WorkspaceWrite {
                    network_access: true,
                    writable_roots: &plan["writableRoots"],
                },
                model_reasoning_effort: (!effort.is_empty()).then_some(effort),
            },
            model: model(plan),
            thread_id: resume.then(|| &plan["sessionId"]),
            exclude_turns: resume.then_some(true),
        };
        let method = if resume {
            "thread/resume"
        } else {
            "thread/start"
        };
        let result = self.request(method, serde_json::to_value(params)?).await?;
        self.thread = text(&result["thread"], "id").to_owned();
        self.thread_model = text(&result, "model").to_owned();
        if self.thread.is_empty() {
            return Err(Error::bad_gateway(
                "Codex returned an invalid conversation.",
            ));
        }
        self.emit(json!({ "type": "chat.question.closed" })).await?;
        self.emit(json!({ "type": "thread.started", "thread_id": self.thread }))
            .await?;
        Ok(result)
    }

    /// Every turn of a paginated thread, newest first, without its items.
    async fn list_turns(&mut self) -> Result<Vec<Value>> {
        let mut turns = Vec::new();
        let mut cursor = Value::Null;
        let mut cursors = Cursors::new(1000);
        loop {
            let mut params = json!({
                "threadId": self.thread,
                "limit": 100,
                "itemsView": "notLoaded",
                "sortDirection": "desc",
            });
            if !cursor.is_null() {
                params["cursor"] = cursor;
            }
            let page = self.request("thread/turns/list", params).await?;
            let data = page["data"].as_array().cloned().ok_or_else(|| {
                Error::bad_gateway("Codex returned invalid conversation history.")
            })?;
            turns.extend(data);
            cursor = page["nextCursor"].clone();
            if cursor.is_null() {
                return Ok(turns);
            }
            if !cursors.fresh(&cursor) {
                return Err(Error::bad_gateway(
                    "Codex returned invalid conversation pagination.",
                ));
            }
        }
    }

    // Read individual items, not full turns: one turn can contain megabytes of command
    // output. Summary view omits steered user messages, whose client IDs are necessary to
    // avoid delivering them twice after restart.
    async fn load_items(&mut self, turns: &mut Vec<Value>) -> Result<()> {
        let mut indices: HashMap<String, usize> = turns
            .iter()
            .enumerate()
            .map(|(index, turn)| (text(turn, "id").to_owned(), index))
            .collect();
        let mut cursor = Value::Null;
        let mut cursors = Cursors::new(100_000);
        let mut previous_item_turn = None;
        loop {
            let params = json!({
                "threadId": self.thread,
                "limit": 1,
                "sortDirection": "asc",
                "cursor": cursor,
            });
            let page = self.request("thread/items/list", params).await?;
            let entries = page["data"]
                .as_array()
                .ok_or_else(|| Error::bad_gateway("Codex returned invalid conversation items."))?;
            for entry in entries {
                let turn_id = entry["turnId"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        Error::bad_gateway("Codex returned an invalid conversation turn.")
                    })?;
                let index = if let Some(index) = indices.get(turn_id) {
                    *index
                } else {
                    // After interruption, persisted items can outlive their turn metadata.
                    // Recover receipts without assuming completion. Items arrive oldest
                    // first; turns are stored newest first.
                    let index = previous_item_turn.unwrap_or(turns.len());
                    for position in indices.values_mut() {
                        if *position >= index {
                            *position += 1;
                        }
                    }
                    let recovered = json!({
                        "id": turn_id,
                        "status": "interrupted",
                        "items": [],
                    });
                    turns.insert(index, recovered);
                    indices.insert(turn_id.to_owned(), index);
                    index
                };
                previous_item_turn = Some(index);

                keep_item(&mut turns[index], &entry["item"])?;
            }
            cursor = page["nextCursor"].clone();
            if cursor.is_null() {
                return Ok(());
            }
            if !cursors.fresh(&cursor) {
                return Err(Error::bad_gateway(
                    "Codex returned invalid conversation item pagination.",
                ));
            }
        }
    }

    /// The turns of the thread, with the items needed to settle the request.
    async fn history(&mut self, result: &Value, resume: bool) -> Result<(Vec<Value>, bool)> {
        let turns = result["thread"]["turns"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let paginated = result["thread"]["historyMode"] == "paginated";
        if resume && paginated {
            let mut turns = self.list_turns().await?;
            self.load_items(&mut turns).await?;
            return Ok((turns, true));
        }
        if resume && turns.is_empty() {
            return Err(Error::bad_gateway(
                "Update Codex to resume conversations with paginated history.",
            ));
        }
        Ok((turns, paginated))
    }

    async fn execute(&mut self, plan: &Value) -> Result<()> {
        let effort = self.effort(plan).await;
        let resume = plan["sessionId"].is_string();
        if compacting(plan) && !resume {
            return Err(Error::conflict(
                "This conversation has no saved Codex session to compact.",
            ));
        }
        let result = self.open_thread(plan, &effort, resume).await?;
        if compacting(plan) {
            return self.compact(plan).await;
        }
        let (turns, paginated) = self.history(&result, resume).await?;
        for turn in &turns {
            for item in turn["items"].as_array().into_iter().flatten() {
                if item["type"] == "userMessage"
                    && let Some(id) = item["clientId"].as_str()
                {
                    self.acknowledge(id).await?;
                }
            }
        }
        let message_id = &plan["execution"]["messageId"];
        let accepted = turns.iter().rev().find(|t| {
            t["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|i| i["clientId"] == *message_id))
        });
        let previous = if accepted.is_some() && plan["execution"]["recovery"] == true {
            if paginated {
                turns.first()
            } else {
                turns.last()
            }
        } else {
            accepted
        };
        if let Some(previous) = previous {
            self.acknowledge(text(&plan["execution"], "messageId"))
                .await?;
            if previous["status"] == "completed" {
                for item in previous["items"].as_array().into_iter().flatten() {
                    self.item(item.clone(), "item.completed").await?;
                }
                return self.finish(plan).await;
            }
        }
        self.start_turn(plan, &effort, previous.is_some()).await?;
        self.wait(plan).await
    }

    async fn start_turn(&mut self, plan: &Value, effort: &str, continuing: bool) -> Result<()> {
        // A turn can complete before the first steering tick. Reject an oversized
        // inbox before submitting any input, independently of provider timing.
        crate::chat_inbox::read(inbox(plan), crate::chat_inbox::ADAPTER_LIMIT).await?;

        let message = if continuing {
            format!(
                "Continue the interrupted conversation from its last completed step. Preserve completed work and verify external effects before repeating any action. The pending user request is:\n{}",
                text(&plan["execution"], "text")
            )
        } else {
            crate::chats::execution_text(plan)
        };
        let input =
            crate::attachments::input(&message, &plan["execution"]["attachments"], inbox(plan));
        let (service_tier, speed_notice) = self.service_tier(plan).await?;
        let params = TurnParams {
            thread_id: &self.thread,
            input,
            effort: (!effort.is_empty()).then_some(effort),
            client_user_message_id: (!continuing).then(|| &plan["execution"]["messageId"]),
            model: model(plan),
            service_tier: Some(service_tier.as_str()).filter(|tier| !tier.is_empty()),
        };
        let params = serde_json::to_value(params)?;
        self.turn_submitted = true;
        let started = self.request("turn/start", params).await?;
        self.turn = text(&started["turn"], "id").to_owned();
        self.acknowledge(text(&plan["execution"], "messageId"))
            .await?;
        // Only an attempt Codex accepted says how it asks for the speed, after its message.
        if let Some(notice) = speed_notice {
            self.emit(notice).await?;
        }
        Ok(())
    }

    /// The tier sent with the turn, and the speed notice to record once Codex accepts it. Codex
    /// silently runs Standard for a tier its own catalog lacks, so a faster tier is checked against
    /// this process's model list first, and against the account's recent cached catalog. Codex
    /// silently answers both with its built-in list, which offers Fast, when it cannot fetch the
    /// account's: a list naming the model without the tier is the account's, and decides Standard;
    /// a list naming the tier is only a request, which the account's plan may still not run.
    async fn service_tier(&mut self, plan: &Value) -> Result<(String, Option<Value>)> {
        let tier = text(plan, "serviceTier").to_owned();
        if tier.is_empty() || tier == crate::models::CODEX_STANDARD_TIER {
            return Ok((tier, None));
        }

        // Without a chosen model, Codex runs the thread's, which its configuration may set
        // rather than the catalog's default.
        let model = match text(plan, "model") {
            "" => self.thread_model.clone(),
            model => model.to_owned(),
        };
        let check = if model.is_empty() {
            // No model is known for the turn, so its catalog row cannot be found.
            crate::models::TierCheck::Unconfirmed
        } else {
            tokio::select! {
                () = self.cancel.cancelled() => return Err(Error::conflict(STOPPED)),
                check = crate::models::check_tier(self.session, &model, &tier) => check,
            }
        };
        let account = plan["accountTiers"][&model]
            .as_array()
            .map(|tiers| tiers.iter().any(|listed| listed == tier.as_str()));
        let check = match (check, account) {
            (_, Some(false)) => crate::models::TierCheck::Lacking,
            (crate::models::TierCheck::Offered, None) => crate::models::TierCheck::Unconfirmed,
            (check, _) => check,
        };
        let speed = match check {
            crate::models::TierCheck::Offered => json!({
                "requested": tier,
                "confirmed": true,
            }),
            crate::models::TierCheck::Lacking => json!({
                "requested": tier,
                "applied": crate::models::STANDARD_SPEED,
            }),
            crate::models::TierCheck::Unconfirmed => json!({
                "requested": tier,
                "unconfirmed": true,
            }),
        };
        let notice = json!({
            "type": "turn.speed",
            "messageId": plan["execution"]["messageId"],
            "speed": speed,
        });
        let sent = if check == crate::models::TierCheck::Lacking {
            crate::models::CODEX_STANDARD_TIER.to_owned()
        } else {
            tier
        };
        Ok((sent, Some(notice)))
    }

    /// Compacts the resumed thread. Codex replaces any running turn with its compaction
    /// turn, so Cairn only starts one while the conversation is idle. A restarted attempt
    /// compacts again: compaction has no external effects and keeps the thread usable.
    async fn compact(&mut self, plan: &Value) -> Result<()> {
        self.request(
            "thread/compact/start",
            json!({
                "threadId": self.thread,
            }),
        )
        .await?;
        self.wait(plan).await
    }

    /// Steers the turn with the user's new messages until it completes.
    async fn wait(&mut self, plan: &Value) -> Result<()> {
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while self.completed.is_none() {
            tokio::select! {
                () = self.cancel.cancelled() => return Err(Error::conflict(STOPPED)),
                incoming = self.session.incoming.recv() => {
                    let Some(incoming) = incoming else {
                        return Err(self.session.rpc.failure().await);
                    };
                    self.incoming(incoming).await?;
                }
                _ = interval.tick() => self.steer(plan).await?,
            }
        }
        let turn = self.completed.take().unwrap();
        if turn["status"] != "completed" {
            // Keep the provider's error codes: they decide retries and account switches.
            let mut error = turn
                .get("error")
                .filter(|error| error.is_object())
                .cloned()
                .unwrap_or_else(|| json!({}));
            if text(&error, "message").is_empty() {
                error["message"] = "Conversation interrupted.".into();
            }
            self.emit(json!({ "type": "turn.failed", "error": error }))
                .await?;
            return Err(Error::conflict(text(&error, "message")));
        }
        if compacting(plan) && !self.compacted {
            let message = "Codex finished without reporting a context compaction.";
            self.emit(json!({ "type": "turn.failed", "error": { "message": message } }))
                .await?;
            return Err(Error::bad_gateway(message));
        }
        self.finish(plan).await
    }

    async fn steer(&mut self, plan: &Value) -> Result<()> {
        // Messages sent meanwhile stay queued for the next turn.
        if compacting(plan) {
            return Ok(());
        }
        let Some(bytes) =
            crate::chat_inbox::read(inbox(plan), crate::chat_inbox::ADAPTER_LIMIT).await?
        else {
            return Ok(());
        };
        let Ok(messages) = serde_json::from_slice::<Vec<Value>>(&bytes) else {
            return Ok(());
        };
        for message in messages {
            let id = text(&message, "id");
            let client_id = if message["operationalNotice"] == true {
                format!("{OPERATIONAL_ID}{id}")
            } else {
                id.to_owned()
            };
            if self.completed.is_some()
                || self.seen.contains(&client_id)
                || !self.attempted.insert(id.to_owned())
            {
                continue;
            }
            if message["questionCancelled"] == true {
                continue;
            }
            let input = crate::attachments::input(
                text(&message, "text"),
                &message["attachments"],
                inbox(plan),
            );
            let params = json!({
                "threadId": self.thread,
                "expectedTurnId": self.turn,
                "clientUserMessageId": client_id,
                "input": input,
            });
            if self.request("turn/steer", params).await.is_err() {
                break;
            }
            self.acknowledge(&client_id).await?;
        }
        Ok(())
    }

    async fn finish(&self, plan: &Value) -> Result<()> {
        atomic_write(
            Path::new(text(plan, "output")),
            self.last_message.as_bytes(),
        )
        .await?;
        self.emit(json!({ "type": "turn.completed" })).await
    }
}

/// Keeps what settling a turn needs from one of its items.
fn keep_item(turn: &mut Value, item: &Value) -> Result<()> {
    let items = turn["items"]
        .as_array_mut()
        .ok_or_else(|| Error::bad_gateway("Codex returned invalid conversation history."))?;
    match text(item, "type") {
        "userMessage" => {
            items.push(json!({ "type": "userMessage", "clientId": item["clientId"] }));
        }
        "agentMessage" => {
            // Only the final answer is needed to settle a completed turn. Its tools are
            // already in our durable event log.
            items.retain(|item| item["type"] != "agentMessage");
            items.push(item.clone());
        }
        _ => {}
    }
    Ok(())
}

pub async fn run(
    config: &Config,
    home: &Path,
    plan: Value,
    events: mpsc::Sender<Value>,
    cancel: CancellationToken,
) -> Result<()> {
    let args = plan["args"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut session =
        match Session::codex(config, home, &args, Some(Path::new(text(&plan, "cwd")))).await {
            Ok(session) => session,
            Err(error) => {
                if let Some(failure) = crate::run_retry::startup_failure(&error) {
                    let _ = events.send(failure).await;
                }
                return Err(error);
            }
        };
    let result = run_session(&mut session, home, plan, events, cancel, false).await;
    crate::performance::wait("codex_shutdown", session.close()).await;
    result
}

/// A resident owns the native process; one attempt borrows it until every
/// thread access and account lease has been released. Errors retire the process.
async fn run_session(
    session: &mut Session,
    home: &Path,
    plan: Value,
    events: mpsc::Sender<Value>,
    cancel: CancellationToken,
    reusable: bool,
) -> Result<()> {
    let mut auth = crate::accounts::codex::Client::new(home);
    if home.join("cairn-managed-auth").exists() && auth.is_none() {
        return Err(Error::unavailable(
            "Account authentication service is unavailable.",
        ));
    }
    if let Some(auth) = &mut auth
        && let Err(error) = auth.login(session).await
    {
        if let Some(failure) = crate::run_retry::startup_failure(&error) {
            let _ = events.send(failure).await;
        }
        return Err(error);
    }
    session.auth = auth;
    let mut chat = Chat {
        session,
        events,
        cancel,
        thread: String::new(),
        thread_model: String::new(),
        turn: String::new(),
        completed: None,
        last_message: String::new(),
        seen: HashSet::new(),
        attempted: HashSet::new(),
        turn_submitted: false,
        texts: HashMap::new(),
        subagents: HashMap::new(),
        children: HashMap::new(),
        context: None,
        compacted: false,
    };
    let mut result = chat.execute(&plan).await;
    if let Err(error) = &result
        && !chat.turn_submitted
        && let Some(failure) = crate::run_retry::startup_failure(error)
    {
        chat.emit(failure).await?;
    }
    if result.is_ok() && reusable {
        result = chat.release().await;
    }
    result
}
