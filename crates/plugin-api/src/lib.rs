//! Installation plugin contracts, independent of storage and agent engines.
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod arguments;
mod form;
mod settings;
mod sources;
mod view;

pub use arguments::{parse_arguments, usage};
pub use form::validate_form_result;
pub use settings::validate_settings_schema;
pub use sources::{MAX_MODULES, MAX_SOURCE_BYTES, Sources, module_path, resolve};
pub use view::{validate_action_result, validate_view};

/// Worker protocol and manifest version. Manifests must declare exactly this one.
pub const API_VERSION: u32 = 2;
/// Prefix of a host error carrying a package's `cairn.fail` message.
pub const REJECTED: &str = "rejected: ";
pub const MAX_FRAME_BYTES: usize = 1_048_576;

/// Host capabilities a package may request, with the sentence the owner approves.
pub const CAPABILITIES: &[(&str, &str)] = &[
    (
        "conversations.submit",
        "Start or continue conversations with the connection's agent, for people you allow",
    ),
    (
        "conversations.control",
        "Answer the agent's questions and stop its work, for people you allow",
    ),
    ("storage", "Keep small private notes for each connection"),
    (
        "http",
        "Call the web hosts listed by the package, with the connection's credential when asked",
    ),
    ("timers", "Schedule its own events"),
    (
        "socket",
        "Keep a WebSocket open to the hosts listed by the package, with the connection's credential when asked",
    ),
    (
        "inbound",
        "Hand messages it receives to Cairn, which keeps only those from people you allow",
    ),
    (
        "exec",
        "Run the programs listed by the package on this server, outside the Luau sandbox, with the secrets you enter",
    ),
];

/// The capability a host method belongs to, if any.
pub fn capability(method: &str) -> Option<&'static str> {
    let family = match method {
        "storage.get" | "storage.set" | "storage.delete" | "storage.list" => "storage",
        "http.request" => "http",
        "timers.after" | "timers.cancel" => "timers",
        "conversations.submit" => "conversations.submit",
        "conversations.check" | "conversations.answer" | "conversations.cancel" => {
            "conversations.control"
        }
        "exec.run" | "download" | "archive.executable" => "exec",
        "socket.connect" | "socket.send" | "socket.close" | "socket.timer" => "socket",
        "inbound.accept" => "inbound",
        _ => return None,
    };
    CAPABILITIES
        .iter()
        .map(|(name, _)| *name)
        .find(|name| *name == family)
}

/// How the host injects a connection's secret into authorized HTTP requests.
/// Plugin code never receives the secret itself.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Credential {
    pub label: String,
    pub header: String,
    #[serde(default)]
    pub prefix: String,
}

/// A program on the installation the package may run through `exec`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Program {
    pub name: String,
    /// Absolute path, approved by the owner with the package.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// Package-relative Luau module exporting `resolve` and `install` handlers.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub installer: String,
    #[serde(default)]
    pub description: String,
}

/// Where a command's result goes.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Output {
    /// Printed as JSON; the agent sees it in its command output.
    #[default]
    Json,
    /// Printed as is, followed by a newline.
    Text,
    /// Written to stdout exactly, for pipes. Never stored by Cairn.
    Secret,
}

/// One flag (`--vault ID`) or positional value of a command. Values are text.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Argument {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub positional: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// A regular expression the whole value must match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
}

/// A command agents run in their conversation: `cairn plugin <plugin> <name> …`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Command {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub output: Output,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<Argument>,
}

/// A tool listed by the run's Cairn MCP server. Input is checked against the schema.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Agents the owner grants on each instance may run the package's commands and tools.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentAccess {
    #[serde(default)]
    pub description: String,
}

/// The package's mark: a background colour and an SVG path on a 24×24 grid.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Brand {
    /// `#rrggbb`.
    pub color: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
}

/// How the owner's interfaces name one configured instance, e.g. "Service account".
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Instances {
    pub singular: String,
    pub plural: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// One sentence shown in the owner's plugin catalogue.
    #[serde(default)]
    pub description: String,
    pub api_version: u32,
    pub capabilities: Vec<String>,
    /// Hostnames reachable through `http` (HTTPS) and `socket` (WSS). `*.example.com`
    /// covers every subdomain of `example.com`, not the domain itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<Credential>,
    /// Instance fields Cairn renders, in order: `[{ key, title, type, ... }]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instances: Option<Instances>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand: Option<Brand>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub programs: Vec<Program>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_access: Option<AgentAccess>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<Command>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    /// Added to the prompt of runs whose agent is granted on an enabled instance.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    /// The script's `view` handler describes each instance's panel.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub instance_view: bool,
    /// The script's `test` handler checks an instance.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub instance_test: bool,
    /// The script's `form` handler fills dynamic choices, derived values, errors and
    /// guidance while the owner edits an instance, and checks it before it is saved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub instance_form: bool,
}

pub(crate) fn slug(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

pub(crate) fn short(value: &str, max: usize) -> bool {
    value.chars().count() <= max && !value.chars().any(|c| c.is_control() && c != '\n')
}

/// Compiles a declared pattern. Patterns are anchored to the whole value.
pub fn pattern(source: &str) -> Option<regex::Regex> {
    if source.is_empty() || source.len() > 200 {
        return None;
    }
    regex::RegexBuilder::new(&format!("^(?:{source})$"))
        .size_limit(1 << 20)
        .build()
        .ok()
}

fn hostname(value: &str) -> bool {
    let value = value.strip_prefix("*.").unwrap_or(value);
    value.len() <= 253
        && value.split('.').count() >= 2
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
}

fn program_path(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 200
        && !value.ends_with('/')
        && value.split('/').skip(1).all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-' | b'+'))
        })
}

fn tool_name(value: &str, plugin: &str) -> bool {
    let name_ok = !value.is_empty()
        && value.len() <= 64
        && value.starts_with(|c: char| c.is_ascii_lowercase())
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'));
    name_ok && (value == plugin || value.starts_with(&format!("{plugin}_")))
}

impl Manifest {
    /// Both source formats produce the same validated contract and approval digest.
    pub fn parse(format: &str, source: &str) -> Result<Self, &'static str> {
        if source.len() > 65_536 {
            return Err("Plugin manifest is too large.");
        }
        let manifest: Self = match format {
            "json" => serde_json::from_str(source).map_err(|_| "Invalid JSON plugin manifest."),
            "toml" => toml::from_str(source).map_err(|_| "Invalid TOML plugin manifest."),
            _ => Err("Plugin manifests must be JSON or TOML."),
        }?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.api_version != API_VERSION {
            return Err("Plugin API version is incompatible.");
        }
        if !slug(&self.id, 80)
            || self.name.trim().is_empty()
            || self.name.chars().count() > 100
            || self.version.is_empty()
            || self.version.len() > 32
            || !self
                .version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'+'))
            || self.description.chars().count() > 200
        {
            return Err("Invalid plugin manifest.");
        }
        let mut seen = Vec::new();
        for capability in &self.capabilities {
            if !CAPABILITIES.iter().any(|(name, _)| name == capability) {
                return Err("Plugin manifest requests an unknown capability.");
            }
            if seen.contains(&capability) {
                return Err("Plugin manifest repeats a capability.");
            }
            seen.push(capability);
        }
        if self.hosts.len() > 10 || !self.hosts.iter().all(|host| hostname(host)) {
            return Err("Plugin manifest lists invalid hosts.");
        }
        let network = self.permits_capability("http") || self.permits_capability("socket");
        if self.hosts.is_empty() == network {
            return Err("Plugin manifest must list hosts exactly when it requests http or socket.");
        }
        if let Some(credential) = &self.credential
            && (credential.label.trim().is_empty()
                || credential.label.chars().count() > 60
                || !slug(&credential.header, 40)
                || credential.prefix.len() > 20
                || !credential
                    .prefix
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b' '))
        {
            return Err("Plugin manifest has an invalid credential rule.");
        }
        validate_settings_schema(&self.settings)?;
        if let Some(instances) = &self.instances
            && [&instances.singular, &instances.plural]
                .iter()
                .any(|label| label.trim().is_empty() || !short(label, 40) || label.contains('\n'))
        {
            return Err("Plugin manifest has invalid instance labels.");
        }
        if let Some(brand) = &self.brand {
            let color = brand.color.len() == 7
                && brand.color.starts_with('#')
                && brand.color[1..].bytes().all(|c| c.is_ascii_hexdigit());
            // Path data only: commands, numbers and separators, nothing markup could use.
            let path = brand.path.len() <= 8000
                && brand
                    .path
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b' ' | b',' | b'.' | b'-'))
                && brand
                    .path
                    .bytes()
                    .filter(u8::is_ascii_alphabetic)
                    .all(|c| b"MmLlHhVvCcSsQqTtAaZz".contains(&c));
            if !color || !path {
                return Err("Plugin manifest has an invalid brand.");
            }
        }
        self.validate_programs()?;
        self.validate_agent_interface()
    }

    fn validate_programs(&self) -> Result<(), &'static str> {
        if self.programs.is_empty() == self.permits_capability("exec") {
            return Err("Plugin manifest must list programs exactly when it requests exec.");
        }
        if self.programs.len() > 8 {
            return Err("Plugin manifest lists too many programs.");
        }
        let mut names = Vec::new();
        for program in &self.programs {
            let valid_source = match (program.path.is_empty(), program.installer.is_empty()) {
                (false, true) => program_path(&program.path),
                (true, false) => module_path(&program.installer) && self.permits_capability("http"),
                _ => false,
            };

            if !slug(&program.name, 40)
                || names.contains(&program.name.as_str())
                || !valid_source
                || !short(&program.description, 200)
            {
                return Err("Plugin manifest has an invalid program.");
            }
            names.push(&program.name);
        }
        Ok(())
    }

    fn validate_agent_interface(&self) -> Result<(), &'static str> {
        let interface =
            !self.commands.is_empty() || !self.tools.is_empty() || !self.instructions.is_empty();
        if interface && self.agent_access.is_none() {
            return Err("Plugin commands, tools and instructions need agentAccess.");
        }
        if let Some(access) = &self.agent_access
            && !short(&access.description, 300)
        {
            return Err("Plugin manifest has an invalid agent access description.");
        }
        if !short(&self.instructions, 4000) {
            return Err("Plugin instructions are too long.");
        }
        if self.commands.len() > 32 {
            return Err("Plugin manifest declares too many commands.");
        }
        let mut names = Vec::new();
        for command in &self.commands {
            if !slug(&command.name, 40)
                || !command.name.starts_with(|c: char| c.is_ascii_lowercase())
                || names.contains(&command.name.as_str())
                || !short(&command.description, 300)
            {
                return Err("Plugin manifest has an invalid command.");
            }
            names.push(&command.name);
            arguments::validate(command)?;
        }
        if self.tools.len() > 16 {
            return Err("Plugin manifest declares too many tools.");
        }
        let mut names = Vec::new();
        for tool in &self.tools {
            let schema = serde_json::to_string(&tool.input_schema).unwrap_or_default();
            if !tool_name(&tool.name, &self.id.replace('-', "_"))
                || names.contains(&tool.name.as_str())
                || tool.description.trim().is_empty()
                || !short(&tool.description, 4000)
                || tool.input_schema["type"] != "object"
                || schema.len() > 16_384
            {
                return Err("Plugin manifest has an invalid tool.");
            }
            names.push(&tool.name);
        }
        Ok(())
    }

    pub fn permits_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|item| item == capability)
    }

    /// A host method is allowed only through a capability the manifest requests.
    /// Packages that bring their own connection (`socket` or `inbound`) also report its
    /// state with `instance.status`.
    pub fn permits(&self, method: &str) -> bool {
        if method == "instance.status" {
            return self.permits_capability("socket") || self.permits_capability("inbound");
        }
        capability(method).is_some_and(|capability| self.permits_capability(capability))
    }

    /// Whether a hostname is listed, exactly or under a `*.` wildcard.
    pub fn allows_host(&self, host: &str) -> bool {
        self.hosts
            .iter()
            .any(|allowed| match allowed.strip_prefix("*.") {
                Some(domain) => host
                    .strip_suffix(domain)
                    .is_some_and(|sub| sub.len() > 1 && sub.ends_with('.')),
                None => allowed == host,
            })
    }

    /// Packages that react to people, connections or timers keep a worker per instance.
    /// Others only run for commands, tools, views, actions, forms and tests.
    pub fn needs_worker(&self) -> bool {
        [
            "socket",
            "inbound",
            "conversations.submit",
            "conversations.control",
            "timers",
        ]
        .iter()
        .any(|capability| self.permits_capability(capability))
    }

    pub fn command(&self, name: &str) -> Option<&Command> {
        self.commands.iter().find(|command| command.name == name)
    }

    pub fn tool(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    pub fn program(&self, name: &str) -> Option<&Program> {
        self.programs.iter().find(|program| program.name == name)
    }

    /// Keys of settings stored as secrets.
    pub fn secret_settings(&self) -> impl Iterator<Item = &str> {
        self.settings
            .iter()
            .filter(|field| field["type"] == "secret")
            .filter_map(|field| field["key"].as_str())
    }
}

/// Newline-delimited frames between the host and one long-lived worker.
#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Frame {
    /// Host → worker, once: compile the package and return its handler table.
    Load {
        api_version: u32,
        /// `main.luau`.
        source: String,
        /// The package's other files, loaded on `require`.
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        modules: std::collections::BTreeMap<String, String>,
        deadline_ms: u64,
    },
    /// Worker → host: the package loaded and exposes handlers.
    Ready,
    /// Host → worker: run one handler to completion.
    Event { name: String, payload: Value },
    /// Worker → host: a host capability call from the running handler.
    Call { method: String, args: Value },
    /// Host → worker: the call's result.
    Reply { value: Value },
    /// Host → worker: the call was refused. Only a short code crosses the boundary.
    Denied { code: String },
    /// Worker → host: the handler returned.
    Done { value: Value },
    /// Worker → host: loading or the handler failed. Details stay in the worker.
    Failure,
    /// Worker → host: the handler called `cairn.fail(message)`, a message for people.
    Rejected { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(value: Value) -> Result<(), &'static str> {
        serde_json::from_value::<Manifest>(value)
            .map_err(|_| "parse")?
            .validate()
    }

    #[test]
    fn json_and_toml_manifests_have_the_same_validated_contract() {
        let json = r#"{"id":"probe","name":"Probe","version":"1.0.0","apiVersion":2,"capabilities":["exec"],"programs":[{"name":"probe","path":"/usr/bin/probe"}]}"#;
        let toml = r#"
id = "probe"
name = "Probe"
version = "1.0.0"
apiVersion = 2
capabilities = ["exec"]
[[programs]]
name = "probe"
path = "/usr/bin/probe"
"#;
        let expected = Manifest::parse("json", json).unwrap();
        let actual = Manifest::parse("toml", toml).unwrap();
        assert_eq!(actual, expected);
        assert!(
            !serde_json::to_string(&actual)
                .unwrap()
                .contains("installer")
        );
        assert!(Manifest::parse("toml", &format!("{toml}\nunknown = true")).is_err());
        assert!(Manifest::parse("yaml", json).is_err());
        assert!(Manifest::parse("toml", "id = [").is_err());
    }

    #[test]
    fn programs_declare_exactly_one_path_or_installer() {
        let mut value = json!({
            "id": "probe", "name": "Probe", "version": "1", "apiVersion": 2,
            "capabilities": ["http", "exec"], "hosts": ["downloads.example.com"],
            "programs": [{ "name": "probe", "installer": "programs/probe.luau" }],
        });
        assert!(manifest(value.clone()).is_ok());
        for invalid in [
            "/tmp/install.luau",
            "../install.luau",
            "main.luau",
            "install.sh",
        ] {
            value["programs"][0]["installer"] = json!(invalid);
            assert!(manifest(value.clone()).is_err());
        }
        value["programs"][0]["installer"] = json!("programs/probe.luau");
        value["programs"][0]["path"] = json!("/usr/bin/probe");
        assert!(manifest(value).is_err());
    }

    #[test]
    fn manifests_declare_known_capabilities_and_exact_https_hosts() {
        let base = json!({
            "id": "example",
            "name": "Example",
            "version": "1.0.0",
            "apiVersion": 2,
            "capabilities": ["http", "storage"],
            "hosts": ["api.example.com"],
            "credential": { "label": "API key", "header": "authorization", "prefix": "Bearer " },
            "settings": [{ "key": "team", "title": "Team", "type": "string" }]
        });
        assert!(manifest(base.clone()).is_ok());
        for (key, value) in [
            ("apiVersion", json!(1)),
            ("apiVersion", json!(3)),
            ("capabilities", json!(["shell"])),
            ("capabilities", json!(["http", "http"])),
            ("hosts", json!(["https://api.example.com"])),
            ("hosts", json!(["localhost"])),
            ("hosts", json!([])),
            ("version", json!("1.0/../..")),
            (
                "credential",
                json!({ "label": "Key", "header": "Authorization\n" }),
            ),
            (
                "settings",
                json!({ "team": { "title": "Team", "type": "object" } }),
            ),
        ] {
            let mut invalid = base.clone();
            invalid[key] = value;
            assert!(manifest(invalid).is_err(), "{key}");
        }
        let mut unknown = base;
        unknown["native"] = json!(true);
        assert!(manifest(unknown).is_err());
    }

    #[test]
    fn methods_map_to_their_capability() {
        let manifest: Manifest = serde_json::from_value(json!({
            "id": "example", "name": "Example", "version": "1", "apiVersion": 2,
            "capabilities": ["storage"]
        }))
        .unwrap();
        assert!(manifest.permits("storage.set"));
        assert!(!manifest.permits("http.request"));
        assert!(!manifest.permits("storage"));
        assert!(!manifest.permits("native.execute"));
        assert!(!manifest.permits("conversations.cancel"));
        assert!(!manifest.permits("instance.status"));
    }

    #[test]
    fn sockets_need_hosts_and_wildcards_cover_subdomains_only() {
        let manifest: Manifest = serde_json::from_value(json!({
            "id": "chat", "name": "Chat", "version": "1", "apiVersion": 2,
            "capabilities": ["socket", "inbound"],
            "hosts": ["*.chat.example", "api.example.com"]
        }))
        .unwrap();
        manifest.validate().unwrap();
        assert!(manifest.permits("socket.send") && manifest.permits("inbound.accept"));
        assert!(manifest.permits("instance.status") && manifest.needs_worker());
        assert!(manifest.allows_host("gateway.chat.example"));
        assert!(manifest.allows_host("a.b.chat.example"));
        assert!(!manifest.allows_host("chat.example"));
        assert!(!manifest.allows_host("evilchat.example"));
        assert!(manifest.allows_host("api.example.com"));
        assert!(!manifest.allows_host("x.api.example.com"));
        let mut hostless = serde_json::to_value(&manifest).unwrap();
        hostless["hosts"] = json!([]);
        let hostless: Manifest = serde_json::from_value(hostless).unwrap();
        assert!(hostless.validate().is_err());
        for host in ["*", "*.com.", "**.example.com", "*example.com"] {
            let mut invalid = serde_json::to_value(&manifest).unwrap();
            invalid["hosts"] = json!([host]);
            let invalid: Manifest = serde_json::from_value(invalid).unwrap();
            assert!(invalid.validate().is_err(), "{host}");
        }
    }
}
