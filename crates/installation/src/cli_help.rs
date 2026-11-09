//! Local help must run before services, runtime workers or privileged setup.
use cairn_installation::error::{Error, Result};

struct Command {
    name: &'static str,
    arguments: &'static str,
    description: &'static str,
    details: &'static str,
    internal: bool,
}

const COMMANDS: &[Command] = &[
    Command {
        name: "serve",
        arguments: "",
        description: "Start the installation server (the default command).",
        details: "Configuration comes from the environment, optionally overridden by CAIRN_CONFIG.\nCommon settings: HOST, PORT, DATA_DIR, AGENT_HOME, WORKSPACE_ROOTS and PUBLIC_URL.\nSee docs/PRODUCTION-CAIRN-SETUP.md for installation and deployment options.",
        internal: false,
    },
    Command {
        name: "claim",
        arguments: "",
        description: "Link this installation to its owner through Beacon.",
        details: "Displays a verification URL and device code. Complete the claim in your browser.\nCAIRN_BEACON_ORIGIN selects Beacon; CAIRN_INSTALLATION_NAME sets the display name.",
        internal: false,
    },
    Command {
        name: "rotate-token",
        arguments: "",
        description: "Rotate the installation's relay credential.",
        details: "Restart the installation manager afterwards to use its new identity.",
        internal: false,
    },
    Command {
        name: "node-enroll",
        arguments: "<manager-https-origin> <private-node-directory>",
        description: "Enroll an execution node with an installation.",
        details: "Uses the manager's HTTPS origin and stores the node identity in the private directory.",
        internal: false,
    },
    Command {
        name: "node-daemon",
        arguments: "<private-node-directory>",
        description: "Run an enrolled node and its managed runtime.",
        details: "Uses the identity in the private node directory. Runs until stopped.",
        internal: false,
    },
    Command {
        name: "node-connect",
        arguments: "<private-node-directory>",
        description: "Connect an enrolled node to its manager.",
        details: "Uses the identity in the private node directory. Runs until stopped.",
        internal: false,
    },
    Command {
        name: "chat",
        arguments: "[coding-agent-executable]",
        description: "Run a coding agent from a JSON execution plan on stdin.",
        details: "Consumes one execution plan from stdin. The optional executable overrides the\nplan's coding agent binary. Normally started by Cairn, inside a conversation.",
        internal: false,
    },
    Command {
        name: "plugin",
        arguments: "<package> <command> [arguments]",
        description: "Discover and use plugins granted to the current agent.",
        details: PLUGIN_HELP,
        internal: false,
    },
    Command {
        name: "version",
        arguments: "",
        description: "Print the Cairn version.",
        details: "Aliases: cairn --version, cairn -V.",
        internal: false,
    },
    Command {
        name: "guest",
        arguments: "",
        description: "Run the VM guest service.",
        details: "Started by the managed VM runtime.",
        internal: true,
    },
    Command {
        name: "native-controller",
        arguments: "",
        description: "Run the native execution controller.",
        details: "Started by the node runtime; requires its storage and execution environment.",
        internal: true,
    },
    Command {
        name: "native-enclave",
        arguments: "",
        description: "Run a native conversation enclave.",
        details: "Started by the native controller; requires its managed namespace and execution plan.",
        internal: true,
    },
    Command {
        name: "native-docker",
        arguments: "",
        description: "Prepare the native Docker environment.",
        details: "Used by the native runtime. May create or start managed Docker resources.",
        internal: true,
    },
    Command {
        name: "native-docker-reset",
        arguments: "",
        description: "Reset the managed native Docker environment.",
        details: "Used for runtime recovery. Changes managed Docker resources.",
        internal: true,
    },
    Command {
        name: "native-watchdog",
        arguments: "<lease-file> <attempt>",
        description: "Watch a native execution lease.",
        details: "Started by the native controller for one execution attempt.",
        internal: true,
    },
    Command {
        name: "toolkit-env",
        arguments: "",
        description: "Print the resolved toolkit environment as JSON.",
        details: "Used by the runtime. Output can contain sensitive environment values; do not\ncopy it into conversation history or logs.",
        internal: true,
    },
    Command {
        name: "runner-broker",
        arguments: "",
        description: "Run the execution broker.",
        details: "Started by the managed runtime; requires its broker configuration.",
        internal: true,
    },
    Command {
        name: "runner-client",
        arguments: "<runner-id>",
        description: "Connect to a managed runner.",
        details: "Used by the runtime for the selected runner.",
        internal: true,
    },
    Command {
        name: "runner-entry",
        arguments: "",
        description: "Start an execution from /run/cairn-plan.json.",
        details: "Started inside a managed runtime; consumes its installed execution plan.",
        internal: true,
    },
    Command {
        name: "codex-service",
        arguments: "<socket-path>",
        description: "Run the resident Codex service on a local socket.",
        details: "Requires the managed CODEX_HOME and installation configuration.",
        internal: true,
    },
    Command {
        name: "supervise",
        arguments: "<executable> [arguments]",
        description: "Supervise a managed subprocess.",
        details: "Started by Cairn with a private control socket on file descriptor 3.",
        internal: true,
    },
    Command {
        name: "plugin-worker",
        arguments: "",
        description: "Run the isolated plugin worker protocol.",
        details: "Private stdin/stdout protocol. Started by Cairn; not an interactive command.",
        internal: true,
    },
    Command {
        name: "mcp-code-worker",
        arguments: "",
        description: "Run the isolated MCP code worker protocol.",
        details: "Private stdin/stdout protocol. Started by Cairn; not an interactive command.",
        internal: true,
    },
];

const PLUGIN_HELP: &str = "Usage:
  cairn plugin --list
  cairn plugin <package> --help
  cairn plugin <package> <command> --help
  cairn plugin <package> <command> [arguments]

Discovery:
  --list                 List packages with commands granted to this agent.
  <package> --help        Show granted commands and their descriptions.
  <command> --help        Show arguments, required values and output format.
  -h, --help             Show help without running a plugin command.

Examples (inside an active Cairn conversation):
  cairn plugin --list
  cairn plugin onepassword --help
  cairn plugin onepassword accounts
  cairn plugin onepassword read --help

File transfers (for packages with file access):
  cairn plugin <package> --upload <instance> <source-path>
  cairn plugin <package> --download <instance> <handle> <new-path>
  cairn plugin <package> --delete-file <instance> <handle>
  Uploads accept regular files up to 40 MB. Downloads create a new private file.

Package discovery and commands require an active Cairn conversation. Grants are
checked live; help lists command availability, not permission for every resource.
Commands returning secret output belong in a pipe or script memory, never in conversation logs.
An unavailable channel is an access check failure, not proof a command is absent.";

fn unknown() -> Error {
    Error::bad("Unknown command or help topic. Run 'cairn --help' for available commands.")
}

/// Normalize `help COMMAND` while rejecting malformed requests before any effects.
pub fn normalize(mut args: Vec<String>) -> Result<Vec<String>> {
    if args.first().is_none_or(|arg| arg != "help") {
        return Ok(args);
    }
    args.remove(0);
    if args.is_empty() {
        return Ok(vec!["--help".into()]);
    }
    let command = COMMANDS.iter().find(|command| command.name == args[0]);
    if command.is_none() || args.len() > if args[0] == "plugin" { 3 } else { 1 } {
        return Err(unknown());
    }
    args.push("--help".into());
    Ok(args)
}

pub fn render(args: &[String]) -> Result<Option<String>> {
    let Some(mode) = args.first() else {
        return Ok(None);
    };
    if ["-h", "--help"].contains(&mode.as_str()) {
        if args.len() != 1 {
            return Err(unknown());
        }
        return Ok(Some(general()));
    }
    let transfer_help = mode == "plugin"
        && args.len() == 4
        && ["--upload", "--download", "--delete-file"].contains(&args[2].as_str())
        && ["--help", "-h"].contains(&args[3].as_str());
    let list_help = mode == "plugin"
        && args.len() == 3
        && args[1] == "--list"
        && ["--help", "-h"].contains(&args[2].as_str());
    if transfer_help || list_help {
        return Ok(Some(format!("{PLUGIN_HELP}\n")));
    }
    let local_help = (mode == "plugin" && args.len() == 1)
        || args
            .get(1)
            .is_some_and(|arg| ["-h", "--help"].contains(&arg.as_str()));
    if !local_help {
        return Ok(None);
    }
    if args.len() > 2 {
        return Err(unknown());
    }
    let command = COMMANDS
        .iter()
        .find(|command| command.name == mode)
        .ok_or_else(unknown)?;
    if command.name == "plugin" {
        return Ok(Some(format!(
            "{}\n\n{}\n",
            command.description, command.details
        )));
    }
    Ok(Some(format!(
        "{}\n\nUsage:\n  cairn {}{}\n\n{}\n\nHelp: cairn help [command]\n",
        command.description,
        command.name,
        if command.arguments.is_empty() {
            String::new()
        } else {
            format!(" {}", command.arguments)
        },
        command.details,
    )))
}

fn general() -> String {
    let mut output = format!(
        "Cairn {} — conversations, scheduled work and execution nodes\n\nUsage:\n  cairn [command]\n  cairn help [command]\n  cairn <command> --help\n\nCommands:\n",
        env!("CARGO_PKG_VERSION"),
    );
    for command in COMMANDS.iter().filter(|command| !command.internal) {
        output.push_str(&format!("  {:20} {}\n", command.name, command.description));
    }
    output.push_str("\nOptions:\n  -h, --help           Print help and exit successfully.\n  -V, --version        Print the version and exit.\n\nExamples:\n  cairn serve\n  cairn claim\n  cairn plugin --list\n  cairn help plugin\n\nRuntime commands (normally started by Cairn):\n");
    for command in COMMANDS.iter().filter(|command| command.internal) {
        output.push_str(&format!("  {:20} {}\n", command.name, command.description));
    }
    output.push_str("\nRun 'cairn help <command>' for arguments and requirements.\nHelp prints documentation only; it does not start services or read secrets.\n");
    output
}
