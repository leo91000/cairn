use crate::{
    auth::hex_digest,
    config::Config,
    error::{Error, Result},
    rpc::{Incoming, Session},
    skills::atomic_write,
    validation::{parse, text},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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

struct Question {
    request_id: Value,
    message_id: Option<String>,
}

struct Chat {
    session: Session,
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

impl Chat {
    async fn emit(&self, value: Value) -> Result<()> {
        self.events
            .send(value)
            .await
            .map_err(|_| Error::unavailable("Conversation output stopped."))
    }

    async fn acknowledge(&mut self, id: &str) -> Result<()> {
        if self.seen.insert(id.to_owned()) {
            self.emit(json!({
                "type": "chat.delivered",
                "messageId": id
            }))
            .await?;
        }
        Ok(())
    }

    async fn item(&mut self, item: Value, kind: &str) -> Result<()> {
        if item["type"] == "functionCallOutput"
            && ["request_user_input", "request_user_input_async"].contains(&text(&item, "name"))
        {
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
            if kind == "item.completed"
                && let Some(questions) = item["questions"].as_array()
            {
                let fields = questions
                    .iter()
                    .enumerate()
                    .map(|(i, q)| {
                        json!({
                            "id": i.to_string(),
                            "title": q["title"],
                            "options": q["options"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|label| json!({
                                "label": label
                            }))
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect::<Vec<_>>();
                if let Ok(fields) = parse("questions", fields.into()) {
                    self.emit(json!({
                        "type": "chat.question",
                        "question": {
                            "id": hex_digest(&format!(
                                "{}:{}",
                                self.thread,
                                text(&item, "id")
                            )),
                            "blocking": false,
                            "fields": fields
                        }
                    }))
                    .await?;
                }
            }
        }
        self.emit(json!({
            "type": kind,
            "item": chat_item(item)
        }))
        .await
    }

    async fn incoming(&mut self, incoming: Incoming) -> Result<()> {
        if self.session.handle_auth(&incoming).await? {
            return Ok(());
        }
        let params = incoming.params;
        if let Some(request_id) = incoming.id {
            if incoming.method == "item/tool/requestUserInput"
                && !text(&params, "itemId").is_empty()
                && (self.thread.is_empty() || params["threadId"] == self.thread)
            {
                let fields = params["questions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|q| {
                        json!({
                            "id": q["id"],
                            "title": q["question"],
                            "secret": q["isSecret"].as_bool().unwrap_or(false),
                            "options": q["options"].as_array().cloned().unwrap_or_default()
                        })
                    })
                    .collect::<Vec<_>>();
                if let Ok(fields) = parse("questions", fields.into()) {
                    let thread = if self.thread.is_empty() {
                        text(&params, "threadId")
                    } else {
                        &self.thread
                    };
                    let id = hex_digest(&format!("{thread}:{}", text(&params, "itemId")));
                    self.questions.insert(
                        id.clone(),
                        Question {
                            request_id,
                            message_id: None,
                        },
                    );
                    return self
                        .emit(json!({
                            "type": "chat.question",
                            "question": {
                                "id": id,
                                "blocking": params["isBlocking"] != false,
                                "fields": fields
                            }
                        }))
                        .await;
                }
            }
            return self.session.rpc.reject(request_id).await;
        }
        if params["threadId"].is_string()
            && !self.thread.is_empty()
            && params["threadId"] != self.thread
        {
            return Ok(());
        }
        match incoming.method.as_str() {
            "serverRequest/resolved" => {
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
                    self.emit(json!({
                        "type": "chat.question.closed",
                        "questionId": id
                    }))
                    .await?;
                }
            }
            "turn/started" => {
                self.turn = text(&params["turn"], "id").to_owned();
                self.emit(json!({
                    "type": "turn.started"
                }))
                .await?;
            }
            "item/started" | "item/completed" => {
                self.item(params["item"].clone(), &incoming.method.replace('/', "."))
                    .await?
            }
            "item/agentMessage/delta" => {
                let value = self
                    .texts
                    .entry(text(&params, "itemId").to_owned())
                    .or_default();
                if value.len() + text(&params, "delta").len() > 5_000_000 {
                    return Err(Error::bad_gateway(
                        "Conversation output exceeded the supported limit.",
                    ));
                }
                value.push_str(text(&params, "delta"));
                let value = value.clone();
                self.emit(json!({
                    "type": "item.updated",
                    "item": {
                        "id": params["itemId"],
                        "type": "agent_message",
                        "text": value
                    }
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
                _ = self.cancel.cancelled() => {
                    return Err(Error::conflict("Conversation stopped."));
                }
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

    async fn execute(&mut self, plan: &Value) -> Result<()> {
        let mut effort = text(plan, "reasoning").to_owned();
        if effort.is_empty() {
            // A resumed thread can retain the previous model's effort. Resolve the
            // new model default explicitly instead of accidentally inheriting it.
            if let Ok(models) = crate::models::discover(&mut self.session).await
                && let Some(model) = models.as_array().into_iter().flatten().find(|m| {
                    if text(plan, "model").is_empty() {
                        m["isDefault"] == true
                    } else {
                        m["model"] == plan["model"]
                    }
                })
            {
                effort = text(model, "defaultReasoningEffort").to_owned();
            }
        }
        let mut settings = json!({
            "cwd": plan["cwd"],
            "approvalPolicy": "never",
            "sandbox": if plan["sandbox"] == "yolo" {
                "danger-full-access"
            } else {
                text(plan, "sandbox")
            },
            "developerInstructions": plan["instructions"],
            "config": {
                "features.default_mode_request_user_input": true,
                "sandbox_workspace_write": {
                    "network_access": true,
                    "writable_roots": plan["writableRoots"]
                }
            }
        });
        if !effort.is_empty() {
            settings["config"]["model_reasoning_effort"] = effort.clone().into();
        }
        if !text(plan, "model").is_empty() {
            settings["model"] = plan["model"].clone();
        }
        let resume = plan["sessionId"].is_string();
        if resume {
            settings["threadId"] = plan["sessionId"].clone();
            settings["excludeTurns"] = true.into();
        }
        let result = self
            .request(
                if resume {
                    "thread/resume"
                } else {
                    "thread/start"
                },
                settings,
            )
            .await?;
        self.thread = text(&result["thread"], "id").to_owned();
        if self.thread.is_empty() {
            return Err(Error::bad_gateway(
                "Codex returned an invalid conversation.",
            ));
        }
        self.emit(json!({
            "type": "chat.question.closed"
        }))
        .await?;
        self.emit(json!({
            "type": "thread.started",
            "thread_id": self.thread
        }))
        .await?;
        let mut turns = result["thread"]["turns"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let paginated = result["thread"]["historyMode"] == "paginated";
        if resume && paginated {
            turns.clear();
            let mut cursor = Value::Null;
            let mut cursors = HashSet::new();
            loop {
                let mut params = json!({
                    "threadId": self.thread,
                    "limit": 100,
                    "itemsView": "notLoaded",
                    "sortDirection": "desc"
                });
                if !cursor.is_null() {
                    params["cursor"] = cursor;
                }
                let page = self.request("thread/turns/list", params).await?;
                turns.extend(page["data"].as_array().cloned().ok_or_else(|| {
                    Error::bad_gateway("Codex returned invalid conversation history.")
                })?);
                cursor = page["nextCursor"].clone();
                if cursor.is_null() {
                    break;
                }
                if !cursors.insert(cursor.to_string()) || cursors.len() > 1000 {
                    return Err(Error::bad_gateway(
                        "Codex returned invalid conversation pagination.",
                    ));
                }
            }
            // Read individual items, not full turns: one turn can contain megabytes
            // of command output. Summary view omits steered user messages, whose
            // client IDs are necessary to avoid delivering them twice after restart.
            let mut indices: HashMap<String, usize> = turns
                .iter()
                .enumerate()
                .map(|(index, turn)| (text(turn, "id").to_owned(), index))
                .collect();
            let mut cursor = Value::Null;
            let mut cursors = HashSet::new();
            let mut previous_item_turn = None;
            loop {
                let page = self
                    .request(
                        "thread/items/list",
                        json!({
                            "threadId": self.thread,
                            "limit": 1,
                            "sortDirection": "asc",
                            "cursor": cursor
                        }),
                    )
                    .await?;
                let entries = page["data"].as_array().ok_or_else(|| {
                    Error::bad_gateway("Codex returned invalid conversation items.")
                })?;
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
                        // After interruption, persisted items can outlive their turn
                        // metadata. Recover receipts without assuming completion.
                        // Items arrive oldest first; turns are stored newest first.
                        let index = previous_item_turn.unwrap_or(turns.len());
                        for position in indices.values_mut() {
                            if *position >= index {
                                *position += 1;
                            }
                        }
                        turns.insert(
                            index,
                            json!({"id":turn_id,"status":"interrupted","items":[]}),
                        );
                        indices.insert(turn_id.to_owned(), index);
                        index
                    };
                    previous_item_turn = Some(index);
                    let item = &entry["item"];
                    let items = turns[index]["items"].as_array_mut().ok_or_else(|| {
                        Error::bad_gateway("Codex returned invalid conversation history.")
                    })?;
                    match text(item, "type") {
                        "userMessage" => items.push(json!({
                            "type": "userMessage",
                            "clientId": item["clientId"]
                        })),
                        "agentMessage" => {
                            // Only the final answer is needed to settle a completed
                            // turn. Its tools are already in our durable event log.
                            items.retain(|item| item["type"] != "agentMessage");
                            items.push(item.clone());
                        }
                        _ => {}
                    }
                }
                cursor = page["nextCursor"].clone();
                if cursor.is_null() {
                    break;
                }
                if !cursors.insert(cursor.to_string()) || cursors.len() > 100_000 {
                    return Err(Error::bad_gateway(
                        "Codex returned invalid conversation item pagination.",
                    ));
                }
            }
        } else if resume && turns.is_empty() {
            return Err(Error::bad_gateway(
                "Update Codex to resume conversations with paginated history.",
            ));
        }
        for turn in &turns {
            for item in turn["items"].as_array().into_iter().flatten() {
                if item["type"] == "userMessage"
                    && let Some(id) = item["clientId"].as_str()
                {
                    self.acknowledge(id).await?;
                }
            }
        }
        let accepted = turns.iter().rev().find(|t| {
            t["items"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|i| i["clientId"] == plan["execution"]["messageId"])
            })
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
        let message = if previous.is_some() {
            format!(
                "Continue the interrupted conversation from its last completed step. Preserve completed \
                    work and verify external effects before repeating any action. The pending user request \
                    is:\n{}",
                text(&plan["execution"], "text")
            )
        } else {
            crate::chats::execution_text(plan)
        };
        let mut params = json!({
            "threadId": self.thread,
            "input": crate::attachments::input(
                &message,
                &plan["execution"]["attachments"],
                Path::new(text(plan, "inputDirectory"))
            )
        });
        if !effort.is_empty() {
            params["effort"] = effort.into();
        }
        if previous.is_none() {
            params["clientUserMessageId"] = plan["execution"]["messageId"].clone();
        }
        if !text(plan, "model").is_empty() {
            params["model"] = plan["model"].clone();
        }
        let started = self.request("turn/start", params).await?;
        self.turn = text(&started["turn"], "id").to_owned();
        self.acknowledge(text(&plan["execution"], "messageId"))
            .await?;
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while self.completed.is_none() {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    return Err(Error::conflict("Conversation stopped."));
                }
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
            self.emit(json!({
                "type": "turn.failed",
                "error": turn.get("error").cloned().unwrap_or_else(|| json!(
                    {
                    "message": "Conversation interrupted."
                }
                ))
            }))
            .await?;
            return Err(Error::conflict("Conversation interrupted."));
        }
        self.finish(plan).await
    }

    async fn steer(&mut self, plan: &Value) -> Result<()> {
        let Ok(bytes) =
            tokio::fs::read(Path::new(text(plan, "inputDirectory")).join("messages.json")).await
        else {
            return Ok(());
        };
        if bytes.len() > 2_000_000 {
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
                let result = json!({
                    "answers": answers
                        .iter()
                        .map(|(id, answers)| (
                            id.clone(),
                            json!({
                        "answers": answers
                    })
                        ))
                        .collect::<serde_json::Map<_,
                    _>>()
                });
                self.session
                    .rpc
                    .reply(question.request_id.clone(), result)
                    .await?;
                continue;
            }
            let params = json!({
                "threadId": self.thread,
                "expectedTurnId": self.turn,
                "clientUserMessageId": id,
                "input": crate::attachments::input(
                    text(&message, "text"),
                    &message["attachments"],
                    Path::new(text(plan, "inputDirectory"))
                )
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
        self.emit(json!({
            "type": "turn.completed"
        }))
        .await
    }
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
    let mut auth = crate::accounts::codex::Client::new(home);
    if home.join("leo-managed-auth").exists() && auth.is_none() {
        session.close().await;
        return Err(Error::unavailable(
            "Account authentication service is unavailable.",
        ));
    }
    if let Some(auth) = &mut auth
        && let Err(error) = auth.login(&mut session).await
    {
        session.close().await;
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
    let result = chat.execute(&plan).await;
    chat.session.close().await;
    result
}
