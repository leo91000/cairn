use crate::{
    config::{id, now},
    error::{Error, Result, required},
    notifications,
    provider::Provider,
    service::{Service, task_projects},
    store::{Db, merge},
    validation::{parse, text},
};
use serde_json::{Value, json};
use std::collections::HashSet;

// Transfer the visible transcript, never native session state or private answers.
// Recent exchanges are bounded so a long-running chat cannot exhaust the new
// provider's context before it receives the user's next request.
fn handoff_context(db: &Db<'_>, run: &str) -> Result<String> {
    let mut statement = db.0.prepare_cached(
        "SELECT e.type,
          CASE WHEN e.type='chat.user' AND e.text!='Answered a private question.' THEN COALESCE(json_extract(e.payload,'$.text'),e.text) ELSE e.text END,
          json_object('item',json_object('text',substr(json_extract(e.payload,'$.item.text'),-100001)),
            'attachments',json_extract(e.payload,'$.attachments'))
          FROM events e WHERE e.run_id=? AND (
          e.type='chat.user' OR (e.type='item.completed'
          AND json_extract(e.payload,'$.item.type')='agent_message'
          AND NOT EXISTS (SELECT 1 FROM events n WHERE n.run_id=e.run_id AND n.id>e.id
            AND n.type='item.completed' AND json_extract(n.payload,'$.item.type')='agent_message'
            AND json_extract(n.payload,'$.item.id')=json_extract(e.payload,'$.item.id')))
        ) ORDER BY e.id DESC LIMIT 201",
    )?;
    let entries = statement
        .query_map([run], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut remaining = 100_000;
    let mut parts = Vec::new();
    let mut truncated = false;
    for (kind, visible, payload) in entries {
        let payload: Value = payload
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or(Value::Null);
        let body = if kind == "chat.user" {
            visible.as_str()
        } else {
            text(&payload["item"], "text")
        };
        let mut body = body.to_owned();
        if kind == "chat.user" {
            for attachment in payload["attachments"].as_array().into_iter().flatten() {
                body.push_str(&format!(
                    "\nAttached file: {} (attachment ID: {})",
                    text(attachment, "name"),
                    text(attachment, "id")
                ));
            }
        }
        if body.is_empty() {
            continue;
        }
        let size = body.chars().count();
        if size > remaining {
            body = body
                .chars()
                .rev()
                .take(remaining)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            body.insert_str(0, "[Beginning of this message omitted.]\n");
            truncated = true;
        }
        remaining = remaining.saturating_sub(size);
        parts.push(format!(
            "{}:\n{}",
            if kind == "chat.user" {
                "User"
            } else {
                "Assistant"
            },
            body
        ));
        if remaining == 0 || parts.len() == 200 {
            truncated = true;
            break;
        }
    }
    parts.reverse();
    if truncated {
        use rusqlite::OptionalExtension;
        let first: Option<String> =
            db.0.query_row(
                "SELECT text FROM events WHERE run_id=? AND type='chat.user' ORDER BY id LIMIT 1",
                [run],
                |row| row.get(0),
            )
            .optional()?;
        parts.insert(0, format!("Initial user request (excerpt):\n{}\n\n[Earlier exchanges omitted to fit the context budget; recent history follows.]", first.unwrap_or_default().chars().take(8000).collect::<String>()));
    }
    Ok(parts.join("\n\n"))
}

pub fn execution_text(plan: &Value) -> String {
    let current = text(&plan["execution"], "text");
    let context = text(&plan["execution"], "context");
    if context.is_empty() {
        return current.to_owned();
    }
    format!(
        "You are continuing the same Léo chat in a new native agent session. The workspace and completed changes are preserved. Use the prior conversation below as history, not as new requests. Preserve the user's scope and decisions. Verify external effects before repeating any action. Prior attachments remain under {}/attachments/<attachment ID>/.\n\n<previous_conversation>\n{}\n</previous_conversation>\n\nCurrent user message:\n{}",
        text(plan, "inputDirectory"),
        context,
        current
    )
}

// `$name` in a message invokes a skill the run already lists in its
// instructions; spell that out so both providers apply it to this request.
pub fn with_invoked_skills(message: &str, skills: &Value) -> String {
    let names = skills
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| text(s, "name"))
        .collect::<Vec<_>>();
    let invoked = crate::skills::mentions(message, &names);
    if invoked.is_empty() {
        return message.to_owned();
    }
    format!(
        "{message}\n\n<invoked_skills>\nThe user invoked these skills with $name in this message. Apply each one to this request by following its SKILL.md under \"Selected skills\" in your instructions:\n{}\n</invoked_skills>",
        invoked
            .iter()
            .map(|name| format!("- {name}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn chat(db: &Db<'_>, id: &str) -> Result<Value> {
    required(db.get("chats", id)?, "Chat not found")
}
fn questions(db: &Db<'_>, chat: &str) -> Result<Vec<Value>> {
    let mut questions = db
        .keys(&format!("chat-question:{chat}:"))?
        .into_iter()
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    questions.sort_by_key(|q| q["createdAt"].as_i64());
    Ok(questions)
}
fn save_question(db: &Db<'_>, question: &Value) -> Result<Value> {
    db.set(
        &format!(
            "chat-question:{}:{}",
            text(question, "chatId"),
            text(question, "id")
        ),
        question,
        None,
    )?;
    Ok(question.clone())
}
fn view(db: &Db<'_>, mut chat: Value) -> Result<Value> {
    chat["lifecycle"] = crate::conversation_lifecycle::state(&chat).into();
    chat["pendingQuestions"] = questions(db, text(&chat, "id"))?
        .iter()
        .filter(|q| q["status"] == "pending")
        .count()
        .into();
    chat["agentName"] = db
        .get("agents", text(&chat, "agentId"))?
        .map(|a| a["name"].clone())
        .unwrap_or_else(|| {
            chat.get("agentName")
                .cloned()
                .unwrap_or("Deleted agent".into())
        });
    chat["projectName"] = if chat["projectId"].is_null() {
        Value::Null
    } else {
        db.get("projects", text(&chat, "projectId"))?
            .map(|p| p["name"].clone())
            .unwrap_or_else(|| {
                chat.get("projectName")
                    .cloned()
                    .unwrap_or("Deleted project".into())
            })
    };
    chat["status"] = db
        .run(text(&chat, "runId"))?
        .map(|r| r["status"].clone())
        .unwrap_or_else(|| "idle".into());
    Ok(chat)
}
pub(crate) fn list(db: &Db<'_>) -> Result<Vec<Value>> {
    Ok(list_all(db)?
        .into_iter()
        .filter(|chat| crate::conversation_lifecycle::in_view(chat, "active"))
        .collect())
}
pub(crate) fn list_all(db: &Db<'_>) -> Result<Vec<Value>> {
    db.list("chats")?
        .into_iter()
        .map(|chat| view(db, chat))
        .collect()
}
pub(crate) fn detail(db: &Db<'_>, id: &str) -> Result<Value> {
    let mut result = view(db, chat(db, id)?)?;
    if crate::conversation_lifecycle::state(&result) != "active" {
        result["questions"] = json!([]);
        result["messages"] = json!([]);
        result["run"] = Value::Null;
        result["pendingQuestions"] = 0.into();
        return Ok(result);
    }
    result["questions"] = questions(db, id)?.into();
    result["messages"] = db.messages(id)?.into();
    result["run"] = db.run(text(&result, "runId"))?.unwrap_or(Value::Null);
    Ok(result)
}
fn validate_steer(db: &Db<'_>, chat: &Value, message: &Value) -> Result<()> {
    if message["mode"] != "steer" {
        return Ok(());
    }
    if let Some(run) = db.run(text(chat, "runId"))?
        && ["queued", "running"].contains(&text(&run, "status"))
        && ((!text(message, "provider").is_empty()
            && text(message, "provider") != Provider::of_run(&run).as_str())
            || (!text(message, "model").is_empty()
                && message["model"] != run["snapshot"]["agent"]["model"])
            || (!text(message, "reasoning").is_empty()
                && message["reasoning"] != run["snapshot"]["agent"]["reasoning"]))
    {
        return Err(Error::new(
            409,
            "Queue this message to change provider, model or reasoning on the next turn.",
        ));
    }
    Ok(())
}
fn send(
    db: &Db<'_>,
    chat_id: &str,
    mut values: Value,
    answer: Option<(Value, Value)>,
) -> Result<Value> {
    let mut chat = chat(db, chat_id)?;
    crate::conversation_lifecycle::require_active(&chat)?;
    crate::attachments::message(db, chat_id, &mut values)?;
    let messages = db.messages(chat_id)?;
    if let Some(existing) = messages.iter().find(|m| m["id"] == values["id"]) {
        if existing["text"] != values["text"]
            || text(existing, "provider") != text(&values, "provider")
            || existing["model"] != values["model"]
            || text(existing, "reasoning") != text(&values, "reasoning")
            || !crate::attachments::same(existing, &values)
            || answer
                .as_ref()
                .is_some_and(|(q, _)| existing["questionId"] != q["id"])
        {
            return Err(Error::new(
                409,
                "This message identifier has already been used.",
            ));
        }
        return Ok(existing.clone());
    }
    if messages
        .iter()
        .filter(|m| m["status"] != "delivered")
        .count()
        >= 20
    {
        return Err(Error::new(
            409,
            "The queue is full. Wait for a reply or remove a queued message.",
        ));
    }
    if db.0.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE id=?)",
        [text(&values, "id")],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(Error::new(
            409,
            "This message identifier has already been used.",
        ));
    }
    let agent = required(
        db.get("agents", text(&chat, "agentId"))?,
        "This agent is no longer available.",
    )?;
    crate::nodes::require_node(&agent)?;
    task_projects(&agent, &chat, &db.list("projects")?)?;
    if db
        .run(text(&chat, "runId"))?
        .is_some_and(|r| !r["workspaceCleanedAt"].is_null())
    {
        return Err(Error::new(
            409,
            "This workspace has been cleaned up. Start a new chat.",
        ));
    }
    validate_steer(db, &chat, &values)?;
    if messages.is_empty() {
        let title = if text(&values, "text").is_empty() {
            text(&values["attachments"][0], "name")
        } else {
            text(&values, "text")
        };
        chat["title"] = title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(90)
            .collect::<String>()
            .into();
    }
    chat["updatedAt"] = now().into();
    chat["lastActivityAt"] = chat["updatedAt"].clone();
    db.put("chats", &chat)?;
    if let Some((mut question, answers)) = answer {
        question["status"] = "answering".into();
        question["messageId"] = values["id"].clone();
        save_question(db, &question)?;
        values["questionId"] = question["id"].clone();
        values["answers"] = answers;
    }
    merge(
        &mut values,
        &json!({
        "chatId":chat_id,"status":"queued","createdAt":now()}
        ),
    );
    db.put_message(&values)
}
impl Service {
    pub async fn chat_list(&self) -> Result<Vec<Value>> {
        self.store.read(|db| list(db)).await
    }
    pub async fn chat_detail(&self, id: &str) -> Result<Value> {
        let id = id.to_owned();
        self.store.read(move |db| detail(db, &id)).await
    }
    pub async fn chat_create(&self, input: Value) -> Result<Value> {
        let mut value = parse("chat", input)?;
        self.store
            .transaction(move |db| {
                let agent = required(db.get("agents", text(&value, "agentId"))?, "Agent not found")?;
                task_projects(&agent, &value, &db.list("projects")?)?;
                merge(
                    &mut value,
                    &json!({
                    "id":id(),"title":"New chat","runId":null,"paused":false,"createdAt":now(),"updatedAt":now()}
                    ),
                );
                db.put("chats", &value)
            })
            .await
    }
    pub async fn chat_send(&self, id: &str, input: Value) -> Result<Value> {
        let values = parse("message", input)?;
        let id = id.to_owned();
        let result = self
            .store
            .transaction(move |db| send(db, &id, values, None))
            .await?;
        self.worker.notify();
        Ok(result)
    }
    pub async fn chat_edit(&self, id: &str, message: &str, input: Option<Value>) -> Result<Value> {
        let (id, message) = (id.to_owned(), message.to_owned());
        let values = input
            .map(|mut input| {
                input["id"] = message.clone().into();
                parse("message", input)
            })
            .transpose()?;
        let result = self
            .store
            .transaction(move |db| {
                let chat = chat(db, &id)?;
                crate::conversation_lifecycle::require_active(&chat)?;
                let mut current = required(
                    db.messages(&id)?.into_iter().find(|m| m["id"] == message),
                    "Message not found",
                )?;
                if current["status"] != "queued" {
                    return Err(Error::new(409, "This message is already being sent."));
                }
                let Some(mut values) = values else {
                    if let Some(mut question) = questions(db, &id)?
                        .into_iter()
                        .find(|q| q["id"] == current["questionId"])
                    {
                        question["status"] = "pending".into();
                        question.as_object_mut().unwrap().remove("messageId");
                        save_question(db, &question)?;
                    }
                    db.0.execute(
                        "DELETE FROM chat_messages WHERE chat_id=? AND id=?",
                        rusqlite::params![id, message],
                    )?;
                    return Ok(json!({
                    "deleted":true}
                    ));
                };
                if current["questionId"].is_string() {
                    return Err(Error::new(409, "A submitted answer cannot be edited."));
                }
                crate::attachments::message(db, &id, &mut values)?;
                validate_steer(db, &chat, &values)?;
                merge(&mut current, &values);
                db.put_message(&current)
            })
            .await?;
        self.worker.notify();
        Ok(result)
    }
    pub async fn chat_pause(&self, id: &str, paused: bool) -> Result<Value> {
        let id = id.to_owned();
        let result = self
            .store
            .write(move |db| {
                let mut chat = chat(db, &id)?;
                crate::conversation_lifecycle::require_active(&chat)?;
                if chat["lastActivityAt"].is_null() {
                    chat["lastActivityAt"] = chat["updatedAt"].clone();
                }
                merge(
                    &mut chat,
                    &json!({
                    "paused":paused,"updatedAt":now()}
                    ),
                );
                db.put("chats", &chat)
            })
            .await?;
        self.worker.notify();
        Ok(result)
    }
    pub async fn chat_acknowledge(&self, run_id: &str, message_id: &str) -> Result<()> {
        let (run_id, message_id) = (run_id.to_owned(), message_id.to_owned());
        self.store
            .transaction(move |db| {
                let Some(mut chat) = db.list("chats")?.into_iter().find(|c| c["runId"] == run_id)
                else {
                    return Ok(());
                };
                let Some(mut message) = db
                    .messages(text(&chat, "id"))?
                    .into_iter()
                    .find(|m| m["id"] == message_id)
                else {
                    return Ok(());
                };
                if message["status"] == "delivered" || message["status"] == "cancelled" || crate::conversation_lifecycle::state(&chat) != "active" {
                    return Ok(());
                }
                message["status"] = "delivered".into();
                db.put_message(&message)?;
                let mut private = false;
                if let Some(mut question) = questions(db, text(&chat, "id"))?
                    .into_iter()
                    .find(|q| q["id"] == message["questionId"])
                {
                    private = question["fields"]
                        .as_array()
                        .is_some_and(|fields| fields.iter().any(|f| f["secret"] == true));
                    merge(
                        &mut question,
                        &json!({
                        "blocking":false,"status":"answered"}
                        ),
                    );
                    save_question(db, &question)?;
                }
                let text = if private {
                    "Answered a private question."
                } else {
                    text(&message, "text")
                };
                db.event(
                    &run_id,
                    "chat.user",
                    text,
                    Some(&json!({
                    "messageId":message_id,"text":text,"attachments":message["attachments"],"createdAt":message["createdAt"]}
                    )),
                )?;
                chat["updatedAt"] = now().into();
                chat["lastActivityAt"] = chat["updatedAt"].clone();
                db.put("chats", &chat)?;
                Ok(())
            })
            .await
    }
    pub async fn question_receive(&self, run_id: &str, input: Value) -> Result<()> {
        let id = text(&input, "id");
        if id.len() != 64
            || !id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || !input["blocking"].is_boolean()
        {
            return Ok(());
        }
        let Ok(fields) = parse("questions", input["fields"].clone()) else {
            return Ok(());
        };
        let mut question = json!({
        "id":id,"blocking":input["blocking"],"fields":fields,"runId":run_id,"status":"pending","createdAt":now()}
        );
        self.store
            .transaction(move |db| {
                let Some(chat) = db
                    .list("chats")?
                    .into_iter()
                    .find(|c| c["runId"] == question["runId"])
                else {
                    return Ok(());
                };
                if crate::conversation_lifecycle::state(&chat) != "active" {
                    return Ok(());
                }
                question["chatId"] = chat["id"].clone();
                if let Some(mut existing) = questions(db, text(&chat, "id"))?
                    .into_iter()
                    .find(|q| q["id"] == question["id"])
                {
                    existing["blocking"] =
                        (existing["status"] == "pending" && question["blocking"] == true).into();
                    save_question(db, &existing)?;
                    return Ok(());
                }
                save_question(db, &question)?;
                notifications::enqueue(db, &question)
            })
            .await
    }
    pub async fn question_release(&self, run_id: &str, id: Option<&str>) -> Result<()> {
        let (run_id, id) = (run_id.to_owned(), id.map(str::to_owned));
        self.store
            .transaction(move |db| {
                let Some(chat) = db.list("chats")?.into_iter().find(|c| c["runId"] == run_id)
                else {
                    return Ok(());
                };
                for mut question in questions(db, text(&chat, "id"))? {
                    if id.as_ref().is_none_or(|id| question["id"] == *id)
                        && question["blocking"] == true
                    {
                        question["blocking"] = false.into();
                        save_question(db, &question)?;
                    }
                }
                Ok(())
            })
            .await
    }
    pub async fn question_answer(&self, chat_id: &str, id: &str, input: Value) -> Result<Value> {
        let values = parse("answer", input)?;
        let (chat_id, id) = (chat_id.to_owned(), id.to_owned());
        let result = self.store
            .transaction(move |db| {
                chat(db, &chat_id)?;
                let question = required(questions(db, &chat_id)?.into_iter().find(|q| q["id"] == id), "Question not found")?;
                let previous = db.messages(&chat_id)?.into_iter().find(|m| m["id"] == values["id"]);
                if question["messageId"] == values["id"] && previous.is_some_and(|m| m["answers"] == values["answers"]) {
                    return Ok(question);
                }
                if question["status"] != "pending" {
                    return Err(Error::new(409, "This question has already been answered."));
                }
                let fields = question["fields"].as_array().unwrap();
                let answers = values["answers"].as_object().unwrap();
                if answers.len() != fields.len() || fields.iter().any(|f| !answers.contains_key(text(f, "id"))) {
                    return Err(Error::bad("Answer each question before sending."));
                }
                let parts = fields.iter().map(|f| format!("{}\n{}", text(f, "title"), answers[text(f, "id")].as_array().unwrap().iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"))).collect::<Vec<_>>();
                let message = parse(
                    "message",
                    json!({
                    "id":values["id"],"mode":"steer","text":format!("My answers to your questions:\n\n{}",parts.join("\n\n"))}
                    ),
                )?;
                send(db, &chat_id, message, Some((question, values["answers"].clone())))?;
                required(questions(db, &chat_id)?.into_iter().find(|q| q["id"] == id), "Question not found")
            })
            .await?;
        self.worker.notify();
        Ok(result)
    }
    pub async fn chat_tick(&self, active: &HashSet<String>) -> Result<()> {
        for chat in self.store.list("chats").await? {
            if crate::conversation_lifecycle::state(&chat) != "active" {
                continue;
            }
            let id = text(&chat, "id").to_owned();
            let run = if chat["runId"].is_string() {
                Some(self.store.run(text(&chat, "runId")).await?)
            } else {
                None
            };
            if let Some(run) = &run
                && ["queued", "running"].contains(&text(run, "status"))
            {
                let run_id = text(run, "id").to_owned();
                let skills = run["snapshot"]["skills"].clone();
                let run = run.clone();
                let directory = self
                    .config
                    .data_dir
                    .join("runs")
                    .join(text(&chat, "runId"))
                    .join("chat-input");
                let chat = chat.clone();
                let mut steering = self
                    .store
                    .transaction(move |db| {
                        let mut steering = Vec::new();
                        if self::chat(db, text(&chat, "id"))
                            .is_ok_and(|c| crate::conversation_lifecycle::state(&c) != "active")
                        {
                            return Ok(steering);
                        }
                        for mut message in db.messages(text(&chat, "id"))? {
                            if (chat["paused"] != true || message["questionId"].is_string())
                                && message["mode"] == "steer"
                                && ["queued", "sending"].contains(&text(&message, "status"))
                                && message["id"] != run["chatExecution"]["messageId"]
                            {
                                message["status"] = "sending".into();
                                db.put_message(&message)?;
                                steering.push(message);
                            }
                        }
                        Ok(steering)
                    })
                    .await?;
                for message in &mut steering {
                    self.prepare_chat_files(&run_id, &message["attachments"])
                        .await?;
                    message["text"] = with_invoked_skills(text(message, "text"), &skills).into();
                }
                crate::skills::private_dir(&directory).await?;
                crate::skills::atomic_write(
                    &directory.join("messages.json"),
                    &serde_json::to_vec(&steering)?,
                )
                .await?;
                continue;
            }
            if chat["paused"] == true
                || run
                    .as_ref()
                    .is_some_and(|r| active.contains(text(r, "id")) || r["recoveryPending"] == true)
            {
                continue;
            }
            if run.as_ref().is_some_and(|r| r["status"] != "succeeded")
                && chat["cancelledByDeletion"] != true
                && chat["sessionRestartRequested"] != true
            {
                self.chat_pause(&id, true).await?;
                continue;
            }
            let message = self
                .store
                .transaction({
                    let id = id.clone();
                    move |db| {
                        let mut messages = db.messages(&id)?;
                        for message in &mut messages {
                            if message["status"] == "sending" {
                                message["status"] = "queued".into();
                                db.put_message(message)?;
                            }
                        }
                        Ok(messages.into_iter().find(|m| m["status"] == "queued"))
                    }
                })
                .await?;
            let Some(message) = message else {
                continue;
            };
            if let Err(error) = self.chat_prepare(chat, run, message).await {
                self.chat_pause(&id, true).await?;
                self.store
                    .set(
                        &format!("chat-error:{id}"),
                        if error.status < 500 {
                            error.message.into()
                        } else {
                            "Unable to prepare this conversation.".into()
                        },
                        None,
                    )
                    .await?;
            }
        }
        Ok(())
    }
    async fn chat_prepare(&self, chat: Value, run: Option<Value>, message: Value) -> Result<()> {
        let prompt = if text(&message, "text").is_empty() {
            "Review the attached files."
        } else {
            text(&message, "text")
        };
        let mut task = parse(
            "task",
            json!({
            "name":chat["title"],"prompt":prompt,"agentId":chat["agentId"],"projectId":chat["projectId"],"worktree":true}
            ),
        )?;
        merge(
            &mut task,
            &json!({
            "id":chat["id"],"createdAt":chat["createdAt"],"nextRun":null}
            ),
        );
        let mut snapshot = self.snapshot(task, "chat").await?;
        let provider = if !text(&message, "provider").is_empty() {
            text(&message, "provider")
        } else {
            Provider::of_agent(
                run.as_ref()
                    .map(|r| &r["snapshot"]["agent"])
                    .unwrap_or(&snapshot["snapshot"]["agent"]),
            )
            .as_str()
        }
        .to_owned();
        if provider != Provider::of_run(&snapshot).as_str() {
            snapshot["snapshot"]["agent"]["model"] = "".into();
            snapshot["snapshot"]["agent"]["reasoning"] = "".into();
        }
        snapshot["snapshot"]["agent"]["provider"] = provider.into();
        if !text(&message, "model").is_empty() {
            snapshot["snapshot"]["agent"]["model"] = message["model"].clone();
            snapshot["snapshot"]["agent"]["reasoning"] = text(&message, "reasoning").into();
        }
        if !text(&message, "reasoning").is_empty() {
            snapshot["snapshot"]["agent"]["reasoning"] = message["reasoning"].clone();
        }
        crate::claude::validate_agent(&snapshot["snapshot"]["agent"])?;
        self.store
            .transaction(move |db| {
                let mut current_chat = self::chat(db, text(&chat, "id"))?;
                let current = db.messages(text(&chat, "id"))?.into_iter().find(|m| m["id"] == message["id"]);
                if crate::conversation_lifecycle::state(&current_chat) != "active" || current_chat["paused"] == true || current.as_ref().is_none_or(|m| m["status"] != "queued" || m["text"] != message["text"] || m["model"] != message["model"] || text(m,"provider") != text(&message,"provider") || text(m,"reasoning") != text(&message,"reasoning") || !crate::attachments::same(m,&message)) {
                    return Ok(());
                }
                let mut execution = json!({
                "messageId":message["id"],"text":with_invoked_skills(text(&message, "text"), &snapshot["snapshot"]["skills"]),"attachments":message["attachments"],"recovery":false}
                );
                if let Some(run) = run {
                    let switched = Provider::of_run(&snapshot) != Provider::of_run(&run);
                    if snapshot["snapshot"]["agent"]["access"] != run["snapshot"]["agent"]["access"] {
                        return Err(Error::new(409, "Agent access changed. Start a new chat with the updated permissions."));
                    }
                    let key = format!("run-checkpoint:{}", text(&run, "id"));
                    let mut checkpoint = match db.kv(&key)? { Some(value) => value, None if current_chat["cancelledByDeletion"] == true => json!({}), None => return Err(Error::new(409,"Run checkpoint not found")) };
                    if switched || checkpoint["freshSession"] == true {
                        checkpoint["freshSession"] = false.into();
                        execution["context"] = handoff_context(db, text(&run, "id"))?.into();
                        checkpoint["launched"] = false.into();
                        checkpoint.as_object_mut().unwrap().remove("controllerRecoveries");
                        db.patch_run(text(&run, "id"), &json!({"sessionId":null,"resumeAvailable":false}))?;
                        db.event(text(&run,"id"), "status", &format!("Continuing with {} · conversation context and workspace preserved", Provider::of_run(&snapshot).label()), None)?;
                    }
                    checkpoint["completed"] = false.into();
                    checkpoint["remainingMs"] = crate::run_limits::budget_ms(&snapshot["snapshot"]["agent"]).into();
                    checkpoint.as_object_mut().unwrap().remove("lastMessage");
                    checkpoint.as_object_mut().unwrap().remove("settled");
                    db.set(&key, &checkpoint, None)?;
                    db.patch_run(
                        text(&run, "id"),
                        &json!({
                        "snapshot":snapshot["snapshot"],"status":"queued","summary":"","error":null,"outcome":null,"finishedAt":null,"cancelRequestedAt":null,"recoveryPending":true,"chatExecution":execution}
                        ),
                    )?;
                } else {
                    snapshot["chatExecution"] = execution;
                    db.add_run(&snapshot, None)?;
                    current_chat["runId"] = snapshot["id"].clone();
                    db.put("chats", &current_chat)?;
                }
                current_chat["cancelledByDeletion"] = false.into();
                current_chat["sessionRestartRequested"] = false.into();
                db.put("chats", &current_chat)?;
                let mut message = message;
                message["status"] = "sending".into();
                db.put_message(&message)?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod handoff_tests {
    use super::*;

    #[test]
    fn provider_changes_must_wait_for_the_next_turn_and_model_aliases_are_valid() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE runs(id TEXT,data TEXT);")
            .unwrap();
        connection.execute("INSERT INTO runs VALUES('run',?)", [json!({"status":"running","snapshot":{"agent":{"provider":"codex","model":"gpt-6-sol","reasoning":"high"}}}).to_string()]).unwrap();
        let db = Db(&connection);
        let chat = json!({"runId":"run"});
        let mut message = parse("message", json!({"id":id(),"text":"Continue", "provider":"claude","model":"opus[1m]","mode":"steer"})).unwrap();
        assert_eq!(
            validate_steer(&db, &chat, &message).unwrap_err().status,
            409
        );
        message["mode"] = "queue".into();
        validate_steer(&db, &chat, &message).unwrap();
        message["mode"] = "steer".into();
        message["provider"] = "codex".into();
        message["model"] = "gpt-6-sol".into();
        validate_steer(&db, &chat, &message).unwrap();
        assert!(
            parse(
                "message",
                json!({"id":id(),"text":"Continue","provider":"unknown"})
            )
            .is_err()
        );
    }

    #[test]
    fn transcript_keeps_visible_history_and_omits_tool_secrets_and_stream_duplicates() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE events(id INTEGER PRIMARY KEY,run_id TEXT,created_at INTEGER,type TEXT,text TEXT,payload TEXT); CREATE TABLE runs(id TEXT,data TEXT);").unwrap();
        let db = Db(&connection);
        db.event(
            "run",
            "chat.user",
            "Keep the existing design",
            Some(&json!({"attachments":[{"id":"file","name":"brief.pdf"}]})),
        )
        .unwrap();
        db.event(
            "run",
            "item.updated",
            "partial",
            Some(&json!({"item":{"id":"reply","type":"agent_message","text":"partial"}})),
        )
        .unwrap();
        for _ in 0..2 {
            db.event("run", "item.completed", "", Some(&json!({"item":{"id":"reply","type":"agent_message","text":"Changes committed"}}))).unwrap();
        }
        db.event(
            "run",
            "chat.user",
            "Answered a private question.",
            Some(&json!({"text":"private-answer-must-not-be-forwarded"})),
        )
        .unwrap();
        db.event("run", "item.completed", "tool-secret", Some(&json!({"item":{"id":"tool","type":"command_execution","aggregated_output":"tool-secret"}}))).unwrap();
        let history = handoff_context(&db, "run").unwrap();
        assert!(history.contains("Keep the existing design"));
        assert!(history.contains("brief.pdf"));
        assert_eq!(history.matches("Changes committed").count(), 1);
        assert!(!history.contains("partial"));
        assert!(!history.contains("tool-secret"));
        assert!(!history.contains("private-answer-must-not-be-forwarded"));
        let plan = json!({"sessionId":"created-before-interruption","execution":{"text":"Continue", "context":history}});
        assert!(execution_text(&plan).contains("Changes committed"));
        assert!(execution_text(&plan).ends_with("Current user message:\nContinue"));
    }

    #[test]
    fn dollar_mentions_invoke_only_listed_skills_outside_code() {
        let skills = json!([{"name":"review"},{"name":"ship-it"},{"name":"docs"}]);
        let text = with_invoked_skills(
            "Use $review then ($ship-it), again $review, not $HOME, a$docs, \\$docs, $reviewer, `$docs` or\n```\n$docs\n```",
            &skills,
        );
        assert!(text.ends_with("\n- review\n- ship-it\n</invoked_skills>"));
        assert!(text.starts_with("Use $review then"));
        assert!(!text.contains("- docs"));
        assert_eq!(
            with_invoked_skills("Price is $5 for $unknown", &skills),
            "Price is $5 for $unknown"
        );
        assert_eq!(with_invoked_skills("$docs", &Value::Null), "$docs");
    }

    #[test]
    fn long_history_retains_original_scope_and_recent_unicode_without_unbounded_context() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE events(id INTEGER PRIMARY KEY,run_id TEXT,created_at INTEGER,type TEXT,text TEXT,payload TEXT); CREATE TABLE runs(id TEXT,data TEXT);").unwrap();
        let db = Db(&connection);
        db.event("run", "chat.user", "Original scope", None)
            .unwrap();
        db.event("run","item.completed","",Some(&json!({"item":{"id":"huge","type":"agent_message","text":format!("{}Recent decision", "é".repeat(150_000))}}))).unwrap();
        let history = handoff_context(&db, "run").unwrap();
        assert!(history.contains("Original scope"));
        assert!(history.contains("Recent decision"));
        assert!(history.contains("omitted"));
        assert!(history.chars().count() < 109_000);
    }
}
