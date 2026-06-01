use serde_json::json;
use vv_agent::{
    FunctionTool, ToolCall, ToolContext, ToolExposure, ToolOrchestrator, ToolOutput,
    ToolResultStatus,
};

#[tokio::test]
async fn function_tool_adapts_to_tool_executor() {
    let tool = FunctionTool::builder("echo")
        .description("Echo args.")
        .json_schema(
            json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}),
        )
        .handler(|_ctx, args: serde_json::Value| async move { Ok(ToolOutput::json(args)) })
        .build()
        .expect("tool");

    let executor = tool.to_executor();

    assert_eq!(executor.name(), "echo");
    assert_eq!(executor.exposure(), ToolExposure::Direct);
}

#[tokio::test]
async fn orchestrator_rejects_disallowed_tool_before_handler_runs() {
    let tool = FunctionTool::builder("hidden")
        .description("Should not run.")
        .json_schema(json!({"type":"object","properties":{},"required":[]}))
        .handler(|_ctx, _args: serde_json::Value| async move { Ok(ToolOutput::text("ran")) })
        .build()
        .expect("tool");
    let orchestrator = ToolOrchestrator::from_tools(vec![tool.to_executor()]);
    let mut context = ToolContext::new("./workspace");
    let result = orchestrator
        .run_one(
            ToolCall::from_raw_arguments("call_1", "hidden", json!({})),
            &mut context,
            vv_agent::ToolRunOptions::default().allow_only(vec!["other"]),
        )
        .await
        .expect("result");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("tool_not_allowed"));
}
