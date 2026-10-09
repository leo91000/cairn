//! Adapter for conversations on the Codex app-server.
use crate::{
    auth::hex_digest,
    config::Config,
    error::{Error, Result},
    rpc::{Incoming, Session},
    skills::atomic_write,
    validation::{parse, text},
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
const MAX_INBOX: usize = 2_000_000;

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
}

/// The plan's model, unless it leaves the choice to Codex.
fn model(plan: &Value) -> Option<&Value> {
    (!text(plan, "model").is_empty()).then(|| &plan["model"])
}

fn inbox(plan: &Value) -> &Path {
    Path::new(text(plan, "inputDirectory"))
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

struct Question {
    request_id: Value,
    message_id: Option<String>,
}

struct Chat<'a> {
    session: &'a mut Session,
    events: mpsc::Sender<Value>,
    cancel: CancellationToken,
    thread: String,
    turn: String,
    completed: Option<Value>,
    last_message: String,
    seen: HashSet<String>,
    attempted: HashSet<String>,
    texts: HashMap<String, String>,
    questions: HashMap<String, Question>,
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
        if self.seen.insert(id.to_owned()) {
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
        if item["type"] == "userMessage" {
            if let Some(id) = item["clientId"].as_str() {
                self.acknowledge(id).await?;
            }
            return Ok(());
        }
        if item["type"] == "agentMessage" {
            if !text(&item, "text").is_empty() {
                self.last_message = text(&item, "text").to_owned();
            }
            if kind == "item.completed" {
                self.ask_in_message(&item).await?;
            }
        }
        self.emit(json!({ "type": kind, "item": chat_item(item) }))
            .await
    }

    /// Questions an agent message asks, shown without blocking the conversation.
    async fn ask_in_message(&self, item: &Value) -> Result<()> {
        let Some(questions) = item["questions"].as_array() else {
            return Ok(());
        };
        let fields = questions
            .iter()
            .enumerate()
            .map(|(index, question)| {
                let options = question["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|label| json!({ "label": label }))
                    .collect::<Vec<_>>();
                json!({ "id": index.to_string(), "title": question["title"], "options": options })
            })
            .collect::<Vec<_>>();
        let Ok(fields) = parse("questions", fields.into()) else {
            return Ok(());
        };
        let id = hex_digest(&format!("{}:{}", self.thread, text(item, "id")));
        self.emit(json!({
            "type": "chat.question",
            "question": { "id": id, "blocking": false, "fields": fields },
        }))
        .await
    }

    /// `item/tool/requestUserInput`: a question the user answers through the inbox.
    async fn request_user_input(&mut self, request_id: Value, params: &Value) -> Result<()> {
        let ours = !text(params, "itemId").is_empty()
            && (self.thread.is_empty() || params["threadId"] == self.thread.as_str());
        if !ours {
            return self.session.rpc.reject(request_id).await;
        }
        let fields = params["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|question| {
                json!({
                    "id": question["id"],
                    "title": question["question"],
                    "secret": question["isSecret"].as_bool().unwrap_or(false),
                    "options": question["options"].as_array().cloned().unwrap_or_default(),
                })
            })
            .collect::<Vec<_>>();
        let Ok(fields) = parse("questions", fields.into()) else {
            return self.session.rpc.reject(request_id).await;
        };
        let thread = if self.thread.is_empty() {
            text(params, "threadId")
        } else {
            &self.thread
        };
        let id = hex_digest(&format!("{thread}:{}", text(params, "itemId")));
        self.questions.insert(
            id.clone(),
            Question {
                request_id,
                message_id: None,
            },
        );
        self.emit(json!({
            "type": "chat.question",
            "question": { "id": id, "blocking": params["isBlocking"] != false, "fields": fields },
        }))
        .await
    }

    /// `serverRequest/resolved`: closes the questions of a request that no longer waits.
    async fn resolved(&mut self, params: &Value) -> Result<()> {
        let ids = self
            .questions
            .iter()
            .filter(|(_, q)| q.request_id == params["requestId"])
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            let question = self.questions.remove(&id).unwrap();
            if let Some(message) = question.message_id {
                self.acknowledge(&message).await?;
            }
            self.emit(json!({ "type": "chat.question.closed", "questionId": id }))
                .await?;
        }
        Ok(())
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
                return self.request_user_input(request_id, &params).await;
            }
            return self.session.rpc.reject(request_id).await;
        }
        let other_thread = params["threadId"].is_string()
            && !self.thread.is_empty()
            && params["threadId"] != self.thread.as_str();
        if other_thread {
            return Ok(());
        }
        match incoming.method.as_str() {
            "serverRequest/resolved" => self.resolved(&params).await?,
            "turn/started" => {
                self.turn = text(&params["turn"], "id").to_owned();
                self.emit(json!({ "type": "turn.started" })).await?;
            }
            "item/started" | "item/completed" => {
                let kind = incoming.method.replace('/', ".");
                self.item(params["item"].clone(), &kind).await?;
            }
            "item/agentMessage/delta" => self.message_delta(&params).await?,
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
                request_user_input: true,
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
        let result = self.open_thread(plan, &effort, resume).await?;
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
        let params = TurnParams {
            thread_id: &self.thread,
            input,
            effort: (!effort.is_empty()).then_some(effort),
            client_user_message_id: (!continuing).then(|| &plan["execution"]["messageId"]),
            model: model(plan),
        };
        let params = serde_json::to_value(params)?;
        let started = self.request("turn/start", params).await?;
        self.turn = text(&started["turn"], "id").to_owned();
        self.acknowledge(text(&plan["execution"], "messageId"))
            .await
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
            let error = turn
                .get("error")
                .cloned()
                .unwrap_or_else(|| json!({ "message": "Conversation interrupted." }));
            self.emit(json!({ "type": "turn.failed", "error": error }))
                .await?;
            return Err(Error::conflict("Conversation interrupted."));
        }
        self.finish(plan).await
    }

    async fn steer(&mut self, plan: &Value) -> Result<()> {
        let Ok(bytes) = tokio::fs::read(inbox(plan).join("messages.json")).await else {
            return Ok(());
        };
        if bytes.len() > MAX_INBOX {
            return Err(Error::bad("Chat inbox is too large."));
        }
        let Ok(messages) = serde_json::from_slice::<Vec<Value>>(&bytes) else {
            return Ok(());
        };
        for message in messages {
            let id = text(&message, "id");
            if self.completed.is_some()
                || self.seen.contains(id)
                || !self.attempted.insert(id.to_owned())
            {
                continue;
            }
            if let Some(question) = self.questions.get_mut(text(&message, "questionId"))
                && let Some(answers) = message["answers"].as_object()
            {
                question.message_id = Some(id.to_owned());
                let answers = answers
                    .iter()
                    .map(|(id, answers)| (id.clone(), json!({ "answers": answers })))
                    .collect::<Map<_, _>>();
                let request_id = question.request_id.clone();
                self.session
                    .rpc
                    .reply(request_id, json!({ "answers": answers }))
                    .await?;
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
                "clientUserMessageId": id,
                "input": input,
            });
            if self.request("turn/steer", params).await.is_err() {
                break;
            }
            self.acknowledge(id).await?;
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
        Session::codex(config, home, &args, Some(Path::new(text(&plan, "cwd")))).await?;
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
        return Err(error);
    }
    session.auth = auth;
    let mut chat = Chat {
        session,
        events,
        cancel,
        thread: String::new(),
        turn: String::new(),
        completed: None,
        last_message: String::new(),
        seen: HashSet::new(),
        attempted: HashSet::new(),
        texts: HashMap::new(),
        questions: HashMap::new(),
    };
    let mut result = chat.execute(&plan).await;
    if result.is_ok() && reusable {
        result = chat.release().await;
    }
    result
}
