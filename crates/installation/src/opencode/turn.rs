use super::{Server, model_id};
use crate::{
    accounts::broker,
    config::Config,
    error::{Error, Result},
    skills::atomic_write,
    validation::{parse, text},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const LIMIT: usize = 2_000_000;

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    message_id: String,
    #[serde(default)]
    submissions: Vec<(String, String)>,
    #[serde(default)]
    submitted_at: Option<i64>,
    #[serde(default)]
    delivered: HashSet<String>,
    #[serde(default)]
    previously_delivered: HashSet<String>,
    #[serde(default)]
    operational_notices: HashSet<String>,
    #[serde(default)]
    complete: bool,
    #[serde(default)]
    answer: String,
    #[serde(default)]
    items: Vec<Value>,
}

async fn emit(events: &mpsc::Sender<Value>, value: Value) -> Result<()> {
    events
        .send(value)
        .await
        .map_err(|_| Error::unavailable("OpenCode output stopped."))
}

impl Receipt {
    fn unexpected_user(&self, info: &Value) -> bool {
        let id = text(info, "id");
        if self.submissions.iter().any(|(_, wire)| wire == id) {
            return false;
        }
        // Native IDs retain only 48 timestamp bits. Creation times distinguish
        // historical messages from continuations even across an ID rollover.
        self.submitted_at
            .zip(info["time"]["created"].as_i64())
            .map_or_else(
                || {
                    self.submissions
                        .first()
                        .is_some_and(|(_, wire)| id >= wire.as_str())
                },
                |(submitted_at, created_at)| created_at >= submitted_at,
            )
    }

    async fn read(path: &Path, id: &str) -> Option<Self> {
        let file = tokio::fs::File::open(path).await.ok()?;
        let bytes = crate::process::read_bounded(file, 8_000_000).await.ok()?;
        let value = serde_json::from_slice::<Self>(&bytes).ok()?;
        if value.message_id == id {
            return Some(value);
        }
        // The manager can still be replacing the previous turn's inbox file.
        // Keep only its acknowledgements; a new turn must not replay old output.
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
            emit(
                events,
                json!({
                    "type": "chat.delivered",
                    "messageId": id,
                }),
            )
            .await?;
        }
        atomic_write(Path::new(text(plan, "output")), self.answer.as_bytes()).await?;
        for item in &self.items {
            emit(
                events,
                json!({
                    "type": "item.completed",
                    "item": item,
                }),
            )
            .await?;
        }
        emit(events, json!({ "type": "turn.completed" })).await
    }
}

fn wire_id(timestamp: i64, counter: u64) -> String {
    // OpenCode encodes the lower six bytes, followed by 14 random characters.
    let time = (timestamp as u64 * 4096 + counter % 4096) & 0xffff_ffff_ffff;
    let random = crate::config::id().replace('-', "");
    format!("msg_{time:012x}{}", &random[..14])
}

async fn parts(body: &str, attachments: &Value, inbox: &Path) -> Result<Vec<Value>> {
    let input = crate::attachments::input(body, attachments, inbox);
    let mut parts = input
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| part["type"] == "text")
        .map(|part| {
            json!({
                "type": "text",
                "text": part["text"],
            })
        })
        .collect::<Vec<_>>();
    for attachment in attachments
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["kind"] == "image")
    {
        let path = inbox
            .join("attachments")
            .join(text(attachment, "id"))
            .join(crate::attachments::filename(text(attachment, "name")));
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .await?;
        let bytes = crate::process::read_bounded(file, 10_000_000).await?;
        let mime = text(attachment, "mediaType");
        if !mime.starts_with("image/") {
            return Err(Error::bad("Invalid OpenCode image attachment."));
        }
        parts.push(json!({
            "type": "file",
            "mime": mime,
            "filename": attachment["name"],
            "url": format!("data:{mime};base64,{}", STANDARD.encode(bytes)),
        }));
    }
    Ok(parts)
}

pub fn chat_item(part: &Value) -> Option<Value> {
    let id = text(part, "id");
    Some(match text(part, "type") {
        "text" => json!({
            "id": id,
            "type": "agent_message",
            "text": part["text"],
        }),
        "reasoning" => json!({
            "id": id,
            "type": "reasoning",
            "text": part["text"],
        }),
        "tool" => {
            let state = &part["state"];
            let status = match text(state, "status") {
                "completed" => "completed",
                "error" => "failed",
                _ => "in_progress",
            };
            if part["tool"] == "todowrite" {
                // The list counts once the call succeeds; it is announced before its arguments.
                if status != "completed" {
                    return None;
                }
                let mut plan = crate::agent_plan::todos(id, &state["input"]);
                plan["status"] = status.into();
                plan
            } else if part["tool"] == "bash" {
                json!({
                    "id": id,
                    "type": "command_execution",
                    "command": state["input"]["command"],
                    "aggregated_output": state["output"],
                    "status": status,
                })
            } else {
                json!({
                    "id": id,
                    "type": "mcp_tool_call",
                    "server": "OpenCode",
                    "tool": part["tool"],
                    "arguments": state["input"],
                    "result": state["output"],
                    "error": state["error"],
                    "status": status,
                })
            }
        }
        _ => return None,
    })
}

fn native_error(error: &Value) -> Error {
    let status = error["data"]["statusCode"]
        .as_u64()
        .or(error["statusCode"].as_u64());
    let message = match status {
        Some(400)
            if error["data"]["message"].as_str().is_some_and(|message| {
                message.contains("This Go model requires Global regions.")
            }) =>
        {
            "OpenCode Go requires Global regions for this model. Review your OpenCode workspace Privacy settings and select Global if appropriate, then resume. To keep your current settings, start a new conversation with another model."
        }
        Some(401 | 403) => {
            "OpenCode authentication failed. Check the model provider and replace its API key in Connections."
        }
        Some(429) => {
            "Usage limit reached for OpenCode. Check this model provider's capacity or connect another account."
        }
        _ => {
            "OpenCode could not complete this response. Check the model, provider connection, and available capacity."
        }
    };
    Error::bad_gateway(message)
}

struct Turn<'a> {
    server: Server,
    plan: &'a Value,
    events: mpsc::Sender<Value>,
    session: String,
    sessions: HashSet<String>,
    receipt: Receipt,
    receipt_path: PathBuf,
    submitted: HashSet<String>,
    pending: HashSet<String>,
    questions: HashMap<String, (String, usize)>,
    user_messages: HashSet<String>,
    items: HashMap<String, Value>,
    finished_items: HashSet<String>,
    wire_counter: u64,
    started: bool,
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
                json!({
                    "type": "chat.delivered",
                    "messageId": id,
                }),
            )
            .await?;
        }
        Ok(())
    }

    async fn submit(&mut self, id: &str, body: &str, attachments: &Value) -> Result<()> {
        let (provider, model) = model_id(text(self.plan, "model"))?;
        self.wire_counter += 1;
        let submitted_at = crate::config::now();
        let wire = wire_id(submitted_at, self.wire_counter);
        let input = parts(
            body,
            attachments,
            Path::new(text(self.plan, "inputDirectory")),
        )
        .await?;
        self.receipt.submitted_at.get_or_insert(submitted_at);
        self.receipt.submissions.push((id.into(), wire.clone()));
        self.receipt.save(&self.receipt_path).await?;
        let mut message = json!({
            "messageID": wire,
            "model": { "providerID": provider, "modelID": model },
            "system": self.plan["instructions"],
            "parts": input,
        });
        if !text(self.plan, "reasoning").is_empty() {
            message["variant"] = self.plan["reasoning"].clone();
        }
        self.server
            .post(&format!("/session/{}/prompt_async", self.session), &message)
            .await?;
        self.submitted.insert(id.into());
        self.pending.insert(id.into());
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
                || self.submitted.contains(id)
                || self.receipt.delivered.contains(id)
                || self.receipt.previously_delivered.contains(id)
            {
                continue;
            }
            let question_id = text(&message, "questionId");
            if message["questionCancelled"] == true {
                if let Some((native_id, _)) = self.questions.get(question_id) {
                    self.server
                        .post(&format!("/question/{native_id}/reject"), &json!({}))
                        .await?;
                    self.questions.remove(question_id);
                }
                self.submitted.insert(id.into());
                continue;
            }
            if let Some((native_id, count)) = self.questions.get(question_id).cloned() {
                let answers = (0..count)
                    .map(|index| {
                        message["answers"][index.to_string()]
                            .as_array()
                            .cloned()
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>();
                self.server
                    .post(
                        &format!("/question/{native_id}/reply"),
                        &json!({ "answers": answers }),
                    )
                    .await?;
                self.questions.remove(question_id);
                self.submitted.insert(id.into());
                self.deliver(id).await?;
                emit(
                    &self.events,
                    json!({
                        "type": "chat.question.closed",
                        "questionId": question_id,
                    }),
                )
                .await?;
            } else if question_id.is_empty() {
                self.submit(id, text(&message, "text"), &message["attachments"])
                    .await?;
            } else {
                return Err(Error::conflict(
                    "This OpenCode question has expired. Send a new message to continue; working files were preserved.",
                ));
            }
        }
        Ok(())
    }

    fn observe_user(&mut self, info: &Value) -> Result<()> {
        let id = text(info, "id");
        self.user_messages.insert(id.into());
        if self.receipt.unexpected_user(info) {
            return Err(Error::bad_gateway(
                "OpenCode created an unsupported continuation. Automatic compaction is disabled for this integration. Start a new conversation; working files were preserved.",
            ));
        }
        Ok(())
    }

    async fn publish_part(&mut self, part: &Value) -> Result<()> {
        let Some(item) = chat_item(part) else {
            return Ok(());
        };
        let id = text(part, "id").to_owned();
        let completed = match text(part, "type") {
            "tool" => ["completed", "error"].contains(&text(&part["state"], "status")),
            _ => !part["time"]["end"].is_null(),
        };
        let previous = self.items.insert(id.clone(), item.clone());
        if previous.is_none() {
            emit(
                &self.events,
                json!({
                    "type": "item.started",
                    "item": item,
                }),
            )
            .await?;
        }
        if completed && self.finished_items.insert(id) {
            emit(
                &self.events,
                json!({
                    "type": "item.completed",
                    "item": item,
                }),
            )
            .await?;
        } else if previous.as_ref() != Some(&item) && !completed {
            emit(
                &self.events,
                json!({
                    "type": "item.updated",
                    "item": item,
                }),
            )
            .await?;
        }
        Ok(())
    }

    async fn on_event(&mut self, event: &Value) -> Result<bool> {
        let props = &event["properties"];
        match text(event, "type") {
            "session.created" if self.sessions.contains(text(&props["info"], "parentID")) => {
                self.sessions.insert(text(&props["info"], "id").into());
            }
            "message.updated" => {
                let info = &props["info"];
                if info["sessionID"] != self.session {
                    return Ok(false);
                }
                if info["role"] == "user" {
                    self.observe_user(info)?;
                    let owned = self
                        .receipt
                        .submissions
                        .iter()
                        .find(|(_, wire)| info["id"] == wire.as_str())
                        .map(|(id, _)| id.clone());
                    if let Some(id) = owned {
                        self.deliver(&id).await?;
                    }
                } else if !info["error"].is_null() {
                    return Err(native_error(&info["error"]));
                }
            }
            "message.part.updated"
                if props["part"]["sessionID"] == self.session
                    && !self
                        .user_messages
                        .contains(text(&props["part"], "messageID"))
                    && !self
                        .receipt
                        .submissions
                        .iter()
                        .any(|(_, wire)| props["part"]["messageID"] == wire.as_str()) =>
            {
                self.publish_part(&props["part"]).await?;
            }
            "message.part.delta"
                if props["sessionID"] == self.session && props["field"] == "text" =>
            {
                let id = text(props, "partID");
                if let Some(item) = self.items.get_mut(id) {
                    let content = format!("{}{}", text(item, "text"), text(props, "delta"));
                    if content.len() > 8_000_000 {
                        return Err(Error::bad_gateway(
                            "OpenCode response exceeded the supported limit.",
                        ));
                    }
                    item["text"] = content.into();
                    emit(
                        &self.events,
                        json!({
                            "type": "item.updated",
                            "item": item,
                        }),
                    )
                    .await?;
                }
            }
            "session.error" if props["sessionID"] == self.session => {
                return Err(native_error(&props["error"]));
            }
            "question.asked" if self.sessions.contains(text(props, "sessionID")) => {
                let id = text(props, "id");
                if !id.starts_with("que_")
                    || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    return Err(Error::bad_gateway("OpenCode returned an invalid question."));
                }
                let fields = props["questions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .map(|(index, question)| {
                        let options = question["options"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|option| {
                                json!({
                                    "label": option["label"],
                                    "description": option["description"].as_str().unwrap_or(""),
                                })
                            })
                            .collect::<Vec<_>>();
                        json!({
                            "id": index.to_string(),
                            "title": question["question"],
                            "options": options,
                        })
                    })
                    .collect::<Vec<_>>();
                let fields = parse("questions", fields.into())?;
                let public_id = question_id(text(props, "sessionID"), id);
                self.questions.insert(
                    public_id.clone(),
                    (id.into(), fields.as_array().unwrap().len()),
                );
                emit(
                    &self.events,
                    json!({
                        "type": "chat.question",
                        "question": { "id": public_id, "blocking": true, "fields": fields },
                    }),
                )
                .await?;
            }
            "question.replied" | "question.rejected"
                if self.sessions.contains(text(props, "sessionID")) =>
            {
                let id = question_id(text(props, "sessionID"), text(props, "requestID"));
                self.questions.remove(&id);
                emit(
                    &self.events,
                    json!({
                        "type": "chat.question.closed",
                        "questionId": id,
                    }),
                )
                .await?;
            }
            "permission.asked" if self.sessions.contains(text(props, "sessionID")) => {
                let id = text(props, "id");
                if !id.starts_with("per_")
                    || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    return Err(Error::bad_gateway(
                        "OpenCode returned an invalid permission request.",
                    ));
                }
                // Configured MCP denials remain denials. Unexpected prompts are rejected;
                // the engine may explain them to the user rather than escalate access.
                self.server.post(&format!("/permission/{id}/reply"), &json!({
                    "reply": "reject",
                    "message": "This tool is outside the agent's access policy. Ask the user in your response.",
                })).await?;
            }
            "session.idle" if props["sessionID"] == self.session => {
                return self.settle(false).await;
            }
            "session.status"
                if props["sessionID"] == self.session && props["status"]["type"] == "idle" =>
            {
                return self.settle(false).await;
            }
            "session.status"
                if props["sessionID"] == self.session
                    && props["status"]["type"] == "busy"
                    && !self.started =>
            {
                self.started = true;
                emit(&self.events, json!({ "type": "turn.started" })).await?;
            }
            _ => {}
        }
        Ok(false)
    }

    async fn settle(&mut self, recovering: bool) -> Result<bool> {
        let messages = self
            .server
            .get(&format!("/session/{}/message?limit=100", self.session))
            .await?;
        if self.receipt.submitted_at.is_none() {
            // Older receipts did not record submission time. Infer it from the
            // native history before checking continuations, regardless of order.
            self.receipt.submitted_at = messages
                .as_array()
                .into_iter()
                .flatten()
                .filter(|message| {
                    message["info"]["role"] == "user"
                        && self
                            .receipt
                            .submissions
                            .iter()
                            .any(|(_, wire)| message["info"]["id"] == wire.as_str())
                })
                .filter_map(|message| message["info"]["time"]["created"].as_i64())
                .min();
        }
        for message in messages.as_array().into_iter().flatten() {
            if message["info"]["role"] == "user" {
                self.observe_user(&message["info"])?;
            }
        }
        let Some(response) = messages
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| {
                m["info"]["role"] == "assistant"
                    && self
                        .receipt
                        .submissions
                        .iter()
                        .any(|(_, wire)| m["info"]["parentID"] == wire.as_str())
            })
            .max_by_key(|message| {
                let info = &message["info"];
                (
                    info["time"]["created"].as_i64().unwrap_or(0),
                    text(info, "id"),
                )
            })
        else {
            return Ok(false);
        };
        let info = &response["info"];
        if !info["error"].is_null() {
            let current = self
                .receipt
                .submissions
                .last()
                .is_some_and(|(_, wire)| info["parentID"] == wire.as_str());
            // A replacement account resumes the same native history. Its new
            // submission must not inherit the previous account's terminal error.
            if recovering || !current {
                return Ok(false);
            }
            return Err(native_error(&info["error"]));
        }
        if info["time"]["completed"].is_null() {
            return Ok(false);
        }
        if info["finish"] != "stop" {
            let latest = self
                .receipt
                .submissions
                .last()
                .is_some_and(|(_, wire)| info["parentID"] == wire.as_str());
            if latest
                && !text(info, "finish").is_empty()
                && info["finish"] != "tool-calls"
                && info["finish"] != "unknown"
            {
                return Err(Error::bad_gateway(
                    "OpenCode stopped before completing the response. Check the model output limit and provider policy.",
                ));
            }
            return Ok(false);
        }
        let Some(parent) = self
            .receipt
            .submissions
            .iter()
            .rposition(|(_, wire)| info["parentID"] == wire.as_str())
        else {
            return Ok(false);
        };
        let settled = self.receipt.submissions[..=parent]
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let later = self.receipt.submissions[parent + 1..]
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<HashSet<_>>();
        for id in settled {
            self.deliver(&id).await?;
            if !later.contains(&id) {
                self.pending.remove(&id);
            }
        }
        if !self.pending.is_empty() || !self.questions.is_empty() {
            return Ok(false);
        }
        let answer = response["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|part| part["type"] == "text")
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        self.receipt.answer = answer;
        self.receipt.items = response["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|part| part["type"] == "text")
            .filter_map(chat_item)
            .collect();
        self.receipt.save(&self.receipt_path).await?;
        Ok(true)
    }

    async fn complete(&mut self) -> Result<()> {
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
        () = cancel.cancelled() => Err(Error::bad_gateway("OpenCode execution stopped.")),
    }
}

async fn execute(
    config: &Config,
    plan: Value,
    events: mpsc::Sender<Value>,
    cancel: CancellationToken,
) -> Result<()> {
    if plan["sandbox"] != "yolo" {
        return Err(Error::bad(
            "OpenCode currently requires an isolated YOLO execution environment.",
        ));
    }
    let initial = text(&plan["execution"], "messageId");
    let receipt_path = Path::new(text(&plan, "output")).with_extension("opencode-receipt.json");
    let saved = Receipt::read(&receipt_path, initial).await;
    if let Some(receipt) = saved
        .as_ref()
        .filter(|r| r.complete && r.delivered.contains(initial))
    {
        return receipt.replay(&plan, &events).await;
    }
    let home = std::env::var_os("CAIRN_OPENCODE_HOME")
        .map_or_else(|| config.home.join(".opencode"), PathBuf::from);
    let credentials = broker::request(
        &broker::socket(&home),
        &broker::Request::default(),
        Duration::from_secs(10),
    )
    .await?;
    let (provider, _) = model_id(text(&plan, "model"))?;
    if credentials["modelProvider"] != provider {
        return Err(Error::bad(
            "This OpenCode account does not match the selected model provider.",
        ));
    }
    let mut native = plan["opencodeConfig"].clone();
    if !native.is_object() {
        native = json!({});
    }
    native["model"] = plan["model"].clone();
    native["small_model"] = plan["model"].clone();
    native["enabled_providers"] = json!([provider]);
    native["share"] = "disabled".into();
    native["autoupdate"] = false.into();
    native["plugin"] = json!([]);
    native["compaction"] = json!({ "auto": false, "prune": false });
    if native["permission"].is_null() {
        native["permission"] = json!({ "*": "allow" });
    }
    let server = tokio::select! {
        result = crate::performance::wait(
            "opencode_startup",
            Server::start(config, &home, Path::new(text(&plan, "cwd")), &native, Some(&credentials)),
        ) => result?,
        () = cancel.cancelled() => return Err(Error::bad_gateway("OpenCode execution stopped.")),
    };
    let session = if let Some(id) = plan["sessionId"].as_str() {
        if !id.starts_with("ses_") || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(Error::bad("Invalid OpenCode session."));
        }
        let saved_session = server.get(&format!("/session/{id}")).await?;
        if saved_session["id"] != id {
            return Err(Error::conflict(
                "The OpenCode session is unavailable. Working files were preserved.",
            ));
        }
        id.to_owned()
    } else {
        text(&server.create().await?, "id").to_owned()
    };
    if !session.starts_with("ses_")
        || !session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(Error::bad_gateway("OpenCode returned an invalid session."));
    }
    emit(
        &events,
        json!({
            "type": "thread.started",
            "thread_id": session,
        }),
    )
    .await?;
    let mut turn = Turn {
        server,
        plan: &plan,
        events,
        sessions: HashSet::from([session.clone()]),
        session,
        receipt: saved.unwrap_or_else(|| Receipt {
            message_id: initial.into(),
            ..Receipt::default()
        }),
        receipt_path,
        submitted: HashSet::new(),
        pending: HashSet::new(),
        questions: HashMap::new(),
        user_messages: HashSet::new(),
        items: HashMap::new(),
        finished_items: HashSet::new(),
        wire_counter: 0,
        started: false,
    };
    let mut stream = turn.server.subscribe().await?;
    if !turn.receipt.submissions.is_empty() {
        let messages = turn
            .server
            .get(&format!("/session/{}/message?limit=100", turn.session))
            .await?;
        let received = messages
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["info"]["role"] == "user")
            .map(|m| text(&m["info"], "id").to_owned())
            .collect::<HashSet<_>>();
        // A receipt is saved before submission. Unacknowledged messages that never
        // reached the native session remain eligible for resubmission from the inbox.
        turn.receipt
            .submissions
            .retain(|(id, wire)| received.contains(wire) || turn.receipt.delivered.contains(id));
    }
    turn.submitted = turn
        .receipt
        .submissions
        .iter()
        .map(|(id, _)| id.clone())
        .collect();
    turn.pending = turn.submitted.clone();
    // Recover a native result saved just before the manager's completion receipt.
    if !turn.receipt.submissions.is_empty() && turn.settle(true).await? {
        return turn.complete().await;
    }
    let mut message = crate::chats::execution_text(&plan);
    if plan["execution"]["recovery"] == true {
        message = format!(
            "Continue the interrupted request. Preserve completed work and verify external effects before repeating an action.\n{message}"
        );
    }
    turn.submit(initial, &message, &plan["execution"]["attachments"])
        .await?;
    let mut timer = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                let _ = tokio::time::timeout(Duration::from_secs(1), turn.server.post(&format!("/session/{}/abort", turn.session), &json!({}))).await;
                return Err(Error::bad_gateway("OpenCode execution stopped."));
            }
            _ = timer.tick() => turn.poll_inbox().await?,
            event = stream.recv() => {
                let event = event.ok_or_else(|| Error::bad_gateway("OpenCode event stream closed."))??;
                if turn.on_event(&event).await? {
                    turn.poll_inbox().await?;
                    if turn.pending.is_empty() && turn.questions.is_empty() { return turn.complete().await; }
                }
            }
        }
    }
}

fn question_id(session: &str, native: &str) -> String {
    crate::auth::hex_digest(&format!("opencode:{session}:{native}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todowrite_shows_as_the_agent_plan_once_it_succeeds() {
        let todos = json!([
            {
                "id": "1",
                "content": "Check the workspace",
                "status": "completed",
                "priority": "high",
            },
            {
                "id": "2",
                "content": "Write a changelog",
                "status": "cancelled",
                "priority": "low",
            },
        ]);
        let part = |status: &str| {
            json!({
                "id": "prt_1",
                "type": "tool",
                "tool": "todowrite",
                "state": {
                    "status": status,
                    "input": { "todos": todos },
                    "output": "[]",
                },
            })
        };

        // A call still running, or one that failed, did not change the plan.
        assert!(chat_item(&part("running")).is_none());
        assert!(chat_item(&part("error")).is_none());
        let item = chat_item(&part("completed")).unwrap();

        assert_eq!(item["type"], "todo_list");
        assert_eq!(item["status"], "completed");
        assert_eq!(item["items"][0]["text"], "Check the workspace");
        assert_eq!(item["items"][0]["completed"], true);
        assert_eq!(item["items"][1]["status"], "cancelled");
        assert!(item.get("result").is_none());
    }

    #[test]
    fn wire_ids_use_the_native_six_byte_timestamp() {
        let id = wire_id(1_800_000_000_000, 1);
        assert_eq!(id.len(), 30);
        assert_eq!(&id[..16], "msg_3185c5000001");
    }

    #[test]
    fn continuation_detection_uses_creation_time_instead_of_wrapped_ids() {
        let receipt = Receipt {
            submitted_at: Some(1_800_000_000_000),
            submissions: vec![("initial".into(), "msg_ffffffffffffabcdefghijklmn".into())],
            ..Receipt::default()
        };
        assert!(!receipt.unexpected_user(&json!({
            "id": "msg_ffffffffffffabcdefghijklmn",
            "time": { "created": 1_800_000_000_001_i64 },
        })));
        assert!(!receipt.unexpected_user(&json!({
            "id": "msg_ffffffffffffzyxwvutsrqponm",
            "time": { "created": 1_799_999_999_999_i64 },
        })));
        assert!(receipt.unexpected_user(&json!({
            "id": "msg_000000000000abcdefghijklmn",
            "time": { "created": 1_800_000_000_000_i64 },
        })));
    }
}
