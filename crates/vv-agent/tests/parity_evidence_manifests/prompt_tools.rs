fn normalize_computer_os(value: &str) -> String {
    ["Windows", "macOS", "Linux"]
        .into_iter()
        .fold(value.to_string(), |text, label| text.replace(label, "<OS>"))
}

fn project_prompt_output(mut bundle: PromptBundle, normalizations: &[Value]) -> Value {
    bundle.validate().expect("valid prompt bundle");
    if normalizations.iter().any(|value| value == "computer_os") {
        for section in &mut bundle.sections {
            section.text = normalize_computer_os(&section.text);
        }
        bundle = PromptBundle::new(bundle.sections).expect("normalized prompt bundle");
    }
    json!({
        "flat_prompt": bundle.flatten(),
        "sections": bundle.sections,
        "stable_hash": bundle.stable_hash,
    })
}

fn prompt_section_from_value(raw: &Value) -> PromptSection {
    let mut section = PromptSection::new(
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
    section
}

fn compile_prompt_scenario(input: &Value) -> PromptBundle {
    let mut sections = input["instruction_bundle"]["sections"]
        .as_array()
        .expect("instruction sections")
        .iter()
        .map(prompt_section_from_value)
        .collect::<Vec<_>>();
    sections.extend(
        input["compiler_owned_sections"]
            .as_array()
            .expect("compiler-owned sections")
            .iter()
            .map(prompt_section_from_value),
    );

    let mut provider_sections = input["provider_fragments"]
        .as_array()
        .expect("provider fragments")
        .iter()
        .enumerate()
        .map(|(emission_order, raw)| {
            (
                raw["priority"].as_i64().expect("provider priority"),
                emission_order,
                prompt_section_from_value(raw),
            )
        })
        .collect::<Vec<_>>();
    provider_sections.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| right.2.stable.cmp(&left.2.stable))
            .then_with(|| left.2.id.encode_utf16().cmp(right.2.id.encode_utf16()))
            .then_with(|| left.1.cmp(&right.1))
    });
    sections.extend(
        provider_sections
            .into_iter()
            .map(|(_, _, section)| section),
    );
    PromptBundle::new(sections).expect("compiled prompt bundle")
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
                session_memory_enabled: input["session_memory_enabled"]
                    .as_bool()
                    .expect("session_memory_enabled"),
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
                builder.add_section(prompt_section_from_value(raw));
            }
            builder.build_result()
        }
        "PromptBundle" => PromptBundle::new(
            input["sections"]
                .as_array()
                .expect("prompt sections")
                .iter()
                .map(prompt_section_from_value)
                .collect(),
        )
        .expect("prompt bundle"),
        "AgentCompiler" => {
            let bundle = compile_prompt_scenario(input);
            return json!({
                "section_ids": bundle.sections.iter().map(|section| &section.id).collect::<Vec<_>>(),
                "flat_prompt": bundle.flatten(),
                "stable_hash": bundle.stable_hash,
            });
        }
        producer => panic!("unknown prompt producer: {producer}"),
    };
    let normalizations = scenario["normalizations"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    project_prompt_output(
        bundle,
        normalizations,
    )
}

fn exposure_name(exposure: ToolExposure) -> &'static str {
    match exposure {
        ToolExposure::Direct => "direct",
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
        "contract": "vv-agent-builtin-tools-v2",
        "schema_version": 2,
        "exposure_contract": {
            "allowed_values": ["direct", "hidden"],
            "model_visible_values": ["direct"],
            "host_only_values": ["hidden"],
            "unknown_values": "reject",
        },
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
        BTreeSet::from([
            "AgentCompiler",
            "PromptBundle",
            "SystemPromptBuilder",
            "build_system_prompt_bundle",
        ])
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
    assert_eq!(tools.len(), 15);
    assert!(tools.iter().all(|tool| tool["model_visible"] == true));
}
