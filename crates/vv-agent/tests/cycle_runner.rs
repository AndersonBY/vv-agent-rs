use serde_json::json;
use vv_agent::{
    build_default_registry, CycleRunRequest, CycleRunner, LLMResponse, MemoryManager,
    MemoryManagerConfig, Message, ScriptedLlmClient,
};

#[test]
fn cycle_runner_public_api_builds_assistant_message() {
    let mut response = LLMResponse::new("cycle done");
    response
        .raw
        .insert("reasoning_content".to_string(), json!("cycle reasoning"));
    let runner = CycleRunner::new(
        ScriptedLlmClient::new(vec![response]),
        build_default_registry(),
    );
    let task = vv_agent::AgentTask::new("cycle_api", "demo", "system", "prompt");
    let mut memory_manager = MemoryManager::new(MemoryManagerConfig::default());

    let (messages, cycle) = runner
        .run_cycle(CycleRunRequest::new(
            &task,
            vec![Message::system("system"), Message::user("prompt")],
            1,
            &mut memory_manager,
        ))
        .expect("cycle");

    assert_eq!(cycle.index, 1);
    assert_eq!(cycle.assistant_message, "cycle done");
    assert_eq!(messages.last().expect("assistant").content, "cycle done");
    assert_eq!(
        messages
            .last()
            .expect("assistant")
            .reasoning_content
            .as_deref(),
        Some("cycle reasoning")
    );
}
