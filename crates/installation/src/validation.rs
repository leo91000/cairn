use crate::error::{Error, Result};
use serde_json::Value;
use std::{collections::HashMap, sync::LazyLock};

static SCHEMAS: LazyLock<Value> = LazyLock::new(|| {
    let mut schemas: Value =
        serde_json::from_str(include_str!("../schemas/inputs.json")).expect("checked schemas");
    let catalog: Value = serde_json::from_str(include_str!("../schemas/mcp-tools.json"))
        .expect("checked MCP schemas");
    for tool in catalog.as_array().unwrap() {
        schemas[format!("mcp:{}", text(tool, "name"))] = tool["inputSchema"].clone();
    }
    schemas
});

static VALIDATORS: LazyLock<HashMap<String, jsonschema::Validator>> = LazyLock::new(|| {
    SCHEMAS
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, schema)| {
            (
                key.clone(),
                jsonschema::options()
                    .should_validate_formats(true)
                    .build(schema)
                    .expect("valid input schema"),
            )
        })
        .collect()
});

fn defaults(value: &mut Value, schema: &Value) {
    if let Some(alternatives) = schema["anyOf"].as_array() {
        for branch in alternatives {
            let matching = match branch["type"].as_str() {
                Some("object") => value.is_object(),
                Some("array") => value.is_array(),
                _ => false,
            };
            if matching {
                defaults(value, branch);
            }
        }
    }
    if let (Some(value), Some(properties)) =
        (value.as_object_mut(), schema["properties"].as_object())
    {
        if schema["additionalProperties"] == false {
            value.retain(|key, _| properties.contains_key(key));
        }
        for (key, schema) in properties {
            if !value.contains_key(key)
                && let Some(default) = schema.get("default")
            {
                value.insert(key.clone(), default.clone());
            }
            if let Some(item) = value.get_mut(key) {
                defaults(item, schema);
            }
        }
    }
    if let Some(items) = value.as_array_mut() {
        for item in items {
            defaults(item, &schema["items"]);
        }
    }
}

pub fn parse(kind: &str, mut value: Value) -> Result<Value> {
    let schema = SCHEMAS
        .get(kind)
        .ok_or_else(|| Error::internal("Unknown input schema"))?;
    defaults(&mut value, schema);
    trim_text(kind, &mut value);
    if let Err(error) = VALIDATORS[kind].validate(&value) {
        return Err(Error::bad(format!("{}: {}", error.instance_path(), error)));
    }
    match kind {
        "questions" => unique_question_ids(&value)?,
        "mcp" => mcp_transport(&value)?,
        _ => {}
    }
    Ok(value)
}

fn trim_text(kind: &str, value: &mut Value) {
    let trims: &[&str] = match kind {
        "agent" | "project" => &["name"],
        "task" => &["name", "prompt"],
        "message" => &["text", "model"],
        "mcp" => &["name", "command", "clientId", "scopes"],
        _ => &[],
    };
    for key in trims {
        if let Some(text) = value[*key].as_str() {
            value[*key] = text.trim().into();
        }
    }
    if kind != "answer" {
        return;
    }
    let Some(answers) = value["answers"].as_object_mut() else {
        return;
    };
    for items in answers.values_mut().filter_map(Value::as_array_mut) {
        for item in items {
            if let Some(text) = item.as_str() {
                *item = text.trim().into();
            }
        }
    }
}

fn unique_question_ids(fields: &Value) -> Result<()> {
    let mut ids = std::collections::HashSet::new();
    for field in fields.as_array().into_iter().flatten() {
        if !ids.insert(text(field, "id")) {
            return Err(Error::bad("Question identifiers must be unique"));
        }
    }
    Ok(())
}

fn mcp_transport(value: &Value) -> Result<()> {
    if value["transport"] != "http" {
        if value["command"] == "" || value["auth"] != "none" {
            return Err(Error::bad(
                "Command servers require an executable and use environment variables for authentication.",
            ));
        }
        return Ok(());
    }
    let invalid = || Error::bad("Enter an HTTP(S) URL without embedded credentials or a fragment.");
    let url = url::Url::parse(text(value, "url")).map_err(|_| invalid())?;
    if !["http", "https"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(())
}

/// Parses a validated input into its typed shape (see [`parse`]).
pub fn parse_as<T: serde::de::DeserializeOwned>(kind: &str, value: Value) -> Result<T> {
    Ok(serde_json::from_value(parse(kind, value)?)?)
}

pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

pub fn uuid(value: &str) -> Result<()> {
    uuid::Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| Error::bad("Invalid UUID"))
}

/// Declares a string-valued state enum persisted in JSON documents, with the
/// same conveniences as `RunStatus`: `as_str`, `parse`, `Value == Enum` and
/// `Value::from(Enum)`.
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident { $($variant:ident => $text:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        $vis enum $name {
            $(#[serde(rename = $text)] $variant),+
        }
        impl $name {
            #[allow(dead_code)]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }
            #[allow(dead_code)]
            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
        impl From<$name> for serde_json::Value {
            fn from(value: $name) -> Self {
                Self::String(value.as_str().to_owned())
            }
        }
        impl PartialEq<$name> for serde_json::Value {
            fn eq(&self, other: &$name) -> bool {
                self.as_str() == Some(other.as_str())
            }
        }
    };
}

pub(crate) use string_enum;
