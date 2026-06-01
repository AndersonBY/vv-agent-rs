use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use vv_agent::{
    Agent, FunctionTool, LLMResponse, ModelRef, RunConfig, RunEventPayload, Runner,
    ScriptedModelProvider, ToolCall, ToolOutput,
};

#[tokio::test]
async fn runner_start_yields_tool_started_before_result_is_ready() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let ran = Arc::new(Mutex::new(false));
    let gate_for_tool = gate.clone();
    let ran_for_tool = ran.clone();
    let slow_tool = FunctionTool::builder("slow_tool")
        .description("Wait until test releases the gate.")
        .json_schema(json!({"type":"object","properties":{},"required":[]}))
        .handler(move |_ctx, _args: serde_json::Value| {
            let gate = gate_for_tool.clone();
            let ran = ran_for_tool.clone();
            async move {
                gate.notified().await;
                *ran.lock().expect("lock") = true;
                Ok(ToolOutput::text("slow done"))
            }
        })
        .build()
        .expect("tool");

    let provider = ScriptedModelProvider::new(
        "scripted",
        "demo-model",
        vec![
            LLMResponse::with_tool_calls(
                "calling",
                vec![ToolCall::from_raw_arguments(
                    "call_1",
                    "slow_tool",
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
    let agent = Agent::builder("assistant")
        .instructions("Call slow_tool, then finish.")
        .model(ModelRef::named("demo-model"))
        .tool(slow_tool)
        .build()
        .expect("agent");

    let handle = runner
        .start(&agent, "go", RunConfig::default())
        .await
        .expect("start");
    let mut events = handle.events();
    let mut saw_started = false;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(2), events.next())
        .await
        .expect("event timeout")
    {
        let event = event.expect("event");
        if matches!(event.payload(), RunEventPayload::ToolCallStarted { tool_name, .. } if tool_name == "slow_tool")
        {
            assert!(!handle.state().done);
            saw_started = true;
            gate.notify_one();
            break;
        }
    }

    assert!(saw_started);
    let result = handle.result().await.expect("result");
    assert_eq!(result.final_output(), Some("done"));
    assert!(*ran.lock().expect("lock"));
}
