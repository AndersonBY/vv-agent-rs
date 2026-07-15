use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

pub type UserInput = Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: String,
    #[serde(default, deserialize_with = "deserialize_input_items")]
    pub input: Vec<UserInput>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams {
    pub thread_id: String,
    #[serde(default)]
    pub expected_turn_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerParams {
    pub thread_id: String,
    #[serde(default)]
    pub expected_turn_id: String,
    #[serde(default, deserialize_with = "deserialize_input_items")]
    pub input: Vec<UserInput>,
}

pub type TurnFollowUpParams = TurnSteerParams;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResponse {
    pub thread_id: String,
    pub turn_id: String,
    pub status: TurnStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptResponse {
    pub thread_id: String,
    pub turn_id: String,
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnControlResponse {
    pub thread_id: String,
    pub turn_id: String,
    pub queued: bool,
}

pub type TurnSteerResponse = TurnControlResponse;
pub type TurnFollowUpResponse = TurnControlResponse;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartedParams {
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnCompletedParams {
    pub thread_id: String,
    pub turn_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub status: TurnStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<AppTokenUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AppTurn {
    pub turn_id: String,
    pub thread_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    pub status: TurnStatus,
    pub started_at: f64,
    #[serde(default)]
    pub completed_at: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_input_items")]
    pub input: Vec<UserInput>,
    #[serde(default)]
    pub result: BTreeMap<String, Value>,
}

fn deserialize_input_items<'de, D>(deserializer: D) -> Result<Vec<UserInput>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let items = Vec::<Value>::deserialize(deserializer)?;
    if items.iter().all(Value::is_object) {
        Ok(items)
    } else {
        Err(serde::de::Error::custom("input must be a list of objects"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AppTokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_usage: AppCacheUsage,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AppCacheUsage {
    pub status: String,
    pub read_tokens: Option<u64>,
    pub write_tokens: Option<u64>,
    pub uncached_input_tokens: Option<u64>,
    pub source: Option<String>,
}
