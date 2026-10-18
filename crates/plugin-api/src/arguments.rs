//! Declared command arguments: the host parses `cairn plugin <plugin> <command> …`
//! so packages receive named, validated text values.
use super::{Argument, Command};
use serde_json::{Map, Value};

const DEFAULT_MAX: usize = 2000;

fn argument_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 40
        && value.starts_with(|c: char| c.is_ascii_lowercase())
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn flag(value: &str) -> bool {
    value.strip_prefix("--").is_some_and(|name| {
        super::slug(name, 30) && name.starts_with(|c: char| c.is_ascii_lowercase())
    })
}

pub(crate) fn validate(command: &Command) -> Result<(), &'static str> {
    if command.arguments.len() > 20 {
        return Err("Plugin command declares too many arguments.");
    }
    let mut names = Vec::new();
    let mut flags = Vec::new();
    for argument in &command.arguments {
        let placement_ok = match (&argument.flag, argument.positional) {
            (Some(value), false) => flag(value) && !flags.contains(&value.as_str()),
            (None, true) => true,
            _ => false,
        };
        if !argument_name(&argument.name)
            || names.contains(&argument.name.as_str())
            || !placement_ok
            || !super::short(&argument.description, 300)
            || argument
                .pattern
                .as_deref()
                .is_some_and(|source| super::pattern(source).is_none())
            || argument
                .max_length
                .is_some_and(|max| max == 0 || max > 10_000)
        {
            return Err("Plugin manifest has an invalid command argument.");
        }
        names.push(&argument.name);
        if let Some(value) = &argument.flag {
            flags.push(value);
        }
    }
    Ok(())
}

fn accept(argument: &Argument, value: &str, values: &mut Map<String, Value>) -> Result<(), String> {
    if values.contains_key(&argument.name) {
        return Err(format!("{} is given twice.", label(argument)));
    }
    let max = argument.max_length.unwrap_or(DEFAULT_MAX);
    if value.chars().count() > max || value.chars().any(char::is_control) {
        return Err(format!("{} is too long or invalid.", label(argument)));
    }
    if let Some(source) = &argument.pattern
        && !super::pattern(source).is_some_and(|pattern| pattern.is_match(value))
    {
        return Err(format!("{} has an invalid format.", label(argument)));
    }
    values.insert(argument.name.clone(), Value::String(value.to_owned()));
    Ok(())
}

fn label(argument: &Argument) -> String {
    argument
        .flag
        .clone()
        .unwrap_or_else(|| format!("<{}>", argument.name))
}

/// Usage line for errors and documentation, e.g. `read --account <account> <reference>`.
pub fn usage(command: &Command) -> String {
    let mut parts = vec![command.name.clone()];
    for argument in &command.arguments {
        let part = match &argument.flag {
            Some(flag) => format!("{flag} <{}>", argument.name),
            None => format!("<{}>", argument.name),
        };
        parts.push(if argument.required {
            part
        } else {
            format!("[{part}]")
        });
    }
    parts.join(" ")
}

/// Parses command-line words against the command's declared arguments. Supports
/// `--flag value`, `--flag=value` and positional values in declaration order.
pub fn parse_arguments(command: &Command, words: &[String]) -> Result<Map<String, Value>, String> {
    if words.len() > 64 {
        return Err("Too many command arguments.".into());
    }
    let mut values = Map::new();
    let mut positional = command
        .arguments
        .iter()
        .filter(|argument| argument.positional);
    let mut index = 0;
    while index < words.len() {
        let word = &words[index];
        index += 1;
        if let Some(rest) = word.strip_prefix("--").filter(|_| word != "--") {
            let (name, inline) = match rest.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (rest, None),
            };
            let argument = command
                .arguments
                .iter()
                .find(|argument| argument.flag.as_deref() == Some(&format!("--{name}")))
                .ok_or_else(|| format!("Unknown option --{name}. Usage: {}", usage(command)))?;
            let value = match inline {
                Some(value) => value,
                None => {
                    let value = words
                        .get(index)
                        .ok_or_else(|| format!("{} needs a value.", label(argument)))?;
                    index += 1;
                    value.clone()
                }
            };
            accept(argument, &value, &mut values)?;
        } else {
            let value = if word == "--" {
                let value = words.get(index).ok_or("Missing value after --.")?;
                index += 1;
                value
            } else {
                word
            };
            let argument = positional
                .next()
                .ok_or_else(|| format!("Unexpected argument. Usage: {}", usage(command)))?;
            accept(argument, value, &mut values)?;
        }
    }
    if let Some(missing) = command
        .arguments
        .iter()
        .find(|argument| argument.required && !values.contains_key(&argument.name))
    {
        return Err(format!(
            "{} is required. Usage: {}",
            label(missing),
            usage(command)
        ));
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn command() -> Command {
        serde_json::from_value(json!({
            "name": "read",
            "output": "secret",
            "arguments": [
                { "name": "account", "flag": "--account", "required": true },
                { "name": "reference", "positional": true, "required": true, "pattern": "op://.+", "maxLength": 2000 },
            ],
        }))
        .unwrap()
    }

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn flags_and_positionals_parse_into_named_values() {
        let command = command();
        validate(&command).unwrap();
        let values = parse_arguments(&command, &words(&["--account", "a1", "op://v/i/f"])).unwrap();
        assert_eq!(values["account"], "a1");
        assert_eq!(values["reference"], "op://v/i/f");
        let values = parse_arguments(&command, &words(&["op://v/i/f", "--account=a1"])).unwrap();
        assert_eq!(values["account"], "a1");
        assert_eq!(usage(&command), "read --account <account> <reference>");
    }

    #[test]
    fn unknown_duplicate_missing_and_malformed_values_are_refused() {
        let command = command();
        for input in [
            vec!["--account", "a1"],
            vec!["--account", "a1", "--account", "a2", "op://v/i/f"],
            vec!["--vault", "x", "--account", "a1", "op://v/i/f"],
            vec!["--account", "a1", "https://example.com"],
            vec!["--account", "a1", "op://v/i/f", "extra"],
            vec!["--account"],
            vec!["--account", "a\u{7}", "op://v/i/f"],
        ] {
            assert!(
                parse_arguments(&command, &words(&input)).is_err(),
                "{input:?}"
            );
        }
    }

    #[test]
    fn declarations_need_one_placement_and_valid_patterns() {
        for argument in [
            json!({ "name": "a" }),
            json!({ "name": "a", "flag": "--a", "positional": true }),
            json!({ "name": "a", "flag": "-a" }),
            json!({ "name": "A", "positional": true }),
            json!({ "name": "a", "positional": true, "pattern": "(" }),
        ] {
            let command: Command =
                serde_json::from_value(json!({ "name": "x", "arguments": [argument] })).unwrap();
            assert!(validate(&command).is_err());
        }
    }
}
