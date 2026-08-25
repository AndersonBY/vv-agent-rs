use crate::app_server::protocol::{AppServerError, JsonRpcRequest, ThreadStatusParams};
use crate::app_server::transport::ConnectionId;

use super::{parse_params, MessageProcessor};

impl MessageProcessor {
    pub(super) async fn process_thread_status(
        &mut self,
        connection_id: ConnectionId,
        request: JsonRpcRequest,
    ) {
        let params = match parse_params::<ThreadStatusParams>(request.params) {
            Ok(params) => params,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        let Some(adapter) = self.run_adapter.clone() else {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::internal("App Server runtime is not configured"),
                )
                .await;
            return;
        };
        match adapter.public_thread_status(&params.thread_id).await {
            Ok(response) => {
                let result = serde_json::to_value(response).expect("thread status serializes");
                let _ = self
                    .outgoing
                    .send_response(connection_id, request.id, result)
                    .await;
            }
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
            }
        }
    }
}
