//! Personal consent for an admitted external person. The callback only captures
//! a code; the same native person must finish from another admitted event.
use super::*;
use crate::plugins::{
    capabilities::Context,
    connections::Connection,
    requesters::{External, Principal},
};

pub(super) fn flow_key(resource: &str, provider: &str, subject: &str) -> String {
    format!(
        "plugin-oauth-external-flow:{resource}:{provider}:{}",
        hex_digest(subject)
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    resource: String,
    provider: String,
    scopes: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct Flow {
    key: String,
    event: String,
}

fn within(connection: &Connection, reference: &resources::Reference, person: &External) -> bool {
    connection.id == person.connection_id
        && (person
            .binding
            .resources
            .iter()
            .any(|(id, _)| id == &reference.resource.id)
            || reference
                .revisions
                .iter()
                .any(|(id, _)| id == &person.binding.resource))
}

pub(super) struct Selection {
    pub(super) key: String,
    pub(super) actor: Option<Actor>,
    pub(super) link: Option<(String, Value)>,
}

/// Never use a linked account as fallback for an existing but invalid/expired
/// external authorization. Each proof must pass its own revocation checks.
pub(super) async fn selection(
    s: &Service,
    connection: &Connection,
    reference: &resources::Reference,
    person: &External,
    name: &str,
) -> Result<Option<Selection>> {
    if !within(connection, reference, person) {
        return Err(Error::forbidden(
            "This OAuth resource is outside the admitted person scope.",
        ));
    }
    let resource = &reference.resource.id;
    let key = external_key(resource, name, &person.subject);
    if s.vault.get(&key).await?.is_some() {
        return Ok(Some(Selection {
            key,
            actor: None,
            link: None,
        }));
    }
    let index = subject_key(resource, name, &person.subject);
    let Some(value) = s.store.kv(&index).await? else {
        return Ok(None);
    };
    let link: SubjectLink = serde_json::from_value(value.clone())?;
    if link.authorization != authorization_key(resource, name, Some(&link.actor))
        || requesters::account(s, &link.actor.account).await? != link.actor
    {
        return Err(Error::forbidden(
            "The external identity's installation access changed.",
        ));
    }
    Ok(Some(Selection {
        key: link.authorization,
        actor: Some(link.actor),
        link: Some((index, value)),
    }))
}

async fn status(
    s: &Service,
    context: &Context,
    reference: &resources::Reference,
    person: &External,
    name: &str,
) -> Result<Value> {
    let Some(selected) = selection(s, &context.connection, reference, person, name).await? else {
        return Ok(json!({"status": "disconnected"}));
    };
    let Some(value) = s.vault.get(&selected.key).await? else {
        return Ok(json!({"status": "disconnected"}));
    };
    let authorization: Authorization = serde_json::from_value(value)?;
    let connection = requesters::external(s, person).await?;
    let mut result = summary(&authorization);
    let personal = authorization.personal.as_ref();
    let current = personal
        .is_some_and(|p| p.subject == person.subject && p.actor == selected.actor)
        && connection.owner_access_id.as_deref() == Some(&authorization.owner_access)
        && connection.revision == authorization.instance_revision
        && registry::active(s, &connection.plugin_id).await?.digest == authorization.digest
        && authorization
            .metadata
            .iter()
            .all(|(key, value)| reference.resource.settings.get(key) == Some(value))
        && !authorization
            .tokens
            .expires_at
            .is_some_and(|at| at <= now() && authorization.tokens.refresh.is_none());
    if !current {
        result["status"] = json!("reconnect");
    }
    resources::recheck(s, context, reference).await?;
    Ok(result)
}

pub(crate) async fn call(
    s: &Service,
    context: &Context,
    method: &str,
    args: &Value,
) -> Result<Value> {
    let input: Input = serde_json::from_value(args.clone()).map_err(|_| {
        Error::bad("Choose a personal OAuth provider, resource and declared scopes.")
    })?;
    if !matches!(
        context.event["name"].as_str(),
        Some("message" | "interaction")
    ) {
        return Err(Error::forbidden(
            "Personal OAuth requires an admitted external person.",
        ));
    }
    let Some(Principal::External(person)) =
        Principal::admitted(&context.connection, text(&context.event, "authorId"))
    else {
        return Err(Error::forbidden(
            "Personal OAuth requires native resource admission.",
        ));
    };
    requesters::external(s, &person).await?;
    let reference = resources::reference(s, context, &input.resource).await?;
    if !within(&context.connection, &reference, &person) {
        return Err(Error::forbidden(
            "Personal OAuth is outside this person's admitted resource.",
        ));
    }
    let package = registry::active(s, &context.connection.plugin_id).await?;
    let provider = provider(
        &package.manifest,
        &input.provider,
        &reference.resource.collection,
    )?;
    if provider.personal.is_none() {
        return Err(Error::forbidden(
            "A person cannot connect an owner-managed OAuth provider.",
        ));
    }
    if method != "oauth.start" && input.scopes.is_some() {
        return Err(Error::bad("Scopes are chosen only when starting OAuth."));
    }
    match method {
        "oauth.start" => {
            let source_event = text(&context.event, "id");
            if source_event.is_empty() {
                return Err(Error::forbidden("OAuth requires a native event identity."));
            }
            let mut scopes = input.scopes.unwrap_or_default();
            scopes.sort();
            let authorized_key = external_key(&input.resource, &input.provider, &person.subject);
            if let Some(value) = s.vault.get(&authorized_key).await? {
                let authorized: Authorization = serde_json::from_value(value)?;
                if authorized.source_event == source_event && authorized.requested_scopes == scopes
                {
                    return status(s, context, &reference, &person, &input.provider).await;
                }
            }
            let index = flow_key(&input.resource, &input.provider, &person.subject);
            if let Some(value) = s.store.kv(&index).await? {
                let flow: Flow = serde_json::from_value(value)?;
                if flow.event == source_event {
                    let value = s.store.kv(&stored_key(&flow.key)).await?.ok_or_else(|| {
                        Error::conflict(
                            "The consent exchange was interrupted. Start authorization again.",
                        )
                    })?;
                    let pending: Pending =
                        serde_json::from_value(s.vault.decrypt(&flow.key, &value)?)?;
                    if pending.scopes == scopes && pending.digest == package.digest {
                        check_initiator(s, provider, &pending).await?;
                        s.store
                            .read({
                                let pending = pending.clone();
                                move |db| current(db, &pending).map(|_| ())
                            })
                            .await?;
                        return Ok(json!({
                            "url": pending.start_url.ok_or_else(expired)?,
                            "redirectUri": pending.redirect_uri,
                            "expiresAt": pending.expires_at,
                        }));
                    }
                }
            }
            let mut started = begin(
                s,
                &package,
                &context.connection,
                reference.resource,
                provider,
                Initiator {
                    account: String::new(),
                    owner_access: context
                        .connection
                        .owner_access_id
                        .clone()
                        .ok_or_else(expired)?,
                    actor: None,
                    external: Some(person),
                    source_event: source_event.into(),
                },
                &json!({"scopes": scopes}),
            )
            .await?;
            if let Some(fields) = started.as_object_mut() {
                fields.remove("state");
            }
            Ok(started)
        }
        "oauth.status" => status(s, context, &reference, &person, &input.provider).await,
        "oauth.finish" => {
            let index = flow_key(&input.resource, &input.provider, &person.subject);
            let _lock = s.plugins.oauth.lock(&index).await;
            let Some(flow) = s.store.kv(&index).await? else {
                return status(s, context, &reference, &person, &input.provider).await;
            };
            let flow: Flow = serde_json::from_value(flow)?;
            let key = flow.key;
            let Some(value) = s.store.kv(&stored_key(&key)).await? else {
                return Err(Error::conflict(
                    "The consent exchange was interrupted. Start authorization again.",
                ));
            };
            let pending: Pending = serde_json::from_value(s.vault.decrypt(&key, &value)?)?;
            let expected = pending
                .external
                .clone()
                .map(Principal::External)
                .ok_or_else(expired)?;
            if !expected.same_authority(&Principal::External(person))
                || pending.resource != input.resource
                || pending.provider != input.provider
            {
                return Err(Error::forbidden(
                    "This consent belongs to another person or source route.",
                ));
            }
            let result = finish_pending(s, &package, provider, key.clone(), value, pending).await;
            if !result
                .as_ref()
                .is_ok_and(|value| value["status"] == "pending")
            {
                s.store
                    .transaction(move |db| {
                        if db.kv(&index)?.is_some_and(|value| value["key"] == key) {
                            db.delete(&index)?;
                        }
                        Ok(())
                    })
                    .await?;
            }
            result
        }
        "oauth.disconnect" => {
            let (connection, person, resource, name) = (
                context.connection.clone(),
                person,
                input.resource,
                input.provider,
            );
            s.store
                .transaction(move |db| {
                    requesters::external_in(db, &person)?;
                    resources::reference_in(db, &connection, &package.manifest, &resource)?;
                    disconnect_subject(db, &resource, &name, &person.subject)?;
                    Ok(json!({"status": "disconnected"}))
                })
                .await
        }
        _ => Err(Error::forbidden("Unknown personal OAuth operation.")),
    }
}

/// Disconnect the proved person, including any linked Cairn account. This avoids
/// silently restoring a second credential after the selected one was disconnected.
pub(super) fn disconnect_subject(
    db: &Db<'_>,
    resource: &str,
    name: &str,
    subject: &str,
) -> Result<()> {
    let index = subject_key(resource, name, subject);
    if let Some(value) = db.kv(&index)? {
        let link: SubjectLink = serde_json::from_value(value)?;
        if link.authorization != authorization_key(resource, name, Some(&link.actor)) {
            return Err(expired());
        }
        disconnect_authorization(db, &link.authorization)?;
        db.delete(&index)?;
    }
    let key = external_key(resource, name, subject);
    disconnect_authorization(db, &key)?;
    let flow = flow_key(resource, name, subject);
    if let Some(key) = db
        .kv(&flow)?
        .and_then(|value| value["key"].as_str().map(str::to_owned))
    {
        db.delete(&stored_key(&key))?;
    }
    db.delete(&flow)
}
