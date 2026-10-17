//! Owner consent for an inert setup resource. It shares OAuth protocol handling,
//! while every read/write remains explicitly bound to the draft namespace.
use super::*;
use resources::target::Target;

pub(crate) async fn routes(
    s: &Service,
    input: &crate::http::Input,
    plugin: &str,
    draft: &str,
    path: &[&str],
    tail: &[&str],
) -> Result<Value> {
    let [collection, resource, name] = path else {
        return Err(Error::not_found("OAuth resource not found."));
    };
    if !tail.is_empty() && tail != ["finish"] {
        return Err(Error::not_found("OAuth action not found."));
    }
    let account = input
        .identity
        .as_ref()
        .map_or("", |identity| identity.account_id.as_str());
    let target = Target::setup(s, plugin, draft, account).await?;
    let snapshot = target.setup_snapshot().ok_or_else(expired)?;
    let package = target.package();
    let provider = provider(&package.manifest, name, collection)?;
    if provider.personal.is_some() {
        return Err(Error::forbidden(
            "Activate the resource before connecting personal OAuth.",
        ));
    }
    let resource = resources::get_for(s, target.clone(), collection, resource).await?;
    match (input.method.as_str(), tail.is_empty()) {
        ("POST", true) => {
            target.require_revision(&input.body)?;
            let mut body = input.body.clone();
            if let Some(body) = body.as_object_mut() {
                body.remove("setupRevision");
            }
            let mut connection = snapshot.connection();
            connection.revision.clone_from(&snapshot.revision);
            begin(
                s,
                package,
                &connection,
                resource,
                provider,
                Initiator {
                    account: account.into(),
                    owner_access: snapshot.owner_access_id.clone(),
                    actor: None,
                    external: None,
                    source_event: String::new(),
                    setup_target: Some(target.clone()),
                },
                &body,
            )
            .await
        }
        ("POST", false) => {
            let state = text(&input.body, "state");
            if state.len() != 43 {
                return Err(expired());
            }
            let key = pending_key(state);
            let value = s.store.kv(&stored_key(&key)).await?.ok_or_else(expired)?;
            let pending: Pending = serde_json::from_value(s.vault.decrypt(&key, &value)?)?;
            if pending.setup.is_none()
                || pending.account != account
                || pending.instance != draft
                || pending.resource != resource.id
                || pending.provider != *name
                || pending.actor.is_some()
                || pending.external.is_some()
                || pending.owner_access != snapshot.owner_access_id
            {
                return Err(Error::forbidden(
                    "This authorization belongs to another setup resource.",
                ));
            }
            finish_pending(s, package, provider, key, value, pending).await
        }
        ("GET", true) => {
            let value = s
                .vault
                .get(&authorization_key(&resource.id, name, None))
                .await?;
            let check = target.clone();
            let expected = resource.clone();
            s.store
                .read(move |db| {
                    check.current(db)?;
                    if check
                        .get(db, &expected.id)?
                        .is_none_or(|current| current["revision"] != expected.revision)
                    {
                        return Err(expired());
                    }
                    Ok(())
                })
                .await?;
            let Some(value) = value else {
                return Ok(json!({"status": "disconnected"}));
            };
            let authorization: Authorization = serde_json::from_value(value)?;
            let mut result = summary(&authorization);
            if authorization.personal.is_some()
                || authorization.setup.as_ref().is_none_or(|saved| {
                    saved.id != snapshot.id || saved.revision != snapshot.revision
                })
                || authorization.instance_revision != snapshot.revision
                || authorization.owner_access != snapshot.owner_access_id
                || authorization.digest != package.digest
                || authorization
                    .metadata
                    .iter()
                    .any(|(key, value)| resource.settings.get(key) != Some(value))
                || (authorization
                    .tokens
                    .expires_at
                    .is_some_and(|expiry| expiry <= now())
                    && authorization.tokens.refresh.is_none())
            {
                result["status"] = json!("reconnect");
            }
            Ok(result)
        }
        ("DELETE", true) => {
            target.require_revision(&input.body)?;
            let key = authorization_key(&resource.id, name, None);
            s.store
                .transaction(move |db| {
                    target.current(db)?;
                    let mut current = target.get(db, &resource.id)?.ok_or_else(expired)?;
                    if current["revision"] != resource.revision {
                        return Err(expired());
                    }
                    current["revision"] = json!(id());
                    current["updatedAt"] = json!(now());
                    target.put(db, &current)?;
                    db.delete(&stored_key(&key))
                })
                .await?;
            Ok(json!({"status": "disconnected"}))
        }
        _ => Err(Error::method_not_allowed("Method not allowed.")),
    }
}
