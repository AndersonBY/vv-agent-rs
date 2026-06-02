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
