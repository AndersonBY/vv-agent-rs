use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{json, Value};
use vv_agent::{
    handoff, Agent, AgentResult, BeforeLlmEvent, BeforeLlmPatch, FunctionTool, GuardrailOutcome,
    InputGuardrail, InteractiveAgentClient, InteractiveSessionOptions, LLMResponse, LlmRequest,
    ModelRef, ModelSettings, NormalizedInput, OutputGuardrail, RunContext, RunEventPayload, Runner,
    RuntimeHook, ScriptStep, ScriptedModelProvider, SubAgentConfig, ToolCall, ToolOutput,
};

#[derive(Debug, Deserialize)]
struct TypedStatus {
    status: String,
}

struct RecordingInputGuardrail {
    calls: Arc<Mutex<Vec<String>>>,
}

impl InputGuardrail for RecordingInputGuardrail {
    fn check(
        &self,
        context: &RunContext,
        input: &NormalizedInput,
    ) -> GuardrailOutcome<NormalizedInput> {
        self.calls
            .lock()
            .expect("input guardrail calls")
            .push(format!("{}:{}", context.agent_name, input.text));
        GuardrailOutcome::Allow(input.clone())
    }
}

struct RewritingOutputGuardrail {
    calls: Arc<Mutex<Vec<String>>>,
}

impl OutputGuardrail for RewritingOutputGuardrail {
    fn check(&self, context: &RunContext, output: &AgentResult) -> GuardrailOutcome<AgentResult> {
        self.calls
            .lock()
            .expect("output guardrail calls")
            .push(format!(
                "{}:{}",
                context.agent_name,
                output.final_answer.as_deref().unwrap_or_default()
            ));
        let mut rewritten = output.clone();
        rewritten.final_answer = Some(r#"{"status":"guarded"}"#.to_string());
        GuardrailOutcome::Allow(rewritten)
    }
}

struct RecordingHook {
    calls: Arc<Mutex<Vec<String>>>,
}

impl RuntimeHook for RecordingHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        self.calls
            .lock()
            .expect("hook calls")
            .push(event.task.task_id.clone());
        None
    }
}

#[tokio::test]
async fn interactive_session_preserves_the_complete_public_agent() {
    let workspace = tempfile::tempdir().expect("workspace");
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let first_requests = requests.clone();
    let second_requests = requests.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "parent-model",
        vec![
            ScriptStep::callback(move |request| {
                first_requests
                    .lock()
                    .expect("requests")
                    .push(request.clone());
                Ok(LLMResponse::with_tool_calls(
                    "remember",
                    vec![ToolCall::from_raw_arguments(
                        "remember-call",
                        "remember",
                        json!({"value": "kept"}),
                    )],
                ))
            }),
            ScriptStep::callback(move |request| {
                second_requests
                    .lock()
                    .expect("requests")
                    .push(request.clone());
                Ok(LLMResponse::with_tool_calls(
                    "finish",
                    vec![ToolCall::from_raw_arguments(
                        "finish-call",
                        "task_finish",
                        json!({"message": r#"{"status":"ok"}"#}),
                    )],
                ))
            }),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let tool_calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured_tool_calls = tool_calls.clone();
    let remember = FunctionTool::builder("remember")
        .description("Remember a value.")
        .json_schema(json!({
            "type": "object",
            "properties": {"value": {"type": "string"}},
            "required": ["value"],
            "additionalProperties": false
        }))
        .handler(move |_context, arguments: Value| {
            let captured_tool_calls = captured_tool_calls.clone();
            async move {
                let value = arguments["value"].as_str().unwrap_or_default().to_string();
                captured_tool_calls
                    .lock()
                    .expect("tool calls")
                    .push(value.clone());
                Ok(ToolOutput::text(value))
            }
        })
        .build()
        .expect("remember tool");
    let dynamic_contexts = Arc::new(Mutex::new(Vec::new()));
    let captured_contexts = dynamic_contexts.clone();
    let input_guardrail_calls = Arc::new(Mutex::new(Vec::new()));
    let output_guardrail_calls = Arc::new(Mutex::new(Vec::new()));
    let hook_calls = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder("interactive-agent")
        .dynamic_instructions(move |context, current_agent| {
            captured_contexts.lock().expect("contexts").push((
                current_agent.name().to_string(),
                context.agent_name.clone(),
                context.model.clone(),
                context.workspace.clone(),
                context.metadata.clone(),
            ));
            "Dynamic instructions.".to_string()
        })
        .model(ModelRef::named("parent-model"))
        .model_settings(
            ModelSettings::builder()
                .temperature(0.25)
                .max_tokens(321)
                .build(),
        )
        .tool(remember)
        .input_guardrail(Arc::new(RecordingInputGuardrail {
            calls: input_guardrail_calls.clone(),
        }))
        .output_guardrail(Arc::new(RewritingOutputGuardrail {
            calls: output_guardrail_calls.clone(),
        }))
        .output_type::<TypedStatus>()
        .hook(Arc::new(RecordingHook {
            calls: hook_calls.clone(),
        }))
        .metadata("agent_marker", json!("kept"))
        .sub_agent(
            "researcher",
            SubAgentConfig::new("child-model", "Research the request."),
        )
        .build()
        .expect("agent");
    let session = InteractiveAgentClient::new(runner)
        .create_session(
            agent,
            InteractiveSessionOptions::new().session_id("public-agent-session"),
        )
        .await
        .expect("session");

    let result = session
        .prompt("preserve everything")
        .await
        .expect("interactive prompt");
    let typed: TypedStatus = result.deserialize().expect("typed output");

    assert_eq!(session.agent_name(), "interactive-agent");
    assert_eq!(result.agent_name(), "interactive-agent");
    assert_eq!(typed.status, "guarded");
    assert_eq!(*tool_calls.lock().expect("tool calls"), ["kept"]);
    assert_eq!(
        *input_guardrail_calls.lock().expect("input guardrail calls"),
        ["interactive-agent:preserve everything"]
    );
    assert_eq!(
        *output_guardrail_calls
            .lock()
            .expect("output guardrail calls"),
        [r#"interactive-agent:{"status":"ok"}"#]
    );
    assert_eq!(hook_calls.lock().expect("hook calls").len(), 2);
    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 2);
    let settings = requests[0].model_settings.as_ref().expect("model settings");
    assert_eq!(settings.temperature, Some(0.25));
    assert_eq!(settings.max_tokens, Some(321));
    let tool_names = requests[0]
        .tools
        .iter()
        .filter_map(|schema| schema.pointer("/function/name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"remember"));
    assert!(tool_names.contains(&"create_sub_task"));
    let contexts = dynamic_contexts.lock().expect("contexts");
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].0, "interactive-agent");
    assert_eq!(contexts[0].1, "interactive-agent");
    assert_eq!(contexts[0].2, Some(ModelRef::named("parent-model")));
    assert_eq!(contexts[0].3.as_deref(), Some(workspace.path()));
    assert_eq!(contexts[0].4["agent_marker"], "kept");
    assert_eq!(contexts[0].4["session_id"], "public-agent-session");
}

#[tokio::test]
async fn interactive_session_preserves_public_agent_handoffs() {
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "shared-model",
        vec![
            ScriptStep::callback(|_| {
                Ok(LLMResponse::with_tool_calls(
                    "transfer",
                    vec![ToolCall::from_raw_arguments(
                        "handoff-call",
                        "transfer_to_writer",
                        json!({"input": "write it"}),
                    )],
                ))
            }),
            ScriptStep::callback(|_| {
                Ok(LLMResponse::with_tool_calls(
                    "written",
                    vec![ToolCall::from_raw_arguments(
                        "writer-finish",
                        "task_finish",
                        json!({"message": "writer result"}),
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
    let writer = Agent::builder("writer")
        .instructions("Write.")
        .model(ModelRef::named("shared-model"))
        .build()
        .expect("writer");
    let triage = Agent::builder("triage")
        .instructions("Transfer.")
        .model(ModelRef::named("shared-model"))
        .handoff(handoff(&writer).description("Write the result."))
        .build()
        .expect("triage");
    let session = InteractiveAgentClient::new(runner)
        .create_session(triage, InteractiveSessionOptions::new())
        .await
        .expect("session");

    let result = session.prompt("route this").await.expect("handoff prompt");
    let lifecycle = result
        .events()
        .iter()
        .filter_map(|event| match event.payload() {
            RunEventPayload::HandoffStarted { .. } => Some("handoff_started"),
            RunEventPayload::HandoffCompleted { .. } => Some("handoff_completed"),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(result.agent_name(), "writer");
    assert_eq!(result.final_output(), Some("writer result"));
    assert_eq!(lifecycle, ["handoff_started", "handoff_completed"]);
}
