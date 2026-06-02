pub mod channel;
pub mod stdio;

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::app_server::protocol::{AppServerError, JsonRpcMessage};

pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectionId(u64);

impl ConnectionId {
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransportEvent {
    Opened {
        connection_id: ConnectionId,
    },
    Message {
        connection_id: ConnectionId,
        message: JsonRpcMessage,
    },
    Closed {
        connection_id: ConnectionId,
    },
}

pub trait AppServerTransport {
    fn next_event(&mut self)
        -> TransportFuture<'_, Option<Result<TransportEvent, AppServerError>>>;

    fn send(
        &self,
        connection_id: ConnectionId,
        message: JsonRpcMessage,
    ) -> TransportFuture<'_, Result<(), AppServerError>>;
}
