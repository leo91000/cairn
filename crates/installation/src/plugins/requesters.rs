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

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub(crate) enum Principal {
    Account(Actor),
    External(External),
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct External {
    pub connection_id: String,
    pub connection_revision: String,
    pub binding: super::routes::Binding,
    pub subject: String,
}

impl Principal {
    pub(crate) fn admitted(
        connection: &super::connections::Connection,
        subject: &str,
    ) -> Option<Self> {
        connection.routing.as_ref().map(|binding| {
            Self::External(External {
                connection_id: connection.id.clone(),
                connection_revision: connection.revision.clone(),
                binding: binding.clone(),
                subject: subject.to_owned(),
            })
        })
    }

    /// Interaction context may differ from message context, but not the native
    /// resource, route, revision or subject which supplied its authority.
    pub(crate) fn same_authority(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Account(a), Self::Account(b)) => a == b,
            (Self::External(a), Self::External(b)) => {
                a.connection_id == b.connection_id
                    && a.connection_revision == b.connection_revision
                    && a.subject == b.subject
                    && a.binding.id == b.binding.id
                    && a.binding.revision == b.binding.revision
                    && a.binding.resource == b.binding.resource
                    && a.binding.resources == b.binding.resources
                    && a.binding.digest == b.binding.digest
            }
            _ => false,
        }
    }
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

pub(crate) fn recorded(db: &Db<'_>, chat: &str, message: &str) -> Result<Option<Principal>> {
    db.kv(&key(chat, message))?
        .map(serde_json::from_value)
        .transpose()
        .map_err(Error::from)
}

pub(crate) fn control_prefix(chat: &str) -> String {
    format!("plugin-requester-suspended:{chat}:")
}

fn control_key(chat: &str, run: &Value) -> String {
    control_key_for(
        chat,
        text(run, "id"),
        text(&run["chatExecution"], "messageId"),
    )
}

fn control_key_for(chat: &str, run: &str, message: &str) -> String {
    format!("{}{run}:{message}", control_prefix(chat))
}

fn suspended(db: &Db<'_>, chat: &str, run: &Value) -> Result<bool> {
    Ok(db.kv(&control_key(chat, run))?.is_some())
}

/// A native wakeup continues the task which scheduled it. Its synthetic input ID
/// identifies a new delivery, never a new person or a release of owner control.
pub(crate) fn capture_wakeup(db: &Db<'_>, chat: &str, run: &Value, message: &Value) -> Result<()> {
    let previous = text(&run["chatExecution"], "messageId");
    if message["wakeup"]["messageId"] != previous {
        return Err(Error::conflict("The wakeup's requesting turn changed."));
    }
    let principal = recorded(db, chat, previous)?;
    capture_principal(db, chat, message, principal.as_ref(), Capture::New)?;
    if suspended(db, chat, run)? {
        let key = control_key_for(chat, text(run, "id"), text(message, "id"));
        db.set(&key, &json!(true), None)?;
    }
    Ok(())
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
    actor: Option<&Actor>,
    kind: Capture,
) -> Result<()> {
    let principal = actor.cloned().map(Principal::Account);
    capture_principal(db, chat, message, principal.as_ref(), kind)
}

pub(crate) fn capture_principal(
    db: &Db<'_>,
    chat: &str,
    message: &Value,
    mut actor: Option<&Principal>,
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
        let same = match (current.as_ref(), actor) {
            (Some(current), Some(actor)) => current.same_authority(actor),
            (None, None) => true,
            _ => false,
        };
        if !same {
            if !matches!(kind, Capture::OwnerAnswer) {
                return Err(Error::conflict(
                    "Queue this message to use your own permissions on the next turn.",
                ));
            }
            db.set(&control_key(text(&chat, "id"), &run), &json!(true), None)?;
            db.audit(
                "chat.personal_authority.suspended",
                &json!({ "chatId": chat["id"], "runId": run["id"] }),
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

/// The same native admission authority is used by runs and personal OAuth flows.
pub(crate) fn external_in(db: &Db<'_>, actor: &External) -> Result<super::connections::Connection> {
    let value = db
        .get(super::connections::KIND, &actor.connection_id)?
        .ok_or_else(|| Error::forbidden("The requester's instance was removed."))?;
    let mut expected: super::connections::Connection = serde_json::from_value(value)?;
    if expected.revision != actor.connection_revision {
        return Err(Error::forbidden("The requester's instance changed."));
    }
    expected.routing = Some(actor.binding.clone());
    let connection = super::connections::authorize(db, &expected)?;
    if !connection.allows(&actor.subject) {
        return Err(Error::forbidden("The requester is no longer allowed."));
    }
    Ok(connection)
}

pub(crate) async fn external(
    s: &Service,
    actor: &External,
) -> Result<super::connections::Connection> {
    let actor = actor.clone();
    let connection = s.store.read(move |db| external_in(db, &actor)).await?;
    super::connections::ensure_owner(s, &connection).await?;
    Ok(connection)
}

#[derive(Clone)]
pub(crate) struct RunRequester {
    pub principal: Principal,
    run: String,
    chat: String,
    message: String,
    suspended: bool,
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
            let suspended = suspended(db, chat_id, &run)?;
            Ok(
                recorded(db, chat_id, &message)?.map(|principal| RunRequester {
                    principal,
                    run: run_id,
                    chat: chat_id.into(),
                    message,
                    suspended,
                }),
            )
        })
        .await?;
    if let Some(requester) = &requester {
        recheck(s, requester).await?;
    }
    Ok(requester)
}

pub(crate) async fn recheck(s: &Service, requester: &RunRequester) -> Result<()> {
    match &requester.principal {
        Principal::Account(actor) => {
            if account(s, &actor.account).await? != *actor {
                return Err(Error::forbidden(
                    "The requester's installation access changed.",
                ));
            }
        }
        Principal::External(actor) => {
            external(s, actor).await?;
        }
    }
    let requester = requester.clone();
    s.store.read(move |db| recheck_in(db, &requester)).await
}

/// Local authority fences can share a snapshot with resource and credential checks.
pub(crate) fn recheck_in(db: &Db<'_>, requester: &RunRequester) -> Result<()> {
    if let Principal::External(actor) = &requester.principal {
        external_in(db, actor)?;
    }
    let run = db
        .run(&requester.run)?
        .ok_or_else(|| Error::forbidden("The requester's run was removed."))?;
    if run["status"] != RunStatus::Running
        || !run["cancelRequestedAt"].is_null()
        || run["chatExecution"]["messageId"] != requester.message
        || suspended(db, &requester.chat, &run)? != requester.suspended
        || !db
            .get("chats", &requester.chat)?
            .is_some_and(|chat| chat["runId"] == requester.run)
        || recorded(db, &requester.chat, &requester.message)?.as_ref() != Some(&requester.principal)
    {
        return Err(Error::forbidden(
            "The requester's execution changed or stopped.",
        ));
    }
    Ok(())
}

/// Owner intervention suspends personal credentials, while retaining the original
/// source boundaries for the agent's bot/resource operations.
pub(crate) async fn personal(s: &Service, requester: &RunRequester) -> Result<()> {
    recheck(s, requester).await?;
    if requester.suspended {
        return Err(Error::forbidden(
            "Personal authority is suspended for this turn.",
        ));
    }
    Ok(())
}
