use tokio::sync::mpsc;

use crate::app_server::protocol::{AppServerError, JsonRpcMessage};

use super::{AppServerTransport, ConnectionId, TransportEvent, TransportFuture};

pub struct ChannelTransport {
    inbound_rx: mpsc::Receiver<TransportEvent>,
    outbound_tx: mpsc::Sender<JsonRpcMessage>,
}

pub struct ChannelTransportClient {
    connection_id: ConnectionId,
    inbound_tx: mpsc::Sender<TransportEvent>,
    outbound_rx: mpsc::Receiver<JsonRpcMessage>,
}

impl ChannelTransport {
    pub fn pair(capacity: usize) -> (Self, ChannelTransportClient) {
        let (inbound_tx, inbound_rx) = mpsc::channel(capacity);
        let (outbound_tx, outbound_rx) = mpsc::channel(capacity);
        let connection_id = ConnectionId::new(1);
        (
            Self {
                inbound_rx,
                outbound_tx,
            },
            ChannelTransportClient {
                connection_id,
                inbound_tx,
                outbound_rx,
            },
        )
    }
}

impl AppServerTransport for ChannelTransport {
    fn next_event(
        &mut self,
    ) -> TransportFuture<'_, Option<Result<TransportEvent, AppServerError>>> {
        Box::pin(async move { self.inbound_rx.recv().await.map(Ok) })
    }

    fn send(
        &self,
        _connection_id: ConnectionId,
        message: JsonRpcMessage,
    ) -> TransportFuture<'_, Result<(), AppServerError>> {
        Box::pin(async move {
            self.outbound_tx
                .try_send(message)
                .map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => AppServerError::server_overloaded(),
                    mpsc::error::TrySendError::Closed(_) => {
                        AppServerError::internal("channel transport closed")
                    }
                })
        })
    }
}

impl ChannelTransportClient {
    pub fn connection_id(&self) -> ConnectionId {
        self.connection_id
    }

    pub async fn open(&self) -> Result<(), AppServerError> {
        self.inbound_tx
            .send(TransportEvent::Opened {
                connection_id: self.connection_id,
            })
            .await
            .map_err(|_| AppServerError::internal("channel transport closed"))
    }

    pub async fn send_message(&self, message: JsonRpcMessage) -> Result<(), AppServerError> {
        self.inbound_tx
            .send(TransportEvent::Message {
                connection_id: self.connection_id,
                message,
            })
            .await
            .map_err(|_| AppServerError::internal("channel transport closed"))
    }

    pub async fn close(&self) -> Result<(), AppServerError> {
        self.inbound_tx
            .send(TransportEvent::Closed {
                connection_id: self.connection_id,
            })
            .await
            .map_err(|_| AppServerError::internal("channel transport closed"))
    }

    pub async fn recv_message(&mut self) -> Option<JsonRpcMessage> {
        self.outbound_rx.recv().await
    }
}
