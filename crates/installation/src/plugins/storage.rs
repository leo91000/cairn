//! Small instance-owned values, optionally tied to one pending native delivery.
use super::{
    capabilities::Context,
    connections::{Connection, authorize},
    work,
};
use crate::{
    config::now,
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use serde_json::{Value, json};

fn delivery_owner(
    db: &Db<'_>,
    expected: &Connection,
    event: &Value,
    args: &Value,
    previous: Option<&Value>,
) -> Result<Option<Value>> {
    let requested = match args.get("delivery") {
        None => None,
        Some(Value::String(id)) => Some(id.as_str()),
        Some(_) => return Err(Error::bad("Name a pending delivery.")),
    };
    let existing = previous.and_then(|row| row["deliveryId"].as_str());
    let Some(id) = requested.or(existing) else {
        return Ok(None);
    };
    uuid(id)?;
    authorize(db, expected)?;
    let owned = db.get(work::DELIVERIES, id)?;
    let owned = owned.ok_or_else(|| Error::forbidden("The delivery is no longer pending."))?;
    let native_handler = matches!(text(event, "name"), "question" | "progress" | "reply");
    let updating_control =
        matches!(text(event, "name"), "message" | "interaction") && existing == Some(id);
    if !(native_handler && event["id"] == id || updating_control)
        || existing.is_some_and(|existing| existing != id)
        || owned["connectionId"] != expected.id
        || owned["revision"] != expected.revision
        || owned["routing"] != serde_json::to_value(&expected.routing)?
        || owned["userId"] != event["authorId"]
    {
        return Err(Error::forbidden(
            "This handler cannot own state for that delivery.",
        ));
    }
    Ok(Some(owned))
}

/// Runs inside the same transaction that retires the delivery, including when
/// no package reply can run (revision change, revocation or conversation purge).
pub(crate) fn forget_delivery(db: &Db<'_>, connection: &str, delivery: &str) -> Result<()> {
    db.0.execute(
        "DELETE FROM records WHERE kind='plugin-storage' AND json_extract(data,'$.connectionId')=?1 AND json_extract(data,'$.deliveryId')=?2",
        rusqlite::params![connection, delivery],
    )?;
    Ok(())
}

fn key(args: &Value) -> Result<String> {
    let key = text(args, "key");
    if key.is_empty() || key.chars().count() > 200 || key.chars().any(char::is_control) {
        return Err(Error::bad("Invalid storage key."));
    }
    Ok(key.to_owned())
}

/// Small connection-scoped values. Removed with the connection.
pub(super) async fn call(
    s: &Service,
    context: &Context,
    method: &str,
    args: &Value,
) -> Result<Value> {
    const KIND: &str = "plugin-storage";
    let connection = context.connection.id.clone();
    let management = context.management.clone();
    let expected = context.connection.clone();
    let event = context.event.clone();
    let method = method.to_owned();
    let args = args.clone();
    s.store
        .transaction(move |db| {
            if let Some(authority) = management {
                authority.authorize(db)?;
            }
            let owned = || {
                db.list(KIND).map(|values| {
                    values
                        .into_iter()
                        .filter(|value| value["connectionId"] == connection)
                        .collect::<Vec<_>>()
                })
            };
            if method == "storage.list" {
                let prefix = text(&args, "prefix");
                let mut keys = owned()?
                    .iter()
                    .map(|value| text(value, "key").to_owned())
                    .filter(|key| key.starts_with(prefix))
                    .collect::<Vec<_>>();
                keys.sort();
                return Ok(json!(keys));
            }
            let key = key(&args)?;
            let record_id = format!("storage:{connection}:{}", crate::auth::hex_digest(&key));
            match method.as_str() {
                "storage.get" => Ok(db
                    .get(KIND, &record_id)?
                    .map_or(Value::Null, |value| value["value"].clone())),
                "storage.delete" => {
                    db.remove(KIND, &record_id)?;
                    Ok(Value::Null)
                }
                _ => {
                    let previous = db.get(KIND, &record_id)?;
                    let owner = delivery_owner(db, &expected, &event, &args, previous.as_ref())?;
                    let mut value = args.get("value").cloned().unwrap_or(Value::Null);
                    if method == "storage.max" {
                        let number = value
                            .as_f64()
                            .filter(|value| value.is_finite())
                            .ok_or_else(|| Error::bad("Store a finite number."))?;
                        let previous = db.get(KIND, &record_id)?;
                        let current = previous
                            .as_ref()
                            .map(|row| {
                                row["value"]
                                    .as_f64()
                                    .ok_or_else(|| Error::bad("The stored value is not a number."))
                            })
                            .transpose()?;
                        if current.is_some_and(|current| current >= number) {
                            value = previous.unwrap()["value"].clone();
                        }
                    }
                    if serde_json::to_string(&value)?.len() > 65_536 {
                        return Err(Error::too_large("Stored values are limited to 64 KB."));
                    }
                    if db.get(KIND, &record_id)?.is_none() && owned()?.len() >= 1000 {
                        return Err(Error::too_large(
                            "A connection can keep at most 1000 values.",
                        ));
                    }
                    let mut record = json!({
                        "id": record_id,
                        "connectionId": connection,
                        "key": key,
                        "value": value,
                        "updatedAt": now(),
                    });
                    if let Some(owner) = owner {
                        record["deliveryId"] = owner["id"].clone();
                        record["chatId"] = owner["chatId"].clone();
                    }
                    db.put(KIND, &record)?;
                    Ok(if method == "storage.max" {
                        value
                    } else {
                        Value::Null
                    })
                }
            }
        })
        .await
}
