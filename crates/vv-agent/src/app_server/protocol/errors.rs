use serde_json::Value;

use super::jsonrpc::{JsonRpcError, JsonRpcErrorBody, RequestId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppServerErrorCode {
    ParseError,
    ServerOverloaded,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    InternalError,
    NotInitialized,
    AlreadyInitialized,
    ThreadNotFound,
    ThreadArchived,
    ActiveTurnNotFound,
    TurnIdMismatch,
    ExperimentalApiRequired,
    UnsupportedMethod,
}

impl AppServerErrorCode {
    pub fn code(self) -> i64 {
        match self {
            Self::ParseError => -32700,
            Self::ServerOverloaded => -32001,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams => -32602,
            Self::InternalError => -32603,
            Self::NotInitialized => -32010,
            Self::AlreadyInitialized => -32011,
            Self::ThreadNotFound => -32020,
            Self::ThreadArchived => -32021,
            Self::ActiveTurnNotFound => -32030,
            Self::TurnIdMismatch => -32031,
            Self::ExperimentalApiRequired => -32012,
            Self::UnsupportedMethod => -32013,
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

    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(AppServerErrorCode::ParseError, message)
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

    pub fn thread_not_found() -> Self {
        Self::new(AppServerErrorCode::ThreadNotFound, "Thread not found")
    }

    pub fn thread_archived() -> Self {
        Self::new(AppServerErrorCode::ThreadArchived, "Thread archived")
    }

    pub fn active_turn_not_found() -> Self {
        Self::new(
            AppServerErrorCode::ActiveTurnNotFound,
            "Active turn not found",
        )
    }

    pub fn turn_id_mismatch() -> Self {
        Self::new(AppServerErrorCode::TurnIdMismatch, "Turn id mismatch")
    }

    pub fn server_overloaded() -> Self {
        Self::new(
            AppServerErrorCode::ServerOverloaded,
            "Server overloaded; retry later.",
        )
    }

    pub fn unsupported_method(method: impl Into<String>) -> Self {
        let method = method.into();
        Self::new(
            AppServerErrorCode::UnsupportedMethod,
            format!("Unsupported App Server method: {method}"),
        )
        .with_data(serde_json::json!({ "method": method }))
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
