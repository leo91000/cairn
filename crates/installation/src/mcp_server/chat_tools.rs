//! Chat management shared by public MCP and revocable installation grants.
use crate::{
    chats,
    config::now,
    conversation_lifecycle::{in_view, require_active},
    error::{Error, Result, required},
    installation_mcp::management_authorization,
    service::Service,
    store::Db,
    validation::text,
};
use serde_json::{Value, json};
use std::sync::Arc;

const PAGE_BYTES: usize = 256 * 1024;

/// Stable, small metadata: never include the run snapshot, wakeup payload or answers.
fn summary(chat: &Value) -> Result<Value> {
    let mut result = json!({});
    for key in [
        "id",
        "slug",
        "revision",
        "title",
        "agentId",
        "agentName",
        "projectId",
        "projectName",
        "provider",
        "runId",
        "paused",
        "status",
        "accountWaitReason",
        "accountRequired",
        "lifecycle",
        "createdAt",
        "updatedAt",
        "finishedAt",
        "pendingQuestions",
        "requestedBy",
        "requester",
        "contextUsage",
        "compaction",
        "environmentState",
        "environmentExpiredAt",
        "environmentError",
        "error",
        "lastActivityAt",
        "sessionRestartRequested",
        "canRequestNewSession",
        "pendingSideQuestions",
        "canAskSideQuestion",
        "sideQuestionReason",
        "sideQuestionUnavailable",
        "unread",
        "preview",
    ] {
        result[key] = chat[key].clone();
    }
    Ok(serde_json::to_value(serde_json::from_value::<
        cairn_contracts::Chat,
    >(result)?)?)
}

/// Unread state and previews describe `reader`, the account that authorized the call.
fn list(db: &Db<'_>, args: &Value, reader: Option<&str>) -> Result<Value> {
    let mut chats = chats::list_all(db)?;
    chats.retain(|chat| {
        in_view(chat, text(args, "view"))
            && (args["agentId"].is_null() || chat["agentId"] == args["agentId"])
            && (args["projectId"].is_null() || chat["projectId"] == args["projectId"])
    });
    chats.sort_by(|a, b| {
        b["updatedAt"]
            .as_i64()
            .cmp(&a["updatedAt"].as_i64())
            .then_with(|| text(a, "id").cmp(text(b, "id")))
    });
    let offset = args["offset"].as_u64().unwrap() as usize;
    let limit = args["limit"].as_u64().unwrap() as usize;
    let mut page = chats
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    crate::reads::Reads::load_for(db, reader)?.chats(db, &mut page)?;
    let page = page.iter().map(summary).collect::<Result<Vec<_>>>()?;
    let next = offset + page.len();
    Ok(json!({
        "chats": page,
        "nextOffset": (next < chats.len()).then_some(next),
        "hasMore": next < chats.len(),
    }))
}

fn readable(db: &Db<'_>, id: &str) -> Result<Value> {
    let detail = chats::public_detail(db, id)?;
    Ok(json!({
        "chat": summary(&detail)?,
        "messages": detail["messages"],
        "questions": detail["questions"],
    }))
}

fn detail(db: &Db<'_>, args: &Value) -> Result<Value> {
    use cairn_contracts::{ChatMessagePage, ChatMessagePreview, GetChatOutput};
    let detail = readable(db, text(args, "chatId"))?;
    let offset = args["offset"].as_u64().unwrap() as usize;
    let limit = args["limit"].as_u64().unwrap() as usize;
    let values = detail["messages"].as_array().unwrap();
    let mut messages = Vec::new();
    let mut bytes = 0;
    for value in values.iter().skip(offset).take(limit) {
        let message: cairn_contracts::ChatMessage = serde_json::from_value(value.clone())?;
        let size = serde_json::to_vec(&message)?.len();
        let item = if size <= PAGE_BYTES {
            ChatMessagePage::Complete { message }
        } else {
            ChatMessagePage::Truncated {
                preview: ChatMessagePreview {
                    id: message.id,
                    chat_id: message.chat_id,
                    status: message.status,
                    created_at: message.created_at,
                },
                total_bytes: size
                    .try_into()
                    .map_err(|_| Error::bad("Message is too large."))?,
            }
        };
        let size = serde_json::to_vec(&item)?.len();
        if !messages.is_empty() && bytes + size > PAGE_BYTES {
            break;
        }
        messages.push(item);
        bytes += size;
    }
    let next = offset + messages.len();
    Ok(serde_json::to_value(GetChatOutput {
        chat: serde_json::from_value(detail["chat"].clone())?,
        messages,
        next_offset: (next < values.len()).then_some(next as u32),
        has_more: next < values.len(),
    })?)
}

fn question_page(values: &[Value], args: &Value) -> Result<Value> {
    let offset = args["offset"].as_u64().unwrap() as usize;
    let limit = args["limit"].as_u64().unwrap() as usize;
    let mut questions = Vec::new();
    let mut bytes = 0;
    for value in values.iter().skip(offset).take(limit) {
        let question: cairn_contracts::ChatQuestion = serde_json::from_value(value.clone())?;
        let size = serde_json::to_vec(&question)?.len();
        if !questions.is_empty() && bytes + size > PAGE_BYTES {
            break;
        }
        questions.push(question);
        bytes += size;
    }
    let next = offset + questions.len();
    Ok(serde_json::to_value(
        cairn_contracts::ListChatQuestionsOutput {
            questions,
            next_offset: (next < values.len()).then_some(next as u32),
            has_more: next < values.len(),
        },
    )?)
}

fn side_question_page(detail: &Value, args: &Value) -> Result<Value> {
    let values = detail["sideQuestions"].as_array().unwrap();
    let offset = args["offset"].as_u64().unwrap() as usize;
    let limit = args["limit"].as_u64().unwrap() as usize;
    let mut side_questions = Vec::new();
    let mut bytes = 0;
    for value in values.iter().skip(offset).take(limit) {
        let question: cairn_contracts::ChatSideQuestion = serde_json::from_value(value.clone())?;
        let size = serde_json::to_vec(&question)?.len();
        if !side_questions.is_empty() && bytes + size > PAGE_BYTES {
            break;
        }
        side_questions.push(question);
        bytes += size;
    }
    let next = offset + side_questions.len();
    Ok(serde_json::to_value(
        cairn_contracts::ListChatSideQuestionsOutput {
            side_questions,
            next_offset: (next < values.len()).then_some(next as u32),
            has_more: next < values.len(),
            can_ask_side_question: detail["canAskSideQuestion"] == true,
            side_question_reason: detail["sideQuestionReason"].as_str().map(str::to_owned),
            side_question_unavailable: serde_json::from_value(
                detail["sideQuestionUnavailable"].clone(),
            )?,
        },
    )?)
}

fn record_preview(record: Value, fields: &[&str]) -> Result<(Value, usize)> {
    let bytes = serde_json::to_vec(&record)?.len();
    if bytes <= PAGE_BYTES {
        return Ok((record, bytes));
    }
    let mut preview = json!({
        "truncated": true,
        "totalBytes": bytes
    });
    let mut bytes = serde_json::to_vec(&preview)?.len();
    for field in fields {
        if let Some(value) = record.get(*field) {
            let size = serde_json::to_vec(value)?.len() + field.len() + 4;
            if bytes + size <= PAGE_BYTES {
                preview[*field] = value.clone();
                bytes += size;
            }
        }
    }
    Ok((preview, bytes))
}

pub(super) fn page(values: &[Value], args: &Value, key: &str) -> Result<Value> {
    let offset = args["offset"].as_u64().unwrap() as usize;
    let limit = args["limit"].as_u64().unwrap() as usize;
    let mut items = Vec::new();
    let mut bytes = 0;
    for value in values.iter().skip(offset).take(limit) {
        let (value, size) = record_preview(
            value.clone(),
            &["id", "chatId", "runId", "status", "createdAt", "blocking"],
        )?;
        if !items.is_empty() && bytes + size > PAGE_BYTES {
            break;
        }
        items.push(value);
        bytes += size;
    }
    let next = offset + items.len();
    let mut result = json!({
        "nextOffset": (next < values.len()).then_some(next),
        "hasMore": next < values.len(),
    });
    result[key] = items.into();
    Ok(result)
}

fn content(db: &Db<'_>, args: &Value) -> Result<Value> {
    let data = readable(db, text(args, "chatId"))?.to_string();
    let digest = crate::auth::hex_digest(&data);
    let offset = args["offset"].as_u64().unwrap() as usize;
    if (offset > 0 || args["sha256"].is_string()) && text(args, "sha256") != digest {
        return Err(Error::conflict(
            "Content changed or checksum missing. Restart at offset 0 and pass the returned sha256.",
        ));
    }
    if offset > data.len() || !data.is_char_boundary(offset) {
        return Err(Error::bad(
            "Use the UTF-8 byte offset returned as nextOffset.",
        ));
    }
    let end = data.floor_char_boundary((offset + PAGE_BYTES).min(data.len()));
    Ok(json!({
        "chatId": args["chatId"],
        "encoding": "utf-8",
        "format": "json",
        "offset": offset,
        "totalBytes": data.len(),
        "sha256": digest,
        "data": &data[offset..end],
        "nextOffset": (end < data.len()).then_some(end),
    }))
}

pub(crate) async fn invoke(
    s: &Arc<Service>,
    name: &str,
    args: Value,
    bearer: Option<&str>,
    reader: Option<String>,
) -> Result<Value> {
    let origin = crate::native::policy::origin();
    let control = matches!(
        name,
        "restore_chat" | "resume_chat" | "restart_chat_session" | "switch_chat_coding_agent"
    ) || (name == "set_chat_paused" && args["paused"] != true);
    if origin.known() {
        match name {
            "create_chat" => {
                crate::native::policy::refresh_agent(s, text(&args, "agentId")).await?;
            }
            "send_chat_message" | "edit_chat_message" => {
                crate::native::policy::refresh_chat(s, text(&args, "chatId")).await?;
            }
            _ if control => {
                crate::native::policy::refresh_chat(s, text(&args, "chatId")).await?;
            }
            _ => {}
        }
    }
    let management = management_authorization(bearer);
    let control_id = control.then(|| text(&args, "chatId").to_owned());
    let commit_id = control_id.clone();
    let control_origin = origin.clone();
    let authorize = move |db: &Db<'_>| -> Result<()> {
        management(db)?;
        if let Some(id) = &control_id {
            let chat = required(db.conversation_boundary(id)?, "Chat not found")?;
            crate::native::policy::chat_grant(db, &chat, &control_origin)?;
        }
        Ok(())
    };
    let commit_authority = {
        let check = authorize.clone();
        let source = origin.clone();
        move |db: &Db<'_>| -> Result<()> {
            check(db)?;
            if let Some(id) = &commit_id {
                crate::native::policy::authorize_chat_in(db, id, &source)?;
            }
            Ok(())
        }
    };
    if name == "get_chat_coding_agent_options" {
        return chats::coding_agent::options(s, text(&args, "chatId"), authorize).await;
    }
    if name == "switch_chat_coding_agent" {
        return s.switch_chat_coding_agent(args, authorize).await;
    }
    if matches!(
        name,
        "list_chats"
            | "get_chat"
            | "list_chat_questions"
            | "list_chat_side_questions"
            | "read_chat_content"
    ) {
        let operation = name.to_owned();
        if let Some(reader) = reader.as_deref().filter(|_| name == "list_chats") {
            crate::reads::begin(s, reader).await?;
        }
        return s
            .store
            .read(move |db| {
                let transaction = rusqlite::Transaction::new_unchecked(
                    db.0,
                    rusqlite::TransactionBehavior::Deferred,
                )?;
                let db = &Db(&transaction);
                authorize(db)?;
                let result = match operation.as_str() {
                    "list_chats" => list(db, &args, reader.as_deref())?,
                    "get_chat" => detail(db, &args)?,
                    "read_chat_content" => content(db, &args)?,
                    "list_chat_side_questions" => {
                        let detail = chats::detail(db, text(&args, "chatId"))?;
                        side_question_page(&detail, &args)?
                    }
                    _ => {
                        let detail = chats::detail(db, text(&args, "chatId"))?;
                        question_page(detail["questions"].as_array().unwrap(), &args)?
                    }
                };
                transaction.commit()?;
                Ok(result)
            })
            .await;
    }
    if name == "set_chat_paused" {
        let chat_id = text(&args, "chatId").to_owned();
        let paused = args["paused"] == true;
        let check = authorize.clone();
        let record = s
            .store
            .read({
                let chat_id = chat_id.clone();
                move |db| {
                    check(db)?;
                    let chat = required(db.conversation(&chat_id)?, "Chat not found")?;
                    if chat.lifecycle != cairn_contracts::ConversationLifecycle::Active {
                        return Err(Error::conflict("This conversation has been deleted."));
                    }
                    Ok((
                        chat.clone(),
                        match chat.run_id.as_deref() {
                            Some(run) => Some(required(db.run_state(run)?, "Run not found")?),
                            None => None,
                        },
                    ))
                }
            })
            .await?;
        if !paused
            && !record.0.cancelled_by_deletion
            && !record.0.session_restart_requested
            && let Some(run) = record.1.filter(|run| run.status.is_resumable())
        {
            s.worker
                .resume_authorized(s, &run.id, commit_authority.clone())
                .await?;
        }
        let result = s
            .store
            .transaction(move |db| {
                commit_authority(db)?;
                db.delete(&format!("chat-error:{chat_id}"))?;
                chats::pause_in(db, &chat_id, paused)
            })
            .await?;
        s.worker.notify();
        return Ok(result);
    }
    if name == "restore_chat" {
        return s
            .chat_restore_authorized(text(&args, "chatId"), commit_authority)
            .await;
    }
    if name == "resume_chat" {
        let chat_id = text(&args, "chatId").to_owned();
        let check = authorize.clone();
        let run_id = s
            .store
            .read(move |db| {
                check(db)?;
                let chat = required(db.conversation_boundary(&chat_id)?, "Chat not found")?;
                require_active(&chat)?;
                required(chat["runId"].as_str().map(str::to_owned), "Run not found")
            })
            .await?;
        return s
            .worker
            .resume_authorized(s, &run_id, commit_authority)
            .await;
    }
    if name == "cancel_chat_question" {
        let native = crate::native::policy::chat_authorization(text(&args, "chatId"));
        return s
            .question_cancel_authorized(
                text(&args, "chatId"),
                text(&args, "questionId"),
                move |db| {
                    commit_authority(db)?;
                    native(db)
                },
            )
            .await;
    }
    if name == "answer_chat_question" {
        return s
            .question_answer_authorized(
                text(&args, "chatId"),
                text(&args, "questionId"),
                args["answer"].clone(),
                commit_authority,
            )
            .await;
    }
    if name == "ask_chat_side_question" {
        let question = serde_json::from_value(args["question"].clone())?;
        return s
            .side_question_ask_authorized(text(&args, "chatId"), question, commit_authority)
            .await;
    }
    if name == "cancel_chat_side_question" {
        return s
            .side_question_cancel_authorized(
                text(&args, "chatId"),
                text(&args, "sideQuestionId"),
                authorize,
            )
            .await;
    }
    if name == "delete_chat" {
        return s
            .chat_trash_authorized(
                text(&args, "chatId"),
                args["confirm"] == true,
                commit_authority,
            )
            .await;
    }
    if name == "compact_chat" {
        // Compaction starts model work: the conversation's own grant is checked when it commits.
        return s
            .chat_compact_authorized(text(&args, "chatId"), commit_authority)
            .await;
    }
    if name == "stop_chat" {
        return s
            .worker
            .stop_chat_authorized(s, text(&args, "chatId"), commit_authority)
            .await;
    }

    if origin.known() {
        if name == "create_chat" {
            crate::native::policy::refresh_agent(s, text(&args, "agentId")).await?;
        } else if matches!(name, "send_chat_message" | "edit_chat_message") {
            crate::native::policy::refresh_chat(s, text(&args, "chatId")).await?;
        }
    }
    let name = name.to_owned();
    let result = s
        .store
        .transaction(move |db| {
            commit_authority(db)?;
            let id = text(&args, "chatId");
            match name.as_str() {
                "restart_chat_session" => {
                    if args["confirm"] != true {
                        return Err(Error::conflict("Confirm starting a fresh agent session using the preserved history and files."));
                    }
                    crate::conversation_lifecycle::request_new_session(db, id)
                }
                "create_chat" => chats::create_as(db, args, &origin),
                "send_chat_message" => chats::send(db, id, chats::parse_message(args["message"].clone())?, None, &origin),
                "edit_chat_message" => {
                    let mut message = args["message"].clone();
                    message["id"] = args["messageId"].clone();
                    let message = chats::parse_edit(message)?;
                    chats::edit_as(db, id, text(&args, "messageId"), Some(message), &origin)
                }
                "delete_chat_message" => chats::edit(db, id, text(&args, "messageId"), None),
                "set_chat_paused" => chats::pause_in(db, id, args["paused"] == true),
                "rename_chat" => {
                    let title = text(&args, "title").trim();
                    if title.is_empty() || title.chars().any(char::is_control) {
                        return Err(Error::bad(
                            "Choose a nonempty title without control characters.",
                        ));
                    }
                    if title.chars().count() > 90 { return Err(Error::bad("Choose a title of at most 90 characters.")); }
                    let mut chat = required(db.conversation(id)?, "Chat not found")?;
                    if chat.lifecycle != cairn_contracts::ConversationLifecycle::Active { return Err(Error::conflict("This conversation has been deleted.")); }
                    if chat.revision != text(&args, "revision") { return Err(Error::conflict("This conversation changed. Reload before renaming.")); }
                    if chat.last_activity_at.is_none() { chat.last_activity_at = Some(chat.updated_at); }
                    chat.title = title.to_owned();
                    chat.updated_at = now();
                    Ok(serde_json::to_value(db.save_conversation(&chat)?)?)
                }
                _ => Err(Error::not_found("Unknown tool")),
            }
        })
        .await?;
    s.worker.notify();
    Ok(result)
}
