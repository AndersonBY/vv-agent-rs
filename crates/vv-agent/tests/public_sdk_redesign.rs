use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::json;
use vv_agent::{
    handoff, Agent, AgentStatus, ApprovalPolicy, ExecutionMode, FunctionTool, LLMResponse,
    MemorySession, MessageRole, ModelRef, ModelSettings, NormalizedInput, RunConfig, RunContext,
    RunEvent, RunEventPayload, Runner, ScriptStep, ScriptedModelProvider, Session, SessionItem,
    SubTaskRequest, Tool, ToolCall, ToolContext, ToolOutput, ToolPolicy,
};

#[tokio::test]
async fn agent_runner_facade_runs_one_shot_with_scripted_provider() {
    let provider = ScriptedModelProvider::new(
        "scripted",
        "demo-model",
        vec![finish_response("facade final answer")],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("build runner");
    let agent = Agent::builder("assistant")
        .instructions("Answer directly.")
        .model(ModelRef::named("demo-model"))
        .model_settings(ModelSettings::builder().temperature(0.2).build())
        .build()
        .expect("build agent");

    let result = runner.run(&agent, "say hello").await.expect("run agent");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), Some("facade final answer"));
    assert_eq!(result.agent_name(), "assistant");
    assert_eq!(result.resolved_model().backend, "scripted");
    assert_eq!(result.resolved_model().selected_model, "demo-model");
}

#[tokio::test]
async fn run_config_overrides_agent_model_settings_and_workspace() {
    let captured_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let requests = captured_requests.clone();
    let provider =
        ScriptedModelProvider::from_callback("scripted", "override-model", move |request| {
            requests.lock().expect("lock").push(request.clone());
            Ok(finish_response("override answer"))
        });
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./runner-workspace")
        .build()
        .expect("build runner");
    let agent = Agent::builder("ops")
        .instructions("Use the requested model.")
        .model(ModelRef::named("agent-model"))
        .model_settings(ModelSettings::builder().temperature(0.8).build())
        .build()
        .expect("build agent");
    let config = RunConfig::builder()
        .model(ModelRef::backend("scripted", "override-model"))
        .workspace("./config-workspace")
        .model_settings(
            ModelSettings::builder()
                .temperature(0.1)
                .max_output_tokens(512)
                .extra_body("reasoning", json!({"effort": "low"}))
                .build(),
        )
        .max_cycles(3)
        .build();

    let result = runner
        .run_with_config(&agent, "inspect workspace", config)
        .await
        .expect("run agent with config");

    assert_eq!(result.final_output(), Some("override answer"));
    assert_eq!(result.resolved_model().selected_model, "override-model");
    let requests = captured_requests.lock().expect("lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model, "override-model");
    let temperature = requests[0].metadata["model_settings"]["temperature"]
        .as_f64()
        .expect("temperature");
    assert!((temperature - 0.1).abs() < 0.0001);
    assert_eq!(
        requests[0].metadata["model_settings"]["max_output_tokens"],
        json!(512)
    );
    assert_eq!(
        requests[0].metadata["model_settings"]["extra_body"]["reasoning"],
        json!({"effort": "low"})
    );
}

#[tokio::test]
async fn guardrails_rewrite_input_and_block_forbidden_output() {
    struct RewriteInput;
    impl vv_agent::InputGuardrail for RewriteInput {
        fn check(
            &self,
            _ctx: &RunContext,
            _input: &NormalizedInput,
        ) -> vv_agent::GuardrailOutcome<NormalizedInput> {
            vv_agent::GuardrailOutcome::Allow(NormalizedInput::from("rewritten prompt"))
        }
    }
    struct BlockForbiddenOutput;
    impl vv_agent::OutputGuardrail for BlockForbiddenOutput {
        fn check(
            &self,
            _ctx: &RunContext,
            output: &vv_agent::AgentResult,
        ) -> vv_agent::GuardrailOutcome<vv_agent::AgentResult> {
            if output.final_answer.as_deref() == Some("forbidden") {
                return vv_agent::GuardrailOutcome::Block {
                    message: "blocked final output".to_string(),
                };
            }
            vv_agent::GuardrailOutcome::Allow(output.clone())
        }
    }

    let captured_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let requests = captured_requests.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "guard-model",
        vec![
            ScriptStep::callback(move |request| {
                requests.lock().expect("lock").push(request.clone());
                assert!(request
                    .messages
                    .iter()
                    .any(|message| message.role == MessageRole::User
                        && message.content == "rewritten prompt"));
                Ok(finish_response("allowed"))
            }),
            ScriptStep::callback(|_request| Ok(finish_response("forbidden"))),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("guarded")
        .instructions("Respect guardrails.")
        .model(ModelRef::backend("scripted", "guard-model"))
        .input_guardrail(Arc::new(RewriteInput))
        .output_guardrail(Arc::new(BlockForbiddenOutput))
        .build()
        .expect("agent");

    let allowed = runner
        .run(&agent, "original prompt")
        .await
        .expect("allowed");
    assert_eq!(allowed.final_output(), Some("allowed"));
    assert_eq!(captured_requests.lock().expect("lock").len(), 1);

    let blocked = runner
        .run(&agent, "another original prompt")
        .await
        .expect("blocked result");
    assert_eq!(blocked.status(), AgentStatus::Failed);
    assert_eq!(
        blocked.result().error.as_deref(),
        Some("blocked final output")
    );
}

#[tokio::test]
async fn trace_jsonl_exporter_records_run_and_agent_spans() {
    let trace_path =
        std::env::temp_dir().join(format!("vv-agent-trace-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&trace_path);
    let provider = ScriptedModelProvider::new(
        "scripted",
        "trace-model",
        vec![finish_response("trace done")],
    );
    let trace = vv_agent::JsonlTraceExporter::new(&trace_path).expect("trace exporter");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .default_run_config(RunConfig::builder().trace_sink(Arc::new(trace)).build())
        .build()
        .expect("runner");
    let agent = Agent::builder("traced")
        .instructions("Emit trace spans.")
        .model(ModelRef::backend("scripted", "trace-model"))
        .build()
        .expect("agent");

    let result = runner.run(&agent, "trace please").await.expect("run");

    assert_eq!(result.final_output(), Some("trace done"));
    let contents = std::fs::read_to_string(&trace_path).expect("trace file");
    let values = contents
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json line"))
        .collect::<Vec<_>>();
    assert!(values
        .iter()
        .any(|value| value["event"] == "span_start" && value["span"]["name"] == "run"));
    assert!(values
        .iter()
        .any(|value| value["event"] == "span_end" && value["span"]["name"] == "agent"));
    let _ = std::fs::remove_file(&trace_path);
}

#[tokio::test]
async fn session_store_conformance_accepts_sqlite_store() {
    let store = vv_agent::SqliteSessionStore::open_memory().expect("store");

    vv_agent::session_store_conformance(&store)
        .await
        .expect("conformance");
}

#[tokio::test]
async fn sqlite_session_store_persists_items_across_reopen() {
    let db = tempfile::NamedTempFile::new().expect("temp sqlite db");
    let store = vv_agent::SqliteSessionStore::open(db.path()).expect("store");
    let session = store.session("thread-persist");
    session.clear().await.expect("clear");
    session
        .add_items(vec![
            SessionItem::User {
                content: "hello".to_string(),
            },
            SessionItem::Tool {
                content: "tool output".to_string(),
                tool_call_id: "call_1".to_string(),
            },
        ])
        .await
        .expect("append");
    drop(store);

    let reopened = vv_agent::SqliteSessionStore::open(db.path()).expect("reopen");
    let items = reopened
        .session("thread-persist")
        .get_items(None)
        .await
        .expect("items");

    assert_eq!(
        items,
        vec![
            SessionItem::User {
                content: "hello".to_string(),
            },
            SessionItem::Tool {
                content: "tool output".to_string(),
                tool_call_id: "call_1".to_string(),
            },
        ]
    );
}

#[test]
fn model_ref_and_run_event_are_serializable_public_contracts() {
    assert_eq!(ModelRef::named("demo").model(), "demo");
    assert_eq!(
        ModelRef::backend("backend-a", "demo").backend_name(),
        Some("backend-a")
    );

    let event = RunEvent::run_completed("run_1", "trace_1", "assistant", AgentStatus::Completed);
    let encoded = serde_json::to_value(&event).expect("serialize event");
    assert_eq!(encoded["type"], "run_completed");
    let decoded: RunEvent = serde_json::from_value(encoded).expect("deserialize event");
    assert_eq!(decoded.run_id(), "run_1");
}

#[test]
fn execution_mode_is_the_public_backend_facade_for_run_config() {
    let default_mode = ExecutionMode::default();
    assert!(matches!(default_mode, ExecutionMode::Inline));

    let config = RunConfig::builder()
        .execution_mode(ExecutionMode::Threaded { max_workers: 2 })
        .build();

    assert!(matches!(
        config.execution_backend.expect("execution backend"),
        vv_agent::RuntimeExecutionBackend::Thread(_)
    ));
}

#[test]
fn tool_output_supports_structured_payloads() {
    assert_eq!(
        ToolOutput::text("hello").to_result("call_1").content,
        "hello"
    );
    assert_eq!(
        ToolOutput::json(json!({"ok": true}))
            .to_result("call_2")
            .metadata["output_type"],
        json!("json")
    );
    let error = ToolOutput::error("bad input")
        .with_code("invalid_input")
        .retryable(true)
        .to_result("call_3");
    assert_eq!(error.error_code.as_deref(), Some("invalid_input"));
    assert_eq!(error.metadata["retryable"], json!(true));
}

#[tokio::test]
async fn function_tool_parses_typed_args_and_returns_structured_output() {
    #[derive(Debug, Deserialize)]
    struct EchoArgs {
        message: String,
    }

    let tool = FunctionTool::builder("echo_json")
        .description("Echo typed args.")
        .json_schema(json!({
            "type": "object",
            "properties": {
                "message": {"type": "string"}
            },
            "required": ["message"]
        }))
        .handler(|_ctx, args: EchoArgs| async move {
            Ok(ToolOutput::json(json!({"echo": args.message})))
        })
        .build()
        .expect("build function tool");
    let provider = ScriptedModelProvider::new(
        "scripted",
        "demo-model",
        vec![
            LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "call_echo",
                    "echo_json",
                    json!({"message": "hello"}),
                )],
            ),
            finish_response("tool completed"),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("build runner");
    let agent = Agent::builder("tool-user")
        .instructions("Call echo_json, then finish.")
        .model(ModelRef::named("demo-model"))
        .tool(tool)
        .build()
        .expect("build agent");

    let result = runner.run(&agent, "echo hello").await.expect("run");

    assert_eq!(result.final_output(), Some("tool completed"));
    let first_cycle = &result.result().cycles[0];
    assert_eq!(
        first_cycle.tool_results[0].metadata["output_type"],
        json!("json")
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&first_cycle.tool_results[0].content)
            .expect("json output"),
        json!({"echo": "hello"})
    );
}

#[tokio::test]
async fn runner_stream_returns_typed_runtime_events_before_result() {
    let provider = ScriptedModelProvider::new(
        "scripted",
        "demo-model",
        vec![
            LLMResponse::with_tool_calls(
                "calling tool",
                vec![ToolCall::from_raw_arguments(
                    "call_echo",
                    "echo_json",
                    json!({"message": "stream"}),
                )],
            ),
            finish_response("stream done"),
        ],
    );
    let echo = FunctionTool::builder("echo_json")
        .description("Echo typed args.")
        .json_schema(json!({
            "type": "object",
            "properties": {"message": {"type": "string"}},
            "required": ["message"]
        }))
        .handler(|_ctx, args: serde_json::Value| async move { Ok(ToolOutput::json(args)) })
        .build()
        .expect("build tool");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("build runner");
    let agent = Agent::builder("stream-agent")
        .instructions("Call echo_json, then finish.")
        .model(ModelRef::named("demo-model"))
        .tool(echo)
        .build()
        .expect("build agent");

    let mut stream = runner
        .stream(&agent, "stream please")
        .await
        .expect("stream");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("event"));
    }
    let result = stream.into_result().await.expect("stream result");

    assert_eq!(result.final_output(), Some("stream done"));
    assert!(events
        .iter()
        .any(|event| event.agent_name() == Some("stream-agent")
            && matches!(event.payload(), RunEventPayload::RunStarted { .. })));
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::AssistantDelta { delta } if delta == "calling tool"
    )));
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::ToolCallCompleted { tool_name, .. } if tool_name == "echo_json"
    )));
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::RunCompleted { status } if *status == AgentStatus::Completed
    )));
}

#[tokio::test]
async fn memory_session_persists_context_across_runner_calls() {
    let captured_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let requests = captured_requests.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "demo-model",
        vec![
            vv_agent::ScriptStep::callback(move |request| {
                requests.lock().expect("lock").push(request.clone());
                Ok(finish_response("first"))
            }),
            vv_agent::ScriptStep::callback(|request| {
                assert!(request
                    .messages
                    .iter()
                    .any(|message| message.role == MessageRole::User
                        && message.content == "first prompt"));
                Ok(finish_response("second"))
            }),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("build runner");
    let agent = Agent::builder("session-agent")
        .instructions("Use prior context when available.")
        .model(ModelRef::named("demo-model"))
        .build()
        .expect("build agent");
    let session = MemorySession::new("thread-001");

    runner
        .run_with_config(
            &agent,
            "first prompt",
            RunConfig::builder().session(session.clone()).build(),
        )
        .await
        .expect("first run");
    let second = runner
        .run_with_config(
            &agent,
            "second prompt",
            RunConfig::builder().session(session.clone()).build(),
        )
        .await
        .expect("second run");

    assert_eq!(second.final_output(), Some("second"));
    let items = session.get_items(None).await.expect("items");
    assert!(items
        .iter()
        .any(|item| matches!(item, SessionItem::User { content } if content == "first prompt")));
    assert!(items
        .iter()
        .any(|item| matches!(item, SessionItem::Assistant { content } if content == "second")));
    assert_eq!(captured_requests.lock().expect("lock").len(), 1);
}

#[test]
fn agent_as_tool_builds_public_tool_contract() {
    let child = Agent::builder("researcher")
        .instructions("Collect facts and return a concise summary.")
        .model(ModelRef::backend("scripted", "research-model"))
        .build()
        .expect("build child");

    let tool = child
        .as_tool()
        .name("research")
        .description("Delegate fact collection.")
        .build()
        .expect("agent tool");

    assert_eq!(tool.name(), "research");
    assert_eq!(tool.description(), "Delegate fact collection.");
    assert_eq!(
        tool.parameters_schema()["properties"]["task_description"]["type"],
        "string"
    );

    let request = tool
        .request_from_arguments(json!({"task_description": "summarize README"}))
        .expect("request");
    assert_eq!(request.agent_name, "researcher");
    assert_eq!(request.task_description, "summarize README");
    let _: SubTaskRequest = request;
}

#[tokio::test]
async fn agent_as_tool_runs_child_agent_and_returns_output_to_parent() {
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "parent-model",
        vec![
            ScriptStep::callback(|request| {
                assert_eq!(request.model, "parent-model");
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "call_research",
                        "research",
                        json!({"task_description": "summarize sdk redesign"}),
                    )],
                ))
            }),
            ScriptStep::callback(|request| {
                assert_eq!(request.model, "child-model");
                assert!(request
                    .messages
                    .iter()
                    .any(|message| message.role == MessageRole::System
                        && message.content.contains("Collect facts")));
                Ok(finish_response("child facts"))
            }),
            ScriptStep::callback(|request| {
                assert_eq!(request.model, "parent-model");
                assert!(request
                    .messages
                    .iter()
                    .any(|message| message.role == MessageRole::Tool
                        && message.content.contains("child facts")));
                Ok(finish_response("parent used child facts"))
            }),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let child = Agent::builder("researcher")
        .instructions("Collect facts for the parent.")
        .model(ModelRef::backend("scripted", "child-model"))
        .build()
        .expect("child");
    let parent = Agent::builder("writer")
        .instructions("Call research, then finish with the child facts.")
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

    let result = runner
        .run(&parent, "write from research")
        .await
        .expect("run");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), Some("parent used child facts"));
}

#[tokio::test]
async fn agent_background_task_returns_pollable_task_handle() {
    let provider = ScriptedModelProvider::new(
        "scripted",
        "draft-model",
        vec![finish_response("background draft")],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("build runner");
    let drafter = Agent::builder("drafter")
        .instructions("Draft the requested report.")
        .model(ModelRef::backend("scripted", "draft-model"))
        .build()
        .expect("build drafter");
    let background_tool = drafter
        .as_background_task()
        .name("draft_report")
        .description("Start a draft report in the background.")
        .build()
        .expect("background tool");

    assert_eq!(background_tool.name(), "draft_report");
    assert_eq!(
        background_tool.parameters_schema()["properties"]["task_description"]["type"],
        "string"
    );

    let mut context = ToolContext::new("./workspace");
    let start = background_tool
        .start(
            &runner,
            &mut context,
            json!({"task_description": "draft the sdk redesign report"}),
        )
        .expect("start background task");
    assert_eq!(start.agent_name(), "drafter");
    assert_eq!(start.status(), AgentStatus::Running);
    assert!(start.task_id().starts_with("bg_agent_"));

    let completed = start.wait().await.expect("wait for result");
    assert_eq!(completed.status(), AgentStatus::Completed);
    assert_eq!(completed.final_output(), Some("background draft"));

    let polled = start.poll().expect("poll completed result");
    assert_eq!(polled.status(), AgentStatus::Completed);
    assert_eq!(polled.final_output(), Some("background draft"));
}

#[tokio::test]
async fn tool_approval_interrupts_run_and_resume_executes_approved_call() {
    #[derive(Debug, Deserialize)]
    struct DeleteArgs {
        path: String,
    }

    let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let executed_for_tool = executed.clone();
    let delete_file = FunctionTool::builder("delete_file")
        .description("Delete a file.")
        .json_schema(json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }))
        .handler(move |_ctx, args: DeleteArgs| {
            let executed = executed_for_tool.clone();
            async move {
                executed.lock().expect("lock").push(args.path.clone());
                Ok(ToolOutput::text(format!("deleted {}", args.path)))
            }
        })
        .build()
        .expect("tool");
    let provider = ScriptedModelProvider::from_callback("scripted", "approval-model", |_request| {
        Ok(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "call_delete",
                "delete_file",
                json!({"path": "danger.txt"}),
            )],
        ))
    });
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("approver")
        .instructions("Delete only after approval.")
        .model(ModelRef::backend("scripted", "approval-model"))
        .tool(delete_file)
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::Always,
            ..ToolPolicy::default()
        })
        .build()
        .expect("agent");

    let mut stream = runner
        .stream(&agent, "delete danger.txt")
        .await
        .expect("stream");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("event"));
    }
    let result = stream.into_result().await.expect("approval result");

    assert_eq!(result.status(), AgentStatus::WaitUser);
    assert!(executed.lock().expect("lock").is_empty());
    let requested = events.iter().find_map(|event| match event.payload() {
        RunEventPayload::ApprovalRequested {
            request_id,
            tool_name,
            ..
        } if tool_name == "delete_file" => Some(request_id.clone()),
        _ => None,
    });
    let interruption_id = requested.expect("approval requested event");
    let mut state = result.into_state().expect("run state");
    state.approve(&interruption_id).expect("approve call");

    let resumed = runner.resume(state).await.expect("resume");

    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("deleted danger.txt"));
    assert_eq!(
        executed.lock().expect("lock").as_slice(),
        &["danger.txt".to_string()]
    );
}

#[tokio::test]
async fn handoff_switches_current_agent_and_emits_typed_event() {
    let captured_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let requests = captured_requests.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "triage-model",
        vec![
            ScriptStep::callback(move |request| {
                requests.lock().expect("lock").push(request.clone());
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "handoff_research",
                        "transfer_to_researcher",
                        json!({"input": "research the sdk redesign"}),
                    )],
                ))
            }),
            ScriptStep::callback(|request| {
                assert_eq!(request.model, "research-model");
                assert!(request
                    .messages
                    .iter()
                    .any(|message| message.role == MessageRole::System
                        && message.content.contains("Collect facts")));
                assert!(request
                    .messages
                    .iter()
                    .any(|message| message.role == MessageRole::User
                        && message.content == "research the sdk redesign"));
                Ok(finish_response("researcher final"))
            }),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("build runner");
    let researcher = Agent::builder("researcher")
        .instructions("Collect facts and summarize them.")
        .model(ModelRef::backend("scripted", "research-model"))
        .build()
        .expect("build researcher");
    let triage = Agent::builder("triage")
        .instructions("Route work to the right specialist.")
        .model(ModelRef::backend("scripted", "triage-model"))
        .handoff(handoff(&researcher).description("Use for research tasks"))
        .build()
        .expect("build triage");

    let mut stream = runner
        .stream(&triage, "please research the sdk redesign")
        .await
        .expect("stream");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("event"));
    }
    let result = stream.into_result().await.expect("result");

    assert_eq!(result.agent_name(), "researcher");
    assert_eq!(result.final_output(), Some("researcher final"));
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::HandoffCompleted {
            source_agent,
            target_agent,
            ..
        } if source_agent == "triage" && target_agent == "researcher"
    )));
    assert_eq!(captured_requests.lock().expect("lock").len(), 1);
}

fn finish_response(message: &str) -> LLMResponse {
    let mut args = BTreeMap::new();
    args.insert("message".to_string(), json!(message));
    LLMResponse::with_tool_calls("", vec![ToolCall::new("finish", "task_finish", args)])
}
