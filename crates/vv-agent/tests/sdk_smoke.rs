#![allow(deprecated)]

use std::collections::BTreeMap;

use vv_agent::{
    AgentDefinition, AgentRuntime, AgentSDKClient, AgentSDKOptions, LLMResponse, NoToolPolicy,
    ScriptedLlmClient, ToolExecutionResult,
};

#[test]
fn sdk_client_can_run_a_simple_prompt() {
    let llm = ScriptedLlmClient::new(vec![LLMResponse::new("final answer")]);
    let runtime = AgentRuntime::new(llm);
    let client = AgentSDKClient::new(AgentSDKOptions::default()).with_runtime(runtime);
    let mut agent = AgentDefinition::default_for_model("demo");
    agent.no_tool_policy = NoToolPolicy::Finish;

    let run = client.run_with_agent(agent, "say hello").expect("run");

    assert_eq!(run.result.final_answer.as_deref(), Some("final answer"));
    assert_eq!(run.result.status, vv_agent::AgentStatus::Completed);

    let execution = ToolExecutionResult::success("call_1", "ok");
    assert_eq!(execution.to_message().content, "ok");
    let _ = BTreeMap::<String, String>::new();
}
