use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    Agent, AgentStatus, FunctionTool, LLMResponse, LlmRequest, ModelRef, RunConfig, Runner,
    ScriptStep, ScriptedModelProvider, ToolCall, ToolOutput,
};

#[tokio::test]
async fn resume_with_input_restores_messages_and_shared_state() {
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let first_requests = requests.clone();
    let second_requests = requests.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "resume-model",
        vec![
            ScriptStep::callback(move |request| {
                first_requests
                    .lock()
                    .expect("requests")
                    .push(request.clone());
                Ok(LLMResponse::with_tool_calls(
                    "need input",
                    vec![ToolCall::from_raw_arguments(
                        "ask_1",
                        "ask_user",
                        json!({"question": "Which color?"}),
                    )],
                ))
            }),
            ScriptStep::callback(move |request| {
                second_requests
                    .lock()
                    .expect("requests")
                    .push(request.clone());
                Ok(LLMResponse::with_tool_calls(
                    "resumed",
                    vec![ToolCall::from_raw_arguments(
                        "finish_1",
                        "task_finish",
                        json!({"message": "selected blue"}),
                    )],
                ))
            }),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("resume-agent")
        .instructions("Ask once, then finish.")
        .model(ModelRef::named("resume-model"))
        .build()
        .expect("agent");
    let config = RunConfig::builder()
        .initial_shared_state([("marker".to_string(), json!("preserved"))].into())
        .build();

    let interrupted = runner
        .run_with_config(&agent, "choose", config)
        .await
        .expect("interrupted run");
    assert_eq!(interrupted.status(), AgentStatus::WaitUser);
    let interrupted_messages = interrupted.result().messages.clone();

    let resumed = runner
        .resume_with_input(interrupted.into_state().expect("run state"), "blue")
        .await
        .expect("resumed run");

    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("selected blue"));
    assert_eq!(resumed.result().shared_state["marker"], json!("preserved"));
    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.len() > interrupted_messages.len());
    for (actual, expected) in requests[1].messages.iter().zip(&interrupted_messages) {
        assert_eq!(actual.role, expected.role);
        assert_eq!(actual.content, expected.content);
        assert_eq!(actual.tool_calls, expected.tool_calls);
        assert_eq!(actual.tool_call_id, expected.tool_call_id);
    }
    assert_eq!(
        requests[1].messages.last().expect("resume input").content,
        "blue"
    );
}

#[tokio::test]
async fn resumed_successful_ordinary_tool_preserves_max_cycles_status() {
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "resume-model",
        vec![
            ScriptStep::from(LLMResponse::with_tool_calls(
                "need input",
                vec![ToolCall::from_raw_arguments(
                    "ask_1",
                    "ask_user",
                    json!({"question": "Continue?"}),
                )],
            )),
            ScriptStep::from(LLMResponse::with_tool_calls(
                "use tool",
                vec![ToolCall::from_raw_arguments(
                    "ordinary_1",
                    "ordinary",
                    json!({}),
                )],
            )),
        ],
    );
    let ordinary = FunctionTool::builder("ordinary")
        .handler(|_context, _arguments: serde_json::Value| async {
            Ok(ToolOutput::text("ordinary success"))
        })
        .build()
        .expect("ordinary tool");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("resume-agent")
        .instructions("Ask, then use an ordinary tool.")
        .model(ModelRef::named("resume-model"))
        .tool(ordinary)
        .build()
        .expect("agent");
    let config = RunConfig::builder().max_cycles(1).build();
    let interrupted = runner
        .run_with_config(&agent, "start", config)
        .await
        .expect("interrupted");
    assert_eq!(interrupted.status(), AgentStatus::WaitUser);

    let resumed = runner
        .resume_with_input(interrupted.into_state().expect("state"), "continue")
        .await
        .expect("resume");

    assert_eq!(resumed.status(), AgentStatus::MaxCycles);
    assert_eq!(
        resumed.final_output(),
        Some("Reached max cycles without finish signal.")
    );
    assert_eq!(
        resumed.result().cycles[0].tool_results[0].content,
        "ordinary success"
    );
}
