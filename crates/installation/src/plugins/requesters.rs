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
use std::sync::Arc;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Actor {
    pub account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_id: Option<String>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub(crate) enum Principal {
    Account(Actor),
    External(Box<External>),
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
            Self::External(Box::new(External {
                connection_id: connection.id.clone(),
                connection_revision: connection.revision.clone(),
                binding: binding.clone(),
                subject: subject.to_owned(),
            }))
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
                    && a.binding.person == b.binding.person
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
        access_id: Some(grant.access_id.clone()),
    })
}

/// Keep the authenticated account even when live personal proof is unavailable.
/// Missing proof can never be upgraded within this turn. Positive identity and
/// membership refusals remain enforced.
pub(crate) async fn message_actor(s: &Service, account_id: &str) -> Result<Actor> {
    match account(s, account_id).await {
        Ok(actor) => Ok(actor),
        Err(error) if error.status == 503 => Ok(Actor {
            account: account_id.into(),
            access_id: None,
        }),
        Err(error) => Err(error),
    }
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

fn owner_answer_key(chat: &str, message: &str) -> String {
    format!("{}answer:{message}", control_prefix(chat))
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

/// Steering keeps the current turn's authority, including across a stopped
/// execution. A new requester must enter as a separate queued turn.
fn has_turn_authority(
    db: &Db<'_>,
    chat: &str,
    run: &Value,
    actor: Option<&Principal>,
) -> Result<bool> {
    // Credential suspension never changes who originally authored this turn.
    let current = recorded(db, chat, text(&run["chatExecution"], "messageId"))?;
    Ok(match (current.as_ref(), actor) {
        (Some(Principal::Account(current)), Some(Principal::Account(actor)))
            if current.access_id.is_none() =>
        {
            current.account == actor.account
        }
        (Some(current), Some(actor)) => current.same_authority(actor),
        (None, None) => true,
        _ => false,
    })
}

/// Revalidate queued inputs when the native process is about to consume them.
pub(crate) fn steering_authorized(
    db: &Db<'_>,
    chat: &str,
    run: &Value,
    message: &Value,
    source: &Result<Option<SteeringSource>>,
) -> Result<bool> {
    let message_id = text(message, "id");
    let actor = recorded(db, chat, message_id)?;

    if has_turn_authority(db, chat, run, actor.as_ref())? {
        if matches!(actor, Some(Principal::External(_))) {
            source
                .as_ref()
                .map_err(Clone::clone)?
                .as_ref()
                .ok_or_else(|| Error::unavailable("The external control is awaiting authority."))?
                .recheck_in(db)?;
        }
        return Ok(true);
    }

    // Only the exact answer admitted by the authenticated owner can cross this
    // boundary. A suspended credential alone cannot authorize anonymous inputs.
    Ok(actor.is_none()
        && suspended(db, chat, run)?
        && db.kv(&owner_answer_key(chat, message_id))? == Some(json!(control_key(chat, run))))
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
    let mut owner_answer = None;
    if message["mode"] == "steer"
        && let Some(chat) = db.get("chats", chat)?
        && let Some(run) = db.run(text(&chat, "runId"))?
        && !has_turn_authority(db, text(&chat, "id"), &run, actor)?
    {
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
        owner_answer = Some(json!(control_key(text(&chat, "id"), &run)));
        actor = None;
    }
    if !matches!(kind, Capture::Edit)
        && db.messages(chat)?.iter().any(|m| m["id"] == message_id)
        && recorded(db, chat, message_id)?.as_ref() != actor
    {
        return Err(Error::conflict(
            "This message belongs to another requester.",
        ));
    }

    let owner_key = owner_answer_key(chat, message_id);
    match owner_answer {
        Some(binding) => db.set(&owner_key, &binding, None)?,
        None => db.delete(&owner_key)?,
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
    super::routes::require_person(&connection, &actor.subject)?;
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

/// A dormant plugin condition keeps the exact waiting turn and its source identity.
/// It cannot borrow an execution bearer or become personal OAuth authority.
#[derive(Clone)]
pub(crate) struct Observation {
    run: Value,
    owner: crate::waits::Owner,
    pub(crate) principal: Option<Principal>,
    approval: Option<super::registry::Approval>,
}

impl Observation {
    pub(crate) async fn capture(s: &Service, run: &Value, condition: &Value) -> Result<Self> {
        let run = run.clone();
        let condition = condition.clone();
        let observation = s
            .store
            .read(move |db| {
                let owner = crate::waits::Owner::for_run(db, &run)?;
                if owner.record["wakeup"]["condition"] != condition {
                    return Err(Error::conflict("The waiting condition changed."));
                }
                let principal = captured_run(db, &run)?.map(|requester| requester.principal);
                let approval = match &principal {
                    Some(Principal::External(actor)) => {
                        let connection = external_in(db, actor)?;
                        Some(super::registry::Approval::capture_in(
                            db,
                            &connection.plugin_id,
                            &actor.binding.digest,
                        )?)
                    }
                    _ => None,
                };
                let observation = Self {
                    run,
                    owner,
                    principal,
                    approval,
                };
                observation.recheck_in(db)?;
                Ok(observation)
            })
            .await?;
        observation.recheck(s).await?;
        Ok(observation)
    }

    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        match &self.principal {
            Some(Principal::Account(actor)) if actor.access_id.is_some() => {
                if account(s, &actor.account).await? != *actor {
                    return Err(Error::forbidden("The waiting requester's access changed."));
                }
            }
            Some(Principal::External(actor)) => {
                let connection = external(s, actor).await?;
                super::registry::require_digest(s, &connection.plugin_id, &actor.binding.digest)
                    .await?;
            }
            _ => {}
        }
        let expected = self.clone();
        s.store.read(move |db| expected.recheck_in(db)).await
    }

    pub(crate) fn recheck_in(&self, db: &Db<'_>) -> Result<()> {
        let run = db
            .run(text(&self.run, "id"))?
            .ok_or_else(|| Error::forbidden("The waiting run was removed."))?;
        let owner = self
            .owner
            .reload(db)?
            .ok_or_else(|| Error::forbidden("The waiting task was removed."))?;
        if run["snapshot"]["agent"] != self.run["snapshot"]["agent"]
            || run["trigger"] != self.run["trigger"]
            || crate::waits::execution_id(&run) != crate::waits::execution_id(&self.run)
            || owner.run_id() != text(&run, "id")
            || owner.record["wakeup"] != self.owner.record["wakeup"]
            || !owner.resumable(&run)
            || owner.record["wakeup"]["wakeAt"]
                .as_i64()
                .is_none_or(|at| at <= crate::config::now())
            || captured_run(db, &run)?.map(|requester| requester.principal) != self.principal
        {
            return Err(Error::conflict(
                "The waiting turn changed. Retry its condition.",
            ));
        }
        if let Some(Principal::External(actor)) = &self.principal {
            external_in(db, actor)?;
        }
        if let Some(approval) = &self.approval {
            approval.recheck_in(db)?;
        }
        crate::waits::access(db, &run, &self.owner.record["wakeup"]["condition"])
    }
}

/// Different original requesters never share the result of a condition probe.
pub(crate) fn observation_key_in(db: &Db<'_>, run: &Value) -> Result<Value> {
    Ok(serde_json::to_value(
        captured_run(db, run)?.map(|requester| requester.principal),
    )?)
}

/// Native execution retains its admitted source across capacity waits and slow
/// preparation. Owner intervention suspends OAuth, not this source boundary.
#[derive(Clone)]
pub(crate) struct ExecutionSource(RunRequester, super::registry::Approval);

impl ExecutionSource {
    pub(crate) fn is_unavailable(error: &Error) -> bool {
        matches!(error.status, 408 | 429 | 502 | 503 | 504)
    }

    pub(crate) fn is_revoked(error: &Error) -> bool {
        matches!(error.status, 401 | 403 | 404 | 409)
    }

    pub(crate) fn capture_in(db: &Db<'_>, run: &Value) -> Result<Option<Self>> {
        let Some(requester) = captured_run(db, run)? else {
            return Ok(None);
        };
        let Principal::External(actor) = &requester.principal else {
            return Ok(None);
        };
        let connection = external_in(db, actor)?;
        let approval = super::registry::Approval::capture_in(
            db,
            &connection.plugin_id,
            &actor.binding.digest,
        )?;
        Ok(Some(Self(requester, approval)))
    }

    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        if let Principal::External(actor) = &self.0.principal {
            let connection = external(s, actor).await?;
            super::registry::require_digest(s, &connection.plugin_id, &actor.binding.digest)
                .await?;
        }
        let source = self.clone();
        s.store.read(move |db| source.recheck_in(db)).await
    }

    pub(crate) fn recheck_in(&self, db: &Db<'_>) -> Result<()> {
        self.1.recheck_in(db)?;
        recheck_turn_in(db, &self.0).map(|_| ())
    }
}

/// A queued control keeps one exact package approval across file preparation and
/// final consumption. Owner answers use their own explicit provenance instead.
#[derive(Clone)]
pub(crate) struct SteeringSource {
    source: ExecutionSource,
    approved: Arc<super::registry::ApprovedPackage>,
}

impl SteeringSource {
    pub(crate) async fn capture(s: &Service, run: &Value) -> Result<Option<Self>> {
        let run = run.clone();
        let source = s
            .store
            .read(move |db| {
                let Some(source) = ExecutionSource::capture_in(db, &run)? else {
                    return Ok(None);
                };
                let pending = db.messages(&source.0.chat)?.iter().any(|message| {
                    message["mode"] == "steer"
                        && matches!(message["status"].as_str(), Some("queued" | "sending"))
                        && message["id"] != source.0.message
                });
                Ok(pending.then_some(source))
            })
            .await?;
        let Some(source) = source else {
            return Ok(None);
        };
        let Principal::External(actor) = &source.0.principal else {
            unreachable!();
        };
        let connection = external(s, actor).await?;
        let approved = super::registry::approved(s, &connection.plugin_id).await?;
        if approved.package.digest != actor.binding.digest {
            return Err(Error::forbidden("The control's package approval changed."));
        }
        let result = Self {
            source,
            approved: Arc::new(approved),
        };
        let expected = result.clone();
        s.store.read(move |db| expected.recheck_in(db)).await?;
        Ok(Some(result))
    }

    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        self.source.recheck(s).await?;
        super::registry::require_digest(
            s,
            &self.approved.package.manifest.id,
            &self.approved.package.digest,
        )
        .await?;
        let expected = self.clone();
        s.store.read(move |db| expected.recheck_in(db)).await
    }

    pub(crate) fn recheck_in(&self, db: &Db<'_>) -> Result<()> {
        self.approved.recheck_in(db)?;
        self.source.recheck_in(db)
    }
}

fn captured_run(db: &Db<'_>, run: &Value) -> Result<Option<RunRequester>> {
    let run_id = text(run, "id");
    let message = text(&run["chatExecution"], "messageId");
    if message.is_empty() {
        return Ok(None);
    }
    let Some(chat) = crate::chats::chat_for_run(db, run_id)? else {
        return Ok(None);
    };
    let chat_id = text(&chat, "id");
    let suspended = suspended(db, chat_id, run)?;
    Ok(
        recorded(db, chat_id, message)?.map(|principal| RunRequester {
            principal,
            run: run_id.into(),
            chat: chat_id.into(),
            message: message.into(),
            suspended,
        }),
    )
}

pub(crate) async fn for_run(s: &Service, run: &Value) -> Result<Option<RunRequester>> {
    let run = run.clone();
    let requester = s.store.read(move |db| captured_run(db, &run)).await?;
    if let Some(requester) = &requester {
        recheck(s, requester).await?;
    }
    Ok(requester)
}

pub(crate) async fn recheck(s: &Service, requester: &RunRequester) -> Result<()> {
    match &requester.principal {
        Principal::Account(actor) => {
            if actor.access_id.is_some() && account(s, &actor.account).await? != *actor {
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
    let run = recheck_turn_in(db, requester)?;
    if run["status"] != RunStatus::Running
        || suspended(db, &requester.chat, &run)? != requester.suspended
    {
        return Err(Error::forbidden(
            "The requester's execution changed or stopped.",
        ));
    }
    Ok(())
}

fn recheck_turn_in(db: &Db<'_>, requester: &RunRequester) -> Result<Value> {
    if let Principal::External(actor) = &requester.principal {
        external_in(db, actor)?;
    }
    let run = db
        .run(&requester.run)?
        .ok_or_else(|| Error::forbidden("The requester's run was removed."))?;
    if !RunStatus::of(&run).is_some_and(RunStatus::is_active)
        || !run["cancelRequestedAt"].is_null()
        || run["chatExecution"]["messageId"] != requester.message
        || !db
            .get("chats", &requester.chat)?
            .is_some_and(|chat| chat["runId"] == requester.run)
        || recorded(db, &requester.chat, &requester.message)?.as_ref() != Some(&requester.principal)
    {
        return Err(Error::forbidden(
            "The requester's execution changed or stopped.",
        ));
    }
    Ok(run)
}

/// Owner intervention suspends personal credentials, while retaining the original
/// source boundaries for the agent's bot/resource operations.
pub(crate) async fn personal(s: &Service, requester: &RunRequester) -> Result<()> {
    if let Principal::Account(actor) = &requester.principal
        && actor.access_id.is_none()
    {
        return Err(Error::forbidden(
            "Personal authority was unavailable when this turn was admitted. Queue a new turn.",
        ));
    }
    recheck(s, requester).await?;
    if requester.suspended {
        return Err(Error::forbidden(
            "Personal authority is suspended for this turn.",
        ));
    }
    Ok(())
}
