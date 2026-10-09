//! Persisted shapes of MCP connections, their encrypted secrets and run grants.
use crate::validation::text;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Http,
    Stdio,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    #[default]
    None,
    Bearer,
    #[serde(rename = "oauth")]
    OAuth,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionState {
    #[default]
    Untested,
    Connected,
    NeedsAuth,
    Error,
}

/// A configured MCP connection (`mcps` record). Settings come from the `mcp`
/// input schema; the remaining fields track discovery.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub transport: Transport,
    pub url: String,
    pub command: String,
    pub args: Vec<String>,
    pub auth: AuthKind,
    pub client_id: String,
    pub scopes: String,
    pub allow_private_network: bool,
    pub enabled: bool,
    pub enabled_tools: Option<Vec<String>>,
    pub created_at: i64,
    pub revision: u64,
    pub state: ConnectionState,
    /// Tool catalog as returned by the server.
    pub tools: Vec<Value>,
    pub checked_at: Option<i64>,
    pub error: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl McpServer {
    pub fn is_http(&self) -> bool {
        self.transport == Transport::Http
    }

    pub fn uses_oauth(&self) -> bool {
        self.auth == AuthKind::OAuth
    }

    /// Settings that invalidate stored credentials when they change.
    pub fn same_endpoint(&self, other: &Self) -> bool {
        self.url == other.url
            && self.transport == other.transport
            && self.auth == other.auth
            && self.client_id == other.client_id
            && self.scopes == other.scopes
    }

    pub fn record_success(&mut self, tools: Vec<Value>, checked_at: i64) {
        self.tools = tools;
        self.state = ConnectionState::Connected;
        self.error = String::new();
        self.checked_at = Some(checked_at);
    }

    pub fn reset_state(&mut self, state: ConnectionState) {
        self.state = state;
        self.error = String::new();
        self.checked_at = None;
    }
}

/// Stored OAuth discovery: the MCP resource and its authorization server.
/// Both metadata documents are third-party JSON and kept verbatim.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthDiscovery {
    pub authorization_server_url: String,
    pub resource_metadata_url: String,
    pub resource_metadata: Value,
    pub authorization_server_metadata: Value,
}

impl OAuthDiscovery {
    pub fn resource(&self) -> &str {
        text(&self.resource_metadata, "resource")
    }

    /// Whether the authorization server metadata sets this flag to `true`.
    pub fn server_flag(&self, key: &str) -> bool {
        self.authorization_server_metadata[key] == true
    }

    /// A string field of the authorization server metadata, or "" when absent.
    pub fn server(&self, key: &str) -> &str {
        text(&self.authorization_server_metadata, key)
    }
}

/// Encrypted per-connection credentials (`mcp-secret:{id}` in the vault).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSecrets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    /// Dynamic client registration response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<OAuthDiscovery>,
    /// Token endpoint response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl McpSecrets {
    pub fn token(&self) -> &str {
        self.token.as_deref().unwrap_or("")
    }

    pub fn client_secret(&self) -> &str {
        self.client_secret.as_deref().unwrap_or("")
    }

    pub fn verifier(&self) -> &str {
        self.verifier.as_deref().unwrap_or("")
    }

    pub fn access_token(&self) -> &str {
        self.tokens.as_ref().map_or("", |t| text(t, "access_token"))
    }

    pub fn refresh_token(&self) -> &str {
        self.tokens
            .as_ref()
            .map_or("", |t| text(t, "refresh_token"))
    }

    pub fn env_keys(&self) -> Vec<String> {
        self.env
            .as_ref()
            .map(|env| env.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Forgets any OAuth sign-in so a new authorization starts from scratch.
    pub fn clear_authorization(&mut self) {
        self.tokens = None;
        self.verifier = None;
        self.discovery = None;
        self.token_expires_at = None;
    }
}

/// What a run grant allows on one gateway connection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GrantScope {
    pub revision: u64,
    /// `None` allows every tool the connection exposes.
    pub tools: Option<Vec<String>>,
}

impl GrantScope {
    pub fn allows(&self, tool: &str) -> bool {
        self.tools
            .as_ref()
            .is_none_or(|tools| tools.iter().any(|t| t == tool))
    }
}

/// Run-scoped bearer grant (`mcp-grant:{digest}`) for the gateway and workspace MCPs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunGrant {
    pub run_id: String,
    pub message_id: Value,
    pub servers: BTreeMap<String, GrantScope>,
    pub workspace: bool,
}

/// An OAuth authorization in flight (`mcp-oauth:{digest(state)}`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingAuthorization {
    pub connection_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    pub session: String,
    pub nonce: String,
    #[serde(default)]
    pub native: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// Callback parameters captured for a native client, first response only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback: Option<HashMap<String, String>>,
}
