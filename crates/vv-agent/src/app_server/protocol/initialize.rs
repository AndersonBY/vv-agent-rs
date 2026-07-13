use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: AppClientInfo,
    #[serde(default)]
    pub capabilities: AppClientCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AppClientInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AppClientCapabilities {
    #[serde(default)]
    pub experimental_api: bool,
    #[serde(default)]
    pub opt_out_notification_methods: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub user_agent: String,
    pub protocol_version: String,
    pub capabilities: AppServerCapabilities,
}

impl InitializeResponse {
    pub fn new(capabilities: AppServerCapabilities) -> Self {
        Self {
            user_agent: "vv-agent-app-server".to_string(),
            protocol_version: "v1".to_string(),
            capabilities,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AppServerCapabilities {
    pub model_list: bool,
    pub thread_lifecycle: bool,
    pub notification_opt_out: bool,
    pub schema_export: bool,
    pub approval_resolve: bool,
}

impl AppServerCapabilities {
    pub fn for_runtime(runtime_configured: bool) -> Self {
        Self {
            model_list: true,
            thread_lifecycle: runtime_configured,
            notification_opt_out: true,
            schema_export: true,
            approval_resolve: true,
        }
    }
}
