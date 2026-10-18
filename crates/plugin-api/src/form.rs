//! Instance forms driven by the package: the `form` handler's result while the owner
//! edits an instance. Dynamic choices, derived values, field errors and guidance.
use super::Manifest;
use serde_json::Value;

const MAX_BYTES: usize = 262_144;
const MAX_OPTIONS: usize = 200;

fn short(value: &Value, key: &str, max: usize) -> bool {
    match value.get(key) {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => super::short(text, max),
        Some(_) => false,
    }
}

fn option(value: &Value) -> bool {
    value["value"]
        .as_str()
        .is_some_and(|text| !text.is_empty() && text.chars().count() <= 100)
        && value["label"]
            .as_str()
            .is_some_and(|label| !label.trim().is_empty() && super::short(label, 100))
        && short(value, "description", 200)
        && value.get("disabled").is_none_or(Value::is_boolean)
        && value.as_object().is_some_and(|map| {
            map.keys()
                .all(|key| matches!(key.as_str(), "value" | "label" | "description" | "disabled"))
        })
}

/// Checks `{ options?, values?, errors?, view? }`. Options only for dynamic choice
/// fields, values only for declared non-secret settings, errors for settings, the
/// credential or the whole form (`form`).
pub fn validate_form_result(manifest: &Manifest, result: &Value) -> Result<(), &'static str> {
    if result.is_null() {
        return Ok(());
    }
    let invalid = "Invalid plugin form result.";
    let map = result.as_object().ok_or(invalid)?;
    if serde_json::to_string(result).map_or(true, |json| json.len() > MAX_BYTES)
        || map
            .keys()
            .any(|key| !matches!(key.as_str(), "options" | "values" | "errors" | "view"))
    {
        return Err(invalid);
    }
    let field = |key: &str| manifest.settings.iter().find(|field| field["key"] == key);
    if let Some(options) = map.get("options").filter(|value| !value.is_null()) {
        for (key, items) in options.as_object().ok_or(invalid)? {
            let dynamic = field(key).is_some_and(|field| field["dynamic"] == true);
            let items = items.as_array().ok_or(invalid)?;
            if !dynamic || items.len() > MAX_OPTIONS || !items.iter().all(option) {
                return Err(invalid);
            }
        }
    }
    if let Some(values) = map.get("values").filter(|value| !value.is_null()) {
        for key in values.as_object().ok_or(invalid)?.keys() {
            if field(key).is_none_or(|field| field["type"] == "secret") {
                return Err(invalid);
            }
        }
    }
    if let Some(errors) = map.get("errors").filter(|value| !value.is_null()) {
        for (key, message) in errors.as_object().ok_or(invalid)? {
            let known = matches!(key.as_str(), "credential" | "form") || field(key).is_some();
            let message = message.as_str().unwrap_or("");
            if !known || message.trim().is_empty() || !super::short(message, 300) {
                return Err(invalid);
            }
        }
    }
    match map.get("view") {
        None | Some(Value::Null) => Ok(()),
        Some(view) => super::validate_view(view),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate_settings_schema;
    use serde_json::json;

    fn manifest() -> Manifest {
        serde_json::from_value(json!({
            "id": "chat", "name": "Chat", "version": "1", "apiVersion": 2, "capabilities": [],
            "instanceForm": true,
            "settings": [
                { "key": "space", "title": "Space", "type": "select", "dynamic": true, "refresh": true, "pattern": "\\d+" },
                { "key": "spaceName", "title": "Space name", "type": "string", "hidden": true },
                { "key": "key", "title": "Key", "type": "secret" },
            ],
        }))
        .unwrap()
    }

    #[test]
    fn dynamic_choices_need_no_static_options_but_only_on_choice_fields() {
        let manifest = manifest();
        manifest.validate().unwrap();
        for invalid in [
            json!([{ "key": "a", "title": "A", "type": "string", "dynamic": true }]),
            json!([{ "key": "a", "title": "A", "type": "select" }]),
            json!([{ "key": "a", "title": "A", "type": "select", "dynamic": true, "default": "1" }]),
            json!([{ "key": "a", "title": "A", "type": "boolean", "refresh": "yes" }]),
        ] {
            assert!(
                validate_settings_schema(invalid.as_array().unwrap()).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn results_fill_dynamic_fields_and_name_known_fields_only() {
        let manifest = manifest();
        validate_form_result(
            &manifest,
            &json!({
                "options": { "space": [{ "value": "1", "label": "Main", "description": "Can reply" }, { "value": "2", "label": "Private", "disabled": true }] },
                "values": { "spaceName": "Main" },
                "errors": { "credential": "Rejected token", "space": "Pick one", "form": "Try again" },
                "view": [{ "type": "alert", "tone": "info", "text": "Invite the bot first" }],
            }),
        )
        .unwrap();
        validate_form_result(&manifest, &Value::Null).unwrap();
        for invalid in [
            json!({ "options": { "spaceName": [{ "value": "1", "label": "x" }] } }),
            json!({ "options": { "space": [{ "value": "", "label": "x" }] } }),
            json!({ "options": { "space": [{ "value": "1", "label": "x", "html": "<b>" }] } }),
            json!({ "values": { "key": "leak" } }),
            json!({ "values": { "unknown": 1 } }),
            json!({ "errors": { "unknown": "x" } }),
            json!({ "errors": { "space": "" } }),
            json!({ "view": [{ "type": "html" }] }),
            json!({ "redirect": "https://example.com" }),
        ] {
            assert!(
                validate_form_result(&manifest, &invalid).is_err(),
                "{invalid}"
            );
        }
    }
}
