use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub type SchemaBundle = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SchemaExportResponse {
    pub json_schema: SchemaBundle,
    pub typescript: SchemaBundle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerSchemaError {
    message: String,
}

impl AppServerSchemaError {
    fn committed_bundle(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AppServerSchemaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AppServerSchemaError {}

pub fn generate_app_server_json_schema_bundle() -> Result<SchemaBundle, AppServerSchemaError> {
    committed_bundle(JSON_SCHEMA_FILES)
}

pub fn generate_app_server_typescript_bundle() -> Result<SchemaBundle, AppServerSchemaError> {
    committed_bundle(TYPESCRIPT_SCHEMA_FILES)
}

fn committed_bundle<const N: usize>(
    files: [(&'static str, &'static str); N],
) -> Result<SchemaBundle, AppServerSchemaError> {
    let bundle = files
        .into_iter()
        .map(|(name, source)| (name.to_string(), source.to_string()))
        .collect::<SchemaBundle>();
    if bundle.len() != N {
        return Err(AppServerSchemaError::committed_bundle(
            "duplicate committed App Server schema name",
        ));
    }
    Ok(bundle)
}

const JSON_SCHEMA_FILES: [(&str, &str); 17] = [
    (
        "AppItem",
        include_str!("../../../schema/app-server/json/AppItem.json"),
    ),
    (
        "AppThread",
        include_str!("../../../schema/app-server/json/AppThread.json"),
    ),
    (
        "AppTurn",
        include_str!("../../../schema/app-server/json/AppTurn.json"),
    ),
    (
        "ApprovalDecision",
        include_str!("../../../schema/app-server/json/ApprovalDecision.json"),
    ),
    (
        "ApprovalRequestParams",
        include_str!("../../../schema/app-server/json/ApprovalRequestParams.json"),
    ),
    (
        "ApprovalResolveParams",
        include_str!("../../../schema/app-server/json/ApprovalResolveParams.json"),
    ),
    (
        "ClientRequest",
        include_str!("../../../schema/app-server/json/ClientRequest.json"),
    ),
    (
        "InitializeParams",
        include_str!("../../../schema/app-server/json/InitializeParams.json"),
    ),
    (
        "InitializeResponse",
        include_str!("../../../schema/app-server/json/InitializeResponse.json"),
    ),
    (
        "JsonRpcMessage",
        include_str!("../../../schema/app-server/json/JsonRpcMessage.json"),
    ),
    (
        "SchemaExportResponse",
        include_str!("../../../schema/app-server/json/SchemaExportResponse.json"),
    ),
    (
        "ServerNotification",
        include_str!("../../../schema/app-server/json/ServerNotification.json"),
    ),
    (
        "ServerRequest",
        include_str!("../../../schema/app-server/json/ServerRequest.json"),
    ),
    (
        "ThreadReadResponse",
        include_str!("../../../schema/app-server/json/ThreadReadResponse.json"),
    ),
    (
        "ThreadResumeResponse",
        include_str!("../../../schema/app-server/json/ThreadResumeResponse.json"),
    ),
    (
        "ThreadStartResponse",
        include_str!("../../../schema/app-server/json/ThreadStartResponse.json"),
    ),
    (
        "TurnStartResponse",
        include_str!("../../../schema/app-server/json/TurnStartResponse.json"),
    ),
];

const TYPESCRIPT_SCHEMA_FILES: [(&str, &str); 16] = [
    (
        "AppItem.ts",
        include_str!("../../../schema/app-server/typescript/AppItem.ts"),
    ),
    (
        "AppThread.ts",
        include_str!("../../../schema/app-server/typescript/AppThread.ts"),
    ),
    (
        "AppTurn.ts",
        include_str!("../../../schema/app-server/typescript/AppTurn.ts"),
    ),
    (
        "ApprovalDecision.ts",
        include_str!("../../../schema/app-server/typescript/ApprovalDecision.ts"),
    ),
    (
        "ApprovalRequestParams.ts",
        include_str!("../../../schema/app-server/typescript/ApprovalRequestParams.ts"),
    ),
    (
        "ApprovalResolveParams.ts",
        include_str!("../../../schema/app-server/typescript/ApprovalResolveParams.ts"),
    ),
    (
        "ClientRequest.ts",
        include_str!("../../../schema/app-server/typescript/ClientRequest.ts"),
    ),
    (
        "InitializeParams.ts",
        include_str!("../../../schema/app-server/typescript/InitializeParams.ts"),
    ),
    (
        "InitializeResponse.ts",
        include_str!("../../../schema/app-server/typescript/InitializeResponse.ts"),
    ),
    (
        "SchemaExportResponse.ts",
        include_str!("../../../schema/app-server/typescript/SchemaExportResponse.ts"),
    ),
    (
        "ServerNotification.ts",
        include_str!("../../../schema/app-server/typescript/ServerNotification.ts"),
    ),
    (
        "ServerRequest.ts",
        include_str!("../../../schema/app-server/typescript/ServerRequest.ts"),
    ),
    (
        "ThreadReadResponse.ts",
        include_str!("../../../schema/app-server/typescript/ThreadReadResponse.ts"),
    ),
    (
        "ThreadResumeResponse.ts",
        include_str!("../../../schema/app-server/typescript/ThreadResumeResponse.ts"),
    ),
    (
        "ThreadStartResponse.ts",
        include_str!("../../../schema/app-server/typescript/ThreadStartResponse.ts"),
    ),
    (
        "TurnStartResponse.ts",
        include_str!("../../../schema/app-server/typescript/TurnStartResponse.ts"),
    ),
];
