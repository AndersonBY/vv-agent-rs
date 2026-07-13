use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vv_agent::{
    assemble_context_fragments, Agent, AgentStatus, BeforeLlmEvent, BeforeLlmPatch,
    ContextFragment, ContextRequest, LLMResponse, LlmRequest, ModelRef, RunConfig, Runner,
    RuntimeHook, ScriptStep, ScriptedModelProvider, SubAgentConfig, ToolCall,
};

const CONTRACT_JSON: &str = include_str!("fixtures/parity/public_configured_sub_agent_v1.json");

fn contract() -> Value {
    serde_json::from_str(CONTRACT_JSON).expect("public configured sub-agent fixture")
}

fn fixture_config(contract: &Value, index: usize) -> SubAgentConfig {
    serde_json::from_value(contract["normalization"]["raw_entries"][index]["config"].clone())
        .expect("fixture sub-agent config")
}

fn configured_agent(
    contract: &Value,
    researcher: &SubAgentConfig,
    writer: &SubAgentConfig,
    hook: Option<Arc<dyn RuntimeHook>>,
) -> Agent {
    let raw_researcher_id = contract["normalization"]["raw_entries"][0]["id"]
        .as_str()
        .expect("researcher id");
    let mut builder = Agent::builder("coordinator")
        .instructions("Coordinate the work carefully.")
        .model(ModelRef::named("shared-model"))
        .sub_agent(raw_researcher_id, researcher)
        .sub_agents([("writer", writer)]);
    if let Some(hook) = hook {
        builder = builder.hook(hook);
    }
    builder.build().expect("configured Agent")
}

#[test]
fn agent_builder_normalizes_ids_rejects_collisions_and_clones_configs() {
    let contract = contract();
    let mut researcher = fixture_config(&contract, 0);
    let writer = fixture_config(&contract, 1);
    let agent = configured_agent(&contract, &researcher, &writer, None);

    assert_eq!(
        agent.sub_agents().keys().cloned().collect::<Vec<_>>(),
        serde_json::from_value::<Vec<String>>(contract["normalization"]["normalized_ids"].clone())
            .expect("normalized ids")
    );
    researcher
        .metadata
        .insert("nested".to_string(), json!({"scope": "mutated"}));
    assert_eq!(
        serde_json::to_value(
            agent
                .sub_agents()
                .get("researcher")
                .expect("normalized researcher")
        )
        .expect("retained config"),
        contract["normalization"]["retained_researcher_config"]
    );

    let empty_error = Agent::builder("coordinator")
        .instructions("Coordinate.")
        .sub_agent(" \u{001c}\u{001f} ", &writer)
        .build()
        .err();
    assert_eq!(
        empty_error.as_deref(),
        contract["normalization"]["empty_id_error"].as_str()
    );

    let collision_error = Agent::builder("coordinator")
        .instructions("Coordinate.")
        .sub_agent(" \u{001c}researcher\u{001f} ", &writer)
        .sub_agent(" researcher ", &writer)
        .build()
        .err();
    assert_eq!(
        collision_error.as_deref(),
        contract["normalization"]["collision_error"].as_str()
    );
}

#[test]
fn configured_sub_agent_fragment_matches_the_shared_projection_fixture() {
    let contract = contract();
    let researcher = fixture_config(&contract, 0);
    let writer = fixture_config(&contract, 1);
    let agent = configured_agent(&contract, &researcher, &writer, None);
    let available_sub_agents = agent
        .sub_agents()
        .iter()
        .map(|(id, config)| (id.clone(), config.description.clone()))
        .collect::<BTreeMap<_, _>>();
    let fragment_text =
        vv_agent::prompt::templates::render_sub_agents("en-US", &available_sub_agents);
    let fragment = ContextFragment::new("configured_sub_agents", fragment_text.clone())
        .stable(true)
        .priority(10)
        .source("agent.sub_agents");
    let request = ContextRequest::for_test("coordinator", "delegate").max_prompt_chars(
        contract["projection"]["total_chars"]
            .as_u64()
            .expect("total chars") as usize,
    );
    let bundle = assemble_context_fragments(
        &request,
        vec![
            ContextFragment::new("agent_instructions", agent.instructions())
                .stable(true)
                .priority(0)
                .source("agent.instructions"),
            fragment,
        ],
    )
    .expect("configured sub-agent context bundle");
    let sections = bundle
        .sections
        .iter()
        .map(|section| {
            json!({
                "id": section.id,
                "text": section.text,
                "stable": section.stable,
                "priority": section.priority,
                "source": section.source,
            })
        })
        .collect::<Vec<_>>();

    assert_eq!(
        json!({
            "id": "configured_sub_agents",
            "text": fragment_text,
            "stable": true,
            "priority": 10,
            "source": "agent.sub_agents",
        }),
        contract["projection"]["fragment"]
    );
    assert_eq!(bundle.prompt, contract["projection"]["prompt"]);
    assert_eq!(Value::Array(sections), contract["projection"]["sections"]);
    assert_eq!(
        serde_json::to_value(&bundle.sources).expect("bundle sources"),
        contract["projection"]["sources"]
    );
    assert_eq!(bundle.stable_hash, contract["projection"]["stable_hash"]);
    assert_eq!(bundle.total_chars, contract["projection"]["total_chars"]);
    assert!(bundle.omitted_section_ids.is_empty());
}

#[derive(Clone)]
struct TaskProjection {
    has_sub_agents: bool,
    sub_agents_enabled: bool,
    sub_agents: BTreeMap<String, SubAgentConfig>,
}

#[derive(Default)]
struct TaskProjectionCapture {
    projections: Mutex<Vec<TaskProjection>>,
}

impl RuntimeHook for TaskProjectionCapture {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        self.projections
            .lock()
            .expect("task projections")
            .push(TaskProjection {
                has_sub_agents: event.task.has_sub_agents,
                sub_agents_enabled: event.task.sub_agents_enabled(),
                sub_agents: event.task.sub_agents.clone(),
            });
        None
    }
}

fn captured_step(captured: Arc<Mutex<Vec<LlmRequest>>>, response: LLMResponse) -> ScriptStep {
    ScriptStep::callback(move |request| {
        captured
            .lock()
            .expect("captured requests")
            .push(request.clone());
        Ok(response.clone())
    })
}

fn tool_names(request: &LlmRequest) -> Vec<String> {
    request
        .tools
        .iter()
        .filter_map(|schema| schema["function"]["name"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn public_runner_projects_and_executes_a_configured_child() {
    let contract = contract();
    let researcher = fixture_config(&contract, 0);
    let writer = fixture_config(&contract, 1);
    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = ScriptedModelProvider::from_steps(
        "test",
        "shared-model",
        vec![
            captured_step(
                captured.clone(),
                LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "delegate-research",
                        "create_sub_task",
                        json!({
                            "agent_id": "researcher",
                            "task_description": "Research the contract."
                        }),
                    )],
                ),
            ),
            captured_step(
                captured.clone(),
                LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "child-finish",
                        "task_finish",
                        json!({"message": "child done"}),
                    )],
                ),
            ),
            captured_step(
                captured.clone(),
                LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "parent-finish",
                        "task_finish",
                        json!({"message": "parent done"}),
                    )],
                ),
            ),
        ],
    );
    let task_capture = Arc::new(TaskProjectionCapture::default());
    let agent = configured_agent(&contract, &researcher, &writer, Some(task_capture.clone()));
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("public Runner");
    let max_context_chars = contract["projection"]["total_chars"]
        .as_u64()
        .expect("total chars") as usize;

    let result = runner
        .run_with_config(
            &agent,
            "Delegate the research task.",
            RunConfig::builder()
                .max_context_chars(max_context_chars)
                .build(),
        )
        .await
        .expect("configured child run");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), Some("parent done"));
    assert_eq!(
        serde_json::to_value(result.status()).expect("terminal status"),
        contract["public_runner"]["terminal_status"]
    );

    let projections = task_capture.projections.lock().expect("task projections");
    let parent_projection = projections.first().expect("parent task projection");
    assert_eq!(
        parent_projection.has_sub_agents,
        contract["projection"]["has_sub_agents"]
    );
    assert_eq!(
        parent_projection.sub_agents_enabled,
        contract["projection"]["sub_agents_enabled"]
    );
    assert_eq!(&parent_projection.sub_agents, agent.sub_agents());
    drop(projections);

    let requests = captured.lock().expect("captured requests");
    assert_eq!(requests.len(), 3);
    let parent_request = &requests[0];
    let child_request = &requests[1];
    let resumed_parent_request = &requests[2];
    assert_eq!(
        parent_request.model,
        contract["public_runner"]["parent_model"]
    );
    assert_eq!(
        child_request.model,
        contract["public_runner"]["child_model"]
    );
    assert_eq!(
        child_request.metadata["sub_agent_name"],
        contract["public_runner"]["delegated_agent_id"]
    );
    assert_eq!(child_request.messages[0].content, "Research carefully.");
    assert!(!tool_names(child_request).contains(&"create_sub_task".to_string()));
    assert!(!tool_names(child_request).contains(&"sub_task_status".to_string()));
    assert!(resumed_parent_request.messages.iter().any(|message| {
        message.content.contains(
            contract["public_runner"]["child_final_output"]
                .as_str()
                .expect("child output"),
        )
    }));

    assert_eq!(
        parent_request.messages[0].content,
        contract["projection"]["prompt"]
    );
    let expected_tool_names = serde_json::from_value::<Vec<String>>(
        contract["projection"]["configured_tool_names"].clone(),
    )
    .expect("configured tool names");
    let parent_tool_names = tool_names(parent_request);
    assert_eq!(
        parent_tool_names
            .into_iter()
            .filter(|name| expected_tool_names.contains(name))
            .collect::<Vec<_>>(),
        expected_tool_names
    );
    assert_eq!(
        parent_request.metadata["system_prompt_sources"],
        contract["projection"]["sources"]
    );
    assert_eq!(
        parent_request.metadata["system_prompt_stable_hash"],
        contract["projection"]["stable_hash"]
    );
    let mut expected_metadata_sections = contract["projection"]["sections"]
        .as_array()
        .expect("fixture sections")
        .clone();
    for section in &mut expected_metadata_sections {
        section
            .as_object_mut()
            .expect("fixture section object")
            .remove("priority");
    }
    assert_eq!(
        parent_request.metadata["system_prompt_sections"],
        Value::Array(expected_metadata_sections)
    );
    assert_eq!(
        parent_request.messages[0].content.chars().count(),
        max_context_chars
    );
}
