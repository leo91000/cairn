//! Durable HTTP mutation receipts. A connection-scoped key is claimed before the
//! request leaves the host; crashes and interrupted responses remain ambiguous,
//! so replaying a send cannot silently create a second message.
use super::{
    capabilities::Context,
    connections::{self, Connection},
    oauth, resources,
};
use crate::{
    auth::hex_digest,
    config::now,
    error::{Error, Result},
    service::Service,
};
use serde_json::{Value, json};

pub const KIND: &str = "plugin-http-receipts";

pub enum Claim {
    Fresh(String),
    Cached(Value),
}

pub async fn claim(
    s: &Service,
    context: &Context,
    args: &Value,
    resource: Option<&resources::Access>,
    oauth: Option<&oauth::Reference>,
) -> Result<Option<Claim>> {
    let Some(key) = args.get("idempotencyKey").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let key = key
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 200 && !v.chars().any(char::is_control))
        .ok_or_else(|| Error::bad("Use a stable idempotency key of at most 200 characters."))?;
    if matches!(args["method"].as_str().unwrap_or("GET"), "GET" | "HEAD")
        || args.get("responseFile").is_some_and(|v| !v.is_null())
    {
        return Err(Error::bad(
            "Idempotency applies to mutations returning inline receipts.",
        ));
    }
    let connection = &context.connection;
    // Keep the instance/key stable when authority changes: an earlier uncertain
    // send must conflict, never become a fresh request under a different token.
    // Plugin-selected intent cannot omit or override this native proof.
    let authority = json!({
        "connectionRevision": connection.revision,
        "grant": context.resource_grant.as_ref().map(|grant| grant.receipt_authority()),
        "resource": resource.map(resources::Access::receipt_authority),
        "oauth": oauth.map(oauth::Reference::receipt_authority),
    });
    let input = args.get("idempotencyInput").unwrap_or(args);
    let fingerprint = hex_digest(&serde_json::to_string(&json!({
        "method": args["method"],
        "url": args["url"],
        "input": input,
        "authority": authority,
    }))?);
    let id = format!("{}:{}", connection.id, hex_digest(key));
    let connection = connection.clone();
    let management = context.management.clone();
    s.store.transaction(move |db| {
        if let Some(authority) = &management { authority.authorize(db)?; }
        connections::authorize(db, &connection)?;
        if let Some(existing) = db.get(KIND, &id)? {
            if existing["fingerprint"] != fingerprint { return Err(Error::conflict("This idempotency key was already used for another request.")); }
            if existing["state"] == "completed" { return Ok(Some(Claim::Cached(existing["response"].clone()))); }
            return Err(Error::conflict("The earlier request may have succeeded. Check the service before sending again; this key will not repeat it."));
        }
        if db.list(KIND)?.iter().filter(|v| v["connectionId"] == connection.id).count() >= 10_000 {
            return Err(Error::too_large("This account has reached its HTTP receipt limit."));
        }
        db.put(KIND, &json!({
            "id": id,
            "connectionId": connection.id,
            "fingerprint": fingerprint,
            "state": "pending",
            "createdAt": now(),
        }))?;
        Ok(Some(Claim::Fresh(id)))
    }).await
}

pub async fn complete<F>(
    s: &Service,
    connection: &Connection,
    id: &str,
    response: &Value,
    management: Option<&super::management::Authority>,
    authorize: F,
) -> Result<()>
where
    F: FnOnce(&crate::store::Db<'_>) -> Result<()> + Send + 'static,
{
    // Keep receipts bounded even when a provider returns a large success body.
    let response = if serde_json::to_vec(response)?.len() <= 65_536 {
        response.clone()
    } else {
        json!({ "status": response["status"], "headers": {}, "json": null, "text": null, "resultOmitted": true })
    };
    let management = management.cloned();
    let connection = connection.clone();
    let id = id.to_owned();
    s.store
        .transaction(move |db| {
            publish(
                db,
                &connection,
                &id,
                &response,
                management.as_ref(),
                authorize,
            )
        })
        .await
}

fn publish<F>(
    db: &crate::store::Db<'_>,
    connection: &Connection,
    id: &str,
    response: &Value,
    management: Option<&super::management::Authority>,
    authorize: F,
) -> Result<()>
where
    F: FnOnce(&crate::store::Db<'_>) -> Result<()>,
{
    if let Some(authority) = management {
        authority.authorize(db)?;
    }
    connections::authorize(db, connection)?;
    authorize(db)?;
    let mut record = db
        .get(KIND, id)?
        .ok_or_else(|| Error::conflict("The HTTP receipt was removed."))?;
    record["state"] = json!("completed");
    record["response"] = response.clone();
    db.put(KIND, &record)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{store::Store, vault::Vault};

    #[tokio::test]
    async fn receipt_publication_rechecks_a_credential_after_its_preflight_snapshot() {
        let root = tempfile::TempDir::new().unwrap();
        let store = Store::open(root.path()).unwrap();
        let vault = Vault::new(store.clone(), root.path()).unwrap();
        let mut connection = Connection::draft("fixture-instance", "example", json!({}), vec![]);
        connection.enabled = true;
        connection.revision = "fixture-revision".into();
        store
            .put(
                connections::KIND,
                serde_json::to_value(&connection).unwrap(),
            )
            .await
            .unwrap();

        for revoke in [false, true] {
            let id = format!("fixture-instance:receipt-{revoke}");
            store
                .put(
                    KIND,
                    json!({
                        "id": id, "connectionId": connection.id, "state": "pending",
                    }),
                )
                .await
                .unwrap();
            vault
                .set("fixture-personal", &json!({"revision": "initial"}))
                .await
                .unwrap();
            // A successful preflight is not authority for a later queued write.
            assert!(vault.get("fixture-personal").await.unwrap().is_some());
            if revoke {
                vault.delete("fixture-personal").await.unwrap();
            }
            let expected = connection.clone();
            let receipt = id.clone();
            let credential = vault.clone();
            let result = store
                .transaction(move |db| {
                    publish(
                        db,
                        &expected,
                        &receipt,
                        &json!({"json": {"private": "fixture-result"}}),
                        None,
                        move |db| {
                            if credential.get_in(db, "fixture-personal")?.is_none() {
                                return Err(Error::forbidden(
                                    "The personal authorization was disconnected.",
                                ));
                            }
                            Ok(())
                        },
                    )
                })
                .await;
            let saved = store.get(KIND, &id).await.unwrap().unwrap();
            if revoke {
                assert_eq!(result.unwrap_err().status, 403);
                assert_eq!(saved["state"], "pending");
                assert!(saved.get("response").is_none());
            } else {
                result.unwrap();
                assert_eq!(saved["state"], "completed");
                assert_eq!(saved["response"]["json"]["private"], "fixture-result");
            }
        }
    }
}
