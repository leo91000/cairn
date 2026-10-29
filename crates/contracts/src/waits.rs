//! Durable continuation state. Plugin-owned input/result values remain opaque at that boundary.
use crate::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
#[schemars(with = "BTreeMap<String, serde_json::Value>", extend("x-cairn-json-object" = true))]
#[ts(type = "Record<string, unknown>")]
pub struct PluginWaitData(pub BTreeMap<String, serde_json::Value>);

macro_rules! condition {
    ($name:ident, $($extra:tt)*) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
        #[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
        pub enum $name {
            RunFinished {run_id:String},
            TaskFinished {chat_id:String},
            Plugin {plugin_id:String,condition_type:String,
                #[serde(default,skip_serializing_if="Option::is_none")]
                #[ts(optional)]
                project_id:Option<String>,input:PluginWaitData},
            $($extra)*
        }
    };
}

condition!(WaitExternalCondition,);

condition!(WaitCondition,Group {
    mode:String,
    #[schemars(with="Vec<WaitExternalCondition>")]
    #[ts(type="Array<import('./WaitExternalCondition').WaitExternalCondition>")]
    conditions:Vec<WaitCondition>
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TimeCondition {
    Delay { seconds: i64 },
    At { date: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct WaitDisplay {
    pub title: String,
    pub short: String,
    pub details: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct WaitResultDisplay {
    pub tone: String,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct WaitObservationMember {
    pub condition: WaitCondition,
    #[schemars(with = "Option<WaitResult>")]
    #[ts(type = "import('./WaitResult').WaitResult | null")]
    pub result: Option<Box<WaitObservation>>,
}

macro_rules! observation {
    ($name:ident, $($extra:tt)*) => {
        #[derive(Debug,Clone,Default,PartialEq,Serialize,Deserialize,JsonSchema,TS)]
        #[serde(rename_all="camelCase")]
        pub struct $name {

    pub state: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub outcome_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub waiting_on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub finished_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub paused: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(with="Option<WaitExternalCondition>")]
    #[ts(type="WaitExternalCondition")]
    pub condition: Option<WaitCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub wake_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display: Option<WaitResultDisplay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin_result: Option<PluginWaitData>,            $($extra)*
        }
    };
}

observation!(WaitResult,);

observation!(WaitObservation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub members: Option<Vec<WaitObservationMember>>,
);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct WaitMemberProgress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub observation: Option<WaitObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display: Option<WaitDisplay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub next_check_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatWakeup {
    pub id: String,
    pub message_id: Option<String>,
    pub reason: String,
    #[ts(type = "number")]
    pub seconds: i64,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub wake_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub condition: Option<WaitCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub time_condition: Option<TimeCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub members: Option<Vec<WaitMemberProgress>>,
    #[serde(flatten)]
    pub progress: WaitMemberProgress,
}
