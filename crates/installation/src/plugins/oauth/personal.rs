//! Safe discovery for the current Cairn member. Configuration and every other
//! person's authorization stay behind the owner/resource management boundaries.
use super::{Actor, Authorization, authorization_key, status_summary, stored_key};
use crate::{
    error::{Error, Result},
    plugins::{connections, registry, resources},
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(PartialEq, Eq)]
struct Authority {
    owner_access: String,
    actor: Actor,
}

async fn authority(s: &Service, account: &str) -> Result<Authority> {
    let policy = crate::relay::task_author_policy(s)
        .await?
        .ok_or_else(|| Error::forbidden("The installation has no verified membership."))?;
    let grant = policy.grant(account).ok_or_else(|| {
        Error::forbidden("This account no longer has access to the installation.")
    })?;
    Ok(Authority {
        owner_access: policy.owner.access_id.clone(),
        actor: Actor {
            account: account.into(),
            access_id: Some(grant.access_id.clone()),
        },
    })
}

fn page_cursor(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value else { return Ok(None) };
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() != 3
        || parts[2].is_empty()
        || parts[2].len() > 40
        || !parts[2]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
    {
        return Err(Error::bad("Invalid personal authorization cursor."));
    }
    uuid(parts[0])?;
    uuid(parts[1])?;
    Ok(Some(value.into()))
}

pub(crate) async fn list(s: &Service, account: &str, cursor: Option<&str>) -> Result<Value> {
    let cursor = page_cursor(cursor)?;
    let initial = authority(s, account).await?;
    let ids = s
        .store
        .read(|db| {
            Ok(db
                .list(connections::KIND)?
                .iter()
                .map(|value| text(value, "pluginId").to_owned())
                .collect::<BTreeSet<_>>())
        })
        .await?;
    let mut packages = BTreeMap::new();
    for id in ids {
        // A changed/unapproved package contributes no current provider metadata.
        if let Ok(package) = registry::approved(s, &id).await
            && package
                .package
                .manifest
                .oauth
                .iter()
                .any(|provider| provider.personal.is_some())
        {
            packages.insert(id, package);
        }
    }
    if authority(s, account).await? != initial {
        return Err(Error::forbidden(
            "Installation access changed. Reload personal connections.",
        ));
    }
    for package in packages.values() {
        package.recheck(s).await?;
    }
    let vault = s.vault.clone();
    s.store
        .read(move |db| {
            // Approval, configuration and this person's status share one read snapshot.
            let transaction = rusqlite::Transaction::new_unchecked(
                db.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let db = &Db(&transaction);
            for package in packages.values() {
                package.recheck_in(db)?;
            }
            let connections = db
                .list(connections::KIND)?
                .into_iter()
                .map(serde_json::from_value::<connections::Connection>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let resources = db
                .list(resources::KIND)?
                .into_iter()
                .map(serde_json::from_value::<resources::Resource>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut candidates = Vec::new();
            for connection in &connections {
                let Some(approved) = packages.get(&connection.plugin_id) else {
                    continue;
                };
                let package = &approved.package;
                for resource in resources
                    .iter()
                    .filter(|resource| resource.connection_id == connection.id)
                {
                    for provider in package.manifest.oauth.iter().filter(|provider| {
                        provider.personal.is_some() && provider.collection == resource.collection
                    }) {
                        let identified = provider.metadata.iter().any(|(key, pointer)| {
                            pointer.rsplit('/').next() == Some("id")
                                && resource.settings[key]
                                    .as_str()
                                    .is_some_and(|value| !value.is_empty())
                        });
                        if !identified {
                            continue;
                        }
                        let id = format!("{}:{}:{}", connection.id, resource.id, provider.name);
                        if cursor.as_ref().is_none_or(|cursor| id > *cursor) {
                            candidates.push((id, connection, resource, provider, package));
                        }
                    }
                }
            }
            candidates.sort_by(|a, b| a.0.cmp(&b.0));
            let mut items = Vec::new();
            for (id, connection, resource, provider, package) in candidates {
                let key = authorization_key(&resource.id, &provider.name, Some(&initial.actor));
                let encrypted = db.kv(&stored_key(&key))?;
                let enabled = connection.enabled
                    && resources::reference_in(db, connection, &package.manifest, &resource.id)
                        .is_ok();
                // Pausing does not strand an existing personal authorization: the
                // person can still discover and disconnect their own credential.
                if !enabled && encrypted.is_none() {
                    continue;
                }
                let authorization = match encrypted {
                    Some(value) => {
                        let authorization: Authorization =
                            serde_json::from_value(vault.decrypt(&key, &value)?)?;
                        status_summary(
                            &authorization,
                            connection,
                            resource,
                            &package.digest,
                            Some(&initial.owner_access),
                            Some(&initial.actor),
                            None,
                        )
                    }
                    None => json!({"status": "disconnected"}),
                };
                items.push(json!({
                    "id": id,
                    "pluginId": connection.plugin_id,
                    "pluginName": package.manifest.name,
                    "instanceId": connection.id,
                    "instanceName": connection.name,
                    "collection": resource.collection,
                    "resourceId": resource.id,
                    "resourceName": resource.name,
                    "providerName": provider.name,
                    "title": provider.title,
                    "scopes": provider.scopes,
                    "enabled": enabled,
                    "authorization": authorization,
                }));
                if items.len() > 100 {
                    break;
                }
            }
            let more = items.len() > 100;
            items.truncate(100);
            let next = more.then(|| items.last().unwrap()["id"].clone());
            Ok(json!({"items": items, "nextCursor": next}))
        })
        .await
}
