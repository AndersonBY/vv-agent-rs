use crate::app_server::protocol::{
    AppServerError, JsonRpcRequest, TurnControlResponse, TurnFollowUpParams, TurnInterruptParams,
    TurnInterruptResponse, TurnStartParams, TurnStartResponse, TurnSteerParams,
};
use crate::app_server::transport::ConnectionId;

use super::{parse_params, MessageProcessor};

impl MessageProcessor {
    pub(super) async fn process_turn_start(
        &mut self,
        connection_id: ConnectionId,
        request: JsonRpcRequest,
    ) {
        let params = match parse_params::<TurnStartParams>(request.params) {
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
        let turn = match adapter.start_turn(connection_id, params).await {
            Ok(turn) => turn,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        self.thread_state
            .subscribe(turn.thread_id.clone(), connection_id)
            .await;
        let result = serde_json::to_value(TurnStartResponse {
            thread_id: turn.thread_id.clone(),
            turn_id: turn.turn_id.clone(),
            status: turn.status,
        })
        .expect("turn start response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
        let _ = adapter.notify_turn_started(&turn).await;
        adapter
            .spawn_event_forwarding(turn.thread_id.clone(), turn.turn_id.clone())
            .await;
    }

    pub(super) async fn process_turn_steer(
        &mut self,
        connection_id: ConnectionId,
        request: JsonRpcRequest,
    ) {
        let params = match parse_params::<TurnSteerParams>(request.params) {
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
        match adapter
            .queue_steering(&params.thread_id, &params.expected_turn_id, params.input)
            .await
        {
            Ok(turn_id) => {
                let result = serde_json::to_value(TurnControlResponse {
                    thread_id: params.thread_id,
                    turn_id,
                    queued: true,
                })
                .expect("turn steer response serializes");
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

    pub(super) async fn process_turn_follow_up(
        &mut self,
        connection_id: ConnectionId,
        request: JsonRpcRequest,
    ) {
        let params = match parse_params::<TurnFollowUpParams>(request.params) {
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
        match adapter
            .queue_follow_up(&params.thread_id, &params.expected_turn_id, params.input)
            .await
        {
            Ok(turn_id) => {
                let result = serde_json::to_value(TurnControlResponse {
                    thread_id: params.thread_id,
                    turn_id,
                    queued: true,
                })
                .expect("turn follow-up response serializes");
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

    pub(super) async fn process_turn_interrupt(
        &mut self,
        connection_id: ConnectionId,
        request: JsonRpcRequest,
    ) {
        let params = match parse_params::<TurnInterruptParams>(request.params) {
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
        match adapter
            .interrupt_turn(&params.thread_id, &params.expected_turn_id)
            .await
        {
            Ok(outcome) => {
                let result = serde_json::to_value(TurnInterruptResponse {
                    thread_id: params.thread_id,
                    turn_id: outcome.turn_id,
                    cancelled: outcome.cancelled,
                })
                .expect("turn interrupt response serializes");
                let _ = self
                    .outgoing
                    .send_response(connection_id, request.id, result)
                    .await;
                if let Some(resolved_approval) = outcome.approval_resolved {
                    let _ = adapter.notify_approval_resolved(resolved_approval).await;
                }
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
