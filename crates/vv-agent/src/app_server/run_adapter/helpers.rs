use std::collections::BTreeMap;

use serde_json::Value;

use crate::app_server::protocol::{
    AgentMessageDeltaParams, AppItem, AppServerError, ItemCompletedParams, ItemStartedParams,
    ServerNotification, TurnStatus, UserInput,
};
use crate::app_server::thread_store::ThreadStoreError;
use crate::checkpoint::HostInteractionRequest;
use crate::events::RunEventPayload;
use crate::runtime::state::CheckpointStore;
use crate::types::AgentStatus;

pub(super) fn item_from_notification(notification: &ServerNotification) -> Option<AppItem> {
    match notification {
        ServerNotification::AgentMessageDelta(AgentMessageDeltaParams { item, .. })
        | ServerNotification::ItemStarted(ItemStartedParams { item })
        | ServerNotification::ItemCompleted(ItemCompletedParams { item }) => Some(item.clone()),
        _ => None,
    }
}

/// Hydrate the public host prompt only from the durable notification outbox.
/// The RunEvent carries the execution fact needed to locate that row, but its
/// prompt is never copied directly into an App Server notification.
pub(super) fn hydrate_host_interaction_prompt(
    store: Option<&dyn CheckpointStore>,
    event: &crate::events::RunEvent,
    notifications: &mut [ServerNotification],
) {
    let Some(store) = store else {
        return;
    };
    let RunEventPayload::HostInteractionRequested {
        checkpoint_key,
        interaction_id,
        logical_cycle,
        operation_id,
        tool_call_id,
        request_digest,
        prompt,
        ..
    } = event.payload()
    else {
        return;
    };
    let Ok(request) = HostInteractionRequest::new(
        interaction_id.clone(),
        *logical_cycle,
        operation_id.clone(),
        tool_call_id.clone(),
        prompt.clone(),
    ) else {
        return;
    };
    if request.request_digest != *request_digest {
        return;
    }
    let record_id = crate::checkpoint::record_id_for(checkpoint_key, &request);
    let notification_id = crate::checkpoint::notification_id_for(&record_id);
    let Ok(Some(notification)) = store.get_host_interaction_notification(&notification_id) else {
        return;
    };
    for notification_to_send in notifications {
        if let ServerNotification::ThreadStatusChanged(params) = notification_to_send {
            if params.wait_reason.as_deref() == Some("host_interaction") {
                params.prompt = Some(notification.payload.prompt.clone());
            }
        }
    }
}

pub(super) fn input_text(input: &[UserInput]) -> String {
    input
        .iter()
        .filter_map(|item| {
            if item.get("type").and_then(Value::as_str) == Some("text") {
                item.get("text").and_then(Value::as_str).map(str::to_string)
            } else if let Some(text) = item.get("text").and_then(Value::as_str) {
                Some(text.to_string())
            } else if item.is_null() {
                None
            } else {
                Some(item.to_string())
            }
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn turn_status(status: AgentStatus) -> TurnStatus {
    match status {
        AgentStatus::WaitUser | AgentStatus::ReconciliationRequired => TurnStatus::Interrupted,
        AgentStatus::Deferred => TurnStatus::Interrupted,
        AgentStatus::Completed => TurnStatus::Completed,
        AgentStatus::Pending | AgentStatus::Running => TurnStatus::Running,
        AgentStatus::Failed | AgentStatus::MaxCycles => TurnStatus::Failed,
    }
}

pub(super) fn app_json_object(value: &impl serde::Serialize) -> BTreeMap<String, Value> {
    let Value::Object(fields) =
        serde_json::to_value(value).expect("typed App Server observation must serialize")
    else {
        unreachable!("typed App Server observation must serialize as an object");
    };
    fields.into_iter().collect()
}

pub(super) fn store_error(error: ThreadStoreError) -> AppServerError {
    AppServerError::internal(error.to_string())
}
