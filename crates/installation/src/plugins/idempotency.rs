//! Durable HTTP mutation receipts. A connection-scoped key is claimed before the
//! request leaves the host; crashes and interrupted responses remain ambiguous,
//! so replaying a send cannot silently create a second message.
use super::connections::{self, Connection};
use crate::{
    auth::hex_digest,
    config::now,
    error::{Error, Result},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};

use super::queues::Receipt;

pub enum Claim {
    Fresh(String),
    Cached(Value),
}

pub async fn claim(
    s: &Service,
    connection: &Connection,
    args: &Value,
    management: Option<&super::management::Authority>,
) -> Result<Option<Claim>> {
    let Some(key) = args.get("idempotencyKey").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let key = key
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 200 && !v.chars().any(char::is_control))
        .ok_or_else(|| Error::bad("Use a stable idempotency key of at most 200 characters."))?;
    if matches!(text(args, "method"), "GET" | "HEAD")
        || args.get("responseFile").is_some_and(|v| !v.is_null())
    {
        return Err(Error::bad(
            "Idempotency applies to mutations returning inline receipts.",
        ));
    }
    let input = args.get("idempotencyInput").unwrap_or(args);
    let fingerprint = hex_digest(&serde_json::to_string(&json!({
        "method": args["method"],
        "url": args["url"],
        "input": input,
    }))?);
    let id = format!("{}:{}", connection.id, hex_digest(key));
    let connection = connection.clone();
    let management = management.cloned();
    s.store
        .transaction(move |db| {
            if let Some(authority) = &management {
                authority.authorize(db)?;
            }
            connections::authorize(db, &connection)?;

            if let Some(existing) = db.plugin_receipt(&id)? {
                if existing.fingerprint != fingerprint {
                    return Err(Error::conflict("This idempotency key was already used for another request."));
                }
                if let Some(response) = existing.response {
                    return Ok(Some(Claim::Cached(response)));
                }
                return Err(Error::conflict("The earlier request may have succeeded. Check the service before sending again; this key will not repeat it."));
            }

            if db.plugin_receipt_count(&connection.id)? >= 10_000 {
                return Err(Error::too_large("This account has reached its HTTP receipt limit."));
            }
            db.save_plugin_receipt(&Receipt {
                id: id.clone(),
                instance_id: connection.id.clone(),
                fingerprint,
                response: None,
                created_at: now(),
            })?;
            Ok(Some(Claim::Fresh(id)))
        })
        .await
}

pub async fn complete(
    s: &Service,
    connection: &Connection,
    id: &str,
    response: &Value,
    management: Option<&super::management::Authority>,
) -> Result<()> {
    // Keep receipts bounded even when a provider returns a large success body.
    let response = if serde_json::to_vec(response)?.len() <= 65_536 {
        response.clone()
    } else {
        json!({
            "status": response["status"],
            "headers": {},
            "json": null,
            "text": null,
            "resultOmitted": true,
        })
    };
    let management = management.cloned();
    let connection = connection.clone();
    let id = id.to_owned();
    s.store
        .transaction(move |db| {
            if let Some(authority) = &management {
                authority.authorize(db)?;
            }
            connections::authorize(db, &connection)?;
            let mut record = db
                .plugin_receipt(&id)?
                .ok_or_else(|| Error::conflict("The HTTP receipt was removed."))?;
            record.response = Some(response);
            db.save_plugin_receipt(&record)?;
            Ok(())
        })
        .await
}
