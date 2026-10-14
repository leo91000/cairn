//! Installation plugin contracts, independent of storage and agent engines.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const API_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 1_048_576;
pub const MAX_SOURCE_BYTES: usize = 262_144;

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
        "transport.discord",
        "Receive mentions of a Discord bot through Cairn's Gateway connection",
    ),
    (
        "onepassword.read",
        "Read 1Password items for agents you have granted access",
    ),
];

/// Capabilities only a package shipped inside Cairn may hold.
pub const BUILTIN_ONLY: &[&str] = &["onepassword.read"];

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
        "onepassword.read" => "onepassword.read",
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
    /// Exact hostnames reachable through the `http` capability, always over HTTPS.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<Credential>,
    /// Connection fields Cairn renders, in order: `[{ key, title, type, ... }]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<Value>,
}

fn slug(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn hostname(value: &str) -> bool {
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

impl Manifest {
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
        if self.hosts.is_empty() == self.permits_capability("http") {
            return Err("Plugin manifest must list hosts exactly when it requests http.");
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
        if self.settings.len() > 20 {
            return Err("Plugin manifest declares too many settings.");
        }
        let mut keys = Vec::new();
        for field in &self.settings {
            let key = field["key"].as_str().unwrap_or("");
            let valid_key = !key.is_empty()
                && key.len() <= 40
                && key.bytes().all(|c| c.is_ascii_alphanumeric())
                && !keys.contains(&key);
            keys.push(key);
            let valid_type = matches!(
                field["type"].as_str(),
                Some("string" | "number" | "boolean")
            );
            let valid_title = field["title"]
                .as_str()
                .is_some_and(|title| !title.is_empty() && title.chars().count() <= 60);
            if !valid_key || !valid_type || !valid_title {
                return Err("Plugin manifest has an invalid setting.");
            }
        }
        Ok(())
    }

    pub fn permits_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|item| item == capability)
    }

    /// A host method is allowed only through a capability the manifest requests.
    pub fn permits(&self, method: &str) -> bool {
        capability(method).is_some_and(|capability| self.permits_capability(capability))
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
        source: String,
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
    fn manifests_declare_known_capabilities_and_exact_https_hosts() {
        let base = json!({
            "id": "example",
            "name": "Example",
            "version": "1.0.0",
            "apiVersion": 1,
            "capabilities": ["http", "storage"],
            "hosts": ["api.example.com"],
            "credential": { "label": "API key", "header": "authorization", "prefix": "Bearer " },
            "settings": [{ "key": "team", "title": "Team", "type": "string" }]
        });
        assert!(manifest(base.clone()).is_ok());
        for (key, value) in [
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
            "id": "example", "name": "Example", "version": "1", "apiVersion": 1,
            "capabilities": ["storage"]
        }))
        .unwrap();
        assert!(manifest.permits("storage.set"));
        assert!(!manifest.permits("http.request"));
        assert!(!manifest.permits("storage"));
        assert!(!manifest.permits("native.execute"));
        assert!(!manifest.permits("conversations.cancel"));
    }
}
