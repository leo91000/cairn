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
}

impl Configuration {
    pub fn new(token: String) -> Self {
        Self {
            servers: BTreeMap::new(),
            token,
        }
    }

    pub fn bearer_headers(&self) -> BTreeMap<&'static str, String> {
        BTreeMap::from([("Authorization", format!("Bearer {}", self.token))])
    }
}
