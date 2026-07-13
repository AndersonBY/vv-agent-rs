use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use vv_agent::{
    Agent, AgentStatus, ApprovalBroker, ApprovalDecision, ApprovalError, ApprovalFuture,
    ApprovalPolicy, ApprovalProvider, ApprovalRequest, CancellationToken, FunctionTool,
    LLMResponse, ModelRef, RunConfig, RunEventPayload, RunHandle, RunHandleStatus, Runner,
    ScriptedModelProvider, ToolCall, ToolOutput, ToolPolicy,
};

struct AlwaysAsk;

impl ApprovalProvider for AlwaysAsk {
    fn should_request(&self, _request: &ApprovalRequest) -> bool {
        true
    }

    fn decide(&self, _request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        Box::pin(async { Ok(None) })
    }
}

struct NeverDecides;

impl ApprovalProvider for NeverDecides {
    fn should_request(&self, _request: &ApprovalRequest) -> bool {
        true
    }

    fn decide(&self, _request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        Box::pin(std::future::pending())
    }
}

struct ImmediateByTool {
    requests: Arc<Mutex<Vec<String>>>,
}

impl ApprovalProvider for ImmediateByTool {
    fn should_request(&self, request: &ApprovalRequest) -> bool {
        self.requests
            .lock()
            .expect("request lock")
            .push(format!("{}:{}", request.tool_name, request.tool_call_id));
        true
    }

    fn decide(&self, request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        let decision = if request.tool_name == "session_tool" {
            ApprovalDecision::allow_session()
        } else {
            ApprovalDecision::allow()
        };
        Box::pin(async move { Ok(Some(decision)) })
    }
}

struct RecordingBrokeredApproval {
    requests: Arc<Mutex<Vec<String>>>,
}

struct FailingDecision {
    request_id: Arc<Mutex<String>>,
}

impl ApprovalProvider for FailingDecision {
    fn should_request(&self, request: &ApprovalRequest) -> bool {
        *self.request_id.lock().expect("request id lock") = request.request_id.clone();
        true
    }

    fn decide(&self, _request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        Box::pin(async { Err(ApprovalError::new("approval provider unavailable")) })
    }
}

impl ApprovalProvider for RecordingBrokeredApproval {
    fn should_request(&self, request: &ApprovalRequest) -> bool {
        self.requests
            .lock()
            .expect("request lock")
            .push(format!("{}:{}", request.tool_name, request.tool_call_id));
        true
    }

    fn decide(&self, _request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        Box::pin(async { Ok(None) })
    }
}

#[tokio::test]
async fn approval_request_pauses_tool_until_handle_approves() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_tool = calls.clone();
    let dangerous = FunctionTool::builder("dangerous")
        .description("Requires approval.")
        .needs_approval(true)
        .json_schema(json!({"type":"object","properties":{},"required":[]}))
        .handler(move |_ctx, _args: serde_json::Value| {
            let calls = calls_for_tool.clone();
            async move {
                calls.lock().expect("lock").push("ran".to_string());
                Ok(ToolOutput::text("allowed"))
            }
        })
        .build()
        .expect("tool");

    let provider = ScriptedModelProvider::new(
        "scripted",
        "approval-model",
        vec![
            LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "call_1",
                    "dangerous",
                    json!({}),
                )],
            ),
            LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "finish",
                    "task_finish",
                    json!({"message":"done"}),
                )],
            ),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("approver")
        .instructions("Call dangerous, then finish.")
        .model(ModelRef::named("approval-model"))
        .tool(dangerous)
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::OnRequest,
            ..ToolPolicy::default()
        })
        .build()
        .expect("agent");

    let handle = runner
        .start(
            &agent,
            "go",
            RunConfig::builder()
                .approval_provider(Arc::new(AlwaysAsk))
                .build(),
        )
        .await
        .expect("start");
    let mut events = handle.events();
    let mut request_id = None;
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.next().await {
            let event = event.expect("event");
            if let RunEventPayload::ApprovalRequested { request_id: id, .. } = event.payload() {
                assert!(calls.lock().expect("lock").is_empty());
                request_id = Some(id.clone());
                handle
                    .approve(id, ApprovalDecision::allow())
                    .await
                    .expect("approve");
            }
            if matches!(event.payload(), RunEventPayload::RunCompleted { .. }) {
                break;
            }
        }
    })
    .await
    .expect("approval event timeout");

    assert!(request_id.is_some());
    assert_eq!(calls.lock().expect("lock").as_slice(), &["ran".to_string()]);
    assert_eq!(
        handle.result().await.expect("result").final_output(),
        Some("done")
    );
}

#[tokio::test]
async fn approval_provider_failure_fails_run_without_faking_a_denial() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let (runner, agent) = single_approval_runner("call_1", "done", executions.clone());
    let broker = ApprovalBroker::default();
    let request_id = Arc::new(Mutex::new(String::new()));

    let result = runner
        .run_with_config(
            &agent,
            "go",
            RunConfig::builder()
                .approval_provider(Arc::new(FailingDecision {
                    request_id: request_id.clone(),
                }))
                .approval_broker(broker.clone())
                .build(),
        )
        .await
        .expect("approval provider failure should produce a failed run");

    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(
        result.result().error.as_deref(),
        Some("approval provider unavailable")
    );
    assert!(executions.lock().expect("executions lock").is_empty());
    let request_id = request_id.lock().expect("request id lock").clone();
    assert!(!request_id.is_empty());
    assert!(broker.pending_request(&request_id).is_none());
    let lifecycle = result
        .events()
        .iter()
        .filter_map(|event| match event.payload() {
            RunEventPayload::ApprovalRequested { .. } => Some("approval_requested"),
            RunEventPayload::ApprovalResolved { .. } => Some("approval_resolved"),
            RunEventPayload::RunFailed { .. } => Some("run_failed"),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(lifecycle, ["approval_requested", "run_failed"]);
}

#[tokio::test]
async fn direct_cancellation_token_unblocks_pending_approval_without_running_tool() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_tool = calls.clone();
    let dangerous = FunctionTool::builder("dangerous")
        .description("Requires approval.")
        .needs_approval(true)
        .json_schema(json!({"type":"object","properties":{},"required":[]}))
        .handler(move |_ctx, _args: serde_json::Value| {
            let calls = calls_for_tool.clone();
            async move {
                calls.lock().expect("lock").push("ran".to_string());
                Ok(ToolOutput::text("allowed"))
            }
        })
        .build()
        .expect("tool");
    let provider = ScriptedModelProvider::new(
        "scripted",
        "approval-model",
        vec![LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "call_1",
                "dangerous",
                json!({}),
            )],
        )],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("approver")
        .instructions("Call dangerous.")
        .model(ModelRef::named("approval-model"))
        .tool(dangerous)
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::OnRequest,
            ..ToolPolicy::default()
        })
        .build()
        .expect("agent");
    let token = CancellationToken::default();
    let handle = runner
        .start(
            &agent,
            "go",
            RunConfig::builder()
                .approval_provider(Arc::new(NeverDecides))
                .cancellation_token(token.clone())
                .build(),
        )
        .await
        .expect("start");
    let mut events = handle.events();

    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.next().await {
            let event = event.expect("event");
            if matches!(event.payload(), RunEventPayload::ApprovalRequested { .. }) {
                token.cancel();
                break;
            }
        }
    })
    .await
    .expect("approval event timeout");

    let result = tokio::time::timeout(Duration::from_secs(5), handle.result())
        .await
        .expect("cancelled result timeout")
        .expect("cancelled result");
    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(
        result.result().error.as_deref(),
        Some("Operation was cancelled")
    );
    assert_eq!(handle.state().status, RunHandleStatus::Cancelled);
    assert!(calls.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn cancelling_one_run_does_not_cancel_shared_broker_siblings_or_future_runs() {
    let broker = ApprovalBroker::default();
    let cancelled_calls = Arc::new(Mutex::new(Vec::new()));
    let sibling_calls = Arc::new(Mutex::new(Vec::new()));
    let (cancelled_runner, cancelled_agent) =
        single_approval_runner("cancelled_call", "cancelled", cancelled_calls.clone());
    let (sibling_runner, sibling_agent) =
        single_approval_runner("sibling_call", "sibling done", sibling_calls.clone());
    let cancelled_handle = cancelled_runner
        .start(&cancelled_agent, "go", shared_broker_config(&broker))
        .await
        .expect("start cancelled run");
    let sibling_handle = sibling_runner
        .start(&sibling_agent, "go", shared_broker_config(&broker))
        .await
        .expect("start sibling run");
    let cancelled_request_id = wait_for_approval_request(&cancelled_handle).await;
    let sibling_request_id = wait_for_approval_request(&sibling_handle).await;

    assert!(broker.pending_request(&cancelled_request_id).is_some());
    assert!(broker.pending_request(&sibling_request_id).is_some());
    assert!(cancelled_handle.cancel());

    let cancelled_result = tokio::time::timeout(Duration::from_secs(5), cancelled_handle.result())
        .await
        .expect("cancelled run timeout")
        .expect("cancelled run result");
    assert_eq!(cancelled_result.status(), AgentStatus::Failed);
    assert_eq!(cancelled_handle.state().status, RunHandleStatus::Cancelled);
    assert!(cancelled_calls.lock().expect("cancelled calls").is_empty());
    assert!(broker.pending_request(&sibling_request_id).is_some());

    sibling_handle
        .approve(&sibling_request_id, ApprovalDecision::allow())
        .await
        .expect("approve sibling");
    let sibling_result = tokio::time::timeout(Duration::from_secs(5), sibling_handle.result())
        .await
        .expect("sibling run timeout")
        .expect("sibling run result");
    assert_eq!(sibling_result.final_output(), Some("sibling done"));
    assert_eq!(
        sibling_calls.lock().expect("sibling calls").as_slice(),
        ["dangerous"]
    );

    let future_calls = Arc::new(Mutex::new(Vec::new()));
    let (future_runner, future_agent) =
        single_approval_runner("future_call", "future done", future_calls.clone());
    let future_handle = future_runner
        .start(&future_agent, "go", shared_broker_config(&broker))
        .await
        .expect("start future run");
    let future_request_id = wait_for_approval_request(&future_handle).await;
    assert!(broker.pending_request(&future_request_id).is_some());
    future_handle
        .approve(&future_request_id, ApprovalDecision::allow())
        .await
        .expect("approve future run");
    let future_result = tokio::time::timeout(Duration::from_secs(5), future_handle.result())
        .await
        .expect("future run timeout")
        .expect("future run result");
    assert_eq!(future_result.final_output(), Some("future done"));
    assert_eq!(
        future_calls.lock().expect("future calls").as_slice(),
        ["dangerous"]
    );
}

#[tokio::test]
async fn immediate_provider_allow_session_skips_only_repeated_same_tool_approvals() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let (runner, agent) = session_approval_runner(executions.clone());
    let requests = Arc::new(Mutex::new(Vec::new()));

    let result = runner
        .run_with_config(
            &agent,
            "go",
            RunConfig::builder()
                .approval_provider(Arc::new(ImmediateByTool {
                    requests: requests.clone(),
                }))
                .build(),
        )
        .await
        .expect("run");

    assert_eq!(result.final_output(), Some("done"));
    assert_eq!(
        executions.lock().expect("execution lock").as_slice(),
        [
            "session_tool",
            "session_tool",
            "single_call_tool",
            "single_call_tool",
        ]
    );
    assert_eq!(
        requests.lock().expect("request lock").as_slice(),
        [
            "session_tool:session_1",
            "single_call_tool:single_1",
            "single_call_tool:single_2",
        ]
    );
    assert_eq!(ApprovalDecision::allow_session().action(), "allow_session");
    assert!(ApprovalDecision::allow_session().is_approved());
}

#[tokio::test]
async fn brokered_live_allow_session_skips_same_tool_and_keeps_other_tool_brokered() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let (runner, agent) = session_approval_runner(executions.clone());
    let provider_requests = Arc::new(Mutex::new(Vec::new()));
    let handle = runner
        .start(
            &agent,
            "go",
            RunConfig::builder()
                .approval_provider(Arc::new(RecordingBrokeredApproval {
                    requests: provider_requests.clone(),
                }))
                .build(),
        )
        .await
        .expect("start");
    let mut events = handle.events();
    let mut requested_calls = Vec::new();
    let mut resolved_calls = Vec::new();

    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.next().await {
            let event = event.expect("event");
            match event.payload() {
                RunEventPayload::ApprovalRequested {
                    request_id,
                    tool_call_id,
                    ..
                } => {
                    let request_id = request_id.clone();
                    let tool_call_id = tool_call_id.clone();
                    requested_calls.push(tool_call_id.clone());
                    let decision = if tool_call_id == "session_1" {
                        ApprovalDecision::allow_session()
                    } else {
                        ApprovalDecision::allow()
                    };
                    handle
                        .approve(&request_id, decision)
                        .await
                        .expect("resolve approval");
                }
                RunEventPayload::ApprovalResolved {
                    tool_call_id,
                    approved,
                    ..
                } => resolved_calls.push((tool_call_id.clone(), *approved)),
                RunEventPayload::RunCompleted { .. } => break,
                _ => {}
            }
        }
    })
    .await
    .expect("approval event timeout");

    assert_eq!(requested_calls, ["session_1", "single_1", "single_2"]);
    assert_eq!(
        resolved_calls,
        [
            ("session_1".to_string(), true),
            ("single_1".to_string(), true),
            ("single_2".to_string(), true),
        ]
    );
    assert_eq!(
        provider_requests
            .lock()
            .expect("provider request lock")
            .as_slice(),
        [
            "session_tool:session_1",
            "single_call_tool:single_1",
            "single_call_tool:single_2",
        ]
    );
    assert_eq!(
        executions.lock().expect("execution lock").as_slice(),
        [
            "session_tool",
            "session_tool",
            "single_call_tool",
            "single_call_tool",
        ]
    );
    assert_eq!(
        handle.result().await.expect("result").final_output(),
        Some("done")
    );
}

fn session_approval_runner(executions: Arc<Mutex<Vec<String>>>) -> (Runner, Agent) {
    let session_tool = recording_tool("session_tool", executions.clone());
    let single_call_tool = recording_tool("single_call_tool", executions);
    let provider = ScriptedModelProvider::new(
        "scripted",
        "approval-model",
        vec![
            LLMResponse::with_tool_calls(
                "",
                vec![
                    ToolCall::from_raw_arguments("session_1", "session_tool", json!({})),
                    ToolCall::from_raw_arguments("session_2", "session_tool", json!({})),
                    ToolCall::from_raw_arguments("single_1", "single_call_tool", json!({})),
                    ToolCall::from_raw_arguments("single_2", "single_call_tool", json!({})),
                ],
            ),
            LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "finish",
                    "task_finish",
                    json!({"message":"done"}),
                )],
            ),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("approver")
        .instructions("Call each requested tool, then finish.")
        .model(ModelRef::named("approval-model"))
        .tool(session_tool)
        .tool(single_call_tool)
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::OnRequest,
            ..ToolPolicy::default()
        })
        .build()
        .expect("agent");
    (runner, agent)
}

fn single_approval_runner(
    call_id: &str,
    final_output: &str,
    executions: Arc<Mutex<Vec<String>>>,
) -> (Runner, Agent) {
    let dangerous = recording_tool("dangerous", executions);
    let provider = ScriptedModelProvider::new(
        "scripted",
        "approval-model",
        vec![
            LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    call_id,
                    "dangerous",
                    json!({}),
                )],
            ),
            LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    format!("{call_id}_finish"),
                    "task_finish",
                    json!({"message": final_output}),
                )],
            ),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder(format!("approver_{call_id}"))
        .instructions("Call dangerous, then finish.")
        .model(ModelRef::named("approval-model"))
        .tool(dangerous)
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::OnRequest,
            ..ToolPolicy::default()
        })
        .build()
        .expect("agent");
    (runner, agent)
}

fn shared_broker_config(broker: &ApprovalBroker) -> RunConfig {
    RunConfig::builder()
        .approval_provider(Arc::new(AlwaysAsk))
        .approval_broker(broker.clone())
        .build()
}

async fn wait_for_approval_request(handle: &RunHandle) -> String {
    let mut events = handle.events();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.next().await {
            let event = event.expect("approval event");
            if let RunEventPayload::ApprovalRequested { request_id, .. } = event.payload() {
                return request_id.clone();
            }
        }
        panic!("run ended before requesting approval");
    })
    .await
    .expect("approval request timeout")
}

fn recording_tool(
    name: &'static str,
    executions: Arc<Mutex<Vec<String>>>,
) -> FunctionTool<serde_json::Value> {
    FunctionTool::builder(name)
        .description("Requires approval.")
        .needs_approval(true)
        .json_schema(json!({"type":"object","properties":{},"required":[]}))
        .handler(move |_context, _args: serde_json::Value| {
            let executions = executions.clone();
            async move {
                executions
                    .lock()
                    .expect("execution lock")
                    .push(name.to_string());
                Ok(ToolOutput::text("allowed"))
            }
        })
        .build()
        .expect("tool")
}
