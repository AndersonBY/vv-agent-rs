use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    assemble_context_fragments, Agent, ContextError, ContextFragment, ContextProvider,
    ContextRequest, LLMResponse, ModelRef, NoToolPolicy, PromptBundle, PromptSection, RunConfig,
    Runner, ScriptStep, ScriptedModelProvider, SubAgentConfig, ToolCall,
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

#[test]
fn provider_fragments_use_utf16_id_order_after_priority_and_stability() {
    let request = ContextRequest::for_test("assistant", "input");
    let bundle = assemble_context_fragments(
        &request,
        vec![
            ContextFragment::new("\u{e000}", "bmp")
                .priority(10)
                .stable(true),
            ContextFragment::new("\u{10000}", "supplementary")
                .priority(10)
                .stable(true),
            ContextFragment::new("volatile", "later")
                .priority(10)
                .stable(false),
        ],
    )
    .expect("bundle");

    assert_eq!(
        bundle
            .sections
            .iter()
            .map(|section| section.id.as_str())
            .collect::<Vec<_>>(),
        vec!["\u{10000}", "\u{e000}", "volatile"]
    );
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
        "Check facts.\n\nCurrent order status."
    );
    assert_eq!(
        request
            .prompt_bundle
            .sections
            .iter()
            .map(|section| section.id.as_str())
            .collect::<Vec<_>>(),
        vec!["agent_instructions", "runtime_context"]
    );
    assert_eq!(request.messages[0].content, request.prompt_bundle.flatten());
    assert!(request.metadata.get("system_prompt_sections").is_none());
    assert!(request.metadata.get("system_prompt_sources").is_none());
    assert!(request.metadata.get("system_prompt_stable_hash").is_none());
}

struct CountingProvider {
    calls: Arc<AtomicUsize>,
}

impl ContextProvider for CountingProvider {
    fn fragments(
        &self,
        _request: &ContextRequest<'_>,
    ) -> Result<Vec<ContextFragment>, ContextError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![
            ContextFragment::new("runtime_hint", "Runtime hint.")
                .priority(40)
                .stable(false)
                .source("provider.runtime"),
            ContextFragment::new("policy", "Policy first.")
                .priority(20)
                .stable(true)
                .source("provider.policy"),
        ])
    }
}

#[tokio::test]
async fn runner_resolves_prompt_producers_once_per_run_and_reuses_the_bundle_across_cycles() {
    let instruction_calls = Arc::new(AtomicUsize::new(0));
    let instruction_calls_for_agent = Arc::clone(&instruction_calls);
    let context_calls = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(Vec::new()));
    let steps = (0..6)
        .map(|request_index| {
            let captured_requests = Arc::clone(&captured);
            ScriptStep::callback(move |request| {
                captured_requests
                    .lock()
                    .expect("requests")
                    .push(request.clone());
                if request_index % 3 == 2 {
                    let args = BTreeMap::from([("message".to_string(), json!("done"))]);
                    Ok(LLMResponse::with_tool_calls(
                        "",
                        vec![ToolCall::new("finish", "task_finish", args)],
                    ))
                } else {
                    Ok(LLMResponse::new("continue"))
                }
            })
        })
        .collect();
    let provider = ScriptedModelProvider::from_steps("scripted", "demo-model", steps);
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("ops")
        .dynamic_prompt_bundle(move |_context, _agent| {
            let run_index = instruction_calls_for_agent.fetch_add(1, Ordering::SeqCst);
            PromptBundle::new(vec![
                PromptSection::new("identity", "Identity first.", true)
                    .source("agent.instructions"),
                PromptSection::new("run_data", "Request scoped data.", false)
                    .source("agent.instructions"),
                PromptSection::new(
                    "current_time",
                    format!("2026-07-25T00:00:0{run_index}Z"),
                    false,
                )
                .source("run.clock"),
            ])
            .expect("instruction bundle")
        })
        .model(ModelRef::named("demo-model"))
        .no_tool_policy(NoToolPolicy::Continue)
        .sub_agent(
            "researcher",
            SubAgentConfig::new("demo-model", "Finds source evidence."),
        )
        .build()
        .expect("agent");
    let config = || {
        RunConfig::builder()
            .context_provider(Arc::new(CountingProvider {
                calls: Arc::clone(&context_calls),
            }))
            .max_cycles(3)
            .build()
    };

    runner
        .run_with_config(&agent, "first run", config())
        .await
        .expect("first run");
    runner
        .run_with_config(&agent, "second run", config())
        .await
        .expect("second run");

    assert_eq!(instruction_calls.load(Ordering::SeqCst), 2);
    assert_eq!(context_calls.load(Ordering::SeqCst), 2);
    let requests = captured.lock().expect("requests");
    assert_eq!(requests.len(), 6);
    assert!(requests[..3]
        .iter()
        .all(|request| request.prompt_bundle == requests[0].prompt_bundle));
    assert!(requests[3..]
        .iter()
        .all(|request| request.prompt_bundle == requests[3].prompt_bundle));
    assert_ne!(requests[0].prompt_bundle, requests[3].prompt_bundle);
    assert_eq!(
        requests[0].prompt_bundle.stable_hash,
        requests[3].prompt_bundle.stable_hash
    );
    assert_eq!(
        requests[0]
            .prompt_bundle
            .sections
            .iter()
            .map(|section| section.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "identity",
            "run_data",
            "current_time",
            "configured_sub_agents",
            "policy",
            "runtime_hint",
        ]
    );
    for request in requests.iter() {
        assert_eq!(request.messages[0].content, request.prompt_bundle.flatten());
        assert!(request.metadata.get("system_prompt_sections").is_none());
    }
}
