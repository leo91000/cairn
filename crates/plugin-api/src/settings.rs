//! Declarative instance forms: the field types Cairn renders on the web and Android.
use serde_json::Value;

/// Field types an instance form can use.
pub const FIELD_TYPES: &[&str] = &[
    "string",
    "text",
    "number",
    "boolean",
    "secret",
    "select",
    "multiselect",
    "url",
];

fn optional_text(field: &Value, key: &str, max: usize) -> bool {
    match field.get(key) {
        None | Some(Value::Null) => true,
        Some(Value::String(value)) => super::short(value, max),
        Some(_) => false,
    }
}

fn options(field: &Value) -> Option<Vec<&str>> {
    let options = field["options"].as_array()?;
    if options.is_empty() || options.len() > 50 {
        return None;
    }
    let mut values = Vec::new();
    for option in options {
        let value = option["value"].as_str()?;
        let label = option["label"].as_str()?;
        if value.is_empty()
            || value.chars().count() > 100
            || values.contains(&value)
            || label.trim().is_empty()
            || !super::short(label, 100)
        {
            return None;
        }
        values.push(value);
    }
    Some(values)
}

fn valid_default(field: &Value, kind: &str, choices: Option<&[&str]>) -> bool {
    let Some(value) = field.get("default").filter(|value| !value.is_null()) else {
        return true;
    };
    match kind {
        "string" | "text" | "url" => value.as_str().is_some_and(|text| text.len() <= 2000),
        "number" => value.as_f64().is_some_and(f64::is_finite),
        "boolean" => value.is_boolean(),
        "select" => value
            .as_str()
            .is_some_and(|text| choices.is_some_and(|choices| choices.contains(&text))),
        "multiselect" => value.as_array().is_some_and(|items| {
            items.iter().all(|item| {
                item.as_str()
                    .is_some_and(|text| choices.is_some_and(|choices| choices.contains(&text)))
            })
        }),
        _ => false,
    }
}

/// Checks the manifest's `settings` list. Values are checked again on every save.
pub fn validate_settings_schema(settings: &[Value]) -> Result<(), &'static str> {
    if settings.len() > 30 {
        return Err("Plugin manifest declares too many settings.");
    }
    let mut keys = Vec::new();
    for field in settings {
        let key = field["key"].as_str().unwrap_or("");
        let valid_key = !key.is_empty()
            && key.len() <= 40
            && key.bytes().all(|c| c.is_ascii_alphanumeric())
            && !keys.contains(&key);
        keys.push(key);
        let kind = field["type"].as_str().unwrap_or("");
        let valid_title = field["title"]
            .as_str()
            .is_some_and(|title| !title.trim().is_empty() && super::short(title, 60));
        if !valid_key || !FIELD_TYPES.contains(&kind) || !valid_title {
            return Err("Plugin manifest has an invalid setting.");
        }
        let choices = options(field);
        let needs_options = matches!(kind, "select" | "multiselect");
        let flags_ok = ["required", "hidden"]
            .iter()
            .all(|flag| field.get(*flag).is_none_or(|value| value.is_boolean()));
        let limits_ok = ["maxLength", "minimum", "maximum"].iter().all(|limit| {
            field
                .get(*limit)
                .is_none_or(|value| value.as_f64().is_some_and(f64::is_finite))
        }) && field["maxLength"]
            .as_u64()
            .is_none_or(|max| (1..=16_384).contains(&max));
        let pattern_ok = match field.get("pattern") {
            None | Some(Value::Null) => true,
            Some(Value::String(source)) => {
                matches!(kind, "string" | "text" | "secret" | "url")
                    && super::pattern(source).is_some()
            }
            Some(_) => false,
        };
        if needs_options != field.get("options").is_some()
            || (needs_options && choices.is_none())
            || !flags_ok
            || !limits_ok
            || !pattern_ok
            || !optional_text(field, "description", 300)
            || !optional_text(field, "placeholder", 100)
            || !optional_text(field, "section", 60)
            || (kind == "secret" && field.get("default").is_some())
            || !valid_default(field, kind, choices.as_deref())
        {
            return Err("Plugin manifest has an invalid setting.");
        }
    }
    Ok(())
}
