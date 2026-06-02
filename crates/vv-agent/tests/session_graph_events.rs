use serde_json::json;
use vv_agent::{
    handoff, Agent, LLMResponse, ModelRef, RunEventPayload, Runner, ScriptStep,
    ScriptedModelProvider, ToolCall,
};

#[tokio::test]
async fn agent_as_tool_emits_sub_run_parent_lineage() {
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "parent-model",
        vec![
            ScriptStep::callback(|_| {
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "call_research",
                        "research",
                        json!({"task_description":"facts"}),
                    )],
                ))
            }),
            ScriptStep::callback(|_| {
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "finish_child",
                        "task_finish",
                        json!({"message":"child facts"}),
                    )],
                ))
            }),
            ScriptStep::callback(|_| {
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "finish_parent",
                        "task_finish",
                        json!({"message":"done"}),
                    )],
                ))
            }),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let child = Agent::builder("researcher")
        .instructions("Research.")
        .model(ModelRef::backend("scripted", "child-model"))
        .build()
        .expect("child");
    let parent = Agent::builder("writer")
        .instructions("Call research.")
        .model(ModelRef::backend("scripted", "parent-model"))
        .tool(
            child
                .as_tool()
                .name("research")
                .description("Research facts.")
                .build()
                .expect("tool"),
        )
        .build()
        .expect("parent");

    let mut stream = runner.stream(&parent, "go").await.expect("stream");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("event"));
    }
    let result = stream.into_result().await.expect("result");

    assert_eq!(result.final_output(), Some("done"));
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::SubRunStarted { parent_tool_call_id, .. } if parent_tool_call_id == "call_research"
    )));
    assert!(events.iter().any(|event| event.parent_run_id().is_some()));
}

#[tokio::test]
async fn background_agent_tool_emits_sub_run_parent_lineage() {
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "parent-model",
        vec![
            ScriptStep::callback(|_| {
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "call_draft",
                        "draft_report",
                        json!({"task_description":"draft facts"}),
                    )],
                ))
            }),
            ScriptStep::callback(|_| {
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "finish_parent",
                        "task_finish",
                        json!({"message":"queued"}),
                    )],
                ))
            }),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let drafter = Agent::builder("drafter")
        .instructions("Draft.")
        .model(ModelRef::backend("scripted", "draft-model"))
        .build()
        .expect("drafter");
    let parent = Agent::builder("writer")
        .instructions("Start background draft.")
        .model(ModelRef::backend("scripted", "parent-model"))
        .tool(
            drafter
                .as_background_task()
                .name("draft_report")
                .description("Draft in background.")
                .build()
                .expect("background tool"),
        )
        .build()
        .expect("parent");

    let mut stream = runner.stream(&parent, "go").await.expect("stream");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("event"));
    }
    let result = stream.into_result().await.expect("result");

    assert_eq!(result.final_output(), Some("queued"));
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::SubRunStarted { parent_tool_call_id, .. } if parent_tool_call_id == "call_draft"
    )));
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::SubRunCompleted { parent_tool_call_id, .. } if parent_tool_call_id == "call_draft"
    )));
    assert!(events.iter().any(|event| event.parent_run_id().is_some()));
}

#[tokio::test]
async fn handoff_emits_started_and_completed_events() {
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "triage-model",
        vec![
            ScriptStep::callback(|_| {
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "handoff_call",
                        "transfer_to_researcher",
                        json!({"input":"research facts"}),
                    )],
                ))
            }),
            ScriptStep::callback(|_| {
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "finish_research",
                        "task_finish",
                        json!({"message":"researcher final"}),
                    )],
                ))
            }),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let researcher = Agent::builder("researcher")
        .instructions("Research.")
        .model(ModelRef::backend("scripted", "research-model"))
        .build()
        .expect("researcher");
    let triage = Agent::builder("triage")
        .instructions("Route.")
        .model(ModelRef::backend("scripted", "triage-model"))
        .handoff(handoff(&researcher))
        .build()
        .expect("triage");

    let mut stream = runner.stream(&triage, "go").await.expect("stream");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("event"));
    }

    assert!(events.iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::HandoffStarted { source_agent, target_agent, .. }
            if source_agent == "triage" && target_agent == "researcher"
    )));
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::HandoffCompleted { source_agent, target_agent, .. }
            if source_agent == "triage" && target_agent == "researcher"
    )));
}
