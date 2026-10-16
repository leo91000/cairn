//! Declarative views: a small component tree returned by a package's `view` handler
//! and rendered natively by the web and Android apps. Anything else is refused.
use serde_json::Value;

const TONES: &[&str] = &["neutral", "info", "success", "warning", "danger"];
const MAX_BYTES: usize = 65_536;
const MAX_NODES: usize = 300;
const MAX_DEPTH: usize = 6;

struct Budget {
    nodes: usize,
}

fn text(value: &Value, key: &str, max: usize, required: bool) -> bool {
    match value.get(key) {
        None | Some(Value::Null) => !required,
        Some(Value::String(text)) => {
            (!required || !text.trim().is_empty()) && super::short(text, max)
        }
        Some(_) => false,
    }
}

fn tone(value: &Value, required: bool) -> bool {
    match value.get("tone") {
        None | Some(Value::Null) => !required,
        Some(Value::String(tone)) => TONES.contains(&tone.as_str()),
        Some(_) => false,
    }
}

fn action_name(value: &Value) -> bool {
    value["action"].as_str().is_some_and(|name| {
        !name.is_empty()
            && name.len() <= 40
            && name.starts_with(|c: char| c.is_ascii_lowercase())
            && name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
    })
}

fn small_json(value: &Value, key: &str, max: usize) -> bool {
    value
        .get(key)
        .is_none_or(|value| serde_json::to_string(value).is_ok_and(|json| json.len() <= max))
}

fn badge(value: &Value) -> bool {
    value.is_object() && text(value, "label", 60, true) && tone(value, false)
}

fn button(value: &Value) -> bool {
    value["type"] == "button"
        && text(value, "label", 60, true)
        && action_name(value)
        && small_json(value, "payload", 4096)
        && matches!(
            value.get("style").and_then(Value::as_str),
            None | Some("primary" | "default" | "danger")
        )
        && text(value, "confirm", 300, false)
}

fn https(value: &Value) -> bool {
    value["url"].as_str().is_some_and(|url| {
        url.starts_with("https://") && url.len() <= 2000 && !url.chars().any(char::is_control)
    })
}

fn children(value: &Value, depth: usize, budget: &mut Budget) -> Result<(), &'static str> {
    let items = value["children"]
        .as_array()
        .ok_or("View children must be a list.")?;
    for child in items {
        node(child, depth + 1, budget)?;
    }
    Ok(())
}

fn node(value: &Value, depth: usize, budget: &mut Budget) -> Result<(), &'static str> {
    budget.nodes += 1;
    if depth > MAX_DEPTH || budget.nodes > MAX_NODES {
        return Err("The view is too large.");
    }
    let valid = match value["type"].as_str().unwrap_or("") {
        "section" => {
            text(value, "title", 120, false) && text(value, "description", 500, false) && {
                children(value, depth, budget)?;
                true
            }
        }
        "actions" => value["children"]
            .as_array()
            .is_some_and(|items| items.len() <= 6 && items.iter().all(button)),
        "text" => text(value, "text", 4000, true) && tone(value, false),
        "alert" => {
            text(value, "text", 1000, true) && text(value, "title", 120, false) && tone(value, true)
        }
        "badge" => badge(value),
        "facts" => value["items"].as_array().is_some_and(|items| {
            items.len() <= 30
                && items
                    .iter()
                    .all(|item| text(item, "label", 60, true) && text(item, "value", 500, false))
        }),
        "list" => {
            text(value, "empty", 200, false)
                && value["items"].as_array().is_some_and(|items| {
                    items.len() <= 100
                        && items.iter().all(|item| {
                            text(item, "title", 200, true)
                                && text(item, "subtitle", 500, false)
                                && item.get("badge").is_none_or(badge)
                                && item.get("actions").is_none_or(|actions| {
                                    actions.as_array().is_some_and(|actions| {
                                        actions.len() <= 4 && actions.iter().all(button)
                                    })
                                })
                        })
                })
        }
        "progress" => {
            text(value, "label", 120, false)
                && value["value"].as_f64().is_some_and(f64::is_finite)
                && value["max"]
                    .as_f64()
                    .is_some_and(|max| max.is_finite() && max > 0.0)
        }
        "code" => text(value, "text", 8000, true),
        "link" => text(value, "label", 120, true) && https(value),
        "button" => button(value),
        "form" => {
            let fields = value["fields"]
                .as_array()
                .ok_or("Form fields must be a list.")?;
            super::validate_settings_schema(fields)?;
            !fields.is_empty()
                && value["submit"].is_object()
                && text(&value["submit"], "label", 60, true)
                && action_name(&value["submit"])
                && value.get("values").is_none_or(Value::is_object)
                && small_json(value, "values", 16_384)
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err("The view contains an invalid component.")
    }
}

/// Checks a `view` handler result: a list of components.
pub fn validate_view(view: &Value) -> Result<(), &'static str> {
    if serde_json::to_string(view).map_or(true, |json| json.len() > MAX_BYTES) {
        return Err("The view is too large.");
    }
    let items = view.as_array().ok_or("A view is a list of components.")?;
    let mut budget = Budget { nodes: 0 };
    for item in items {
        node(item, 1, &mut budget)?;
    }
    Ok(())
}

/// Checks an `action` handler result: `{ notice?, tone?, view? }`.
pub fn validate_action_result(result: &Value) -> Result<(), &'static str> {
    if result.is_null() {
        return Ok(());
    }
    if !result.is_object()
        || !text(result, "notice", 300, false)
        || !tone(result, false)
        || result.as_object().is_some_and(|map| {
            map.keys()
                .any(|key| !matches!(key.as_str(), "notice" | "tone" | "view"))
        })
    {
        return Err("Invalid action result.");
    }
    match result.get("view") {
        None | Some(Value::Null) => Ok(()),
        Some(view) => validate_view(view),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn known_components_render_and_others_are_refused() {
        let view = json!([
            { "type": "section", "title": "Vaults", "children": [
                { "type": "facts", "items": [{ "label": "Items", "value": "12" }] },
                { "type": "list", "empty": "No vaults", "items": [
                    { "title": "Production", "badge": { "label": "Read", "tone": "success" },
                      "actions": [{ "type": "button", "label": "Open", "action": "open", "payload": { "id": "v1" } }] }
                ] },
                { "type": "progress", "label": "Quota", "value": 3, "max": 10 },
            ] },
            { "type": "alert", "tone": "warning", "text": "Token expires soon" },
            { "type": "form", "fields": [{ "key": "name", "title": "Name", "type": "string" }],
              "submit": { "label": "Save", "action": "save" } },
            { "type": "link", "label": "Docs", "url": "https://example.com" },
            { "type": "code", "text": "op read" },
        ]);
        validate_view(&view).unwrap();
        for invalid in [
            json!([{ "type": "html", "html": "<b>x</b>" }]),
            json!([{ "type": "link", "label": "x", "url": "javascript:alert(1)" }]),
            json!([{ "type": "button", "label": "x", "action": "Bad Name" }]),
            json!([{ "type": "alert", "text": "x" }]),
            json!({ "type": "text", "text": "x" }),
        ] {
            assert!(validate_view(&invalid).is_err(), "{invalid}");
        }
        let mut deep = json!({ "type": "text", "text": "x" });
        for _ in 0..8 {
            deep = json!({ "type": "section", "children": [deep] });
        }
        assert!(validate_view(&json!([deep])).is_err());
    }

    #[test]
    fn action_results_carry_a_notice_or_a_new_view() {
        validate_action_result(&json!({ "notice": "Saved", "tone": "success" })).unwrap();
        validate_action_result(&json!({ "view": [{ "type": "text", "text": "Done" }] })).unwrap();
        validate_action_result(&Value::Null).unwrap();
        assert!(validate_action_result(&json!({ "redirect": "https://x" })).is_err());
    }
}
