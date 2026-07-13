use std::collections::HashSet;

use tokio::sync::mpsc;

use crate::app_server::outgoing::OutgoingEnvelope;
use crate::app_server::processor::MessageProcessor;
use crate::app_server::protocol::{AppServerError, RequestId};
use crate::app_server::transport::{
    AppServerTransport, ConnectionId, TransportConnectionMode, TransportEvent,
};

pub struct AppServer<T: AppServerTransport> {
    transport: T,
    processor: MessageProcessor,
    outgoing: mpsc::Receiver<OutgoingEnvelope>,
}

impl<T: AppServerTransport> AppServer<T> {
    pub fn new(
        transport: T,
        processor: MessageProcessor,
        outgoing: mpsc::Receiver<OutgoingEnvelope>,
    ) -> Self {
        Self {
            transport,
            processor,
            outgoing,
        }
    }

    pub async fn run(&mut self) -> Result<(), AppServerError> {
        let mut open_connections = HashSet::new();
        let mut disconnected_connections = HashSet::new();
        loop {
            tokio::select! {
                biased;
                envelope = self.outgoing.recv() => {
                    let Some(envelope) = envelope else {
                        self.disconnect_all(&mut open_connections).await;
                        return Ok(());
                    };
                    if !self
                        .processor
                        .outgoing()
                        .is_connection_registered(envelope.connection_id)
                        .await
                    {
                        continue;
                    }
                    if let Err(error) = self.transport
                        .send(envelope.connection_id, envelope.message)
                        .await
                    {
                        disconnected_connections.insert(envelope.connection_id);
                        self.disconnect_connection(&mut open_connections, envelope.connection_id)
                            .await;
                        if self.transport.connection_mode() == TransportConnectionMode::Single {
                            self.disconnect_all(&mut open_connections).await;
                            return Err(error);
                        }
                    }
                }
                event = self.transport.next_event() => {
                    match event {
                        Some(Ok(TransportEvent::Opened { connection_id })) => {
                            disconnected_connections.remove(&connection_id);
                            open_connections.insert(connection_id);
                            self.processor
                                .outgoing()
                                .register_connection(connection_id)
                                .await;
                        }
                        Some(Ok(TransportEvent::Message {
                            connection_id,
                            message,
                        })) => {
                            if disconnected_connections.contains(&connection_id) {
                                continue;
                            }
                            open_connections.insert(connection_id);
                            self.processor.process_message(connection_id, message).await;
                        }
                        Some(Ok(TransportEvent::ProtocolError {
                            connection_id,
                            error,
                        })) => {
                            if disconnected_connections.contains(&connection_id) {
                                continue;
                            }
                            open_connections.insert(connection_id);
                            self.processor
                                .outgoing()
                                .register_connection(connection_id)
                                .await;
                            let _ = self
                                .processor
                                .outgoing()
                                .send_error(connection_id, RequestId::Null, error)
                                .await;
                        }
                        Some(Ok(TransportEvent::Closed { connection_id })) => {
                            disconnected_connections.insert(connection_id);
                            self.disconnect_connection(&mut open_connections, connection_id).await;
                        }
                        Some(Err(error)) => {
                            self.disconnect_all(&mut open_connections).await;
                            return Err(error);
                        }
                        None => {
                            self.disconnect_all(&mut open_connections).await;
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    pub fn processor(&self) -> &MessageProcessor {
        &self.processor
    }

    async fn disconnect_connection(
        &mut self,
        open_connections: &mut HashSet<ConnectionId>,
        connection_id: ConnectionId,
    ) {
        open_connections.remove(&connection_id);
        self.processor.disconnect_connection(connection_id).await;
    }

    async fn disconnect_all(&mut self, open_connections: &mut HashSet<ConnectionId>) {
        let mut connection_ids = std::mem::take(open_connections);
        connection_ids.extend(self.processor.connection_ids());
        for connection_id in connection_ids {
            self.processor.disconnect_connection(connection_id).await;
        }
    }
}
