//! Help renders declared metadata only; it never calls a plugin or its programs.
use cairn_plugin_api::{Command, Output, usage};

/// Match the command parser: flags take one value; `--` escapes one positional value.
pub(super) fn requested(command: &Command, words: &[String]) -> bool {
    let mut index = 0;
    while let Some(word) = words.get(index) {
        if word == "--" {
            index += 2;
            continue;
        }
        if command
            .arguments
            .iter()
            .any(|argument| argument.flag.as_ref() == Some(word))
        {
            index += 2;
            continue;
        }
        if word == "--help" || word == "-h" {
            return true;
        }
        index += 1;
    }
    false
}

pub(super) fn package(id: &str, name: &str, description: &str, commands: &[Command]) -> String {
    let mut output = format!(
        "{name} — {description}\n\nUsage:\n  cairn plugin {id} <command> [arguments]\n\nCommands granted to this agent:\n"
    );
    for command in commands {
        output.push_str(&format!(
            "  {}\n      {}\n",
            usage(command),
            command.description
        ));
    }
    output.push_str(&format!("\nRun 'cairn plugin {id} <command> --help' for argument details.\nResource restrictions and instance settings still apply to each invocation.\n"));
    output
}

pub(super) fn command(package: &str, command: &Command) -> String {
    let mut output = format!(
        "{}\n\nUsage:\n  cairn plugin {package} {}\n",
        command.description,
        usage(command)
    );
    if !command.arguments.is_empty() {
        output.push_str("\nArguments:\n");
        for argument in &command.arguments {
            let label = argument.flag.as_ref().map_or_else(
                || format!("<{}>", argument.name),
                |flag| format!("{flag} <{}>", argument.name),
            );
            output.push_str(&format!(
                "  {label} ({})\n",
                if argument.required {
                    "required"
                } else {
                    "optional"
                }
            ));
            if !argument.description.is_empty() {
                output.push_str(&format!("      {}\n", argument.description));
            }
            if let Some(pattern) = &argument.pattern {
                output.push_str(&format!("      Format: {pattern}\n"));
            }
            output.push_str(&format!(
                "      Maximum length: {} characters.\n",
                argument.max_length.unwrap_or(2000)
            ));
        }
    }
    output.push_str("\nOptions:\n  -h, --help    Show this help without running the command.\n  -- <value>    Escape one positional value beginning with --.\n\n");
    output.push_str(match command.output {
        Output::Json => "Output: JSON, followed by a newline.\n",
        Output::Text => "Output: text, followed by a newline.\n",
        Output::Secret => "Output: secret bytes, without an added newline. Pipe stdout directly into the\nconsuming command, or capture it in script memory. Never print or log the value.\n",
    });
    output.push_str("\nFlags accept --flag value or --flag=value. Grants and resource restrictions are\nchecked live when the command runs. This help reads metadata only.\n");
    output
}
