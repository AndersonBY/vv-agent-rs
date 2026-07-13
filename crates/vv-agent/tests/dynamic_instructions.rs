use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    Agent, BeforeLlmEvent, BeforeLlmPatch, FunctionTool, LLMResponse, LlmClient, ModelError,
    ModelProvider, ModelRef, ResolvedModelConfig, RunConfig, Runner, RuntimeHook,
    ScriptedModelProvider, ToolCall, ToolOutput,
};

#[derive(Clone)]
struct AliasResolvingProvider {
    inner: ScriptedModelProvider,
}

impl ModelProvider for AliasResolvingProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        assert_eq!(model, &ModelRef::named("requested-alias"));
        Ok(ResolvedModelConfig::new(
            "scripted",
            "requested-alias",
            "resolved-model",
            "resolved-model",
            Vec::new(),
        )
        .with_token_limits(Some(128_000), Some(16_384))
        .with_capabilities(true, true, false))
    }

    fn client(&self, resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        self.inner.client(resolved)
    }
}

#[test]
fn agent_rejects_empty_name_and_static_instructions() {
    assert_eq!(
        Agent::builder(" ")
            .instructions("Valid instructions.")
            .build()
            .err()
            .as_deref(),
        Some("agent name cannot be empty")
    );
    assert_eq!(
        Agent::builder("assistant")
            .instructions(" ")
            .build()
            .err()
            .as_deref(),
        Some("agent instructions cannot be empty")
    );
}

#[tokio::test]
async fn dynamic_instructions_receive_the_current_run_context() {
    let observed_context = Arc::new(Mutex::new(None));
    let observed_for_instructions = observed_context.clone();
    let provider = ScriptedModelProvider::from_callback("scripted", "demo-model", |request| {
        assert!(request.messages[0].content.contains("tenant=acme"));
        assert!(request.messages[0].content.contains("agent=assistant"));
        assert!(request.messages[0].content.contains("run=run_"));
        Ok(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "finish",
                "task_finish",
                json!({"message": "done"}),
            )],
        ))
    });
    let agent = Agent::builder("assistant")
        .dynamic_instructions(move |context, current_agent| {
            *observed_for_instructions.lock().expect("context") = Some(context.run_id.clone());
            assert_eq!(current_agent.name(), "assistant");
            assert_eq!(context.agent_name, current_agent.name());
            assert_eq!(context.model.as_ref(), Some(&ModelRef::named("demo-model")));
            assert_eq!(
                context
                    .app_state::<String>()
                    .map(std::string::String::as_str),
                Some("req-1")
            );
            format!(
                "tenant={} agent={} run={}",
                context.metadata["tenant"].as_str().unwrap_or_default(),
                context.agent_name,
                context.run_id,
            )
        })
        .metadata("tenant", json!("acme"))
        .model(ModelRef::named("demo-model"))
        .build()
        .expect("agent");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");

    let result = runner
        .run_with_config(
            &agent,
            "go",
            RunConfig::builder().app_state("req-1".to_string()).build(),
        )
        .await
        .expect("run");

    assert_eq!(result.final_output(), Some("done"));
    assert!(observed_context
        .lock()
        .expect("context")
        .as_deref()
        .is_some_and(|run_id| run_id.starts_with("run_")));
}

#[tokio::test]
async fn resolved_model_alias_is_used_by_dynamic_instructions_and_tool_enablement() {
    let instructions_model = Arc::new(Mutex::new(None));
    let observed_instructions_model = instructions_model.clone();
    let tool_model = Arc::new(Mutex::new(None));
    let observed_tool_model = tool_model.clone();
    let tool = FunctionTool::builder("resolved_only")
        .enabled_if(move |context| {
            *observed_tool_model.lock().expect("tool model") = context.run.model.clone();
            context.run.model.as_ref() == Some(&ModelRef::named("resolved-model"))
        })
        .handler(|_context, _arguments: serde_json::Value| async { Ok(ToolOutput::text("unused")) })
        .build()
        .expect("tool");
    let provider = AliasResolvingProvider {
        inner: ScriptedModelProvider::from_callback("scripted", "resolved-model", |request| {
            assert_eq!(request.model, "resolved-model");
            assert!(request
                .tools
                .iter()
                .any(|schema| schema["function"]["name"] == "resolved_only"));
            Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "finish",
                    "task_finish",
                    json!({"message": "done"}),
                )],
            ))
        }),
    };
    let agent = Agent::builder("assistant")
        .dynamic_instructions(move |context, _agent| {
            *observed_instructions_model
                .lock()
                .expect("instructions model") = context.model.clone();
            "Finish.".to_string()
        })
        .model(ModelRef::named("requested-alias"))
        .tool(tool)
        .build()
        .expect("agent");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");

    let result = runner.run(&agent, "go").await.expect("run");

    assert_eq!(result.final_output(), Some("done"));
    assert_eq!(
        *instructions_model.lock().expect("instructions model"),
        Some(ModelRef::named("resolved-model"))
    );
    assert_eq!(
        *tool_model.lock().expect("tool model"),
        Some(ModelRef::named("resolved-model"))
    );
}

struct OrderedHook {
    name: &'static str,
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl RuntimeHook for OrderedHook {
    fn before_llm(&self, _event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        self.order.lock().expect("hook order").push(self.name);
        None
    }
}

#[tokio::test]
async fn agent_hooks_run_before_per_run_hooks() {
    let provider = ScriptedModelProvider::from_callback("scripted", "demo-model", |_request| {
        Ok(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "finish",
                "task_finish",
                json!({"message": "done"}),
            )],
        ))
    });
    let order = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder("assistant")
        .instructions("Finish.")
        .model(ModelRef::named("demo-model"))
        .hook(Arc::new(OrderedHook {
            name: "agent",
            order: order.clone(),
        }))
        .build()
        .expect("agent");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");

    let result = runner
        .run_with_config(
            &agent,
            "go",
            RunConfig::builder()
                .hook(Arc::new(OrderedHook {
                    name: "run",
                    order: order.clone(),
                }))
                .build(),
        )
        .await
        .expect("run");

    assert_eq!(result.final_output(), Some("done"));
    assert_eq!(*order.lock().expect("hook order"), vec!["agent", "run"]);
}
