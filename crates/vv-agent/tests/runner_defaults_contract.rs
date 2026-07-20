use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    AfterCycleDecision, AfterCycleSnapshot, Agent, AgentStatus, FunctionTool, LLMResponse,
    LlmClient, LlmRequest, ModelError, ModelProvider, ModelRef, ModelSettings, ResolvedModelConfig,
    RunConfig, Runner, ScriptStep, ScriptedModelProvider, ToolCall, ToolOutput,
};

const MAX_CYCLES_RANGE_ERROR: &str = "max_cycles must be between 1 and 4294967295";

#[derive(Clone)]
struct ClientCountingProvider {
    client_calls: Arc<AtomicUsize>,
}

#[tokio::test]
async fn runner_default_after_cycle_hooks_run_before_per_run_hooks() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let default_order = order.clone();
    let default_hook = Arc::new(move |_snapshot: &AfterCycleSnapshot| {
        default_order.lock().expect("order").push("default");
        Ok(Some(AfterCycleDecision::continue_run()))
    });
    let run_order = order.clone();
    let run_hook = Arc::new(move |_snapshot: &AfterCycleSnapshot| {
        run_order.lock().expect("order").push("run");
        Ok(Some(AfterCycleDecision::continue_run()))
    });
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo-model",
            vec![finish_response("done")],
        ))
        .workspace(".")
        .default_run_config(
            RunConfig::builder()
                .after_cycle_hook_arc(default_hook)
                .build(),
        )
        .build()
        .expect("runner");
    let agent = Agent::builder("after-cycle-order")
        .instructions("Finish.")
        .model(ModelRef::named("demo-model"))
        .build()
        .expect("agent");

    let result = runner
        .run_with_config(
            &agent,
            "go",
            RunConfig::builder().after_cycle_hook_arc(run_hook).build(),
        )
        .await
        .expect("run");

    assert_eq!(result.final_output(), Some("done"));
    assert_eq!(*order.lock().expect("order"), ["default", "run"]);
}

impl ModelProvider for ClientCountingProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        Ok(ResolvedModelConfig::new(
            "counting",
            model.model(),
            model.model(),
            model.model(),
            Vec::new(),
        ))
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        self.client_calls.fetch_add(1, Ordering::SeqCst);
        Err(ModelError::Config(
            "model client must not be acquired".to_string(),
        ))
    }
}

fn runner_with_counting_provider(
    client_calls: Arc<AtomicUsize>,
    default_run_config: RunConfig,
) -> Runner {
    Runner::builder()
        .model_provider(ClientCountingProvider { client_calls })
        .workspace(".")
        .default_run_config(default_run_config)
        .build()
        .expect("runner")
}

async fn assert_max_cycles_rejected_before_client(
    runner: &Runner,
    agent: &Agent,
    config: RunConfig,
    client_calls: &AtomicUsize,
) {
    let error = match runner.run_with_config(agent, "go", config).await {
        Ok(_) => panic!("zero max_cycles must fail"),
        Err(error) => error,
    };
    assert_eq!(error, MAX_CYCLES_RANGE_ERROR);
    assert_eq!(client_calls.load(Ordering::SeqCst), 0);
}

fn finish_response(message: &str) -> LLMResponse {
    LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::from_raw_arguments(
            "finish",
            "task_finish",
            json!({"message": message}),
        )],
    )
}

#[tokio::test]
async fn runner_rejects_zero_max_cycles_from_every_configuration_layer_before_client() {
    let client_calls = Arc::new(AtomicUsize::new(0));
    let runner = runner_with_counting_provider(client_calls.clone(), RunConfig::default());
    let agent = Agent::builder("agent-max-cycles")
        .instructions("Finish.")
        .model(ModelRef::named("counting-model"))
        .max_cycles(0)
        .build()
        .expect("agent");
    assert_max_cycles_rejected_before_client(&runner, &agent, RunConfig::default(), &client_calls)
        .await;

    let client_calls = Arc::new(AtomicUsize::new(0));
    let runner = runner_with_counting_provider(
        client_calls.clone(),
        RunConfig::builder().max_cycles(0).build(),
    );
    let agent = Agent::builder("runner-default-max-cycles")
        .instructions("Finish.")
        .model(ModelRef::named("counting-model"))
        .build()
        .expect("agent");
    assert_max_cycles_rejected_before_client(&runner, &agent, RunConfig::default(), &client_calls)
        .await;

    let client_calls = Arc::new(AtomicUsize::new(0));
    let runner = runner_with_counting_provider(client_calls.clone(), RunConfig::default());
    let agent = Agent::builder("per-run-max-cycles")
        .instructions("Finish.")
        .model(ModelRef::named("counting-model"))
        .build()
        .expect("agent");
    assert_max_cycles_rejected_before_client(
        &runner,
        &agent,
        RunConfig::builder().max_cycles(0).build(),
        &client_calls,
    )
    .await;
}

#[tokio::test]
async fn runner_accepts_max_cycles_upper_bound_and_zero_max_handoffs() {
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo-model",
            vec![finish_response("done")],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Finish.")
        .model(ModelRef::named("demo-model"))
        .build()
        .expect("agent");

    let result = runner
        .run_with_config(
            &agent,
            "go",
            RunConfig::builder()
                .max_cycles(u32::MAX)
                .max_handoffs(0)
                .build(),
        )
        .await
        .expect("run");

    assert_eq!(result.final_output(), Some("done"));
}

#[tokio::test]
async fn configured_runner_uses_shared_provider_model_and_settings_precedence() {
    let captured = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let requests = captured.clone();
    let provider =
        ScriptedModelProvider::from_callback("scripted", "provider-model", move |request| {
            requests.lock().expect("requests").push(request.clone());
            Ok(finish_response("done"))
        })
        .with_default_settings(
            ModelSettings::builder()
                .temperature(0.1)
                .top_p(0.1)
                .max_tokens(100)
                .parallel_tool_calls(false)
                .extra_body("winner", json!("provider"))
                .extra_body("provider_only", json!(true))
                .build(),
        );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .default_run_config(
            RunConfig::builder()
                .model(ModelRef::named("runner-model"))
                .model_settings(
                    ModelSettings::builder()
                        .temperature(0.2)
                        .top_p(0.2)
                        .max_tokens(200)
                        .extra_body("winner", json!("runner"))
                        .extra_body("runner_only", json!(true))
                        .build(),
                )
                .build(),
        )
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Finish.")
        .model(ModelRef::named("agent-model"))
        .model_settings(
            ModelSettings::builder()
                .temperature(0.3)
                .top_p(0.3)
                .extra_body("winner", json!("agent"))
                .extra_body("agent_only", json!(true))
                .build(),
        )
        .build()
        .expect("agent");

    let result = runner
        .run_with_config(
            &agent,
            "go",
            RunConfig::builder()
                .model(ModelRef::named("run-model"))
                .model_settings(
                    ModelSettings::builder()
                        .temperature(0.4)
                        .extra_body("winner", json!("run"))
                        .extra_body("run_only", json!(true))
                        .build(),
                )
                .build(),
        )
        .await
        .expect("run");

    assert_eq!(result.final_output(), Some("done"));
    let requests = captured.lock().expect("requests");
    assert_eq!(requests[0].model, "run-model");
    assert_eq!(
        requests[0].model_settings,
        Some(
            ModelSettings::builder()
                .temperature(0.4)
                .top_p(0.3)
                .max_tokens(200)
                .parallel_tool_calls(false)
                .extra_body("winner", json!("run"))
                .extra_body("provider_only", json!(true))
                .extra_body("runner_only", json!(true))
                .extra_body("agent_only", json!(true))
                .extra_body("run_only", json!(true))
                .build()
        )
    );
}

#[tokio::test]
async fn configured_runner_model_fallback_order() {
    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let models = captured.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "provider-model",
        (0..4)
            .map(|_| {
                let models = models.clone();
                ScriptStep::callback(move |request| {
                    models.lock().expect("models").push(request.model.clone());
                    Ok(finish_response("done"))
                })
            })
            .collect(),
    );
    let runner = Runner::builder()
        .model_provider(provider.clone())
        .workspace(".")
        .default_run_config(
            RunConfig::builder()
                .model(ModelRef::named("runner-model"))
                .build(),
        )
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Finish.")
        .model(ModelRef::named("agent-model"))
        .build()
        .expect("agent");

    runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder()
                .model(ModelRef::named("run-model"))
                .build(),
        )
        .await
        .expect("run model");
    runner.run(&agent, "agent").await.expect("agent model");
    runner
        .run(
            &Agent::builder("runner")
                .instructions("Finish.")
                .build()
                .expect("runner agent"),
            "runner",
        )
        .await
        .expect("runner model");
    Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("provider runner")
        .run(
            &Agent::builder("provider")
                .instructions("Finish.")
                .build()
                .expect("provider agent"),
            "provider",
        )
        .await
        .expect("provider model");

    assert_eq!(
        *captured.lock().expect("models"),
        ["run-model", "agent-model", "runner-model", "provider-model"]
    );
}

#[tokio::test]
async fn per_run_provider_override_does_not_reuse_runner_backend_model() {
    let runner_provider = ScriptedModelProvider::new("runner", "runner-provider-default", vec![]);
    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let override_models = captured.clone();
    let override_provider = ScriptedModelProvider::from_callback(
        "override",
        "override-provider-default",
        move |request| {
            override_models
                .lock()
                .expect("models")
                .push(request.model.clone());
            Ok(finish_response("done"))
        },
    );
    let runner = Runner::builder()
        .model_provider(runner_provider)
        .workspace(".")
        .default_run_config(
            RunConfig::builder()
                .model(ModelRef::backend("runner", "runner-model"))
                .build(),
        )
        .build()
        .expect("runner");

    let result = runner
        .run_with_config(
            &Agent::builder("assistant")
                .instructions("Finish.")
                .build()
                .expect("agent"),
            "go",
            RunConfig::builder()
                .model_provider(override_provider)
                .build(),
        )
        .await
        .expect("run");

    let resolved = result.resolved_model().expect("resolved model");
    assert_eq!(resolved.backend, "override");
    assert_eq!(resolved.model_id, "override-provider-default");
    assert_eq!(
        *captured.lock().expect("models"),
        ["override-provider-default"]
    );
}

#[tokio::test]
async fn approval_resume_preserves_runner_default_metadata() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_for_tool = captured.clone();
    let tool = FunctionTool::builder("guarded_write")
        .needs_approval(true)
        .handler(move |context, _arguments: serde_json::Value| {
            let captured = captured_for_tool.clone();
            async move {
                captured
                    .lock()
                    .expect("metadata")
                    .push(context.metadata.get("runner_default").cloned());
                Ok(ToolOutput::text("approved"))
            }
        })
        .build()
        .expect("tool");
    let provider = ScriptedModelProvider::new(
        "scripted",
        "approval-model",
        vec![
            LLMResponse::with_tool_calls(
                "write",
                vec![ToolCall::from_raw_arguments(
                    "write",
                    "guarded_write",
                    json!({"value": "approved"}),
                )],
            ),
            finish_response("approved"),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .default_run_config(
            RunConfig::builder()
                .metadata("runner_default", json!(true))
                .build(),
        )
        .build()
        .expect("runner");
    let agent = Agent::builder("writer")
        .instructions("Write after approval.")
        .model(ModelRef::named("approval-model"))
        .tool(tool)
        .build()
        .expect("agent");

    let interrupted = runner.run(&agent, "write").await.expect("run");
    assert_eq!(interrupted.status(), AgentStatus::WaitUser);
    let interruption_id = interrupted
        .result()
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_results.iter())
        .find_map(|result| {
            result
                .metadata
                .get("approval_interruption_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .expect("interruption id");
    let mut state = interrupted.into_state().expect("state");
    state.approve(&interruption_id).expect("approve");

    let resumed = runner.resume(state).await.expect("resume");

    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(*captured.lock().expect("metadata"), [Some(json!(true))]);
}

#[tokio::test]
async fn resume_uses_the_runner_that_created_the_state() {
    let origin_requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let first_requests = origin_requests.clone();
    let second_requests = origin_requests.clone();
    let origin_provider = ScriptedModelProvider::from_steps(
        "origin",
        "origin-model",
        vec![
            ScriptStep::callback(move |request| {
                first_requests
                    .lock()
                    .expect("requests")
                    .push(request.model.clone());
                Ok(LLMResponse::with_tool_calls(
                    "need input",
                    vec![ToolCall::from_raw_arguments(
                        "ask",
                        "ask_user",
                        json!({"question": "Which color?"}),
                    )],
                ))
            }),
            ScriptStep::callback(move |request| {
                second_requests
                    .lock()
                    .expect("requests")
                    .push(request.model.clone());
                Ok(finish_response("selected blue"))
            }),
        ],
    );
    let origin = Runner::builder()
        .model_provider(origin_provider)
        .workspace(".")
        .build()
        .expect("origin");
    let receiving = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "receiving",
            "receiving-model",
            vec![],
        ))
        .workspace(".")
        .build()
        .expect("receiving");
    let agent = Agent::builder("assistant")
        .instructions("Ask once, then finish.")
        .build()
        .expect("agent");

    let interrupted = origin.run(&agent, "choose").await.expect("run");
    assert_eq!(interrupted.status(), AgentStatus::WaitUser);

    let resumed = receiving
        .resume_with_input(interrupted.into_state().expect("state"), "blue")
        .await
        .expect("resume");

    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("selected blue"));
    assert_eq!(
        *origin_requests.lock().expect("requests"),
        ["origin-model", "origin-model"]
    );
}

#[tokio::test]
async fn stream_with_config_applies_per_run_model_override() {
    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let models = captured.clone();
    let provider =
        ScriptedModelProvider::from_callback("scripted", "provider-model", move |request| {
            models.lock().expect("models").push(request.model.clone());
            Ok(finish_response("done"))
        });
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Finish.")
        .build()
        .expect("agent");

    let stream = runner
        .stream_with_config(
            &agent,
            "go",
            RunConfig::builder()
                .model(ModelRef::named("run-model"))
                .build(),
        )
        .await
        .expect("stream");
    let result = stream.into_result().await.expect("result");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(*captured.lock().expect("models"), ["run-model"]);
}
