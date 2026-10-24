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

fn default_text(field: &Value, kind: &str, value: &str) -> bool {
    let default_max = if kind == "string" { 200 } else { 2000 };
    let max = field["maxLength"].as_u64().unwrap_or(default_max) as usize;
    let multiline = kind == "text";
    if value.chars().count() > max
        || value
            .chars()
            .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\t')))
    {
        return false;
    }

    let value = value.trim();
    if value.is_empty() {
        return field["required"] != true;
    }
    if kind == "url"
        && !url::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "https" | "http"))
    {
        return false;
    }
    field["pattern"]
        .as_str()
        .is_none_or(|source| super::pattern(source).is_some_and(|re| re.is_match(value)))
}

fn valid_default(field: &Value, kind: &str, choices: Option<&[&str]>) -> bool {
    let Some(value) = field.get("default").filter(|value| !value.is_null()) else {
        return true;
    };
    let choice = |value: &Value| {
        value.as_str().is_some_and(|text| {
            choices.is_some_and(|choices| choices.contains(&text))
                && field["maxLength"]
                    .as_u64()
                    .is_none_or(|max| text.chars().count() as u64 <= max)
        })
    };
    match kind {
        "string" | "text" | "url" => value
            .as_str()
            .is_some_and(|text| default_text(field, kind, text)),
        "number" => value.as_f64().is_some_and(|number| {
            number.is_finite()
                && field["minimum"].as_f64().is_none_or(|min| number >= min)
                && field["maximum"].as_f64().is_none_or(|max| number <= max)
        }),
        "boolean" => value.is_boolean(),
        "select" | "multiselect" if field["dynamic"] == true => false,
        "select" => choice(value),
        "multiselect" => value.as_array().is_some_and(|items| {
            (field["required"] != true || !items.is_empty()) && items.iter().all(choice)
        }),
        _ => false,
    }
}

fn validate_limits(field: &Value, kind: &str) -> Result<(), &'static str> {
    if let Some(value) = field.get("maxLength") {
        let cap = if kind == "secret" { 16_384 } else { 10_000 };
        if !value.as_u64().is_some_and(|max| (1..=cap).contains(&max)) {
            return Err(if kind == "secret" {
                "Plugin setting maxLength must be an integer from 1 to 16384 (secret byte limit)."
            } else {
                "Plugin setting maxLength must be an integer from 1 to 10000 (text limit)."
            });
        }
    }
    if !["minimum", "maximum"].iter().all(|limit| {
        field
            .get(*limit)
            .is_none_or(|value| value.as_f64().is_some_and(f64::is_finite))
    }) {
        return Err("Plugin setting minimum and maximum must be finite numbers.");
    }
    if let (Some(min), Some(max)) = (field["minimum"].as_f64(), field["maximum"].as_f64())
        && min > max
    {
        return Err("Plugin setting minimum must not exceed maximum.");
    }
    Ok(())
}

/// How much a switch lets agents do, shown beside it: `low`, `medium` or `high`.
fn valid_risk(field: &Value, kind: &str) -> bool {
    match field.get("risk") {
        None | Some(Value::Null) => true,
        Some(Value::String(risk)) => {
            kind == "boolean" && matches!(risk.as_str(), "low" | "medium" | "high")
        }
        Some(_) => false,
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
        let choice = matches!(kind, "select" | "multiselect");
        // Dynamic choices come from the package's `form` handler while the owner edits.
        let dynamic = field["dynamic"] == true;
        let needs_options = choice && !dynamic;
        let flags_ok = ["required", "hidden", "dynamic", "remote", "refresh"]
            .iter()
            .all(|flag| field.get(*flag).is_none_or(|value| value.is_boolean()))
            && (!dynamic || choice)
            && (field["remote"] != true || dynamic);
        validate_limits(field, kind)?;
        let pattern_ok = match field.get("pattern") {
            None | Some(Value::Null) => true,
            Some(Value::String(source)) => {
                (matches!(kind, "string" | "text" | "secret" | "url") || dynamic)
                    && super::pattern(source).is_some()
            }
            Some(_) => false,
        };
        if (needs_options || !dynamic) && needs_options != field.get("options").is_some()
            || (field.get("options").is_some() && choices.is_none())
            || !flags_ok
            || !pattern_ok
            || !optional_text(field, "description", 300)
            || !optional_text(field, "placeholder", 100)
            || !optional_text(field, "section", 60)
            || !valid_risk(field, kind)
            || (kind == "secret" && field.get("default").is_some())
            || !valid_default(field, kind, choices.as_deref())
        {
            return Err("Plugin manifest has an invalid setting.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(kind: &str) -> Value {
        json!({ "key": "value", "title": "Value", "type": kind })
    }

    #[test]
    fn length_limits_are_positive_integers_with_type_specific_caps() {
        for kind in ["string", "text", "url", "secret"] {
            let cap = if kind == "secret" { 16_384 } else { 10_000 };
            for limit in [
                json!(-1),
                json!(0),
                json!(1.5),
                json!(1.0),
                json!(cap + 1),
                Value::Null,
                json!("10"),
            ] {
                let mut setting = field(kind);
                setting["maxLength"] = limit;
                assert!(
                    validate_settings_schema(std::slice::from_ref(&setting)).is_err(),
                    "{setting}"
                );
            }
            for limit in [1, cap] {
                let mut setting = field(kind);
                setting["maxLength"] = json!(limit);
                validate_settings_schema(&[setting]).unwrap();
            }
        }
        let mut setting = field("text");
        setting["maxLength"] = json!(10_001);
        assert_eq!(
            validate_settings_schema(&[setting]),
            Err("Plugin setting maxLength must be an integer from 1 to 10000 (text limit).")
        );
    }

    #[test]
    fn numeric_bounds_are_ordered_and_defaults_are_in_range() {
        let mut setting = field("number");
        setting["minimum"] = json!(2);
        setting["maximum"] = json!(1);
        assert!(validate_settings_schema(std::slice::from_ref(&setting)).is_err());
        setting["maximum"] = json!(3);
        for value in [json!(1), json!(4), json!("2"), json!(true)] {
            setting["default"] = value;
            assert!(
                validate_settings_schema(std::slice::from_ref(&setting)).is_err(),
                "{setting}"
            );
        }
        for value in [json!(2), json!(2.5), json!(3), Value::Null] {
            setting["default"] = value;
            validate_settings_schema(std::slice::from_ref(&setting)).unwrap();
        }
        setting["minimum"] = json!(3);
        setting["default"] = json!(3);
        validate_settings_schema(&[setting]).unwrap();
    }

    #[test]
    fn text_defaults_obey_save_constraints() {
        for (kind, constraints, rejected, accepted) in [
            ("string", json!({}), "x".repeat(201), "é".repeat(200)),
            ("text", json!({}), "x".repeat(2001), "é".repeat(2000)),
            (
                "string",
                json!({ "maxLength": 2 }),
                "abc".into(),
                "éé".into(),
            ),
            (
                "string",
                json!({ "pattern": "[a-z]+" }),
                "abc1".into(),
                " abc ".into(),
            ),
            (
                "string",
                json!({ "required": true }),
                "  ".into(),
                "a".into(),
            ),
            ("string", json!({}), "a\nb".into(), "ab".into()),
            ("text", json!({}), "a\u{0001}b".into(), "a\n\tb".into()),
            (
                "url",
                json!({}),
                "file:///tmp/a".into(),
                "https://example.com".into(),
            ),
        ] {
            let mut setting = field(kind);
            setting
                .as_object_mut()
                .unwrap()
                .extend(constraints.as_object().unwrap().clone());
            setting["default"] = json!(rejected);
            assert!(
                validate_settings_schema(std::slice::from_ref(&setting)).is_err(),
                "{setting}"
            );
            setting["default"] = json!(accepted);
            validate_settings_schema(&[setting]).unwrap();
        }
    }

    #[test]
    fn choice_defaults_obey_type_membership_and_declared_length() {
        for kind in ["select", "multiselect"] {
            let mut setting = field(kind);
            setting["options"] = json!([{ "value": "eu", "label": "Europe" }]);
            setting["default"] = if kind == "select" {
                json!("eu")
            } else {
                json!(["eu"])
            };
            validate_settings_schema(std::slice::from_ref(&setting)).unwrap();
            setting["maxLength"] = json!(1);
            assert!(validate_settings_schema(std::slice::from_ref(&setting)).is_err());
            setting["maxLength"] = json!(2);
            setting["default"] = if kind == "select" {
                json!("us")
            } else {
                json!(["us"])
            };
            assert!(validate_settings_schema(&[setting]).is_err());
        }
        let mut setting = field("multiselect");
        setting["options"] = json!([{ "value": "eu", "label": "Europe" }]);
        setting["required"] = json!(true);
        setting["default"] = json!([]);
        assert!(validate_settings_schema(&[setting]).is_err());
    }
}
