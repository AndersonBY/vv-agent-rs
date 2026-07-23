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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TurnResumeParams {
    pub thread_id: String,
    pub turn_id: String,
    pub checkpoint_key: String,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TurnResumeResponse {
    pub thread_id: String,
    pub turn_id: String,
    pub run_id: String,
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
    pub checkpoint: Option<CheckpointSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interruption: Option<InterruptionSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TurnStatus>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usage: Option<BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_exhaustion: Option<BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interruption: Option<InterruptionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckpointSummary {
    pub key: String,
    pub resume_attempt: u64,
    pub cycle_index: u64,
    pub status: CheckpointSummaryStatus,
    pub terminal_acknowledged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointSummaryStatus {
    Pending,
    Running,
    WaitUser,
    Completed,
    Failed,
    MaxCycles,
    ReconciliationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InterruptionSummary {
    pub reason: String,
    pub operation_id: String,
    pub operation_kind: InterruptionOperationKind,
    pub cycle_index: u64,
    pub risk: String,
    pub idempotency_support: InterruptionIdempotencySupport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum InterruptionOperationKind {
    Model,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum InterruptionIdempotencySupport {
    Supported,
    Unsupported,
    Unknown,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AppTokenUsage {
    pub schema_version: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cache_usage: AppCacheUsage,
    pub model_calls: Vec<AppModelCallUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AppModelCallUsage {
    pub call_id: String,
    pub operation_id: String,
    pub attempt: u32,
    pub operation: String,
    pub cycle_index: u32,
    pub backend: String,
    pub model: String,
    pub status: String,
    pub usage: AppModelUsage,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AppModelUsage {
    pub schema_version: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub usage_source: String,
    pub cache_usage: AppCacheUsage,
    pub provider_usage: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AppCacheUsage {
    pub status: String,
    pub read_input_tokens: Option<u64>,
    pub write_input_tokens: Option<u64>,
    pub uncached_input_tokens: Option<u64>,
    pub source: Option<String>,
}
