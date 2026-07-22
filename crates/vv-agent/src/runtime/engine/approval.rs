use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::approval::{
    block_on_approval_future, new_approval_request_id, ApprovalError, ApprovalFuture,
    ApprovalRequest,
};
use crate::llm::LlmClient;
use crate::runtime::CancellationToken;
use crate::tools::{
    ApprovalDecision, ApprovalRequirement, ToolContext, ToolOrchestrator, ToolRunOptions,
};
use crate::types::{AgentTask, ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus};

use super::helpers::controls_cancelled;
use super::{AgentRuntime, RuntimeRunControls};

enum ApprovalProviderOutcome {
    Decision(Option<ApprovalDecision>),
    Cancelled,
}

fn await_provider_decision(
    future: ApprovalFuture<Option<ApprovalDecision>>,
    cancellation_token: Option<&CancellationToken>,
) -> Result<ApprovalProviderOutcome, ApprovalError> {
    let Some(cancellation_token) = cancellation_token else {
        return block_on_approval_future(future).map(ApprovalProviderOutcome::Decision);
    };
    if cancellation_token.is_cancelled() {
        return Ok(ApprovalProviderOutcome::Cancelled);
    }

    let (cancel_sender, cancel_receiver) = tokio::sync::oneshot::channel();
    let cancel_sender = Arc::new(Mutex::new(Some(cancel_sender)));
    cancellation_token.on_cancel(move || {
        if let Ok(mut sender) = cancel_sender.lock() {
            if let Some(sender) = sender.take() {
                let _ = sender.send(());
            }
        }
    });
    let raced: ApprovalFuture<ApprovalProviderOutcome> = Box::pin(async move {
        tokio::select! {
            decision = future => decision.map(ApprovalProviderOutcome::Decision),
            _ = cancel_receiver => Ok(ApprovalProviderOutcome::Cancelled),
        }
    });
    block_on_approval_future(raced)
}

fn bind_request_cancellation(
    broker: &crate::approval::ApprovalBroker,
    request_id: &str,
    cancellation_token: Option<&CancellationToken>,
) {
    let Some(token) = cancellation_token else {
        return;
    };
    let broker = broker.clone();
    let request_id = request_id.to_string();
    token.on_cancel(move || {
        let _ = broker.resolve(
            &request_id,
            ApprovalDecision::deny("Operation was cancelled"),
        );
    });
}

pub(super) struct PendingToolApprovalCapture<'a> {
    pub(super) task: &'a AgentTask,
    pub(super) hook_manager: &'a crate::runtime::RuntimeHookManager,
    pub(super) cycle_index: u32,
    pub(super) call: &'a ToolCall,
    pub(super) context: &'a ToolContext,
    pub(super) options: &'a ToolRunOptions,
    pub(super) orchestrator: &'a ToolOrchestrator,
    pub(super) result: &'a ToolExecutionResult,
}

impl<C: LlmClient> AgentRuntime<C> {
    pub(super) fn capture_pending_tool_approval(&self, capture: PendingToolApprovalCapture<'_>) {
        let PendingToolApprovalCapture {
            task,
            hook_manager,
            cycle_index,
            call,
            context,
            options,
            orchestrator,
            result,
        } = capture;
        let Some(interruption_id) = result
            .metadata
            .get("approval_interruption_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return;
        };
        let Some(pending) = self.pending_tool_approval.as_ref() else {
            return;
        };
        let mut slot = pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Some(crate::result::PendingToolApproval {
            interruption_id: interruption_id.to_string(),
            call: call.clone(),
            cycle_index,
            context: context.clone(),
            options: options.clone(),
            orchestrator: orchestrator.clone(),
            task: task.clone(),
            hook_manager: hook_manager.clone(),
        });
    }
}

pub(super) fn approval_provider_result<C: LlmClient>(
    runtime: &AgentRuntime<C>,
    controls: &RuntimeRunControls,
    task: &AgentTask,
    cycle_index: u32,
    call: &ToolCall,
    effective_requirement: ApprovalRequirement,
    tool_metadata: &crate::types::Metadata,
) -> Result<Option<ToolExecutionResult>, ApprovalError> {
    if matches!(effective_requirement, ApprovalRequirement::NotRequired) {
        return Ok(None);
    }
    let local_approval_result = || {
        matches!(effective_requirement, ApprovalRequirement::Required)
            .then(|| approval_required_result(call))
    };
    let Some(execution_context) = controls.execution_context.as_ref() else {
        return Ok(local_approval_result());
    };
    let Some(provider) = execution_context.approval_provider.as_ref() else {
        return Ok(local_approval_result());
    };
    let Some(broker) = execution_context.approval_broker.as_ref() else {
        return Ok(local_approval_result());
    };
    if broker.allows_tool_for_session(&call.name)? {
        return Ok(None);
    }
    let identity = &execution_context.metadata;
    let run_id = identity
        .get("_vv_agent_run_id")
        .or_else(|| task.metadata.get("_vv_agent_run_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&task.task_id)
        .to_string();
    let trace_id = identity
        .get("_vv_agent_trace_id")
        .or_else(|| identity.get("trace_id"))
        .or_else(|| task.metadata.get("_vv_agent_trace_id"))
        .or_else(|| task.metadata.get("trace_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&run_id)
        .to_string();
    let agent_name = identity
        .get("_vv_agent_agent_name")
        .or_else(|| task.metadata.get("_vv_agent_agent_name"))
        .or_else(|| task.metadata.get("agent_name"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&task.task_id)
        .to_string();
    let mut request =
        ApprovalRequest::for_tool_call(run_id, trace_id, agent_name, cycle_index, call);
    request.metadata.insert(
        "tool_metadata".to_string(),
        Value::Object(tool_metadata.clone().into_iter().collect()),
    );
    if let Some(session_id) = identity
        .get("_vv_agent_session_id")
        .or_else(|| task.metadata.get("_vv_agent_session_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        request.metadata.insert(
            "session_id".to_string(),
            Value::String(session_id.to_string()),
        );
    }
    if !provider.should_request(&request) {
        return Ok(None);
    }

    broker.register(request.clone())?;
    let cancellation_token = controls.effective_cancellation_token();
    bind_request_cancellation(broker, &request.request_id, cancellation_token.as_ref());
    runtime.emit_log(
        controls,
        "approval_requested",
        BTreeMap::from([
            ("task_id".to_string(), Value::String(request.run_id.clone())),
            (
                "agent_name".to_string(),
                Value::String(request.agent_name.clone()),
            ),
            ("cycle".to_string(), Value::from(cycle_index)),
            (
                "request_id".to_string(),
                Value::String(request.request_id.clone()),
            ),
            (
                "tool_call_id".to_string(),
                Value::String(request.tool_call_id.clone()),
            ),
            (
                "tool_name".to_string(),
                Value::String(request.tool_name.clone()),
            ),
            (
                "preview".to_string(),
                Value::String(request.preview.clone()),
            ),
            (
                "message".to_string(),
                Value::String(format!("Approval required for tool {}.", request.tool_name)),
            ),
            ("arguments".to_string(), request.arguments.clone()),
        ]),
    );

    match await_provider_decision(provider.decide(&request), cancellation_token.as_ref()) {
        Ok(ApprovalProviderOutcome::Decision(Some(decision)))
            if decision.action() == "needs_approval" => {}
        Ok(ApprovalProviderOutcome::Decision(None)) | Ok(ApprovalProviderOutcome::Cancelled) => {}
        Ok(ApprovalProviderOutcome::Decision(Some(decision))) => {
            // A host may resolve the request while decide() is running. In that
            // case the broker already owns the winning decision.
            let _ = broker.resolve(&request.request_id, decision);
        }
        Err(error) => {
            let _ = broker.discard(&request.request_id);
            return Err(error);
        }
    }
    let decision =
        match broker.wait_blocking(&request.request_id, execution_context.approval_timeout) {
            Ok(decision) => decision,
            Err(error) => {
                let _ = broker.discard(&request.request_id);
                return Err(error);
            }
        };
    let decision_action = decision.action();
    let decision_reason = decision.reason().to_string();
    let decision_metadata = decision.metadata().cloned().unwrap_or_default();

    runtime.emit_log(
        controls,
        "approval_resolved",
        BTreeMap::from([
            ("task_id".to_string(), Value::String(request.run_id.clone())),
            (
                "agent_name".to_string(),
                Value::String(request.agent_name.clone()),
            ),
            ("cycle".to_string(), Value::from(cycle_index)),
            (
                "request_id".to_string(),
                Value::String(request.request_id.clone()),
            ),
            (
                "tool_call_id".to_string(),
                Value::String(request.tool_call_id.clone()),
            ),
            (
                "tool_name".to_string(),
                Value::String(request.tool_name.clone()),
            ),
            (
                "action".to_string(),
                Value::String(decision_action.to_string()),
            ),
            ("reason".to_string(), Value::String(decision_reason.clone())),
            (
                "decision_metadata".to_string(),
                Value::Object(decision_metadata.into_iter().collect()),
            ),
        ]),
    );

    if controls_cancelled(controls) {
        return Ok(Some(approval_error_result(
            call,
            "tool_execution_cancelled",
            "Operation was cancelled",
        )));
    }

    Ok(match decision_action {
        "allow" | "allow_session" => None,
        "needs_approval" => Some(approval_error_result(
            call,
            "approval_unresolved",
            "Approval was not resolved.",
        )),
        "deny" => Some(approval_resolution_error_result(
            call,
            &request.request_id,
            "deny",
            "tool_approval_denied",
            canonical_approval_message(
                decision_reason,
                format!("Approval denied for tool {}.", call.name),
            ),
        )),
        "timeout" => Some(approval_resolution_error_result(
            call,
            &request.request_id,
            "timeout",
            "tool_approval_timeout",
            canonical_approval_message(decision_reason, "Approval request timed out."),
        )),
        _ => Some(approval_error_result(
            call,
            "approval_unresolved",
            "Approval was not resolved.",
        )),
    })
}

fn canonical_approval_message(reason: String, default: impl Into<String>) -> String {
    if reason.trim().is_empty() {
        default.into()
    } else {
        reason
    }
}

fn approval_resolution_error_result(
    call: &ToolCall,
    request_id: &str,
    action: &str,
    error_code: &str,
    message: String,
) -> ToolExecutionResult {
    let arguments = Value::Object(call.arguments.clone().into_iter().collect());
    ToolExecutionResult {
        tool_call_id: call.id.clone(),
        content: serde_json::json!({
            "ok": false,
            "error": message,
            "error_code": error_code,
            "tool_name": call.name,
        })
        .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: Some(error_code.to_string()),
        metadata: BTreeMap::from([
            (
                "mode".to_string(),
                Value::String("approval_resolved".to_string()),
            ),
            (
                "request_id".to_string(),
                Value::String(request_id.to_string()),
            ),
            ("tool_name".to_string(), Value::String(call.name.clone())),
            ("arguments".to_string(), arguments),
            ("action".to_string(), Value::String(action.to_string())),
            ("message".to_string(), Value::String(message)),
        ]),
        image_url: None,
        image_path: None,
    }
}

pub(super) fn approval_error_result(
    call: &ToolCall,
    error_code: &str,
    message: impl Into<String>,
) -> ToolExecutionResult {
    let message = message.into();
    ToolExecutionResult {
        tool_call_id: call.id.clone(),
        content: serde_json::json!({
            "ok": false,
            "error": message,
            "error_code": error_code,
            "tool_name": call.name.clone(),
        })
        .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: Some(error_code.to_string()),
        metadata: BTreeMap::new(),
        image_url: None,
        image_path: None,
    }
}

fn approval_required_result(call: &ToolCall) -> ToolExecutionResult {
    let interruption_id = new_approval_request_id();
    let message = format!("Approval required for tool {}.", call.name);
    let arguments = Value::Object(call.arguments.clone().into_iter().collect());
    ToolExecutionResult {
        tool_call_id: call.id.clone(),
        content: message.clone(),
        status: ToolResultStatus::WaitResponse,
        directive: ToolDirective::WaitUser,
        error_code: Some("tool_approval_required".to_string()),
        metadata: BTreeMap::from([
            (
                "mode".to_string(),
                Value::String("approval_requested".to_string()),
            ),
            ("approval_required".to_string(), Value::Bool(true)),
            (
                "approval_interruption_id".to_string(),
                Value::String(interruption_id.clone()),
            ),
            ("request_id".to_string(), Value::String(interruption_id)),
            ("tool_name".to_string(), Value::String(call.name.clone())),
            ("arguments".to_string(), arguments),
            ("message".to_string(), Value::String(message)),
        ]),
        image_url: None,
        image_path: None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use serde_json::json;

    use super::{await_provider_decision, bind_request_cancellation, ApprovalProviderOutcome};
    use crate::approval::{ApprovalBroker, ApprovalRequest};
    use crate::runtime::CancellationToken;
    use crate::tools::ApprovalDecision;
    use crate::types::ToolCall;

    fn request(call_id: &str) -> ApprovalRequest {
        ApprovalRequest::for_tool_call(
            "run",
            "trace",
            "agent",
            1,
            &ToolCall::new(
                call_id,
                "dangerous",
                BTreeMap::from([("scope".to_string(), json!(call_id))]),
            ),
        )
    }

    #[test]
    fn cancellation_resolves_only_the_bound_approval_request() {
        let broker = ApprovalBroker::default();
        let child = request("child");
        let sibling = request("sibling");
        let token = CancellationToken::default();
        broker.register(child.clone()).expect("register child");
        broker.register(sibling.clone()).expect("register sibling");
        bind_request_cancellation(&broker, &child.request_id, Some(&token));

        token.cancel();

        assert!(matches!(
            broker
                .wait_blocking(&child.request_id, Some(Duration::from_millis(10)))
                .expect("child decision"),
            ApprovalDecision::Denied(reason) if reason == "Operation was cancelled"
        ));
        assert!(broker.pending_request(&sibling.request_id).is_some());
        broker
            .resolve(&sibling.request_id, ApprovalDecision::allow())
            .expect("resolve sibling");
        assert!(matches!(
            broker
                .wait_blocking(&sibling.request_id, Some(Duration::from_millis(10)))
                .expect("sibling decision"),
            ApprovalDecision::Approved
        ));

        let future = request("future");
        broker.register(future.clone()).expect("register future");
        assert!(broker.pending_request(&future.request_id).is_some());
    }

    #[test]
    fn cancellation_interrupts_a_provider_future_that_never_resolves() {
        let token = CancellationToken::default();
        let token_for_thread = token.clone();
        let join = std::thread::spawn(move || {
            await_provider_decision(Box::pin(std::future::pending()), Some(&token_for_thread))
        });
        std::thread::sleep(Duration::from_millis(10));
        token.cancel();

        assert!(matches!(
            join.join()
                .expect("join provider waiter")
                .expect("provider outcome"),
            ApprovalProviderOutcome::Cancelled
        ));
    }
}
