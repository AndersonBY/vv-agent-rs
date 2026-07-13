use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    assemble_context_fragments, Agent, ContextError, ContextFragment, ContextProvider,
    ContextRequest, LLMResponse, ModelRef, RunConfig, Runner, ScriptedModelProvider, ToolCall,
};

struct StaticProvider;

impl ContextProvider for StaticProvider {
    fn fragments(
        &self,
        _request: &ContextRequest<'_>,
    ) -> Result<Vec<ContextFragment>, ContextError> {
        Ok(vec![
            ContextFragment::new("volatile", "second")
                .stable(false)
                .priority(20)
                .source("test"),
            ContextFragment::new("stable", "first")
                .stable(true)
                .priority(10)
                .cache_hint("cache"),
        ])
    }
}

#[test]
fn context_fragments_are_ordered_budgeted_and_hashed() {
    let request = ContextRequest::for_test("assistant", "input").max_prompt_chars(20);
    let fragments = StaticProvider.fragments(&request).expect("fragments");
    let bundle = assemble_context_fragments(&request, fragments).expect("bundle");

    assert_eq!(bundle.prompt, "first\n\nsecond");
    assert_eq!(bundle.sections[0].id, "stable");
    assert_eq!(bundle.sections[0].priority, 10);
    assert_eq!(bundle.sections[0].source, None);
    assert_eq!(bundle.sections[0].cache_hint.as_deref(), Some("cache"));
    assert!(!bundle.stable_hash.is_empty());
    assert_eq!(bundle.sources["volatile"], "test");
    assert_eq!(bundle.total_chars, bundle.prompt.chars().count());
    assert_eq!(bundle.metadata_sections()[0]["cache_hint"], "cache");
}

#[test]
fn context_budget_counts_unicode_characters_instead_of_utf8_bytes() {
    let request = ContextRequest::for_test("assistant", "input").max_prompt_chars(4);
    let bundle =
        assemble_context_fragments(&request, vec![ContextFragment::new("unicode", "你好世界")])
            .expect("bundle");

    assert_eq!(bundle.prompt, "你好世界");
    assert_eq!(bundle.total_chars, 4);
    assert!(bundle.omitted_section_ids.is_empty());
}

struct InspectingProvider;

impl ContextProvider for InspectingProvider {
    fn fragments(
        &self,
        request: &ContextRequest<'_>,
    ) -> Result<Vec<ContextFragment>, ContextError> {
        assert_eq!(request.agent_name, "ops");
        assert_eq!(request.input, "analyze order");
        assert_eq!(request.model.as_deref(), Some("demo-model"));
        assert!(request
            .trace_id
            .as_deref()
            .is_some_and(|value| !value.is_empty()));
        assert!(request.workspace.is_some());
        assert_eq!(request.metadata["request_id"], json!("r1"));
        Ok(vec![ContextFragment::new(
            "runtime_context",
            "Current order status.",
        )
        .priority(-10)
        .source("test")])
    }
}

#[tokio::test]
async fn runner_globally_orders_instructions_and_provider_context_with_cache_metadata() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_requests = Arc::clone(&captured);
    let provider = ScriptedModelProvider::from_callback("scripted", "demo-model", move |request| {
        captured_requests
            .lock()
            .expect("requests")
            .push(request.clone());
        let args = BTreeMap::from([("message".to_string(), json!("done"))]);
        Ok(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new("finish", "task_finish", args)],
        ))
    });
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("ops")
        .instructions("Check facts.")
        .model(ModelRef::named("demo-model"))
        .build()
        .expect("agent");
    let config = RunConfig::builder()
        .context_provider(Arc::new(InspectingProvider))
        .metadata("request_id", json!("r1"))
        .build();

    let result = runner
        .run_with_config(&agent, "analyze order", config)
        .await
        .expect("run");

    assert_eq!(result.final_output(), Some("done"));
    let requests = captured.lock().expect("requests");
    let request = requests.first().expect("model request");
    assert_eq!(
        request.messages[0].content,
        "Current order status.\n\nCheck facts."
    );
    assert_eq!(
        request.metadata["system_prompt_sources"],
        json!({
            "agent_instructions": "agent.instructions",
            "runtime_context": "test"
        })
    );
    assert_eq!(
        request.metadata["system_prompt_sections"][0]["id"],
        json!("runtime_context")
    );
    assert!(request.metadata["system_prompt_stable_hash"].is_string());
}
