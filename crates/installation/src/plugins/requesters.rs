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
use serde_json::Value;

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

/// Called in the same transaction as the message write. Reusing another
/// requester's message ID cannot transfer authority, even with identical text.
pub(crate) fn capture(
    db: &Db<'_>,
    chat: &str,
    message: &Value,
    actor: Option<&Actor>,
    replacing: bool,
) -> Result<()> {
    let message_id = text(message, "id");
    if !replacing
        && db.messages(chat)?.iter().any(|m| m["id"] == message_id)
        && recorded(db, chat, message_id)?.as_ref() != actor
    {
        return Err(Error::conflict(
            "This message belongs to another requester.",
        ));
    }
    if message["mode"] == "steer"
        && let Some(chat) = db.get("chats", chat)?
        && let Some(run) = db.run(text(&chat, "runId"))?
        && RunStatus::of(&run).is_some_and(RunStatus::is_active)
        && recorded(
            db,
            text(&chat, "id"),
            text(&run["chatExecution"], "messageId"),
        )?
        .as_ref()
            != actor
    {
        return Err(Error::conflict(
            "Queue this message to use your own permissions on the next turn.",
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
    let requester = s
        .store
        .read(move |db| {
            let Some(chat) = crate::chats::chat_for_run(db, &run_id)? else {
                return Ok(None);
            };
            let chat_id = text(&chat, "id");
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
