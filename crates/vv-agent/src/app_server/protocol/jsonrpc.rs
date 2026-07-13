use schemars::JsonSchema;
use serde::de::Error as _;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use ts_rs::TS;

use super::errors::AppServerErrorCode;

pub const JSON_RPC_VERSION: &str = "2.0";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Integer(i64),
    Null,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema, TS)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Notification(JsonRpcNotification),
    Response(JsonRpcResponse),
    Error(JsonRpcError),
}

#[derive(Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct JsonRpcRequest {
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct JsonRpcNotification {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct JsonRpcResponse {
    pub id: RequestId,
    pub result: Value,
}

#[derive(Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct JsonRpcError {
    pub id: RequestId,
    pub error: JsonRpcErrorBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct JsonRpcErrorBody {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRpcRequestWire {
    jsonrpc: String,
    id: RequestId,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRpcNotificationWire {
    jsonrpc: String,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRpcResponseWire {
    jsonrpc: String,
    id: RequestId,
    result: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRpcErrorWire {
    jsonrpc: String,
    id: RequestId,
    error: JsonRpcErrorBody,
}

fn validate_version<E: serde::de::Error>(version: &str) -> Result<(), E> {
    if version == JSON_RPC_VERSION {
        Ok(())
    } else {
        Err(E::custom("jsonrpc must be exactly 2.0"))
    }
}

fn non_null_id_error(id: &RequestId) -> Option<&'static str> {
    if matches!(id, RequestId::Null) {
        Some("JSON-RPC request id cannot be null")
    } else {
        None
    }
}

fn validate_method<E: serde::de::Error>(method: &str) -> Result<(), E> {
    if method.is_empty() {
        Err(E::custom("JSON-RPC method cannot be empty"))
    } else {
        Ok(())
    }
}

impl<'de> Deserialize<'de> for JsonRpcRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = JsonRpcRequestWire::deserialize(deserializer)?;
        validate_version::<D::Error>(&wire.jsonrpc)?;
        if let Some(message) = non_null_id_error(&wire.id) {
            return Err(D::Error::custom(message));
        }
        validate_method::<D::Error>(&wire.method)?;
        Ok(Self {
            id: wire.id,
            method: wire.method,
            params: wire.params,
        })
    }
}

impl<'de> Deserialize<'de> for JsonRpcNotification {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = JsonRpcNotificationWire::deserialize(deserializer)?;
        validate_version::<D::Error>(&wire.jsonrpc)?;
        validate_method::<D::Error>(&wire.method)?;
        Ok(Self {
            method: wire.method,
            params: wire.params,
        })
    }
}

impl<'de> Deserialize<'de> for JsonRpcResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = JsonRpcResponseWire::deserialize(deserializer)?;
        validate_version::<D::Error>(&wire.jsonrpc)?;
        if let Some(message) = non_null_id_error(&wire.id) {
            return Err(D::Error::custom(message));
        }
        Ok(Self {
            id: wire.id,
            result: wire.result,
        })
    }
}

impl<'de> Deserialize<'de> for JsonRpcError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = JsonRpcErrorWire::deserialize(deserializer)?;
        validate_version::<D::Error>(&wire.jsonrpc)?;
        Ok(Self {
            id: wire.id,
            error: wire.error,
        })
    }
}

impl<'de> Deserialize<'de> for JsonRpcMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| D::Error::custom("JSON-RPC message must be an object"))?;
        let has_method = object.contains_key("method");
        let has_id = object.contains_key("id");
        let has_result = object.contains_key("result");
        let has_error = object.contains_key("error");
        if has_method && !has_result && !has_error {
            return if has_id {
                serde_json::from_value::<JsonRpcRequest>(value)
                    .map(Self::Request)
                    .map_err(D::Error::custom)
            } else {
                serde_json::from_value::<JsonRpcNotification>(value)
                    .map(Self::Notification)
                    .map_err(D::Error::custom)
            };
        }
        if has_id && has_result && !has_error && !has_method {
            return serde_json::from_value::<JsonRpcResponse>(value)
                .map(Self::Response)
                .map_err(D::Error::custom);
        }
        if has_id && has_error && !has_result && !has_method {
            return serde_json::from_value::<JsonRpcError>(value)
                .map(Self::Error)
                .map_err(D::Error::custom);
        }
        Err(D::Error::custom("Invalid JSON-RPC message"))
    }
}

impl Serialize for JsonRpcRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Some(message) = non_null_id_error(&self.id) {
            return Err(<S::Error as serde::ser::Error>::custom(message));
        }
        let mut map = serializer.serialize_map(Some(3 + usize::from(self.params.is_some())))?;
        map.serialize_entry("jsonrpc", JSON_RPC_VERSION)?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("method", &self.method)?;
        if let Some(params) = &self.params {
            map.serialize_entry("params", params)?;
        }
        map.end()
    }
}

impl Serialize for JsonRpcNotification {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(2 + usize::from(self.params.is_some())))?;
        map.serialize_entry("jsonrpc", JSON_RPC_VERSION)?;
        map.serialize_entry("method", &self.method)?;
        if let Some(params) = &self.params {
            map.serialize_entry("params", params)?;
        }
        map.end()
    }
}

impl Serialize for JsonRpcResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Some(message) = non_null_id_error(&self.id) {
            return Err(<S::Error as serde::ser::Error>::custom(message));
        }
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("jsonrpc", JSON_RPC_VERSION)?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("result", &self.result)?;
        map.end()
    }
}

impl Serialize for JsonRpcError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("jsonrpc", JSON_RPC_VERSION)?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("error", &self.error)?;
        map.end()
    }
}

impl JsonRpcError {
    pub fn new(id: RequestId, code: AppServerErrorCode, message: impl Into<String>) -> Self {
        Self {
            id,
            error: JsonRpcErrorBody {
                code: code.code(),
                message: message.into(),
                data: None,
            },
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.error.data = Some(data);
        self
    }
}
