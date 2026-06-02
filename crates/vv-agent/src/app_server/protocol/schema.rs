use std::collections::BTreeMap;
use std::fmt;

use schemars::{schema_for, JsonSchema};
use ts_rs::TS;

use super::{ClientRequest, JsonRpcMessage, ServerNotification, ServerRequest};

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
    Ok(bundle)
}

pub fn generate_app_server_typescript_bundle() -> Result<SchemaBundle, AppServerSchemaError> {
    let mut bundle = SchemaBundle::new();
    insert_typescript::<ClientRequest>(&mut bundle, "ClientRequest.ts");
    insert_typescript::<ServerNotification>(&mut bundle, "ServerNotification.ts");
    insert_typescript::<ServerRequest>(&mut bundle, "ServerRequest.ts");
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
