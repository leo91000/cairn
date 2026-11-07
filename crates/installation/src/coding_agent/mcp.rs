//! Authorized MCP servers before conversion into a native CLI's configuration.
use std::collections::BTreeMap;

pub enum Server {
    Http {
        url: String,
        timeout: Option<u64>,
        enabled_tools: Option<Vec<String>>,
    },
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        enabled_tools: Option<Vec<String>>,
        tools: Vec<String>,
    },
}

pub struct Configuration {
    pub servers: BTreeMap<String, Server>,
    pub token: String,
    /// A plan turn: tools are already restricted to read-only ones. Engines that
    /// cannot allowlist command-server tools must reject any other tool name.
    pub planning: bool,
}

impl Configuration {
    pub fn new(token: String) -> Self {
        Self {
            servers: BTreeMap::new(),
            token,
            planning: false,
        }
    }

    pub fn bearer_headers(&self) -> BTreeMap<&'static str, String> {
        BTreeMap::from([("Authorization", format!("Bearer {}", self.token))])
    }
}
