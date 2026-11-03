//! Edit only changed native properties, retaining comments and unrelated formatting.
use super::{parse, render};
use crate::{
    error::{Error, Result},
    provider::Provider,
};
use jsonc_parser::{
    ParseOptions,
    cst::{CstInputValue, CstObject, CstRootNode},
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) fn changed(
    before: Option<&Value>,
    after: Option<&Value>,
    prefix: &str,
    output: &mut BTreeSet<String>,
) {
    if before == after {
        return;
    }
    if let (Some(Value::Object(before)), Some(Value::Object(after))) = (before, after) {
        for key in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
            changed(
                before.get(key),
                after.get(key),
                &format!("{prefix}/{}", super::pointer(key)),
                output,
            );
        }
        return;
    }
    output.insert(prefix.to_owned());
}

pub(super) fn rewrite(provider: Provider, source: &str, after: &Value) -> Result<String> {
    let before = parse(provider, source)?;
    let mut changes = BTreeSet::new();
    changed(Some(&before), Some(after), "", &mut changes);
    if changes.is_empty() {
        return Ok(source.into());
    }
    let edited = if provider == Provider::Codex {
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|_| Error::bad("Invalid native TOML."))?;
        for path in &changes {
            let keys = keys(path);
            toml_set(document.as_table_mut(), &keys, after.pointer(path))?;
        }
        document.to_string()
    } else {
        let options = options(provider);
        let source = if source.trim().is_empty() {
            "{}\n"
        } else {
            source
        };
        let root = CstRootNode::parse(source, &options)
            .map_err(|_| Error::bad("Invalid native JSON document."))?;
        let object = root
            .object_value_or_create()
            .ok_or_else(|| Error::bad("Native configuration must be an object."))?;
        for path in &changes {
            json_set(&object, &keys(path), after.pointer(path))?;
        }
        root.to_string()
    };
    // Duplicate JSON properties and TOML's dotted table representation can be
    // ambiguous to a concrete editor. Never return a document with other values.
    if parse(provider, &edited)? != *after {
        return render(provider, after);
    }
    Ok(edited)
}

fn keys(path: &str) -> Vec<String> {
    path[1..]
        .split('/')
        .map(|key| key.replace("~1", "/").replace("~0", "~"))
        .collect()
}

pub(super) fn options(provider: Provider) -> ParseOptions {
    ParseOptions {
        allow_comments: provider == Provider::Opencode,
        allow_trailing_commas: provider == Provider::Opencode,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    }
}

fn toml_set(
    table: &mut dyn toml_edit::TableLike,
    keys: &[String],
    replacement: Option<&Value>,
) -> Result<()> {
    let key = &keys[0];
    if keys.len() == 1 {
        if let Some(value) = replacement {
            let mut value = value
                .serialize(toml_edit::ser::ValueSerializer::new())
                .map_err(|_| Error::bad("Unsupported native TOML value."))?;
            if let Some(old) = table.get(key).and_then(toml_edit::Item::as_value) {
                *value.decor_mut() = old.decor().clone();
            }
            if let Some(slot) = table.get_mut(key) {
                *slot = toml_edit::Item::Value(value);
            } else {
                table.insert(key, toml_edit::Item::Value(value));
            }
        } else {
            table.remove(key);
        }
        return Ok(());
    }
    if replacement.is_none() && table.get(key).is_none() {
        return Ok(());
    }
    if !table.get(key).is_some_and(toml_edit::Item::is_table_like) {
        if replacement.is_none() {
            return Ok(());
        }
        let mut child = toml_edit::Table::new();
        child.set_implicit(true);
        table.insert(key, toml_edit::Item::Table(child));
    }
    toml_set(
        table.get_mut(key).unwrap().as_table_like_mut().unwrap(),
        &keys[1..],
        replacement,
    )
}

fn json_set(object: &CstObject, keys: &[String], replacement: Option<&Value>) -> Result<()> {
    let key = &keys[0];
    if keys.len() == 1 {
        if let Some(value) = replacement {
            let value = json_input(value);
            if let Some(property) = object.get(key) {
                property.set_value(value);
            } else {
                object.append(key, value);
            }
        } else if let Some(property) = object.get(key) {
            property.remove();
        }
        return Ok(());
    }
    let child = match object.object_value_or_create(key) {
        Some(child) => child,
        None if replacement.is_none() => return Ok(()),
        None => {
            object
                .get(key)
                .unwrap()
                .set_value(CstInputValue::Object(vec![]));
            object.object_value_or_create(key).unwrap()
        }
    };
    json_set(&child, &keys[1..], replacement)
}

fn json_input(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(value) => CstInputValue::Bool(*value),
        Value::Number(value) => CstInputValue::Number(value.to_string()),
        Value::String(value) => CstInputValue::String(value.clone()),
        Value::Array(values) => CstInputValue::Array(values.iter().map(json_input).collect()),
        Value::Object(values) => CstInputValue::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), json_input(value)))
                .collect(),
        ),
    }
}
