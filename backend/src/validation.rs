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
    if kind == "answer"
        && let Some(answers) = value["answers"].as_object_mut()
    {
        for answer in answers.values_mut() {
            if let Some(items) = answer.as_array_mut() {
                for item in items {
                    if let Some(text) = item.as_str() {
                        *item = text.trim().into();
                    }
                }
            }
        }
    }
    if let Err(error) = VALIDATORS[kind].validate(&value) {
        return Err(Error::bad(format!("{}: {}", error.instance_path(), error)));
    }
    if kind == "questions" {
        let mut ids = std::collections::HashSet::new();
        for field in value.as_array().unwrap() {
            if !ids.insert(text(field, "id")) {
                return Err(Error::bad("Question identifiers must be unique"));
            }
        }
    }
    if kind == "mcp" {
        if value["transport"] == "http" {
            let url = url::Url::parse(value["url"].as_str().unwrap_or("")).map_err(|_| {
                Error::bad("Enter an HTTP(S) URL without embedded credentials or a fragment.")
            })?;
            if !["http", "https"].contains(&url.scheme())
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(Error::bad(
                    "Enter an HTTP(S) URL without embedded credentials or a fragment.",
                ));
            }
        } else if value["command"] == "" || value["auth"] != "none" {
            return Err(Error::bad(
                "Command servers require an executable and use environment variables for authentication.",
            ));
        }
    }
    Ok(value)
}

pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

pub fn uuid(value: &str) -> Result<()> {
    uuid::Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| Error::bad("Invalid UUID"))
}
