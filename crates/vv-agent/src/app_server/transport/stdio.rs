use std::io::{self, BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::app_server::protocol::{AppServerError, JsonRpcMessage};

use super::{AppServerTransport, ConnectionId, TransportEvent, TransportFuture};

pub struct StdioJsonlTransport<W = io::Stdout> {
    connection_id: ConnectionId,
    writer: Arc<Mutex<W>>,
    events: mpsc::UnboundedReceiver<Result<TransportEvent, AppServerError>>,
}

impl StdioJsonlTransport<io::Stdout> {
    pub fn new() -> Self {
        Self::from_io(
            ConnectionId::new(1),
            BufReader::new(io::stdin()),
            io::stdout(),
        )
    }
}

impl Default for StdioJsonlTransport<io::Stdout> {
    fn default() -> Self {
        Self::new()
    }
}

impl<W> StdioJsonlTransport<W>
where
    W: Write + Send + 'static,
{
    pub fn from_io<R>(connection_id: ConnectionId, mut reader: R, writer: W) -> Self
    where
        R: BufRead + Send + 'static,
    {
        let writer = Arc::new(Mutex::new(writer));
        let (event_tx, events) = mpsc::unbounded_channel();
        let reader_events = event_tx.clone();
        let spawn_result = std::thread::Builder::new()
            .name("vv-agent-app-server-stdio".to_string())
            .spawn(move || {
                if reader_events
                    .send(Ok(TransportEvent::Opened { connection_id }))
                    .is_err()
                {
                    return;
                }
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => {
                            let _ =
                                reader_events.send(Ok(TransportEvent::Closed { connection_id }));
                            return;
                        }
                        Ok(_) => match parse_jsonl_message(&line) {
                            Ok(Some(message)) => {
                                if reader_events
                                    .send(Ok(TransportEvent::Message {
                                        connection_id,
                                        message,
                                    }))
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                if reader_events
                                    .send(Ok(TransportEvent::ProtocolError {
                                        connection_id,
                                        error,
                                    }))
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        },
                        Err(error) => {
                            let _ = reader_events
                                .send(Err(AppServerError::internal(error.to_string())));
                            return;
                        }
                    }
                }
            });
        if let Err(error) = spawn_result {
            let _ = event_tx.send(Err(AppServerError::internal(error.to_string())));
        }
        Self {
            connection_id,
            writer,
            events,
        }
    }
}

impl<W> AppServerTransport for StdioJsonlTransport<W>
where
    W: Write + Send + 'static,
{
    fn next_event(
        &mut self,
    ) -> TransportFuture<'_, Option<Result<TransportEvent, AppServerError>>> {
        Box::pin(async move { self.events.recv().await })
    }

    fn send(
        &self,
        connection_id: ConnectionId,
        message: JsonRpcMessage,
    ) -> TransportFuture<'_, Result<(), AppServerError>> {
        let expected_connection_id = self.connection_id;
        let writer = Arc::clone(&self.writer);
        Box::pin(async move {
            if connection_id != expected_connection_id {
                return Err(AppServerError::internal("unknown stdio connection"));
            }
            write_jsonl(&writer, &message)
        })
    }
}

pub fn parse_jsonl_message(line: &str) -> Result<Option<JsonRpcMessage>, AppServerError> {
    if line.trim().is_empty() {
        return Ok(None);
    }
    let value = serde_json::from_str::<serde_json::Value>(line)
        .map_err(|_| AppServerError::parse_error("Parse error"))?;
    serde_json::from_value::<JsonRpcMessage>(value)
        .map(Some)
        .map_err(|_| AppServerError::invalid_request("Invalid Request"))
}

pub fn serialize_jsonl_message(message: &JsonRpcMessage) -> Result<String, AppServerError> {
    let mut line = serde_json::to_string(message)
        .map_err(|error| AppServerError::internal(error.to_string()))?;
    line.push('\n');
    Ok(line)
}

fn write_jsonl<W: Write>(
    writer: &Arc<Mutex<W>>,
    message: &JsonRpcMessage,
) -> Result<(), AppServerError> {
    let line = serialize_jsonl_message(message)?;
    let mut writer = writer
        .lock()
        .map_err(|_| AppServerError::internal("stdio writer lock poisoned"))?;
    writer
        .write_all(line.as_bytes())
        .and_then(|()| writer.flush())
        .map_err(|error| AppServerError::internal(error.to_string()))
}
