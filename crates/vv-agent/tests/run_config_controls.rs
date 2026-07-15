use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vv_agent::tools::build_default_registry;
use vv_agent::{
    Agent, AgentStatus, LLMResponse, LlmClient, LlmError, LlmRequest, ModelError, ModelProvider,
    ModelRef, ResolvedModelConfig, RunConfig, Runner, SubTaskManager, SubTaskOutcome, ToolCall,
    ToolExecutionResult,
};

const CONTROL_CONTRACT: &str = include_str!("fixtures/parity/run_config_controls_v1.json");

#[derive(Clone)]
struct ProbeClient {
    step: Arc<AtomicUsize>,
    debug_paths: Arc<Mutex<Vec<PathBuf>>>,
}

impl LlmClient for ProbeClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.complete_with_stream(request, None)
    }

    fn clone_with_debug_dump_dir(&self, debug_dump_dir: &Path) -> Option<Arc<dyn LlmClient>> {
        self.debug_paths
            .lock()
            .expect("debug paths")
            .push(debug_dump_dir.to_path_buf());
        Some(Arc::new(self.clone()))
    }

    fn complete_with_stream(
        &self,
        request: LlmRequest,
        stream_callback: Option<vv_agent::LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        assert!(request
            .messages
            .iter()
            .any(|message| message.content == "injected before cycle"));
        if let Some(callback) = stream_callback {
            callback(&BTreeMap::from([
                ("type".to_string(), json!("assistant_delta")),
                ("delta".to_string(), json!("streamed")),
            ]));
        }
        match self.step.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(LLMResponse::with_tool_calls(
                "x".repeat(100),
                vec![
                    ToolCall::from_raw_arguments("probe_1", "probe_tool", json!({})),
                    ToolCall::from_raw_arguments("probe_2", "probe_tool", json!({})),
                ],
            )),
            1 => Ok(LLMResponse::with_tool_calls(
                "done",
                vec![ToolCall::from_raw_arguments(
                    "finish",
                    "task_finish",
                    json!({"message": "done"}),
                )],
            )),
            _ => Err(LlmError::ScriptExhausted),
        }
    }
}

#[derive(Clone)]
struct ProbeProvider {
    client: ProbeClient,
}

impl ModelProvider for ProbeProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        Ok(ResolvedModelConfig::new(
            "probe",
            model.model(),
            model.model(),
            "resolved-probe-model",
            Vec::new(),
        )
        .with_capabilities(true, true, false))
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        Ok(Arc::new(self.client.clone()))
    }
}

#[tokio::test]
async fn run_config_wires_per_run_registry_debug_and_runtime_controls() {
    let debug_paths = Arc::new(Mutex::new(Vec::new()));
    let provider = ProbeProvider {
        client: ProbeClient {
            step: Arc::new(AtomicUsize::new(0)),
            debug_paths: debug_paths.clone(),
        },
    };
    let manager = SubTaskManager::default();
    manager.record_outcome(
        "preloaded",
        SubTaskOutcome {
            task_id: "preloaded".to_string(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Completed,
            session_id: Some("session-preloaded".to_string()),
            final_answer: Some("ready".to_string()),
            wait_reason: None,
            error: None,
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
            cycles: 1,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        },
    );

    let factory_calls = Arc::new(AtomicUsize::new(0));
    let tool_runs = Arc::new(AtomicUsize::new(0));
    let factory_calls_for_config = factory_calls.clone();
    let tool_runs_for_factory = tool_runs.clone();
    let log_events = Arc::new(Mutex::new(Vec::<(String, BTreeMap<String, Value>)>::new()));
    let log_events_for_config = log_events.clone();
    let stream_payloads = Arc::new(Mutex::new(Vec::<BTreeMap<String, Value>>::new()));
    let stream_payloads_for_config = stream_payloads.clone();
    let interruption_used = Arc::new(AtomicBool::new(false));
    let interruption_used_for_config = interruption_used.clone();
    let debug_dir = tempfile::tempdir().expect("debug dir");

    let config = RunConfig::builder()
        .model_provider(provider)
        .tool_registry_factory(move || {
            factory_calls_for_config.fetch_add(1, Ordering::SeqCst);
            let mut registry = build_default_registry();
            let tool_runs = tool_runs_for_factory.clone();
            registry
                .register_tool(
                    "probe_tool",
                    "Probe per-run controls.",
                    Arc::new(move |context, _arguments| {
                        assert!(context
                            .sub_task_manager
                            .as_ref()
                            .and_then(|manager| manager.get("preloaded"))
                            .is_some());
                        tool_runs.fetch_add(1, Ordering::SeqCst);
                        ToolExecutionResult::success(&context.tool_call_id, "probed")
                    }),
                )
                .expect("register probe tool");
            registry
        })
        .debug_dump_dir(debug_dir.path())
        .log_preview_chars(40)
        .before_cycle_messages(|_cycle_index, _messages, _shared_state| {
            vec![vv_agent::Message::user("injected before cycle")]
        })
        .interruption_messages(move || {
            if interruption_used_for_config.swap(true, Ordering::SeqCst) {
                Vec::new()
            } else {
                vec![vv_agent::Message::user("STEER_NOW")]
            }
        })
        .sub_task_manager(manager)
        .runtime_log_handler(move |event, payload| {
            log_events_for_config
                .lock()
                .expect("log events")
                .push((event.to_string(), payload.clone()));
        })
        .runtime_stream_callback(move |payload| {
            stream_payloads_for_config
                .lock()
                .expect("stream payloads")
                .push(payload.clone());
        })
        .build();
    let agent = Agent::builder("assistant")
        .instructions("Use probe_tool and finish.")
        .model(ModelRef::named("requested-probe-model"))
        .build()
        .expect("agent");
    let runner = Runner::builder().build().expect("runner");

    let result = runner
        .run_with_config(&agent, "go", config)
        .await
        .expect("run");

    assert_eq!(result.final_output(), Some("done"));
    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);
    assert_eq!(tool_runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        debug_paths.lock().expect("debug paths").as_slice(),
        [debug_dir.path()]
    );
    assert!(result
        .result()
        .messages
        .iter()
        .any(|message| message.content == "STEER_NOW"));
    assert_eq!(
        result.result().cycles[0].tool_results[1]
            .error_code
            .as_deref(),
        Some("skipped_due_to_steering")
    );
    let logs = log_events.lock().expect("log events");
    assert!(logs.iter().any(|(event, _)| event == "run_steered"));
    let preview = logs
        .iter()
        .find_map(|(event, payload)| {
            (event == "cycle_llm_response")
                .then(|| payload.get("assistant_preview").and_then(Value::as_str))
                .flatten()
        })
        .expect("assistant preview");
    assert_eq!(preview.chars().count(), 40);
    assert_eq!(stream_payloads.lock().expect("stream payloads").len(), 2);
}

#[tokio::test]
async fn unsupported_debug_dump_configuration_fails_before_the_first_model_call() {
    let provider = vv_agent::ScriptedModelProvider::new(
        "scripted",
        "demo-model",
        vec![LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "finish",
                "task_finish",
                json!({"message": "unused"}),
            )],
        )],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Finish.")
        .model(ModelRef::named("demo-model"))
        .build()
        .expect("agent");
    let debug_dir = tempfile::tempdir().expect("debug dir");

    let result = runner
        .run_with_config(
            &agent,
            "go",
            RunConfig::builder()
                .debug_dump_dir(debug_dir.path())
                .build(),
        )
        .await;
    let error = match result {
        Ok(_) => panic!("scripted client does not support debug dump"),
        Err(error) => error,
    };

    assert_eq!(
        error,
        "configured LLM client does not support debug_dump_dir"
    );
}

#[test]
fn run_config_control_manifest_has_no_open_capability_gaps() {
    let contract: Value = serde_json::from_str(CONTROL_CONTRACT).expect("control contract");
    assert_eq!(contract["version"], 1);
    assert_eq!(contract["framework_defaults"]["max_cycles"], 10);
    assert_eq!(contract["framework_defaults"]["max_handoffs"], 10);
    assert_eq!(contract["app_server_defaults"]["max_cycles"], 80);

    let controls = contract["per_run_controls"]
        .as_array()
        .expect("per-run controls");
    assert_eq!(controls.len(), 21);
    assert!(controls.iter().all(|entry| entry["status"] == "equivalent"));
    let capabilities = controls
        .iter()
        .map(|entry| entry["capability"].as_str().expect("capability"))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(capabilities.contains("per_run_tool_registry"));
    assert!(capabilities.contains("cycle_injection"));
    assert!(capabilities.contains("raw_runtime_observers"));
    assert!(capabilities.contains("diagnostics"));
    assert!(capabilities.contains("no_tool_policy"));
}
