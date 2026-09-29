//! Field readers for third-party JSON: a value of an unexpected type reads as missing
//! instead of rejecting the whole message.
use super::usage::Usage;
use crate::provider::Provider;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// A string, or "" for anything else.
pub fn string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(Value::deserialize(deserializer)?
        .as_str()
        .unwrap_or_default()
        .to_owned())
}

/// A string, or `None` for anything else.
pub fn optional_string<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Ok(Value::deserialize(deserializer)?
        .as_str()
        .map(str::to_owned))
}

/// An integer, or `None` for anything else.
pub fn integer<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    Ok(Value::deserialize(deserializer)?.as_i64())
}

/// Exactly `true`.
pub fn flag<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(Value::deserialize(deserializer)? == true)
}

/// The strings of an array, or none for anything else.
pub fn strings<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect())
}

/// A readable `T`, or its default for anything else.
pub fn or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

/// "claude", or Codex for anything else.
pub fn provider<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Provider, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(if value == "claude" {
        Provider::Claude
    } else {
        Provider::Codex
    })
}

/// Unreadable usage counts as unknown usage.
pub fn usage<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Usage>, D::Error> {
    Ok(Usage::parse(&Value::deserialize(deserializer)?))
}
