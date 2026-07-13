use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use vv_agent::{
    build_default_registry, Agent, AgentStatus, ApprovalBroker, ApprovalDecision, ApprovalFuture,
    ApprovalProvider, ApprovalRequest, FunctionTool, LLMResponse, LlmRequest, MemorySession,
    Message, ModelRef, RunHandleStatus, Runner, ScriptStep, ScriptedModelProvider, Session,
    SessionItem, StaticTool, ToolCall, ToolOutput, ToolRegistry, ToolResultStatus,
};
use vv_agent::{
    InteractiveAgentClient, InteractiveSessionError, InteractiveSessionEvent,
    InteractiveSessionOptions,
};

struct DeferredApprovalProvider {
    requests: Arc<AtomicUsize>,
    request_ids: std::sync::mpsc::Sender<String>,
}

struct SynchronouslyResolvedApprovalProvider {
    broker: ApprovalBroker,
}

impl ApprovalProvider for SynchronouslyResolvedApprovalProvider {
    fn should_request(&self, request: &ApprovalRequest) -> bool {
        request.tool_name == "dangerous"
    }

    fn decide(&self, request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        let result = self
            .broker
            .resolve(&request.request_id, ApprovalDecision::allow());
        Box::pin(async move { result.map(|()| None) })
    }
}

impl ApprovalProvider for DeferredApprovalProvider {
    fn should_request(&self, request: &ApprovalRequest) -> bool {
        if request.tool_name != "dangerous" {
            return false;
        }
        self.requests.fetch_add(1, Ordering::SeqCst);
        true
    }

    fn decide(&self, request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        self.request_ids
            .send(request.request_id.clone())
            .expect("send approval request id");
        Box::pin(async { Ok(None) })
    }
}

include!("interactive_session/session_basics.rs");
include!("interactive_session/steering.rs");
include!("interactive_session/events_and_follow_ups.rs");
include!("interactive_session/lifecycle.rs");

fn scripted_runner(responses: Vec<LLMResponse>) -> Runner {
    Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo-model",
            responses,
        ))
        .workspace(".")
        .build()
        .expect("runner")
}

fn scripted_agent() -> Agent {
    Agent::builder("assistant")
        .instructions("Finish with a concise answer.")
        .model(ModelRef::named("demo-model"))
        .build()
        .expect("agent")
}

fn finish_response(message: &str) -> LLMResponse {
    LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::from_raw_arguments(
            "finish",
            "task_finish",
            json!({"message": message}),
        )],
    )
}

fn drain_events(
    receiver: &mut vv_agent::interactive::InteractiveSessionSubscription,
) -> Vec<InteractiveSessionEvent> {
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    events
}
