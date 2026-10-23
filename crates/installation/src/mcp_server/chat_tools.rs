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
fn summary(chat: &Value) -> Value {
    let mut result = json!({});
    for key in [
        "id",
        "title",
        "agentId",
        "agentName",
        "projectId",
        "projectName",
        "provider",
        "runId",
        "paused",
        "status",
        "lifecycle",
        "createdAt",
        "updatedAt",
        "finishedAt",
        "pendingQuestions",
    ] {
        result[key] = chat[key].clone();
    }
    result
}

fn list(db: &Db<'_>, args: &Value) -> Result<Value> {
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
    let page: Vec<_> = chats.iter().skip(offset).take(limit).map(summary).collect();
    let next = offset + page.len();
    Ok(json!({
        "chats": page,
        "nextOffset": (next < chats.len()).then_some(next),
        "hasMore": next < chats.len(),
    }))
}

fn readable(db: &Db<'_>, id: &str) -> Result<Value> {
    let mut detail = chats::detail(db, id)?;
    let private: Vec<_> = detail["questions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|question| {
            question["fields"]
                .as_array()
                .is_some_and(|fields| fields.iter().any(|field| field["secret"] == true))
        })
        .map(|question| question["id"].clone())
        .collect();
    for message in detail["messages"].as_array_mut().unwrap() {
        if private.contains(&message["questionId"]) {
            message["text"] = "Private answer".into();
        }
        message.as_object_mut().unwrap().remove("answers");
    }
    Ok(json!({
        "chat": summary(&detail),
        "messages": detail["messages"],
        "questions": detail["questions"],
    }))
}

fn detail(db: &Db<'_>, args: &Value) -> Result<Value> {
    let detail = readable(db, text(args, "chatId"))?;
    let mut result = page(detail["messages"].as_array().unwrap(), args, "messages")?;
    result["chat"] = detail["chat"].clone();
    Ok(result)
}

pub(super) fn page(values: &[Value], args: &Value, key: &str) -> Result<Value> {
    let offset = args["offset"].as_u64().unwrap() as usize;
    let limit = args["limit"].as_u64().unwrap() as usize;
    let mut items = Vec::new();
    let mut bytes = 0;
    for value in values.iter().skip(offset).take(limit) {
        let (value, size) = super::run_record_preview(
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

pub(super) async fn invoke(
    s: &Arc<Service>,
    name: &str,
    args: Value,
    bearer: Option<&str>,
) -> Result<Value> {
    let origin = crate::native::policy::origin();
    let control = matches!(
        name,
        "restore_chat" | "resume_chat" | "restart_chat_session"
    ) || (name == "set_chat_paused" && args["paused"] != true);
    if control && origin.known() {
        crate::native::policy::refresh_chat(s, text(&args, "chatId")).await?;
    }
    let management = management_authorization(bearer);
    let control_id = control.then(|| text(&args, "chatId").to_owned());
    let commit_id = control_id.clone();
    let control_origin = origin.clone();
    let authorize = move |db: &Db<'_>| -> Result<()> {
        management(db)?;
        if let Some(id) = &control_id {
            let chat = required(db.get("chats", id)?, "Chat not found")?;
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
    if matches!(
        name,
        "list_chats" | "get_chat" | "list_chat_questions" | "read_chat_content"
    ) {
        let operation = name.to_owned();
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
                    "list_chats" => list(db, &args)?,
                    "get_chat" => detail(db, &args)?,
                    "read_chat_content" => content(db, &args)?,
                    _ => {
                        let detail = chats::detail(db, text(&args, "chatId"))?;
                        page(detail["questions"].as_array().unwrap(), &args, "questions")?
                    }
                };
                transaction.commit()?;
                Ok(result)
            })
            .await;
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
                let chat = required(db.get("chats", &chat_id)?, "Chat not found")?;
                require_active(&chat)?;
                required(chat["runId"].as_str().map(str::to_owned), "Run not found")
            })
            .await?;
        return s
            .worker
            .resume_authorized(s, &run_id, commit_authority)
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
    if name == "delete_chat" {
        return s
            .chat_trash_authorized(
                text(&args, "chatId"),
                args["confirm"] == true,
                commit_authority,
            )
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
                "send_chat_message" => chats::send(db, id, crate::validation::parse("message", args["message"].clone())?, None, &origin),
                "edit_chat_message" => {
                    let mut message = args["message"].clone();
                    message["id"] = args["messageId"].clone();
                    let message = crate::validation::parse("message", message)?;
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
                    let mut chat = required(db.get("chats", id)?, "Chat not found")?;
                    require_active(&chat)?;
                    if chat["lastActivityAt"].is_null() {
                        chat["lastActivityAt"] = chat["updatedAt"].clone();
                    }
                    chat["title"] = title.into();
                    chat["updatedAt"] = now().into();
                    db.put("chats", &chat)
                }
                _ => Err(Error::not_found("Unknown tool")),
            }
        })
        .await?;
    s.worker.notify();
    Ok(result)
}
