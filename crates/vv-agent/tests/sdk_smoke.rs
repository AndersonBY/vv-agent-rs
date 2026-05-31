use std::collections::BTreeMap;

use serde_json::json;
use vv_agent::{
    Agent, AgentStatus, LLMResponse, ModelRef, Runner, ScriptedModelProvider, ToolCall,
};

#[tokio::test]
async fn runner_facade_can_run_a_simple_prompt() {
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo",
            vec![finish_response("final answer")],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("demo")
        .instructions("Answer and finish.")
        .model(ModelRef::named("demo"))
        .build()
        .expect("agent");

    let result = runner.run(&agent, "say hello").await.expect("run");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), Some("final answer"));
}

fn finish_response(message: &str) -> LLMResponse {
    let mut args = BTreeMap::new();
    args.insert("message".to_string(), json!(message));
    LLMResponse::with_tool_calls("", vec![ToolCall::new("finish", "task_finish", args)])
}
