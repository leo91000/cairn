//! Personal credentials follow an authenticated message, never a plugin argument,
//! a conversation's first author or the installation's default owner.
use crate::{
    error::{Error, Result},
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::text,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Actor {
    pub account: String,
    pub access_id: String,
}

pub(crate) async fn account(s: &Service, account: &str) -> Result<Actor> {
    let policy = crate::relay::task_author_policy(s)
        .await?
        .ok_or_else(|| Error::forbidden("The installation has no verified membership."))?;
    let grant = policy.grant(account).ok_or_else(|| {
        Error::forbidden("This account no longer has access to the installation.")
    })?;
    Ok(Actor {
        account: account.into(),
        access_id: grant.access_id.clone(),
    })
}

pub(crate) fn prefix(chat: &str) -> String {
    format!("plugin-message-requester:{chat}:")
}

fn key(chat: &str, message: &str) -> String {
    format!("{}{message}", prefix(chat))
}

fn recorded(db: &Db<'_>, chat: &str, message: &str) -> Result<Option<Actor>> {
    db.kv(&key(chat, message))?
        .map(serde_json::from_value)
        .transpose()
        .map_err(Error::from)
}

pub(crate) fn control_prefix(chat: &str) -> String {
    format!("plugin-requester-suspended:{chat}:")
}

fn control_key(chat: &str, run: &Value) -> String {
    format!(
        "{}{}:{}",
        control_prefix(chat),
        text(run, "id"),
        text(&run["chatExecution"], "messageId")
    )
}

fn suspended(db: &Db<'_>, chat: &str, run: &Value) -> Result<bool> {
    Ok(db.kv(&control_key(chat, run))?.is_some())
}

#[derive(Clone, Copy)]
pub(crate) enum Capture {
    New,
    Edit,
    /// Current-owner control may proceed, but suspends personal authority for
    /// the running turn instead of inheriting its original requester's token.
    OwnerAnswer,
}

/// Called in the same transaction as the message write. Reusing another
/// requester's message ID cannot transfer authority, even with identical text.
pub(crate) fn capture(
    db: &Db<'_>,
    chat: &str,
    message: &Value,
    mut actor: Option<&Actor>,
    kind: Capture,
) -> Result<()> {
    let message_id = text(message, "id");
    if message["mode"] == "steer"
        && let Some(chat) = db.get("chats", chat)?
        && let Some(run) = db.run(text(&chat, "runId"))?
        && RunStatus::of(&run).is_some_and(RunStatus::is_active)
    {
        let current = if suspended(db, text(&chat, "id"), &run)? {
            None
        } else {
            recorded(
                db,
                text(&chat, "id"),
                text(&run["chatExecution"], "messageId"),
            )?
        };
        if current.as_ref() != actor {
            if !matches!(kind, Capture::OwnerAnswer) {
                return Err(Error::conflict(
                    "Queue this message to use your own permissions on the next turn.",
                ));
            }
            db.set(&control_key(text(&chat, "id"), &run), &json!(true), None)?;
            db.audit(
                "chat.personal_authority.suspended",
                &json!({"chatId": chat["id"], "runId": run["id"]}),
            )?;
            actor = None;
        }
    }
    if !matches!(kind, Capture::Edit)
        && db.messages(chat)?.iter().any(|m| m["id"] == message_id)
        && recorded(db, chat, message_id)?.as_ref() != actor
    {
        return Err(Error::conflict(
            "This message belongs to another requester.",
        ));
    }
    match actor {
        Some(actor) => db.set(&key(chat, message_id), &serde_json::to_value(actor)?, None),
        None => db.delete(&key(chat, message_id)),
    }
}

#[derive(Clone)]
pub(crate) struct RunRequester {
    pub actor: Actor,
    run: String,
    chat: String,
    message: String,
}

pub(crate) async fn for_run(s: &Service, run: &Value) -> Result<Option<RunRequester>> {
    let run_id = text(run, "id").to_owned();
    let message = text(&run["chatExecution"], "messageId").to_owned();
    if message.is_empty() {
        // Scheduled work and external principals need their own explicit authority.
        return Ok(None);
    }
    let run = run.clone();
    let requester = s
        .store
        .read(move |db| {
            let Some(chat) = crate::chats::chat_for_run(db, &run_id)? else {
                return Ok(None);
            };
            let chat_id = text(&chat, "id");
            if suspended(db, chat_id, &run)? {
                return Ok(None);
            }
            Ok(recorded(db, chat_id, &message)?.map(|actor| RunRequester {
                actor,
                run: run_id,
                chat: chat_id.into(),
                message,
            }))
        })
        .await?;
    if let Some(requester) = &requester {
        recheck(s, requester).await?;
    }
    Ok(requester)
}

pub(crate) async fn recheck(s: &Service, requester: &RunRequester) -> Result<()> {
    if account(s, &requester.actor.account).await? != requester.actor {
        return Err(Error::forbidden(
            "The requester's installation access changed.",
        ));
    }
    let requester = requester.clone();
    s.store
        .read(move |db| {
            let run = db
                .run(&requester.run)?
                .ok_or_else(|| Error::forbidden("The requester's run was removed."))?;
            if run["status"] != RunStatus::Running
                || !run["cancelRequestedAt"].is_null()
                || run["chatExecution"]["messageId"] != requester.message
                || suspended(db, &requester.chat, &run)?
                || !db
                    .get("chats", &requester.chat)?
                    .is_some_and(|chat| chat["runId"] == requester.run)
                || recorded(db, &requester.chat, &requester.message)?.as_ref()
                    != Some(&requester.actor)
            {
                return Err(Error::forbidden(
                    "The requester's execution changed or stopped.",
                ));
            }
            Ok(())
        })
        .await
}
