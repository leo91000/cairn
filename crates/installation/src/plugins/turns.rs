//! A queued external request may start a fresh turn after the previous one stops.
//! Its intent is private host state, independently rechecked when the queue advances.
use super::{connections, requesters};
use crate::{
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
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

/// Freeze only a range that has already ended before the next turn can start.
/// A request without its own terminator stays fenced from all future output.
pub(crate) fn abandon(
    db: &Db<'_>,
    chat: &str,
    run: &str,
    message: &str,
    failed: bool,
) -> Result<()> {
    let delivered: Option<i64> = db.0.query_row(
        "SELECT id FROM events WHERE run_id=?1 AND type IN ('chat.user','chat.wakeup.delivered')
         AND json_extract(payload,'$.messageId')=?2 ORDER BY id LIMIT 1",
        rusqlite::params![run, message], |row| row.get(0),
    ).optional()?;
    let record = db.run(run)?;
    let reply = match delivered {
        Some(delivered) => {
            super::work::finished_turn(db, run, message, delivered, record.as_ref())?.map(
                |(end, status)| {
                    json!({
                        "delivered": delivered,
                        "end": end,
                        "failed": status == "failed",
                        "status": status,
                    })
                },
            )
        }
        None => None,
    };
    let mut ended = json!({"runId": run, "failed": failed});
    if let Some(reply) = reply {
        ended["reply"] = reply;
    }
    db.set(&abandoned_key(chat, message), &ended, None)
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

/// Refuse only the exact request still waiting in this run. A newer turn, an
/// edited message or a replaced intent must not be cancelled by a slow check.
fn refuse(
    db: &Db<'_>,
    chat_id: &str,
    run_id: &str,
    message_id: &str,
    intent: &Value,
) -> Result<()> {
    let Some(chat) = db.get("chats", chat_id)? else {
        return Ok(());
    };
    if chat["runId"] != run_id {
        return Ok(());
    }

    let Some(run) = db.run(run_id)? else {
        return Ok(());
    };
    let Some((mut message, current)) = candidate(db, &chat, &run)? else {
        return Ok(());
    };
    if message["id"] != message_id || current != *intent {
        return Ok(());
    }

    message["status"] = crate::chats::MessageStatus::Cancelled.into();
    db.put_message(&message)?;
    db.delete(&key(chat_id, message_id))?;
    db.set(
        &abandoned_key(chat_id, message_id),
        &serde_json::json!({
            "runId": run_id,
            "failed": false,
        }),
        None,
    )?;

    db.audit(
        "chat.new-turn.refused",
        &serde_json::json!({
            "chatId": chat_id,
            "runId": run_id,
            "messageId": message_id,
        }),
    )?;

    Ok(())
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

    let source = match principal {
        Some(requesters::Principal::External(person)) => requesters::external(s, &person)
            .await
            .map(|connection| (person, connection)),
        _ => Err(Error::forbidden(
            "This new turn no longer has its admitted external requester.",
        )),
    };
    let run_id = text(run, "id");
    let (person, connection) = match source {
        Ok(source) => source,
        Err(error) if error.status == 503 => return Ok(Advance::Waiting),
        Err(error) if requesters::ExecutionSource::is_revoked(&error) => {
            let (chat, run, message, intent) = (
                chat_id.clone(),
                run_id.to_owned(),
                message_id.clone(),
                intent.clone(),
            );
            s.store
                .transaction(move |db| refuse(db, &chat, &run, &message, &intent))
                .await?;
            return Ok(Advance::Waiting);
        }
        Err(error) => return Err(error),
    };

    let expected_run = run_id.to_owned();
    let expected_chat = chat_id.clone();
    let ready = s
        .worker
        .request_new_turn_authorized(s, run_id, &chat_id, move |db| {
            let chat = db
                .get("chats", &expected_chat)?
                .ok_or_else(|| Error::not_found("Conversation no longer exists."))?;
            let run = db
                .run(&expected_run)?
                .ok_or_else(|| Error::not_found("Execution no longer exists."))?;
            let Some((next, current)) = candidate(db, &chat, &run)? else {
                return Ok(false);
            };
            if next["id"] != message_id || current != intent {
                return Ok(false);
            }

            let authority = (|| {
                connections::authorize(db, &connection)?;
                requesters::external_in(db, &person)?;
                if db.kv(text(&intent, "binding"))?.as_ref()
                    != Some(&Value::String(expected_chat.clone()))
                    || requesters::recorded(db, &expected_chat, &message_id)?
                        != Some(requesters::Principal::External(person))
                {
                    return Err(Error::forbidden(
                        "The new turn's source or requester changed.",
                    ));
                }
                Ok(())
            })();

            match authority {
                Ok(()) => Ok(true),
                Err(error) if requesters::ExecutionSource::is_revoked(&error) => {
                    refuse(db, &expected_chat, &expected_run, &message_id, &intent)?;
                    Ok(false)
                }
                Err(error) => Err(error),
            }
        })
        .await?;
    Ok(if ready {
        Advance::Ready
    } else {
        Advance::Waiting
    })
}
