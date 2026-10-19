//! Owner-managed draft, route and permission lists share the ordinary plugin cursor
//! contract, with both record and byte bounds below the relay body limit.
use crate::{
    error::{Error, Result},
    validation::{text, uuid},
};
use serde_json::{Value, json};

const MAX_ITEMS: usize = 32;
const MAX_BYTES: usize = cairn_protocol::MAX_BODY / 2;

pub(super) fn page(mut values: Vec<Value>, cursor: Option<&str>) -> Result<Value> {
    if let Some(cursor) = cursor {
        uuid(cursor)?;
    }
    values.retain(|value| cursor.is_none_or(|cursor| text(value, "id") > cursor));
    values.sort_by(|a, b| text(a, "id").cmp(text(b, "id")));

    let mut items = Vec::new();
    // Reserve the envelope and the UUID cursor independently of record content.
    let mut bytes = 256;
    let mut more = false;
    for value in values {
        if items.len() == MAX_ITEMS {
            more = true;
            break;
        }
        let size = serde_json::to_vec(&value)?.len() + 1;
        if bytes + size > MAX_BYTES {
            if items.is_empty() {
                return Err(Error::too_large(
                    "The saved management record is too large to list.",
                ));
            }
            more = true;
            break;
        }
        bytes += size;
        items.push(value);
    }
    let next = more.then(|| items.last().unwrap()["id"].clone());
    Ok(json!({ "items": items, "nextCursor": next }))
}
