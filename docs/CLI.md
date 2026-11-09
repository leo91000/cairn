# Cairn command-line help

Start with `cairn --help` (or `cairn -h`, `cairn help`). It lists installation
commands, plugin discovery and the runtime commands normally started by Cairn.
Use `cairn help <command>` or `cairn <command> --help` for usage, arguments and
requirements. Local help exits successfully, writes to stdout and works without
installation configuration, a running server or credentials. It never starts a
service, worker or privileged runtime setup. Unknown commands and malformed help
topics fail on stderr with a hint to the general help.
Local help flags also work after command arguments. `supervise` preserves flags
after its executable as child arguments; use `cairn help supervise` for its help.

## Plugin discovery inside a conversation

```sh
cairn plugin --list
cairn plugin onepassword --help
cairn plugin onepassword read --help
# Equivalent command help:
cairn help plugin onepassword read
```

`cairn plugin` and `cairn plugin --help` explain the discovery and file transfer
syntax locally. Package and command help use the conversation's private channel
and live grants: only commands granted on an enabled instance of an approved,
active package are listed. A tool-only grant does not advertise CLI commands.
Resource restrictions and owner settings still apply when invoking a command.
Missing conversation credentials or an unavailable channel produce an explicit
error; neither establishes that a plugin command is absent.

Command help comes from the package manifest: description, required and optional
arguments, formats, length limits and output type. It executes no plugin script,
program, provider API or secret read. A standalone `--help` or `-h` requests help;
a flag value such as `--item --help` remains a literal value. `-- <value>` escapes
one positional value, matching the existing plugin argument parser.

Commands producing secret output must be piped into the consuming command or
captured in script memory. Never run them just to inspect their output in a
conversation. See [1Password](ONEPASSWORD.md) and [plugin commands](PLUGINS.md).
