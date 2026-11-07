use crate::*;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Http,
    Stdio,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    #[default]
    None,
    Bearer,
    #[serde(rename = "oauth")]
    OAuth,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionState {
    #[default]
    Untested,
    Connected,
    NeedsAuth,
    Error,
}

/// Current connection state. Tool descriptors are third-party protocol JSON.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(default, rename_all = "camelCase")]
pub struct McpServer {
    pub id: String,
    pub name: String,
    #[schemars(with = "String", pattern("^(http|stdio)$"))]
    pub transport: Transport,
    pub url: String,
    pub command: String,
    pub args: Vec<String>,
    #[schemars(with = "String", pattern("^(none|bearer|oauth)$"))]
    pub auth: AuthKind,
    pub client_id: String,
    pub scopes: String,
    pub allow_private_network: bool,
    pub enabled: bool,
    pub enabled_tools: Option<Vec<String>>,
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub created_at: i64,
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub revision: u64,
    #[schemars(with = "String", pattern("^(untested|connected|needs-auth|error)$"))]
    pub state: ConnectionState,
    /// Tool catalog as returned by the server.
    #[schemars(with = "Vec<McpTool>")]
    #[ts(as = "Vec<McpTool>")]
    pub tools: Vec<Value>,
    #[ts(type = "number | null")]
    pub checked_at: Option<i64>,
    pub error: String,
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

/// Public projection of a third-party tool. Schemas remain opaque JSON.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    #[ts(type = "unknown")]
    pub input_schema: Value,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(default, rename_all = "camelCase")]
pub struct McpView {
    #[serde(flatten)]
    #[ts(flatten)]
    pub server: McpServer,
    pub has_token: bool,
    pub has_client_secret: bool,
    pub env_keys: Vec<String>,
    pub callback_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
pub struct McpEnvironment(
    #[schemars(extend(
        "propertyNames" = schemars::json_schema!({"type":"string","pattern":"^[A-Za-z_]\\w*$"}),
        "additionalProperties" = schemars::json_schema!({"type":"string","maxLength":10000})
    ))]
    pub BTreeMap<String, String>,
);

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpInput {
    #[schemars(length(min = 1, max = 100))]
    pub name: String,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(http|stdio)$"))]
    pub transport: Transport,
    #[serde(default)]
    #[schemars(length(max = 2000))]
    pub url: String,
    #[serde(default)]
    #[schemars(length(max = 1000))]
    pub command: String,
    #[serde(default)]
    #[schemars(length(max = 100), inner(length(max = 4000)))]
    pub args: Vec<String>,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(none|bearer|oauth)$"))]
    pub auth: AuthKind,
    #[serde(default)]
    #[schemars(length(max = 500))]
    pub client_id: String,
    #[serde(default)]
    #[schemars(length(max = 2000))]
    pub scopes: String,
    #[serde(default)]
    pub allow_private_network: bool,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    #[schemars(length(max = 500), inner(length(min = 1, max = 200)))]
    pub enabled_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 10000))]
    #[ts(optional)]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 10000))]
    #[ts(optional)]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub env: Option<McpEnvironment>,
    #[serde(default)]
    #[schemars(length(max = 100))]
    pub remove_env: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpReplacementInput {
    #[schemars(length(min = 1, max = 100))]
    pub name: String,
    #[schemars(with = "String", pattern("^(http|stdio)$"))]
    pub transport: Transport,
    #[schemars(length(max = 2000))]
    pub url: String,
    #[schemars(length(max = 1000))]
    pub command: String,
    #[schemars(length(max = 100), inner(length(max = 4000)))]
    pub args: Vec<String>,
    #[schemars(with = "String", pattern("^(none|bearer|oauth)$"))]
    pub auth: AuthKind,
    #[schemars(length(max = 500))]
    pub client_id: String,
    #[schemars(length(max = 2000))]
    pub scopes: String,
    pub allow_private_network: bool,
    pub enabled: bool,
    #[schemars(length(max = 500), inner(length(min = 1, max = 200)))]
    #[serde(deserialize_with = "explicit_tools")]
    #[schemars(required, extend("type" = ["array", "null"]))]
    pub enabled_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 10000))]
    #[ts(optional)]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 10000))]
    #[ts(optional)]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub env: Option<McpEnvironment>,
    #[schemars(length(max = 100))]
    #[serde(default)]
    pub remove_env: Vec<String>,
}

fn explicit_tools<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error> {
    Option::<Vec<String>>::deserialize(deserializer)
}

impl From<McpReplacementInput> for McpInput {
    fn from(input: McpReplacementInput) -> Self {
        Self {
            name: input.name,
            transport: input.transport,
            url: input.url,
            command: input.command,
            args: input.args,
            auth: input.auth,
            client_id: input.client_id,
            scopes: input.scopes,
            allow_private_network: input.allow_private_network,
            enabled: input.enabled,
            enabled_tools: input.enabled_tools,
            token: input.token,
            client_secret: input.client_secret,
            env: input.env,
            remove_env: input.remove_env,
        }
    }
}

fn enabled() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpIdInput {
    #[serde(deserialize_with = "mcp_id")]
    #[schemars(with = "Uuid")]
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateMcpInput {
    #[serde(deserialize_with = "mcp_id")]
    #[schemars(with = "Uuid")]
    pub id: String,
    #[serde(deserialize_with = "mcp_revision")]
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub revision: u64,
    pub connection: McpReplacementInput,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListMcpsInput {}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ListMcpsOutput {
    pub mcps: Vec<McpView>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpMutationOutput {
    pub completed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BeginMcpInput {
    #[serde(deserialize_with = "mcp_id")]
    #[schemars(with = "Uuid")]
    pub id: String,
    #[serde(default)]
    pub native: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct BeginMcpOutput {
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpConnectionStatus {
    pub pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub result: Option<String>,
}

fn mcp_id<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let id = String::deserialize(deserializer)?;
    Uuid::parse_str(&id).map_err(serde::de::Error::custom)?;
    Ok(id)
}

fn mcp_revision<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let revision = u64::deserialize(deserializer)?;
    if revision > 9007199254740991 {
        return Err(serde::de::Error::custom(
            "Revision exceeds the wire integer range.",
        ));
    }
    Ok(revision)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_contract_defaults_and_secrets_are_separate_from_views() {
        let corpus: Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/mcp-contracts.json"))
                .unwrap();
        for value in corpus["creates"].as_array().unwrap() {
            let input: McpInput = serde_json::from_value(value.clone()).unwrap();
            let wire = serde_json::to_value(&input).unwrap();
            assert_eq!(
                serde_json::to_value(serde_json::from_value::<McpInput>(wire.clone()).unwrap())
                    .unwrap(),
                wire
            );
        }
        for value in corpus["creates"].as_array().unwrap() {
            let settings =
                serde_json::to_value(serde_json::from_value::<McpInput>(value.clone()).unwrap())
                    .unwrap();
            let command = serde_json::json!({"id":"00000000-0000-4000-8000-000000000001","revision":1,"connection":settings});
            assert!(serde_json::from_value::<UpdateMcpInput>(command.clone()).is_ok());
            for field in [
                "name",
                "transport",
                "url",
                "command",
                "args",
                "auth",
                "clientId",
                "scopes",
                "allowPrivateNetwork",
                "enabled",
                "enabledTools",
            ] {
                let mut invalid = command.clone();
                invalid["connection"].as_object_mut().unwrap().remove(field);
                assert!(
                    serde_json::from_value::<UpdateMcpInput>(invalid).is_err(),
                    "{field}"
                );
            }
        }
        for value in corpus["invalidCreates"].as_array().unwrap() {
            assert!(
                serde_json::from_value::<McpInput>(value.clone()).is_err(),
                "{value}"
            );
        }
        let view = serde_json::to_value(McpView::default()).unwrap();
        for key in [
            "token",
            "clientSecret",
            "env",
            "tokens",
            "verifier",
            "discovery",
        ] {
            assert!(view.get(key).is_none());
        }
    }
}
