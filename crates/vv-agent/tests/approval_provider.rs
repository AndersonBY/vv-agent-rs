use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use vv_agent::{
    Agent, ApprovalDecision, ApprovalFuture, ApprovalProvider, ApprovalRequest, FunctionTool,
    LLMResponse, ModelRef, RunConfig, RunEventPayload, Runner, ScriptedModelProvider, ToolCall,
    ToolOutput,
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

#[tokio::test]
async fn approval_request_pauses_tool_until_handle_approves() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_tool = calls.clone();
    let dangerous = FunctionTool::builder("dangerous")
        .description("Requires approval.")
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
