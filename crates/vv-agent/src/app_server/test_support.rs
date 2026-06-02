use std::collections::BTreeMap;

use serde_json::json;

use crate::app_server::client::AppServerClient;
use crate::app_server::processor::MessageProcessor;
use crate::app_server::thread_store::SqliteThreadStore;
use crate::app_server::transport::ConnectionId;
use crate::{
    Agent, FunctionTool, LLMResponse, ModelRef, Runner, ScriptedModelProvider, ToolCall, ToolOutput,
};

pub fn scripted_app_server_client(responses: Vec<LLMResponse>) -> AppServerClient {
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo-model",
            responses,
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Answer the user, then finish.")
        .model(ModelRef::named("demo-model"))
        .build()
        .expect("agent");
    client_from_runner(runner, agent)
}

pub fn approval_app_server_client() -> AppServerClient {
    let dangerous = FunctionTool::builder("dangerous")
        .description("Requires approval.")
        .json_schema(json!({"type":"object","properties":{},"required":[]}))
        .handler(|_ctx, _args: serde_json::Value| async move { Ok(ToolOutput::text("allowed")) })
        .build()
        .expect("tool");
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
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
                finish_response("done"),
            ],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("approver")
        .instructions("Call dangerous, then finish.")
        .model(ModelRef::named("approval-model"))
        .tool(dangerous)
        .build()
        .expect("agent");
    client_from_runner(runner, agent)
}

pub fn finish_response(message: &str) -> LLMResponse {
    let mut args = BTreeMap::new();
    args.insert("message".to_string(), json!(message));
    LLMResponse::with_tool_calls(message, vec![ToolCall::new("finish", "task_finish", args)])
}

fn client_from_runner(runner: Runner, agent: Agent) -> AppServerClient {
    let (processor, outgoing) = MessageProcessor::new_for_tests_with_runtime(
        128,
        runner,
        agent,
        SqliteThreadStore::in_memory().expect("store"),
    );
    AppServerClient::new_for_processor(processor, outgoing, ConnectionId::new(1))
}
