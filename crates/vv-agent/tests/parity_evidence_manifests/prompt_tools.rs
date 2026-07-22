fn stable_hash(sections: &[Value]) -> String {
    let stable_text = sections
        .iter()
        .filter(|section| section["stable"].as_bool() == Some(true))
        .filter_map(|section| section["text"].as_str())
        .collect::<String>();
    format!("{:x}", Sha256::digest(stable_text.as_bytes()))
}

fn normalize_computer_os(value: &str) -> String {
    ["Windows", "macOS", "Linux"]
        .into_iter()
        .fold(value.to_string(), |text, label| text.replace(label, "<OS>"))
}

fn project_prompt_output(mut bundle: BuiltSystemPrompt, normalizations: &[Value]) -> Value {
    assert_eq!(bundle.stable_hash, stable_hash(&bundle.sections));
    if normalizations.iter().any(|value| value == "computer_os") {
        bundle.prompt = normalize_computer_os(&bundle.prompt);
        for section in &mut bundle.sections {
            let Some(text) = section["text"].as_str() else {
                continue;
            };
            section["text"] = Value::String(normalize_computer_os(text));
        }
    }
    let normalized_stable_hash = stable_hash(&bundle.sections);
    json!({
        "prompt": bundle.prompt,
        "sections": bundle.sections,
        "stable_hash": normalized_stable_hash,
    })
}

fn render_prompt_scenario(scenario: &Value) -> Value {
    let input = &scenario["input"];
    let bundle = match scenario["producer"].as_str().expect("prompt producer") {
        "build_system_prompt_bundle" => {
            let available_sub_agents: BTreeMap<String, String> =
                serde_json::from_value(input["available_sub_agents"].clone())
                    .expect("available_sub_agents");
            let options = BuildSystemPromptOptions {
                language: input["language"].as_str().expect("language").to_string(),
                allow_interruption: input["allow_interruption"]
                    .as_bool()
                    .expect("allow_interruption"),
                use_workspace: input["use_workspace"].as_bool().expect("use_workspace"),
                enable_todo_management: input["enable_todo_management"]
                    .as_bool()
                    .expect("enable_todo_management"),
                agent_type: input["agent_type"].as_str().map(str::to_string),
                available_sub_agents,
                available_skills: Some(input["available_skills"].clone()),
                workspace: None,
                current_time_utc: Some(
                    input["current_time_utc"]
                        .as_str()
                        .expect("current_time_utc")
                        .to_string(),
                ),
                session_memory_context: input["session_memory_context"]
                    .as_str()
                    .expect("session_memory_context")
                    .to_string(),
            };
            build_system_prompt_bundle_with_options(
                input["original_system_prompt"]
                    .as_str()
                    .expect("original_system_prompt"),
                options,
            )
        }
        "SystemPromptBuilder" => {
            let mut builder = SystemPromptBuilder::default();
            for raw in input["sections"].as_array().expect("prompt sections") {
                let mut section = PromptSection::constant(
                    raw["id"].as_str().expect("section id"),
                    raw["text"].as_str().expect("section text"),
                    raw["stable"].as_bool().expect("section stable"),
                );
                if let Some(source) = raw["source"].as_str() {
                    section = section.source(source);
                }
                if let Some(cache_hint) = raw["cache_hint"].as_str() {
                    section = section.cache_hint(cache_hint);
                }
                if let Some(metadata) = raw["metadata"].as_object() {
                    for (key, value) in metadata {
                        section = section.metadata(key, value.clone());
                    }
                }
                builder.add_section(section);
            }
            builder.build_result()
        }
        producer => panic!("unknown prompt producer: {producer}"),
    };
    project_prompt_output(
        bundle,
        scenario["normalizations"]
            .as_array()
            .expect("normalizations"),
    )
}

fn exposure_name(exposure: ToolExposure) -> &'static str {
    match exposure {
        ToolExposure::Direct => "direct",
        ToolExposure::Deferred => "deferred",
        ToolExposure::DirectModelOnly => "direct_model_only",
        ToolExposure::Hidden => "hidden",
    }
}

fn approval_name(approval: &ToolApprovalRule) -> &'static str {
    match approval {
        ToolApprovalRule::Static(ApprovalRequirement::NotRequired) => "not_required",
        ToolApprovalRule::Static(ApprovalRequirement::Required) => "required",
        ToolApprovalRule::Static(ApprovalRequirement::Provider) => "provider",
        ToolApprovalRule::Predicate(_) => "dynamic",
    }
}

fn kind_name(kind: ToolSpecKind) -> &'static str {
    match kind {
        ToolSpecKind::Function => "function",
        ToolSpecKind::Agent => "agent",
        ToolSpecKind::BackgroundAgent => "background_agent",
        ToolSpecKind::Handoff => "handoff",
    }
}

fn build_builtin_tools_manifest() -> Value {
    let registry = vv_agent::build_default_registry();
    let tools = registry
        .executors()
        .into_iter()
        .map(|executor| {
            let spec = executor.spec(&ToolSpecContext).expect("default tool spec");
            let schema = registry
                .get_schema(executor.name())
                .expect("default tool schema");
            let function = schema["function"].as_object().expect("function schema");
            assert_eq!(function["name"], executor.name());
            assert_eq!(function["description"], executor.description());
            let timeout_seconds = executor
                .timeout()
                .map(|duration| json!(duration.as_secs_f64()))
                .unwrap_or(Value::Null);
            json!({
                "approval": approval_name(&spec.approval),
                "description": executor.description(),
                "exposure": exposure_name(executor.exposure()),
                "kind": kind_name(spec.kind),
                "metadata": executor.metadata(),
                "model_visible": executor.exposure() != ToolExposure::Hidden,
                "name": executor.name(),
                "parameters": function["parameters"],
                "strict": spec.strict_schema,
                "timeout_seconds": timeout_seconds,
                "type": schema["type"],
            })
        })
        .collect::<Vec<_>>();
    json!({
        "contract": "vv-agent-builtin-tools-v1",
        "schema_version": 1,
        "tools": tools,
    })
}

#[test]
fn prompt_bundle_manifest_uses_real_rust_prompt_producers() {
    let mut fixture = load_fixture("prompt_bundle.json");
    let scenarios = fixture["scenarios"]
        .as_array_mut()
        .expect("prompt scenarios");
    let producers = scenarios
        .iter()
        .map(|scenario| scenario["producer"].as_str().expect("producer"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        producers,
        BTreeSet::from(["SystemPromptBuilder", "build_system_prompt_bundle"])
    );
    for scenario in scenarios {
        let output = render_prompt_scenario(scenario);
        assert_eq!(scenario["output"], output, "{}", scenario["id"]);
        scenario["output"] = output;
    }
}

#[test]
fn builtin_tools_manifest_uses_real_rust_default_registry() {
    let fixture = load_fixture("builtin_tools.json");
    let actual = build_builtin_tools_manifest();
    assert_eq!(fixture, actual);
    let tools = fixture["tools"].as_array().expect("builtin tools");
    assert_eq!(tools.len(), 16);
    assert!(tools.iter().all(|tool| tool["model_visible"] == true));
}
