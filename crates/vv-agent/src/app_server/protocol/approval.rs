use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequestParams {
    pub thread_id: String,
    pub turn_id: String,
    pub request_id: String,
    pub tool_name: String,
    pub preview: String,
    pub choices: Vec<ApprovalDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResolveParams {
    pub thread_id: String,
    pub turn_id: String,
    pub request_id: String,
    pub decision: ApprovalDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecision {
    Allow,
    Deny,
}
