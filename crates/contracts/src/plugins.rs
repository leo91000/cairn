//! Cairn's instance envelope. Only manifest-defined values remain extensible.
use crate::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
#[schemars(inline)]
pub struct PluginInstanceId(pub Uuid);

impl std::fmt::Display for PluginInstanceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum PluginAccessMode {
    #[default]
    Allowlist,
    Requests,
    Everyone,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    NeedsReview,
    Ready,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum PluginActivityStatus {
    #[default]
    Working,
    Replied,
    Failed,
    Stopped,
}

/// Validated against the selected package's manifest, never used for Cairn policy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
#[schemars(inline, extend("x-cairn-json-object" = true))]
pub struct PluginValues(
    #[ts(type = "Record<string, any>")] pub BTreeMap<String, serde_json::Value>,
);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginLabel {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginStatus {
    #[serde(default)]
    #[schemars(
        with = "String",
        pattern("^(disconnected|connecting|connected|needs_review|ready)$")
    )]
    pub state: PluginState,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginUsage {
    #[serde(default)]
    pub hour: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginRequest {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mentions: u32,
    #[serde(default, deserialize_with = "wire_integer")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginActivity {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub chat_id: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(working|replied|failed|stopped)$"))]
    pub status: PluginActivityStatus,
    #[serde(default, deserialize_with = "wire_integer")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub created_at: i64,
}

/// Explicit owner/agent view: no credential, approved policy or native provenance.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginConnection {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub plugin_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub revision: String,
    pub settings: PluginValues,
    #[serde(default)]
    pub secret_keys: Vec<String>,
    #[serde(default)]
    pub agent_ids: Vec<String>,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(allowlist|requests|everyone)$"))]
    pub access_mode: PluginAccessMode,
    #[serde(default)]
    pub user_ids: Vec<String>,
    #[serde(default)]
    pub people: Vec<PluginLabel>,
    #[serde(default = "messages")]
    #[schemars(range(min = 1, max = 100))]
    pub max_messages_per_hour: u32,
    #[serde(default = "per_person")]
    #[schemars(range(min = 1, max = 50))]
    pub max_per_person_per_hour: u32,
    pub status: PluginStatus,
    pub usage: PluginUsage,
    #[serde(default)]
    pub requests: Vec<PluginRequest>,
    #[serde(default)]
    pub activity: Vec<PluginActivity>,
}

fn messages() -> u32 {
    20
}

fn per_person() -> u32 {
    5
}

/// Full Cairn configuration, with manifest settings and secret fields separate.
/// Missing agentIds retains grants; agents are forbidden from submitting it at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginConfiguration {
    #[schemars(length(min = 1, max = 100))]
    pub name: String,
    pub enabled: bool,
    pub settings: PluginValues,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<PluginValues>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 16384))]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 200))]
    pub credential_flow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_from: Option<PluginInstanceId>,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(allowlist|requests|everyone)$"))]
    pub access_mode: PluginAccessMode,
    #[serde(default)]
    #[schemars(length(max = 100), inner(length(min = 1, max = 64)))]
    pub user_ids: Vec<String>,
    #[serde(default)]
    #[schemars(length(max = 100))]
    pub people: Vec<PluginLabel>,
    #[serde(default = "messages")]
    #[schemars(range(min = 1, max = 100))]
    pub max_messages_per_hour: u32,
    #[serde(default = "per_person")]
    #[schemars(range(min = 1, max = 50))]
    pub max_per_person_per_hour: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<Vec<Uuid>>", length(max = 100))]
    pub agent_ids: Option<Vec<String>>,
    /// Required on replacement. Creation has no existing revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginFormInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<PluginInstanceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub settings: PluginValues,
    pub secrets: PluginValues,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_flow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_from: Option<PluginInstanceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginActionInput {
    #[schemars(length(min = 1, max = 40))]
    pub action: String,
    #[serde(default)]
    #[ts(type = "any")]
    pub payload: Option<serde_json::Value>,
    pub values: PluginValues,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginEnabledInput {
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginInstanceInput {
    pub plugin_id: String,
    pub instance_id: PluginInstanceId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ManagePluginInput {
    Setup {
        plugin_id: String,
    },
    Form {
        plugin_id: String,
        #[serde(default)]
        instance_id: Option<PluginInstanceId>,
        data: PluginFormInput,
    },
    Save {
        plugin_id: String,
        #[serde(default)]
        instance_id: Option<PluginInstanceId>,
        data: PluginConfiguration,
    },
    Enabled {
        plugin_id: String,
        instance_id: PluginInstanceId,
        data: PluginEnabledInput,
    },
    Delete {
        plugin_id: String,
        instance_id: PluginInstanceId,
    },
    Disconnect {
        plugin_id: String,
        instance_id: PluginInstanceId,
    },
    Test {
        plugin_id: String,
        instance_id: PluginInstanceId,
    },
    View {
        plugin_id: String,
        instance_id: PluginInstanceId,
    },
    Action {
        plugin_id: String,
        instance_id: PluginInstanceId,
        data: PluginActionInput,
    },
    Signin {
        plugin_id: String,
        #[serde(default)]
        instance_id: Option<PluginInstanceId>,
        data: PluginFormInput,
    },
    SigninStatus {
        plugin_id: String,
        flow_id: String,
    },
    SigninCancel {
        plugin_id: String,
        flow_id: String,
    },
    People {
        plugin_id: String,
        instance_id: PluginInstanceId,
        user_id: String,
        #[schemars(with = "String", pattern("^(approve|deny|remove)$"))]
        operation: PluginPersonAction,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListPluginInstancesOutput {
    pub connections: Vec<PluginConnection>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginOption {
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginFormResult {
    pub options: BTreeMap<String, Vec<PluginOption>>,
    pub values: PluginValues,
    pub errors: BTreeMap<String, String>,
    #[serde(default)]
    #[ts(type = "Array<any>")]
    pub view: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginSigninFlow {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub plugin_id: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub user_code: String,
    #[serde(default)]
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default)]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub expires_at: i64,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ManagePluginOutput {
    Setup {
        manifest: Box<PluginManifest>,
        setup: PluginSetup,
        callback_url: Option<String>,
        management_url: Option<String>,
    },
    Form {
        form: PluginFormResult,
    },
    Saved {
        instance: PluginConnection,
    },
    Enabled {
        instance: PluginConnection,
    },
    Deleted {
        deleted: bool,
    },
    Disconnected {
        disconnected: bool,
        revoked: Option<bool>,
        message: String,
    },
    Tested {
        ok: bool,
        message: String,
    },
    View {
        #[ts(type = "any")]
        view: serde_json::Value,
    },
    /// The package-defined view/action result has already passed its manifest validator.
    Action {
        #[ts(type = "any")]
        result: serde_json::Value,
    },
    Signin {
        flow: PluginSigninFlow,
    },
    SigninCancelled {
        cancelled: bool,
    },
    People {
        instance: PluginConnection,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum PluginPersonAction {
    Approve,
    Deny,
    Remove,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_commands_round_trip_the_shared_corpus() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/plugin-contracts.json"
        ))
        .unwrap();
        for input in corpus["inputs"].as_array().unwrap() {
            let command: ManagePluginInput = serde_json::from_value(input.clone()).unwrap();
            let encoded = serde_json::to_value(&command).unwrap();
            assert_eq!(command, serde_json::from_value(encoded).unwrap());
        }
        for input in corpus["invalidInputs"].as_array().unwrap() {
            assert!(
                serde_json::from_value::<ManagePluginInput>(input.clone()).is_err(),
                "{input}"
            );
        }
    }
}
