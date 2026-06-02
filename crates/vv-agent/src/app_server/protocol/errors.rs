use serde_json::Value;

use super::jsonrpc::{JsonRpcError, JsonRpcErrorBody, RequestId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppServerErrorCode {
    ServerOverloaded,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    InternalError,
    NotInitialized,
    AlreadyInitialized,
    ExperimentalApiRequired,
}

impl AppServerErrorCode {
    pub fn code(self) -> i64 {
        match self {
            Self::ServerOverloaded => -32001,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams => -32602,
            Self::InternalError => -32603,
            Self::NotInitialized => -32010,
            Self::AlreadyInitialized => -32011,
            Self::ExperimentalApiRequired => -32012,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppServerError {
    code: AppServerErrorCode,
    message: String,
    data: Option<Value>,
}

impl AppServerError {
    pub fn new(code: AppServerErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(AppServerErrorCode::InvalidParams, message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(AppServerErrorCode::InvalidRequest, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(AppServerErrorCode::InternalError, message)
    }

    pub fn not_initialized() -> Self {
        Self::new(AppServerErrorCode::NotInitialized, "Not initialized")
    }

    pub fn already_initialized() -> Self {
        Self::new(
            AppServerErrorCode::AlreadyInitialized,
            "Already initialized",
        )
    }

    pub fn server_overloaded() -> Self {
        Self::new(
            AppServerErrorCode::ServerOverloaded,
            "Server overloaded; retry later.",
        )
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn code(&self) -> AppServerErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn into_json_rpc_error(self, id: RequestId) -> JsonRpcError {
        JsonRpcError {
            id,
            error: JsonRpcErrorBody {
                code: self.code.code(),
                message: self.message,
                data: self.data,
            },
        }
    }
}
