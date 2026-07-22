use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use vv_agent::{
    FunctionTool, StaticTool, Tool, ToolCall, ToolContext, ToolExposure, ToolOrchestrator,
    ToolOutput, ToolRegistry, ToolResultStatus, ToolRunOptions, ToolSpecContext,
};

#[tokio::test]
async fn function_tool_adapts_to_tool_executor() {
    let tool = FunctionTool::builder("echo")
        .description("Echo args.")
        .json_schema(
            json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}),
        )
        .handler(|_ctx, args: serde_json::Value| async move { Ok(ToolOutput::json(args)) })
        .build()
        .expect("tool");

    let executor = tool.to_executor();

    assert_eq!(executor.name(), "echo");
    assert_eq!(executor.exposure(), ToolExposure::Direct);
}

#[test]
fn function_tool_metadata_survives_builder_clone_spec_and_executor() {
    let tool = FunctionTool::builder("classified_lookup")
        .metadata("risk_level", json!("high"))
        .metadata("audit", json!({"owner": "security"}))
        .handler(|_context, _arguments: Value| async { Ok(ToolOutput::text("unused")) })
        .build()
        .expect("metadata tool");
    let cloned = tool.clone();

    assert_eq!(cloned.metadata(), tool.metadata());
    assert_eq!(cloned.metadata()["risk_level"], json!("high"));

    let spec = cloned.as_tool_spec();
    assert_eq!(&spec.metadata, tool.metadata());

    let executor = cloned.to_executor();
    assert_eq!(executor.metadata(), tool.metadata());
    let executor_spec = executor.spec(&ToolSpecContext).expect("executor spec");
    assert_eq!(&executor_spec.metadata, tool.metadata());
}

#[tokio::test]
async fn orchestrator_rejects_disallowed_tool_before_handler_runs() {
    let tool = FunctionTool::builder("hidden")
        .description("Should not run.")
        .json_schema(json!({"type":"object","properties":{},"required":[]}))
        .handler(|_ctx, _args: serde_json::Value| async move { Ok(ToolOutput::text("ran")) })
        .build()
        .expect("tool");
    let orchestrator = ToolOrchestrator::from_tools(vec![tool.to_executor()]);
    let mut context = ToolContext::new("./workspace");
    let result = orchestrator
        .run_one(
            ToolCall::from_raw_arguments("call_1", "hidden", json!({})),
            &mut context,
            vv_agent::ToolRunOptions::default().allow_only(vec!["other"]),
        )
        .await
        .expect("result");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("tool_not_allowed"));
}

#[tokio::test]
async fn planned_name_gate_rejects_unknown_calls_before_executor_lookup() {
    let orchestrator = ToolOrchestrator::default();
    let mut context = ToolContext::new("./workspace");

    let result = orchestrator
        .run_one(
            ToolCall::from_raw_arguments("unknown_call", "unknown_tool", json!({})),
            &mut context,
            ToolRunOptions::default().planned_names(["task_finish"]),
        )
        .await
        .expect("planned-name denial");

    assert_eq!(result.error_code.as_deref(), Some("tool_not_allowed"));
    assert_eq!(result.metadata["policy_source"], json!("planned_name"));
}

#[tokio::test]
async fn approval_predicate_receives_the_current_tool_call_context() {
    let observed = Arc::new(std::sync::Mutex::new(None));
    let observed_for_predicate = observed.clone();
    let tool = FunctionTool::builder("guarded")
        .json_schema(json!({
            "type": "object",
            "properties": {
                "scope": {"type": "string"}
            },
            "required": ["scope"]
        }))
        .needs_approval_if(move |context, arguments| {
            *observed_for_predicate.lock().expect("approval context") = Some((
                context.tool_call_id.clone(),
                context.tool_name.clone(),
                context.arguments.clone(),
            ));
            arguments.get("scope") == Some(&json!("dangerous"))
        })
        .handler(|_context, _arguments: Value| async { Ok(ToolOutput::text("unused")) })
        .build()
        .expect("guarded tool");
    let orchestrator = ToolOrchestrator::from_tools(vec![tool.to_executor()]);
    let mut context = ToolContext::new("./workspace");
    let call =
        ToolCall::from_raw_arguments("guarded_call", "guarded", json!({"scope": "dangerous"}));

    let result = orchestrator
        .run_one(call, &mut context, ToolRunOptions::default())
        .await
        .expect("approval result");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(
        observed.lock().expect("approval context").as_ref(),
        Some(&(
            "guarded_call".to_string(),
            "guarded".to_string(),
            std::collections::BTreeMap::from([("scope".to_string(), json!("dangerous"),)]),
        ))
    );
}

#[test]
fn function_tool_strictness_and_hidden_exposure_are_enforced_by_the_registry() {
    let tool = FunctionTool::builder("internal_lookup")
        .description("Internal lookup.")
        .strict_schema(false)
        .exposure(ToolExposure::Hidden)
        .handler(|_ctx, _args: Value| async move { Ok(ToolOutput::text("hidden")) })
        .build()
        .expect("tool");
    let spec = tool.as_tool_spec();

    assert_eq!(spec.schema["function"]["strict"], json!(false));
    assert_eq!(spec.exposure, ToolExposure::Hidden);

    let mut registry = ToolRegistry::new();
    registry.register(spec).expect("register hidden tool");
    assert!(registry
        .list_openai_schemas(None)
        .expect("list schemas")
        .is_empty());
}

#[tokio::test]
async fn function_tool_timeout_returns_the_shared_retryable_error_contract() {
    let tool = FunctionTool::builder("slow")
        .description("Slow tool.")
        .timeout(Duration::from_millis(10))
        .handler(|_ctx, _args: Value| async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(ToolOutput::text("late"))
        })
        .build()
        .expect("tool");
    let orchestrator = ToolOrchestrator::from_tools(vec![tool.to_executor()]);
    let mut context = ToolContext::new("./workspace");

    let result = orchestrator
        .run_one(
            ToolCall::from_raw_arguments("call_slow", "slow", json!({})),
            &mut context,
            vv_agent::ToolRunOptions::default(),
        )
        .await
        .expect("timeout result");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("tool_timeout"));
    assert_eq!(result.metadata["output_type"], json!("error"));
    assert_eq!(result.metadata["retryable"], json!(true));
    assert_eq!(
        serde_json::from_str::<Value>(&result.content).unwrap()["retryable"],
        json!(true)
    );
}

#[test]
fn function_tool_rejects_a_zero_timeout() {
    let result = FunctionTool::builder("invalid_timeout")
        .timeout(Duration::ZERO)
        .handler(|_ctx, _args: Value| async move { Ok(ToolOutput::text("unused")) })
        .build();

    assert!(result.is_err());
    assert_eq!(
        result.err().as_deref(),
        Some("tool timeout must be greater than zero")
    );
}

#[test]
fn static_tool_schema_emits_strict_contract() {
    let tool = StaticTool::new(
        "static_echo",
        "Echo a value.",
        json!({"type": "object", "properties": {}, "required": []}),
        Arc::new(|_context, _arguments| vv_agent::ToolExecutionResult::success("", "ok")),
    );

    let spec = tool.as_tool_spec();

    assert_eq!(spec.schema["function"]["strict"], json!(true));
}

#[tokio::test]
async fn function_tool_errors_use_canonical_code_and_optional_mapper() {
    let default_error = FunctionTool::builder("default_error")
        .handler(|_context, _arguments: Value| async {
            Err::<ToolOutput, _>("database unavailable".to_string())
        })
        .build()
        .expect("default error tool");
    let mapped_error = FunctionTool::builder("mapped_error")
        .failure_error_function(|error| format!("redacted: {error}"))
        .handler(|_context, _arguments: Value| async {
            Err::<ToolOutput, _>("secret detail".to_string())
        })
        .build()
        .expect("mapped error tool");
    let orchestrator = ToolOrchestrator::from_tools(vec![
        default_error.to_executor(),
        mapped_error.to_executor(),
    ]);
    let mut context = ToolContext::new("./workspace");

    let default_result = orchestrator
        .run_one(
            ToolCall::from_raw_arguments("default_call", "default_error", json!({})),
            &mut context,
            ToolRunOptions::default(),
        )
        .await
        .expect("default error result");
    let mapped_result = orchestrator
        .run_one(
            ToolCall::from_raw_arguments("mapped_call", "mapped_error", json!({})),
            &mut context,
            ToolRunOptions::default(),
        )
        .await
        .expect("mapped error result");

    assert_eq!(
        default_result.error_code.as_deref(),
        Some("tool_execution_failed")
    );
    assert!(default_result
        .content
        .contains("Tool execution failed (default_error): database unavailable"));
    assert_eq!(default_result.tool_call_id, "default_call");
    assert_eq!(
        mapped_result.error_code.as_deref(),
        Some("tool_execution_failed")
    );
    assert!(mapped_result.content.contains("redacted: secret detail"));
    assert_eq!(mapped_result.tool_call_id, "mapped_call");
}

#[tokio::test]
async fn reserved_exposure_variants_match_python_direct_semantics() {
    let deferred = FunctionTool::builder("deferred_tool")
        .exposure(ToolExposure::Deferred)
        .handler(|_context, _arguments: Value| async { Ok(ToolOutput::text("deferred")) })
        .build()
        .expect("deferred tool");
    let model_only = FunctionTool::builder("model_only_tool")
        .exposure(ToolExposure::DirectModelOnly)
        .handler(|_context, _arguments: Value| async { Ok(ToolOutput::text("model-only")) })
        .build()
        .expect("model-only tool");
    let mut registry = ToolRegistry::new();
    registry
        .register(deferred.as_tool_spec())
        .expect("register deferred");
    registry
        .register(model_only.as_tool_spec())
        .expect("register model-only");

    let schema_names = registry
        .list_openai_schemas(None)
        .expect("schemas")
        .into_iter()
        .filter_map(|schema| schema["function"]["name"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    assert_eq!(schema_names, vec!["deferred_tool", "model_only_tool"]);

    let orchestrator = ToolOrchestrator::from_tools(registry.executors());
    let mut context = ToolContext::new("./workspace");
    for name in ["deferred_tool", "model_only_tool"] {
        let result = orchestrator
            .run_one(
                ToolCall::from_raw_arguments(format!("{name}_call"), name, json!({})),
                &mut context,
                ToolRunOptions::default(),
            )
            .await
            .expect("reserved exposure result");
        assert_eq!(result.status, ToolResultStatus::Success);
    }
}
