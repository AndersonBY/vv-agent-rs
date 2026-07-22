use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use vv_agent::{
    Agent, AgentStatus, ApprovalPolicy, CapabilityRef, CheckpointConfig, InMemoryCheckpointStore,
    LLMResponse, ModelRef, ModelSettings, NoToolPolicy, OutputValidationResult, ResumePolicy,
    RunConfig, RunEventPayload, RunResult, Runner, ScriptStep, ScriptedModelProvider, ToolCall,
    ToolPolicy, OUTPUT_VALIDATION_FAILED,
};

const FIXTURE: &str = include_str!("fixtures/parity/output_validation.json");

fn expected(case_name: &str) -> Value {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("output validation fixture");
    assert_eq!(fixture["schema_version"], "vv-agent.output-validation.v1");
    fixture["runner_cases"]
        .as_array()
        .expect("runner cases")
        .iter()
        .find(|case| case["name"] == case_name)
        .unwrap_or_else(|| panic!("missing fixture case {case_name}"))["expected"]
        .clone()
}

async fn run(agent: &Agent, output: &str) -> RunResult {
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "validation-model",
            vec![LLMResponse::new(output)],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    runner
        .run_with_config(
            agent,
            "return a value",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .build(),
        )
        .await
        .expect("output validation returns a typed run result")
}

fn agent_builder() -> vv_agent::agent::AgentBuilder {
    Agent::builder("output-validation-agent")
        .instructions("Return the requested value.")
        .model(ModelRef::named("validation-model"))
}

fn build_error(builder: vv_agent::agent::AgentBuilder) -> String {
    match builder.build() {
        Ok(_) => panic!("agent build unexpectedly succeeded"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn output_validation_is_disabled_by_default() {
    let expected = expected("disabled");
    let validator_calls = Arc::new(AtomicUsize::new(0));
    let calls = validator_calls.clone();
    let agent = agent_builder()
        .host_output_validator(move |_output, _context| {
            calls.fetch_add(1, Ordering::SeqCst);
            OutputValidationResult::reject_code("unexpected")
        })
        .build()
        .expect("agent");

    let result = run(&agent, "unchanged").await;

    assert_eq!(
        validator_calls.load(Ordering::SeqCst),
        expected["validator_calls"].as_u64().unwrap() as usize
    );
    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), Some("unchanged"));
}

#[tokio::test]
async fn valid_output_passes_without_repair() {
    let expected = expected("valid_without_repair");
    let validator_calls = Arc::new(AtomicUsize::new(0));
    let calls = validator_calls.clone();
    let agent = agent_builder()
        .output_validation_enabled(true)
        .host_output_validator(move |output, context| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(output, "valid");
            assert_eq!(context.agent_name, "output-validation-agent");
            OutputValidationResult::accept()
        })
        .build()
        .expect("agent");

    let result = run(&agent, "valid").await;

    assert_eq!(
        validator_calls.load(Ordering::SeqCst),
        expected["validator_calls"].as_u64().unwrap() as usize
    );
    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), expected["final_output"].as_str());
}

#[tokio::test]
async fn invalid_output_without_repair_is_typed_failure() {
    let expected = expected("invalid_without_repair_handler");
    let agent = agent_builder()
        .output_validation_enabled(true)
        .host_output_validator(|_output, _context| {
            OutputValidationResult::reject("format_invalid", Some("expected a valid marker"))
        })
        .build()
        .expect("agent");

    let result = run(&agent, "invalid").await;

    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(result.error_code(), expected["error_code"].as_str());
    assert_eq!(result.to_dict()["error_code"], expected["error_code"]);
    assert_eq!(
        result.result().to_dict()["error_code"],
        expected["error_code"]
    );
    assert!(result
        .final_output()
        .is_some_and(|error| error.starts_with(expected["error_code"].as_str().unwrap())));
    assert!(matches!(
        result.events().last().map(vv_agent::RunEvent::payload),
        Some(RunEventPayload::RunFailed { .. })
    ));
}

#[tokio::test]
async fn one_tools_free_repair_is_revalidated() {
    let expected = expected("one_repair_then_valid");
    let validator_calls = Arc::new(AtomicUsize::new(0));
    let repair_calls = Arc::new(AtomicUsize::new(0));
    let validation_counter = validator_calls.clone();
    let repair_counter = repair_calls.clone();
    let settings = ModelSettings::builder().temperature(0.0).build();
    let agent = agent_builder()
        .output_validation_enabled(true)
        .host_output_validator(move |output, _context| {
            validation_counter.fetch_add(1, Ordering::SeqCst);
            if output == "repaired" {
                OutputValidationResult::accept()
            } else {
                OutputValidationResult::reject("format_invalid", Some("repair required"))
            }
        })
        .output_repair_model(ModelRef::named("repair-model"))
        .output_repair_model_settings(settings.clone())
        .output_repair(move |request| {
            repair_counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.invalid_output, "invalid");
            assert_eq!(request.validation_code, "format_invalid");
            assert_eq!(request.model, Some(ModelRef::named("repair-model")));
            assert_eq!(request.model_settings, Some(settings.clone()));
            assert!(request.tools.is_empty());
            Ok("repaired".to_string())
        })
        .build()
        .expect("agent");

    let result = run(&agent, "invalid").await;

    assert_eq!(
        validator_calls.load(Ordering::SeqCst),
        expected["validator_calls"].as_u64().unwrap() as usize
    );
    assert_eq!(
        repair_calls.load(Ordering::SeqCst),
        expected["repair_calls"].as_u64().unwrap() as usize
    );
    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), expected["final_output"].as_str());
}

#[tokio::test]
async fn still_invalid_repair_does_not_attempt_a_second_repair() {
    let expected = expected("repair_result_still_invalid");
    let validator_calls = Arc::new(AtomicUsize::new(0));
    let repair_calls = Arc::new(AtomicUsize::new(0));
    let validation_counter = validator_calls.clone();
    let repair_counter = repair_calls.clone();
    let agent = agent_builder()
        .output_validation_enabled(true)
        .host_output_validator(move |_output, _context| {
            validation_counter.fetch_add(1, Ordering::SeqCst);
            OutputValidationResult::reject_code("still_invalid")
        })
        .output_repair(move |_request| {
            repair_counter.fetch_add(1, Ordering::SeqCst);
            Ok("still invalid".to_string())
        })
        .build()
        .expect("agent");

    let result = run(&agent, "invalid").await;

    assert_eq!(
        validator_calls.load(Ordering::SeqCst),
        expected["validator_calls"].as_u64().unwrap() as usize
    );
    assert_eq!(
        repair_calls.load(Ordering::SeqCst),
        expected["repair_calls"].as_u64().unwrap() as usize
    );
    assert_eq!(result.status(), AgentStatus::Failed);
}

#[tokio::test]
async fn repair_provider_failure_is_typed_validation_failure() {
    let expected = expected("repair_provider_failure");
    let repair_calls = Arc::new(AtomicUsize::new(0));
    let calls = repair_calls.clone();
    let agent = agent_builder()
        .output_validation_enabled(true)
        .host_output_validator(|_output, _context| OutputValidationResult::reject_code("invalid"))
        .output_repair(move |_request| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err("provider unavailable".to_string())
        })
        .build()
        .expect("agent");

    let result = run(&agent, "invalid").await;

    assert_eq!(
        repair_calls.load(Ordering::SeqCst),
        expected["repair_calls"].as_u64().unwrap() as usize
    );
    assert_eq!(result.status(), AgentStatus::Failed);
    assert!(result
        .final_output()
        .is_some_and(|error| error.contains("repair_provider_error")));
    assert!(result
        .final_output()
        .is_some_and(|error| error.starts_with(OUTPUT_VALIDATION_FAILED)));
}

#[test]
fn invalid_output_validation_builder_combinations_fail_closed() {
    assert!(build_error(agent_builder().output_validation_enabled(true))
        .contains("host_output_validator"));
    assert!(build_error(
        agent_builder().output_repair(|request| Ok(request.invalid_output.clone()))
    )
    .contains("host_output_validator"));
    assert!(
        build_error(agent_builder().output_validation_max_repairs(2)).contains("must be 0 or 1")
    );
}

#[tokio::test]
async fn terminal_checkpoint_replay_reuses_validated_result() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let validator_calls = Arc::new(AtomicUsize::new(0));
    let model_counter = model_calls.clone();
    let validator_counter = validator_calls.clone();
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::from_steps(
            "scripted",
            "validation-model",
            vec![ScriptStep::callback(move |_request| {
                model_counter.fetch_add(1, Ordering::SeqCst);
                Ok(LLMResponse::new("done"))
            })],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = agent_builder()
        .output_validation_enabled(true)
        .host_output_validator(move |output, _context| {
            validator_counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!(output, "done");
            OutputValidationResult::accept()
        })
        .build()
        .expect("agent");
    let store = InMemoryCheckpointStore::new();
    let mut checkpoint = CheckpointConfig::with_store(store);
    checkpoint.key = Some("output-validation-replay".to_string());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    checkpoint.capability_refs.insert(
        "output_validator".to_string(),
        CapabilityRef::new("tests.output-validation", "1").expect("capability ref"),
    );
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .checkpoint_config(checkpoint)
        .build();

    let first = runner
        .run_with_config(&agent, "return a value", config.clone())
        .await
        .expect("first run");
    let replay = runner
        .run_with_config(&agent, "return a value", config)
        .await
        .expect("terminal replay");

    assert_eq!(first.status(), AgentStatus::Completed);
    assert_eq!(replay.status(), AgentStatus::Completed);
    assert_eq!(replay.final_output(), Some("done"));
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert_eq!(validator_calls.load(Ordering::SeqCst), 1);
    assert!(!replay.events().iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::RunCompleted { .. } | RunEventPayload::RunFailed { .. }
    )));
}

#[tokio::test]
async fn terminal_checkpoint_replay_reuses_validation_failure() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let validator_calls = Arc::new(AtomicUsize::new(0));
    let model_counter = model_calls.clone();
    let validator_counter = validator_calls.clone();
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::from_steps(
            "scripted",
            "validation-model",
            vec![ScriptStep::callback(move |_request| {
                model_counter.fetch_add(1, Ordering::SeqCst);
                Ok(LLMResponse::new("invalid"))
            })],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = agent_builder()
        .output_validation_enabled(true)
        .host_output_validator(move |_output, _context| {
            validator_counter.fetch_add(1, Ordering::SeqCst);
            OutputValidationResult::reject_code("format_invalid")
        })
        .build()
        .expect("agent");
    let store = InMemoryCheckpointStore::new();
    let mut checkpoint = CheckpointConfig::with_store(store);
    checkpoint.key = Some("output-validation-failure-replay".to_string());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    checkpoint.capability_refs.insert(
        "output_validator".to_string(),
        CapabilityRef::new("tests.output-validation", "1").expect("capability ref"),
    );
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .checkpoint_config(checkpoint)
        .build();

    let first = runner
        .run_with_config(&agent, "return a value", config.clone())
        .await
        .expect("first run");
    let replay = runner
        .run_with_config(&agent, "return a value", config)
        .await
        .expect("terminal replay");

    assert_eq!(first.status(), AgentStatus::Failed);
    assert_eq!(first.error_code(), Some(OUTPUT_VALIDATION_FAILED));
    assert_eq!(replay.status(), AgentStatus::Failed);
    assert_eq!(replay.error_code(), Some(OUTPUT_VALIDATION_FAILED));
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert_eq!(validator_calls.load(Ordering::SeqCst), 1);
    assert!(!replay.events().iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::RunCompleted { .. } | RunEventPayload::RunFailed { .. }
    )));
}

#[tokio::test]
async fn approved_finish_validates_repaired_output_before_terminal_commit() {
    let validator_calls = Arc::new(AtomicUsize::new(0));
    let repair_calls = Arc::new(AtomicUsize::new(0));
    let validator_counter = validator_calls.clone();
    let repair_counter = repair_calls.clone();
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "validation-model",
            vec![LLMResponse::with_tool_calls(
                "draft",
                vec![ToolCall::from_raw_arguments(
                    "finish",
                    "task_finish",
                    json!({"message": "invalid"}),
                )],
            )],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = agent_builder()
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::Always,
            ..ToolPolicy::default()
        })
        .output_validation_enabled(true)
        .host_output_validator(move |output, _context| {
            validator_counter.fetch_add(1, Ordering::SeqCst);
            if output == "repaired" {
                OutputValidationResult::accept()
            } else {
                OutputValidationResult::reject_code("format_invalid")
            }
        })
        .output_repair(move |request| {
            repair_counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.invalid_output, "invalid");
            Ok("repaired".to_string())
        })
        .build()
        .expect("agent");

    let interrupted = runner
        .run(&agent, "finish after approval")
        .await
        .expect("wait");
    assert_eq!(interrupted.status(), AgentStatus::WaitUser);
    assert_eq!(validator_calls.load(Ordering::SeqCst), 0);
    let interruption_id = interrupted.approvals()[0].interruption_id.clone();
    let mut state = interrupted.into_state().expect("state");
    state.approve(&interruption_id).expect("approve");

    let resumed = runner.resume(state).await.expect("resume");

    assert_eq!(validator_calls.load(Ordering::SeqCst), 2);
    assert_eq!(repair_calls.load(Ordering::SeqCst), 1);
    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("repaired"));
    let terminal = resumed.events().last().expect("terminal event");
    assert!(matches!(
        terminal.payload(),
        RunEventPayload::RunCompleted { .. }
    ));
    assert_eq!(terminal.final_output(), Some("repaired"));
}
