use super::*;

impl MessageProcessor {
    pub(super) async fn process_thread_resume(
        &mut self,
        connection_id: ConnectionId,
        request: JsonRpcRequest,
    ) {
        let params = match parse_params::<ThreadResumeParams>(request.params) {
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
        let active_turn_id = self.thread_state.active_turn_id(&params.thread_id).await;
        let reopened_status = if active_turn_id.is_some() {
            ThreadStatus::Running
        } else {
            ThreadStatus::Idle
        };
        let snapshot = || {
            let mut snapshot = load_thread_resume_snapshot(adapter.store(), &params.thread_id)?;
            if snapshot.thread.status == ThreadStatus::Closed {
                adapter
                    .store()
                    .set_active_turn(
                        &params.thread_id,
                        active_turn_id.as_deref(),
                        reopened_status,
                    )
                    .map_err(store_error)?;
                snapshot.thread.status = reopened_status;
            }
            Ok(snapshot)
        };
        let snapshot = if params.subscribe {
            self.thread_state
                .subscribe_and_snapshot(params.thread_id.clone(), connection_id, snapshot)
                .await
        } else {
            snapshot()
        };
        let snapshot = match snapshot {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        if !params.subscribe {
            self.thread_state.reopen(&params.thread_id).await;
        }
        let result = serde_json::to_value(snapshot).expect("thread resume response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
    }
}
