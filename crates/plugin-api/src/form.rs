//! Instance forms driven by the package: the `form` handler's result while the owner
//! edits an instance. Dynamic choices, derived values, field errors and guidance.
use super::Manifest;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_BYTES: usize = 262_144;
const MAX_OPTIONS: usize = 200;

/// One bounded remote choice lookup, independent of a full form evaluation.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FormLookup {
    pub field: String,
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

impl FormLookup {
    pub fn parse(settings: &[Value], input: Option<&Value>) -> Result<Option<Self>, &'static str> {
        let Some(input) = input.filter(|input| !input.is_null()) else {
            return Ok(None);
        };
        let invalid = "Name a declared remote choice field and a bounded query/cursor.";
        let lookup: Self = serde_json::from_value(input.clone()).map_err(|_| invalid)?;
        let remote = settings.iter().any(|field| {
            field["key"] == lookup.field
                && field["dynamic"] == true
                && field["remote"] == true
                && matches!(field["type"].as_str(), Some("select" | "multiselect"))
        });
        if !remote
            || !super::short(&lookup.query, 200)
            || lookup
                .cursor
                .as_ref()
                .is_some_and(|cursor| !valid_cursor(cursor))
        {
            return Err(invalid);
        }
        Ok(Some(lookup))
    }
}

fn valid_cursor(cursor: &str) -> bool {
    !cursor.trim().is_empty() && cursor.len() <= 4096 && !cursor.chars().any(char::is_control)
}

/// Lookup callbacks cannot derive values, guidance or options for another field.
pub fn validate_lookup_result(
    settings: &[Value],
    global_errors: &[&str],
    lookup: &FormLookup,
    result: &Value,
) -> Result<(), &'static str> {
    validate_form_fields(settings, global_errors, result)?;
    let invalid = "A choice lookup can only return its options, page and errors.";
    if result.get("values").is_some() || result.get("view").is_some() {
        return Err(invalid);
    }
    for key in ["options", "pages"] {
        if result[key]
            .as_object()
            .is_some_and(|map| map.keys().any(|key| key != &lookup.field))
        {
            return Err(invalid);
        }
    }
    if result["errors"].as_object().is_some_and(|map| {
        map.keys()
            .any(|key| key != &lookup.field && !global_errors.contains(&key.as_str()))
    }) {
        return Err(invalid);
    }
    Ok(())
}

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
    validate_form_fields(&manifest.settings, &["credential", "form"], result)
}

/// Validate a form against its own declared fields and explicitly allowed global
/// errors. Resource forms cannot derive root settings or return secret values.
pub fn validate_form_fields(
    settings: &[Value],
    global_errors: &[&str],
    result: &Value,
) -> Result<(), &'static str> {
    if result.is_null() {
        return Ok(());
    }
    let invalid = "Invalid plugin form result.";
    let map = result.as_object().ok_or(invalid)?;
    if serde_json::to_string(result).map_or(true, |json| json.len() > MAX_BYTES)
        || map.keys().any(|key| {
            !matches!(
                key.as_str(),
                "options" | "pages" | "values" | "errors" | "view"
            )
        })
    {
        return Err(invalid);
    }
    let field = |key: &str| settings.iter().find(|field| field["key"] == key);
    if let Some(options) = map.get("options").filter(|value| !value.is_null()) {
        for (key, items) in options.as_object().ok_or(invalid)? {
            let dynamic = field(key).is_some_and(|field| field["dynamic"] == true);
            let items = items.as_array().ok_or(invalid)?;
            let mut ids = std::collections::HashSet::new();
            if !dynamic
                || items.len() > MAX_OPTIONS
                || !items.iter().all(option)
                || items.iter().any(|item| !ids.insert(item["value"].as_str()))
            {
                return Err(invalid);
            }
        }
    }
    if let Some(pages) = map.get("pages").filter(|value| !value.is_null()) {
        for (key, page) in pages.as_object().ok_or(invalid)? {
            let remote =
                field(key).is_some_and(|field| field["remote"] == true && field["dynamic"] == true);
            let page = page.as_object().ok_or(invalid)?;
            if !remote
                || page.len() != 1
                || !page.get("nextCursor").is_some_and(|cursor| {
                    cursor.is_null() || cursor.as_str().is_some_and(valid_cursor)
                })
            {
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
            let known = global_errors.contains(&key.as_str()) || field(key).is_some();
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

    #[test]
    fn scoped_forms_cannot_fill_another_schema_or_publish_secrets() {
        let fields = manifest().settings;
        validate_form_fields(
            &fields,
            &["form", "parentId"],
            &json!({
                "values": {"spaceName": "General"}, "errors": {"parentId": "Choose a workspace"}
            }),
        )
        .unwrap();
        for invalid in [
            json!({"values": {"key": "not-public"}}),
            json!({"values": {"appClientId": "another-schema"}}),
            json!({"errors": {"credential": "An app-level error"}}),
            json!({"options": {"space": vec![json!({"value": "1", "label": "General"}); MAX_OPTIONS + 1]}}),
        ] {
            assert!(
                validate_form_fields(&fields, &["form", "parentId"], &invalid).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn remote_lookups_are_bounded_and_declared() {
        let mut fields = manifest().settings;
        fields[0]["remote"] = json!(true);
        for kind in ["select", "multiselect"] {
            fields[0]["type"] = json!(kind);
            validate_settings_schema(&fields).unwrap();
            let request = json!({"field": "space", "query": "", "cursor": "opaque page"});
            let lookup = FormLookup::parse(&fields, Some(&request)).unwrap().unwrap();
            assert_eq!(serde_json::to_value(&lookup).unwrap(), request);
        }
        for request in [
            json!({"field": "space"}),
            json!({"field": "spaceName", "query": ""}),
            json!({"field": "space", "query": "x".repeat(201)}),
            json!({"field": "space", "query": "", "cursor": ""}),
            json!({"field": "space", "query": "", "cursor": "x".repeat(4097)}),
            json!({"field": "space", "query": "", "cursor": "line\nbreak"}),
            json!({"field": "space", "query": "", "unexpected": true}),
        ] {
            assert!(
                FormLookup::parse(&fields, Some(&request)).is_err(),
                "{request}"
            );
        }
        fields[0]["remote"] = json!(false);
        assert!(FormLookup::parse(&fields, Some(&json!({"field": "space", "query": ""}))).is_err());
        fields[1]["remote"] = json!(true);
        assert!(validate_settings_schema(&fields).is_err());
    }

    #[test]
    fn remote_results_distinguish_empty_continuations_completion_and_unknown() {
        let mut fields = manifest().settings;
        fields[0]["remote"] = json!(true);
        let lookup = FormLookup::parse(
            &fields,
            Some(&json!({"field": "space", "query": "Engineering"})),
        )
        .unwrap()
        .unwrap();
        for result in [
            json!({"options": {"space": []}, "pages": {"space": {"nextCursor": "next"}}}),
            json!({"options": {"space": []}, "pages": {"space": {"nextCursor": null}}}),
            json!({"options": {"space": []}}),
            json!({"errors": {"form": "Sign in again"}}),
            Value::Null,
        ] {
            validate_lookup_result(&fields, &["form"], &lookup, &result).unwrap();
        }
        for result in [
            json!({"values": {}}),
            json!({"view": []}),
            json!({"errors": {"spaceName": "another field"}}),
            json!({"pages": {"spaceName": {"nextCursor": null}}}),
            json!({"pages": {"space": {}}}),
            json!({"pages": {"space": {"nextCursor": ""}}}),
            json!({"pages": {"space": {"nextCursor": "next", "total": 200}}}),
            json!({"options": {"space": [{"value": "1", "label": "One"}, {"value": "1", "label": "Duplicate"}]}}),
        ] {
            assert!(
                validate_lookup_result(&fields, &["form"], &lookup, &result).is_err(),
                "{result}"
            );
        }
    }

    #[test]
    fn remote_choices_require_an_invocable_configuration_form() {
        let mut manifest = manifest();
        manifest.settings[0]["remote"] = json!(true);
        manifest.validate().unwrap();
        manifest.instance_form = false;
        assert!(manifest.validate().is_err());
        manifest.instance_form = true;
        manifest.capabilities.push("resources".into());
        manifest.resources = serde_json::from_value(json!([{
            "name": "entries", "title": "Entries", "singular": "Entry",
            "settings": manifest.settings,
        }]))
        .unwrap();
        assert!(manifest.validate().is_err());
        manifest.resources[0].form = true;
        manifest.validate().unwrap();
        assert!(
            crate::validate_view(&json!([{
                "type": "form", "fields": manifest.settings,
                "submit": {"action": "save", "label": "Save"},
            }]))
            .is_err()
        );
    }
}
