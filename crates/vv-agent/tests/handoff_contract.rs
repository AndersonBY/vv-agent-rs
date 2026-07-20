use std::sync::{Arc, Condvar, Mutex};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use vv_agent::{
    handoff, Agent, AgentStatus, ApprovalPolicy, GuardrailOutcome, InputGuardrail, LLMResponse,
    ModelRef, NormalizedInput, RunConfig, RunContext, Runner, ScriptStep, ScriptedModelProvider,
    StaticTool, ToolCall, ToolContext, ToolExecutionResult, ToolPolicy, ToolRegistry,
    ToolResultStatus, ToolUseBehavior,
};

const CONTRACT: &str = include_str!("fixtures/parity/handoff_contract_v1.json");
const CONTRACT_SHA256: &str = "c35c2335bd4a79626afca8459eb2966722f0539e1a0efc8014bd14b132100a74";

fn contract() -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(CONTRACT.as_bytes())),
        CONTRACT_SHA256
    );
    serde_json::from_str(CONTRACT).expect("handoff contract fixture")
}

#[test]
fn handoff_default_name_and_metadata_match_the_python_contract() {
    let target = Agent::builder("Research Agent")
        .instructions("Research.")
        .build()
        .expect("agent");

    let transfer = handoff(&target)
        .metadata("routing_group", json!("research"))
        .build();

    assert_eq!(transfer.tool_name(), "transfer_to_research_agent");
    assert_eq!(transfer.metadata()["routing_group"], json!("research"));
}

#[test]
fn handoff_tool_schema_and_marker_match_shared_contract() {
    let writer = Agent::builder("writer")
        .instructions("Write.")
        .model(ModelRef::named("writer"))
        .build()
        .expect("writer");
    let transfer = handoff(&writer)
        .description("Use for writing.")
        .metadata("routing_group", json!("writing"))
        .build();
    let mut registry = ToolRegistry::new();
    registry
        .register(transfer.as_tool_spec("triage"))
        .expect("handoff tool");
    let mut context = ToolContext::new("./workspace");

    assert_eq!(
        registry
            .get_schema("transfer_to_writer")
            .expect("handoff schema"),
        contract()["tool_schema"]
    );
    let result = registry
        .execute(
            &ToolCall::from_raw_arguments(
                "handoff-call",
                "transfer_to_writer",
                json!({"input": "write this"}),
            ),
            &mut context,
        )
        .expect("handoff result");
    assert_eq!(
        serde_json::from_str::<Value>(&result.content).expect("handoff JSON"),
        contract()["tool_result"]["content"]
    );
    assert_eq!(
        serde_json::to_value(&result.metadata).expect("metadata"),
        contract()["tool_result"]["metadata"]
    );

    let invalid = registry
        .execute(
            &ToolCall::from_raw_arguments("invalid", "transfer_to_writer", json!({"input": "   "})),
            &mut context,
        )
        .expect("invalid handoff result");
    assert_eq!(invalid.status, ToolResultStatus::Error);
    assert_eq!(
        invalid.error_code.as_deref(),
        Some("invalid_handoff_arguments")
    );
}

#[tokio::test]
async fn handoff_switches_runner_agent_and_completes_after_the_target_run() {
    let requested_models = Arc::new(Mutex::new(Vec::new()));
    let first_models = requested_models.clone();
    let second_models = requested_models.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "triage-model",
        vec![
            ScriptStep::callback(move |request| {
                first_models
                    .lock()
                    .expect("models")
                    .push(request.model.clone());
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "handoff-call",
                        "transfer_to_writer",
                        json!({"input": "write this"}),
                    )],
                ))
            }),
            ScriptStep::callback(move |request| {
                second_models
                    .lock()
                    .expect("models")
                    .push(request.model.clone());
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "writer-finish",
                        "task_finish",
                        json!({"message": "written by target"}),
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
        .model(ModelRef::backend("scripted", "writer-model"))
        .build()
        .expect("writer");
    let triage = Agent::builder("triage")
        .instructions("Route.")
        .model(ModelRef::backend("scripted", "triage-model"))
        .handoff(
            handoff(&writer)
                .description("Use for writing.")
                .metadata("routing_group", json!("writing")),
        )
        .build()
        .expect("triage");

    let result = runner.run(&triage, "please write").await.expect("run");

    assert_eq!(result.agent_name(), "writer");
    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), Some("written by target"));
    assert_eq!(
        *requested_models.lock().expect("models"),
        vec!["triage-model", "writer-model"]
    );

    let events = result
        .events()
        .iter()
        .map(|event| serde_json::to_value(event).expect("event JSON"))
        .collect::<Vec<_>>();
    let index_of = |event_type: &str, agent_name: Option<&str>| {
        events
            .iter()
            .position(|event| {
                event["type"] == event_type
                    && agent_name.is_none_or(|name| event["agent_name"] == name)
            })
            .unwrap_or_else(|| panic!("missing {event_type} event"))
    };
    let legacy_index = index_of("handoff", Some("triage"));
    let source_terminal_index = events
        .iter()
        .position(|event| {
            event["agent_name"] == "triage"
                && matches!(
                    event["type"].as_str(),
                    Some("run_completed" | "run_failed" | "run_cancelled")
                )
        })
        .expect("source terminal");
    let started_index = index_of("handoff_started", Some("triage"));
    let target_started_index = index_of("run_started", Some("writer"));
    let target_terminal_index = events
        .iter()
        .position(|event| {
            event["agent_name"] == "writer"
                && matches!(
                    event["type"].as_str(),
                    Some("run_completed" | "run_failed" | "run_cancelled")
                )
        })
        .expect("target terminal");
    let completed_index = index_of("handoff_completed", Some("triage"));
    assert!(legacy_index < source_terminal_index);
    assert!(source_terminal_index < started_index);
    assert!(started_index < target_started_index);
    assert!(target_started_index < target_terminal_index);
    assert!(target_terminal_index < completed_index);
    assert_eq!(
        events[completed_index]["child_run_id"],
        events[target_started_index]["run_id"]
    );
    assert_eq!(
        events[started_index]["metadata"]["routing_group"],
        "writing"
    );
    assert_eq!(
        events[completed_index]["metadata"]["routing_group"],
        "writing"
    );
}

struct BlockingGuardrail;

impl InputGuardrail for BlockingGuardrail {
    fn check(
        &self,
        _context: &RunContext,
        _input: &NormalizedInput,
    ) -> GuardrailOutcome<NormalizedInput> {
        GuardrailOutcome::Block {
            message: "writer blocked".to_string(),
        }
    }
}

#[tokio::test]
async fn handoff_target_guardrail_failure_is_the_final_result() {
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "triage-model",
        vec![ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "handoff-call",
                "transfer_to_writer",
                json!({"input": "write this"}),
            )],
        ))],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let writer = Agent::builder("writer")
        .instructions("Write.")
        .model(ModelRef::backend("scripted", "writer-model"))
        .input_guardrail(Arc::new(BlockingGuardrail))
        .build()
        .expect("writer");
    let triage = Agent::builder("triage")
        .instructions("Route.")
        .model(ModelRef::backend("scripted", "triage-model"))
        .handoff(handoff(&writer))
        .build()
        .expect("triage");

    let result = runner.run(&triage, "please write").await.expect("run");
    assert_eq!(result.agent_name(), "writer");
    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(result.final_output(), Some("writer blocked"));
    let completed = result
        .events()
        .iter()
        .map(|event| serde_json::to_value(event).expect("event"))
        .find(|event| event["type"] == "handoff_completed")
        .expect("handoff completed");
    assert_eq!(completed["status"], "failed");
    assert_eq!(completed["child_run_id"], result.run_id());
}

#[tokio::test]
async fn handoff_chain_enforces_independent_max_handoffs() {
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "first-model",
        vec![
            ScriptStep::response(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "handoff-first",
                    "transfer_to_middle",
                    json!({"input": "to middle"}),
                )],
            )),
            ScriptStep::response(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "handoff-middle",
                    "transfer_to_final",
                    json!({"input": "to final"}),
                )],
            )),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let final_agent = Agent::builder("final")
        .instructions("Finish.")
        .model(ModelRef::backend("scripted", "final-model"))
        .build()
        .expect("final");
    let middle = Agent::builder("middle")
        .instructions("Route again.")
        .model(ModelRef::backend("scripted", "middle-model"))
        .handoff(handoff(&final_agent))
        .build()
        .expect("middle");
    let first = Agent::builder("first")
        .instructions("Route.")
        .model(ModelRef::backend("scripted", "first-model"))
        .handoff(handoff(&middle))
        .build()
        .expect("first");

    let outcome = runner
        .run_with_config(
            &first,
            "start",
            RunConfig::builder().max_handoffs(1).build(),
        )
        .await;
    let error = match outcome {
        Ok(_) => panic!("expected handoff limit error"),
        Err(error) => error,
    };
    assert!(error.contains("maximum handoff depth exceeded"));
}

#[tokio::test]
async fn handoff_preserves_mutated_shared_state_for_target_tools() {
    let setter = StaticTool::new(
        "set_handoff_state",
        "Set handoff state.",
        json!({"type": "object", "properties": {}, "required": []}),
        Arc::new(|context: &mut ToolContext, _arguments| {
            context
                .shared_state
                .insert("handoff_value".to_string(), json!("preserved"));
            ToolExecutionResult::success(&context.tool_call_id, "set")
        }),
    );
    let reader = StaticTool::new(
        "read_handoff_state",
        "Read handoff state.",
        json!({"type": "object", "properties": {}, "required": []}),
        Arc::new(|context: &mut ToolContext, _arguments| {
            let value = context
                .shared_state
                .get("handoff_value")
                .and_then(Value::as_str)
                .unwrap_or_default();
            ToolExecutionResult::success(&context.tool_call_id, value)
        }),
    );
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "triage-model",
        vec![
            ScriptStep::response(LLMResponse::with_tool_calls(
                "",
                vec![
                    ToolCall::from_raw_arguments("set", "set_handoff_state", json!({})),
                    ToolCall::from_raw_arguments(
                        "handoff",
                        "transfer_to_writer",
                        json!({"input": "read the state"}),
                    ),
                ],
            )),
            ScriptStep::response(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "read",
                    "read_handoff_state",
                    json!({}),
                )],
            )),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let writer = Agent::builder("writer")
        .instructions("Read state.")
        .model(ModelRef::backend("scripted", "writer-model"))
        .tool(reader)
        .tool_use_behavior(ToolUseBehavior::StopOnFirstTool)
        .build()
        .expect("writer");
    let triage = Agent::builder("triage")
        .instructions("Set state and route.")
        .model(ModelRef::backend("scripted", "triage-model"))
        .tool(setter)
        .handoff(handoff(&writer))
        .build()
        .expect("triage");

    let result = runner
        .run_with_config(
            &triage,
            "start",
            RunConfig::builder()
                .initial_shared_state([("initial".to_string(), json!(true))].into_iter().collect())
                .build(),
        )
        .await
        .expect("run");

    assert_eq!(result.agent_name(), "writer");
    assert_eq!(result.final_output(), Some("preserved"));
    assert_eq!(result.result().shared_state["initial"], json!(true));
    assert_eq!(
        result.result().shared_state["handoff_value"],
        json!("preserved")
    );
}

#[tokio::test]
async fn run_handle_can_cancel_while_handoff_target_is_running() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let target_gate = gate.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "triage-model",
        vec![
            ScriptStep::response(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "handoff",
                    "transfer_to_writer",
                    json!({"input": "write"}),
                )],
            )),
            ScriptStep::callback(move |_| {
                let (lock, signal) = &*target_gate;
                let mut released = lock.lock().expect("gate");
                while !*released {
                    released = signal.wait(released).expect("gate wait");
                }
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "finish",
                        "task_finish",
                        json!({"message": "done"}),
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
        .model(ModelRef::backend("scripted", "writer-model"))
        .build()
        .expect("writer");
    let triage = Agent::builder("triage")
        .instructions("Route.")
        .model(ModelRef::backend("scripted", "triage-model"))
        .handoff(handoff(&writer))
        .build()
        .expect("triage");

    let handle = runner
        .start(&triage, "start", RunConfig::default())
        .await
        .expect("handle");
    let mut events = handle.events();
    let mut saw_target = false;
    while let Some(event) = events.next().await {
        let event = event.expect("event");
        if event.agent_name() == Some("writer")
            && matches!(
                event.payload(),
                vv_agent::RunEventPayload::RunStarted { .. }
            )
        {
            saw_target = true;
            assert!(handle.cancel());
            let (lock, signal) = &*gate;
            *lock.lock().expect("gate") = true;
            signal.notify_all();
        }
    }

    assert!(saw_target);
    let _ = handle.result().await;
    assert!(handle.state().cancelled);
}

#[tokio::test]
async fn approved_handoff_resume_switches_to_target_agent() {
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "triage-model",
        vec![
            ScriptStep::response(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "handoff",
                    "transfer_to_writer",
                    json!({"input": "write"}),
                )],
            )),
            ScriptStep::response(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "writer-finish",
                    "task_finish",
                    json!({"message": "written"}),
                )],
            )),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let writer = Agent::builder("writer")
        .instructions("Write.")
        .model(ModelRef::backend("scripted", "writer-model"))
        .build()
        .expect("writer");
    let triage = Agent::builder("triage")
        .instructions("Route.")
        .model(ModelRef::backend("scripted", "triage-model"))
        .handoff(handoff(&writer))
        .build()
        .expect("triage");
    let interrupted = runner
        .run_with_config(
            &triage,
            "start",
            RunConfig::builder()
                .tool_policy(ToolPolicy {
                    approval: ApprovalPolicy::Always,
                    ..ToolPolicy::default()
                })
                .build(),
        )
        .await
        .expect("interrupted");
    assert_eq!(interrupted.status(), AgentStatus::WaitUser);
    let interruption_id = interrupted
        .result()
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_results.iter())
        .find_map(|tool_result| {
            tool_result
                .metadata
                .get("approval_interruption_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .expect("approval interruption");
    let mut state = interrupted.into_state().expect("state");
    state.approve(&interruption_id).expect("approve");

    let resumed = runner.resume(state).await.expect("resume");

    assert_eq!(resumed.agent_name(), "writer");
    assert_eq!(resumed.status(), AgentStatus::WaitUser);
    let completed = resumed
        .events()
        .iter()
        .map(|event| serde_json::to_value(event).expect("event"))
        .find(|event| event["type"] == "handoff_completed")
        .expect("handoff completed");
    assert_eq!(completed["child_run_id"], resumed.run_id());
}
