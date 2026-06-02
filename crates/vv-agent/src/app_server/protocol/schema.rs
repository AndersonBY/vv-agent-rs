use std::collections::BTreeMap;
use std::fmt;

use schemars::{schema_for, JsonSchema};
use ts_rs::TS;

use super::{
    AppItem, AppThread, AppTurn, ApprovalRequestParams, ApprovalResolveParams, ClientRequest,
    InitializeParams, InitializeResponse, JsonRpcMessage, ServerNotification, ServerRequest,
    ThreadReadResponse, ThreadResumeResponse, ThreadStartResponse, TurnStartResponse,
};

pub type SchemaBundle = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerSchemaError {
    message: String,
}

impl AppServerSchemaError {
    fn json(error: serde_json::Error) -> Self {
        Self {
            message: error.to_string(),
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
    let mut bundle = SchemaBundle::new();
    insert_json_schema::<ClientRequest>(&mut bundle, "ClientRequest")?;
    insert_json_schema::<ServerNotification>(&mut bundle, "ServerNotification")?;
    insert_json_schema::<ServerRequest>(&mut bundle, "ServerRequest")?;
    insert_json_schema::<JsonRpcMessage>(&mut bundle, "JsonRpcMessage")?;
    insert_json_schema::<InitializeParams>(&mut bundle, "InitializeParams")?;
    insert_json_schema::<InitializeResponse>(&mut bundle, "InitializeResponse")?;
    insert_json_schema::<ThreadStartResponse>(&mut bundle, "ThreadStartResponse")?;
    insert_json_schema::<ThreadReadResponse>(&mut bundle, "ThreadReadResponse")?;
    insert_json_schema::<ThreadResumeResponse>(&mut bundle, "ThreadResumeResponse")?;
    insert_json_schema::<TurnStartResponse>(&mut bundle, "TurnStartResponse")?;
    insert_json_schema::<AppThread>(&mut bundle, "AppThread")?;
    insert_json_schema::<AppTurn>(&mut bundle, "AppTurn")?;
    insert_json_schema::<AppItem>(&mut bundle, "AppItem")?;
    insert_json_schema::<ApprovalRequestParams>(&mut bundle, "ApprovalRequestParams")?;
    insert_json_schema::<ApprovalResolveParams>(&mut bundle, "ApprovalResolveParams")?;
    Ok(bundle)
}

pub fn generate_app_server_typescript_bundle() -> Result<SchemaBundle, AppServerSchemaError> {
    let mut bundle = SchemaBundle::new();
    insert_typescript::<ClientRequest>(&mut bundle, "ClientRequest.ts");
    insert_typescript::<ServerNotification>(&mut bundle, "ServerNotification.ts");
    insert_typescript::<ServerRequest>(&mut bundle, "ServerRequest.ts");
    insert_typescript::<InitializeParams>(&mut bundle, "InitializeParams.ts");
    insert_typescript::<InitializeResponse>(&mut bundle, "InitializeResponse.ts");
    insert_typescript::<ThreadStartResponse>(&mut bundle, "ThreadStartResponse.ts");
    insert_typescript::<ThreadReadResponse>(&mut bundle, "ThreadReadResponse.ts");
    insert_typescript::<ThreadResumeResponse>(&mut bundle, "ThreadResumeResponse.ts");
    insert_typescript::<TurnStartResponse>(&mut bundle, "TurnStartResponse.ts");
    insert_typescript::<AppThread>(&mut bundle, "AppThread.ts");
    insert_typescript::<AppTurn>(&mut bundle, "AppTurn.ts");
    insert_typescript::<AppItem>(&mut bundle, "AppItem.ts");
    insert_typescript::<ApprovalRequestParams>(&mut bundle, "ApprovalRequestParams.ts");
    insert_typescript::<ApprovalResolveParams>(&mut bundle, "ApprovalResolveParams.ts");
    Ok(bundle)
}

fn insert_json_schema<T: JsonSchema>(
    bundle: &mut SchemaBundle,
    name: &'static str,
) -> Result<(), AppServerSchemaError> {
    let schema = schema_for!(T);
    let value = serde_json::to_string_pretty(&schema).map_err(AppServerSchemaError::json)?;
    bundle.insert(name.to_string(), value);
    Ok(())
}

fn insert_typescript<T: TS>(bundle: &mut SchemaBundle, file_name: &'static str) {
    bundle.insert(file_name.to_string(), format!("export {};\n", T::decl()));
}
