//! A queued external request may start a fresh turn after the previous one stops.
//! Its intent is private host state, independently rechecked when the queue advances.
use super::{connections, requesters};
use crate::{
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use serde_json::Value;
use std::collections::HashSet;

pub(crate) fn prefix(chat: &str) -> String {
    format!("plugin-new-turn:{chat}:")
}

pub(crate) fn key(chat: &str, message: &str) -> String {
    format!("{}{message}", prefix(chat))
}

/// A stopped turn stays stopped even when its conversation reuses the same run.
pub(crate) fn abandoned_prefix(chat: &str) -> String {
    format!("plugin-turn-abandoned:{chat}:")
}

pub(crate) fn abandoned_key(chat: &str, message: &str) -> String {
    format!("{}{message}", abandoned_prefix(chat))
}

fn candidate(db: &Db<'_>, chat: &Value, run: &Value) -> Result<Option<(Value, Value)>> {
    if db.keys(&prefix(text(chat, "id")))?.is_empty() {
        return Ok(None);
    }
    // An answer still queued for the stopped turn is abandoned with that turn.
    // Ordinary requests keep their place; only these native answer messages are skipped.
    let answers = db
        .keys_as::<crate::chats::Question>(&crate::chats::question_prefix(text(chat, "id")))?
        .into_iter()
        .map(|(_, question)| question)
        .filter(|question| {
            question.run_id == text(run, "id")
                && question.turn_id == text(&run["chatExecution"], "messageId")
                && question.is_open()
        })
        .filter_map(|question| question.message_id)
        .collect::<HashSet<_>>();
    let next = db.messages(text(chat, "id"))?.into_iter().find(|message| {
        (message["status"] == "queued" || message["status"] == "sending")
            && message["id"] != run["chatExecution"]["messageId"]
            && !answers.contains(text(message, "id"))
    });
    let Some(message) = next else { return Ok(None) };
    Ok(db
        .kv(&key(text(chat, "id"), text(&message, "id")))?
        .map(|intent| (message, intent)))
}

pub(crate) fn pending(db: &Db<'_>, chat: &Value, run: &Value) -> Result<bool> {
    Ok(candidate(db, chat, run)?.is_some())
}

pub(crate) enum Advance {
    NoRequest,
    Waiting,
    Ready,
}

pub(crate) async fn advance(s: &Service, chat: &Value, run: &Value) -> Result<Advance> {
    // A native owner pause always wins over an already queued external request.
    if chat["paused"] == true {
        return Ok(Advance::NoRequest);
    }
    let (chat_snapshot, run_snapshot) = (chat.clone(), run.clone());
    let Some((message, intent)) = s
        .store
        .read(move |db| candidate(db, &chat_snapshot, &run_snapshot))
        .await?
    else {
        return Ok(Advance::NoRequest);
    };
    let chat_id = text(chat, "id").to_owned();
    let message_id = text(&message, "id").to_owned();
    let principal = s
        .store
        .read({
            let (chat, message) = (chat_id.clone(), message_id.clone());
            move |db| requesters::recorded(db, &chat, &message)
        })
        .await?;
    let Some(requesters::Principal::External(person)) = principal else {
        return Err(Error::forbidden(
            "This new turn no longer has its admitted external requester.",
        ));
    };
    let connection = requesters::external(s, &person).await?;
    let run_id = text(run, "id");
    let expected_run = run_id.to_owned();
    let expected_chat = chat_id.clone();
    let ready = s
        .worker
        .request_new_turn_authorized(s, run_id, &chat_id, move |db| {
            connections::authorize(db, &connection)?;
            requesters::external_in(db, &person)?;
            let chat = db
                .get("chats", &expected_chat)?
                .ok_or_else(|| Error::not_found("Conversation no longer exists."))?;
            let run = db
                .run(&expected_run)?
                .ok_or_else(|| Error::not_found("Execution no longer exists."))?;
            let Some((next, current)) = candidate(db, &chat, &run)? else {
                return Err(Error::conflict("The conversation queue changed."));
            };
            if next["id"] != message_id
                || current != intent
                || db.kv(text(&intent, "binding"))?.as_ref()
                    != Some(&Value::String(expected_chat.clone()))
                || requesters::recorded(db, &expected_chat, &message_id)?
                    != Some(requesters::Principal::External(person))
            {
                return Err(Error::forbidden(
                    "The new turn's source or requester changed.",
                ));
            }
            Ok(())
        })
        .await?;
    Ok(if ready {
        Advance::Ready
    } else {
        Advance::Waiting
    })
}
