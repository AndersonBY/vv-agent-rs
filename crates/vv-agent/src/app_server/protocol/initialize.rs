use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: AppClientInfo,
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
    pub server_info: AppServerInfo,
    pub protocol_version: String,
    pub supported_transports: Vec<String>,
    pub capabilities: AppServerCapabilities,
}

impl InitializeResponse {
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        capabilities: AppServerCapabilities,
    ) -> Self {
        Self {
            server_info: AppServerInfo {
                name: name.into(),
                version: version.into(),
            },
            protocol_version: "2026-06-02".to_string(),
            supported_transports: vec!["stdio".to_string()],
            capabilities,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AppServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AppServerCapabilities {
    pub thread: bool,
    pub turn: bool,
    pub item_stream: bool,
    pub approval_requests: bool,
    pub event_replay: bool,
    pub schema_export: bool,
}

impl AppServerCapabilities {
    pub fn mvp() -> Self {
        Self {
            thread: true,
            turn: true,
            item_stream: true,
            approval_requests: true,
            event_replay: true,
            schema_export: true,
        }
    }
}
