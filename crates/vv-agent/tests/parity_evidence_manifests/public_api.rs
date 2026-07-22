fn compile_rust_member(surface: &str, target: &str, name: &str, kind: &str) {
    match (surface, target, kind) {
        ("agent", "vv_agent::Agent", "method") => compile_methods!(
            name,
            vv_agent::Agent,
            [
                name,
                instructions,
                model,
                model_settings,
                tools,
                handoffs,
                output_type_name,
                hooks,
                max_cycles,
                no_tool_policy,
                tool_policy,
                tool_use_behavior,
                metadata,
                sub_agents,
                resolve_instructions,
                as_tool,
                as_background_task,
            ]
        ),
        ("agent", "vv_agent::agent::AgentBuilder", "method") => {
            compile_methods!(
                name,
                vv_agent::agent::AgentBuilder,
                [input_guardrail, output_guardrail]
            )
        }
        ("sub_agent_config", "vv_agent::SubAgentConfig", "field") => compile_fields!(
            name,
            vv_agent::SubAgentConfig,
            [
                model,
                description,
                backend,
                system_prompt,
                max_cycles,
                exclude_tools,
                metadata,
                denied_side_effects,
                denied_capability_tags,
                deny_terminal_tools,
                denied_cost_dimensions,
            ]
        ),
        ("runner", "vv_agent::Runner", "method") => match name {
            "run" => {
                let reference =
                    |value: &vv_agent::Runner, agent: &vv_agent::Agent, input: String| {
                        std::mem::drop(value.run(agent, input));
                    };
                let _ = reference;
            }
            "start" => {
                let reference = |value: &vv_agent::Runner,
                                 agent: &vv_agent::Agent,
                                 input: String,
                                 config: vv_agent::RunConfig| {
                    std::mem::drop(value.start(agent, input, config));
                };
                let _ = reference;
            }
            "stream" => {
                let reference =
                    |value: &vv_agent::Runner, agent: &vv_agent::Agent, input: String| {
                        std::mem::drop(value.stream(agent, input));
                    };
                let _ = reference;
            }
            other => compile_methods!(other, vv_agent::Runner, [resume, builder]),
        },
        ("run_config", "vv_agent::RunConfig", "field") => compile_fields!(
            name,
            vv_agent::RunConfig,
            [
                model,
                model_provider,
                model_settings,
                workspace,
                workspace_backend,
                session,
                initial_messages,
                max_cycles,
                no_tool_policy,
                max_handoffs,
                tool_policy,
                execution_backend,
                cancellation_token,
                hooks,
                after_cycle_hooks,
                event_store,
                event_store_fail_closed,
                approval_provider,
                approval_timeout,
                approval_broker,
                context_providers,
                budget_limits,
                host_cost_meter,
                max_context_chars,
                memory_providers,
                app_state,
                initial_shared_state,
                tool_registry_factory,
                log_preview_chars,
                debug_dump_dir,
                before_cycle_messages,
                interruption_messages,
                sub_task_manager,
                checkpoint_config,
                checkpoint_extensions,
                reconciliation_provider,
                metadata,
                trace_sink,
                trace_id,
                workflow_name,
            ]
        ),
        ("run_config", "vv_agent::RunHandle", "method") => {
            compile_methods!(name, vv_agent::RunHandle, [events])
        }
        ("run_result", "vv_agent::RunResult", "method") => compile_methods!(
            name,
            vv_agent::RunResult,
            [
                input,
                new_items,
                final_output,
                status,
                completion_reason,
                completion_tool_name,
                partial_output,
                budget_usage,
                budget_exhaustion,
                checkpoint_key,
                resume_observation,
                result,
                events,
                token_usage,
                trace_id,
                run_id,
                metadata,
                agent_name,
                resolved_model,
                approval_snapshot,
                into_state,
                to_dict,
            ]
        ),
        ("run_state", "vv_agent::RunState", "method") => compile_methods!(
            name,
            vv_agent::RunState,
            [
                result,
                from_result,
                approve,
                pending_approval_ids,
                approved_interruption_ids,
                approval_snapshot,
            ]
        ),
        ("run_handle", "vv_agent::RunHandle", "method") => match name {
            "steer" => {
                let reference = |value: &vv_agent::RunHandle, input: String| {
                    let _ = value.steer(input);
                };
                let _ = reference;
            }
            "follow_up" => {
                let reference = |value: &vv_agent::RunHandle, input: String| {
                    let _ = value.follow_up(input);
                };
                let _ = reference;
            }
            "approve" => {
                let reference =
                    |value: &vv_agent::RunHandle,
                     request_id: String,
                     decision: vv_agent::ApprovalDecision| {
                        std::mem::drop(value.approve(request_id, decision));
                    };
                let _ = reference;
            }
            other => compile_methods!(
                other,
                vv_agent::RunHandle,
                [cancel, events, result, state, resume,]
            ),
        },
        ("interactive_session", "vv_agent::InteractiveSession", "method") => match name {
            "steer" => {
                let reference = |value: &vv_agent::InteractiveSession, input: String| {
                    let _ = value.steer(input);
                };
                let _ = reference;
            }
            "follow_up" => {
                let reference = |value: &vv_agent::InteractiveSession, input: String| {
                    let _ = value.follow_up(input);
                };
                let _ = reference;
            }
            "approve" => {
                let reference =
                    |value: &vv_agent::InteractiveSession,
                     request_id: String,
                     decision: vv_agent::ApprovalDecision| {
                        let _ = value.approve(request_id, decision);
                    };
                let _ = reference;
            }
            "prompt" => {
                let reference = |value: &vv_agent::InteractiveSession, input: String| {
                    std::mem::drop(value.prompt(input));
                };
                let _ = reference;
            }
            "query" => {
                let reference = |value: &vv_agent::InteractiveSession, input: String| {
                    std::mem::drop(value.query(input));
                };
                let _ = reference;
            }
            other => compile_methods!(
                other,
                vv_agent::InteractiveSession,
                [
                    messages,
                    session,
                    shared_state,
                    latest_run,
                    running,
                    closed,
                    active_run_handle,
                    subscribe,
                    close,
                    clear_queues,
                    cancel,
                    continue_run,
                    state,
                    replace_messages,
                    replace_shared_state,
                ]
            ),
        },
        ("interactive_client", "vv_agent::InteractiveAgentClient", "method") => {
            compile_methods!(name, vv_agent::InteractiveAgentClient, [create_session])
        }
        ("app_server", "vv_agent::app_server::AppServer", "method") => compile_methods!(
            name,
            vv_agent::app_server::AppServer<
                vv_agent::app_server::transport::stdio::StdioJsonlTransport,
            >,
            [run]
        ),
        ("app_server_client", "vv_agent::app_server::client::AppServerClient", "method") => {
            compile_methods!(
                name,
                vv_agent::app_server::client::AppServerClient,
                [
                    initialize,
                    start_thread,
                    resume_thread,
                    read_thread,
                    list_threads,
                    archive_thread,
                    unsubscribe_thread,
                    start_turn,
                    interrupt_turn,
                    resume_turn,
                    steer_turn,
                    follow_up_turn,
                    resolve_approval_request,
                    list_models,
                    export_schema,
                    resolve_approval,
                    send_response,
                    next_message,
                    close,
                ]
            )
        }
        ("tool", "vv_agent::Tool", "method") => compile_methods!(
            name,
            dyn vv_agent::Tool,
            [
                name,
                description,
                parameters_schema,
                strict_schema,
                exposure,
                timeout,
                approval_rule,
                tool_metadata,
                is_enabled,
                as_tool_spec,
            ]
        ),
        ("tool", "vv_agent::ToolExecutor", "method") => {
            compile_methods!(name, dyn vv_agent::ToolExecutor, [metadata, run])
        }
        ("tool_metadata", "vv_agent::ToolMetadata", "field") => compile_fields!(
            name,
            vv_agent::ToolMetadata,
            [
                side_effect,
                idempotency,
                terminal,
                capability_tags,
                cost_dimensions,
            ]
        ),
        ("tool_policy", "vv_agent::ToolPolicy", "field") => compile_fields!(
            name,
            vv_agent::ToolPolicy,
            [
                allowed_tools,
                disallowed_tools,
                approval,
                can_use_tool,
                denied_side_effects,
                denied_capability_tags,
                deny_terminal_tools,
                denied_cost_dimensions,
            ]
        ),
        ("tool_registry", "vv_agent::ToolRegistry", "method") => match name {
            "register_schema" => {
                let reference =
                    |value: &mut vv_agent::ToolRegistry, name: String, schema: Value| {
                        value.register_schema(name, schema)
                    };
                let _ = reference;
            }
            other => compile_methods!(
                other,
                vv_agent::ToolRegistry,
                [
                    register,
                    register_many,
                    get,
                    has_tool,
                    register_schemas,
                    get_schema,
                    list_openai_schemas,
                    execute,
                    executors,
                ]
            ),
        },
        ("workspace_backend", "vv_agent::WorkspaceBackend", "method") => {
            compile_methods!(
                name,
                dyn vv_agent::WorkspaceBackend,
                [
                    list_files, read_text, read_bytes, write_text, file_info, exists, is_file,
                    mkdir,
                ]
            )
        }
        ("memory_provider", "vv_agent::MemoryProvider", "method") => {
            compile_methods!(
                name,
                dyn vv_agent::MemoryProvider,
                [search, save, before_compact, after_compact]
            )
        }
        ("memory_manager", "vv_agent::MemoryManager", "method") => compile_methods!(
            name,
            vv_agent::MemoryManager,
            [
                compact,
                emergency_compact,
                effective_context_window,
                estimate_memory_usage_percentage,
                microcompact_messages,
                apply_session_memory_context,
                strip_session_memory_context,
            ]
        ),
        ("session_memory", "vv_agent::SessionMemory", "method") => compile_methods!(
            name,
            vv_agent::SessionMemory,
            [
                should_extract,
                extract,
                render_as_system_context,
                on_compaction,
                load,
            ]
        ),
        ("skills", "vv_agent::skills", "method") => match name {
            "discover_skill_dirs" => {
                let reference = |path: PathBuf| vv_agent::skills::discover_skill_dirs(path);
                let _ = reference;
            }
            "read_properties" => {
                let reference = |path: PathBuf| vv_agent::skills::read_properties(path);
                let _ = reference;
            }
            "read_skill" => {
                let reference =
                    |path: PathBuf, mode: Option<&str>| vv_agent::skills::read_skill(path, mode);
                let _ = reference;
            }
            "normalize_skill_list" => {
                let _ = vv_agent::skills::normalize_skill_list;
            }
            "render_skills_xml" => {
                let _ = vv_agent::skills::render_skills_xml;
            }
            "validate" => {
                let reference =
                    |path: PathBuf, mode: Option<&str>| vv_agent::skills::validate(path, mode);
                let _ = reference;
            }
            "validate_with_diagnostics" => {
                let reference = |path: PathBuf, mode: Option<&str>| {
                    vv_agent::skills::validate_with_diagnostics(path, mode)
                };
                let _ = reference;
            }
            other => panic!("uncompiled Rust skills function: {other}"),
        },
        ("tracing_span", "vv_agent::Span", "field") => compile_fields!(
            name,
            vv_agent::Span,
            [name, trace_id, span_id, parent_id, started_at, ended_at, metadata,]
        ),
        ("tracing_span", "vv_agent::Span", "method") => {
            compile_methods!(name, vv_agent::Span, [finish])
        }
        ("tracing_sink", "vv_agent::TraceSink", "method") => {
            compile_methods!(
                name,
                dyn vv_agent::TraceSink,
                [on_span_start, on_span_end, flush]
            )
        }
        ("llm_request", "vv_agent::LlmRequest", "field") => compile_fields!(
            name,
            vv_agent::LlmRequest,
            [model, messages, tools, metadata, model_settings,]
        ),
        ("llm_client", "vv_agent::LlmClient", "method") => {
            compile_methods!(
                name,
                dyn vv_agent::LlmClient,
                [complete, complete_with_stream]
            )
        }
        ("model_provider", "vv_agent::ModelProvider", "method") => {
            compile_methods!(
                name,
                dyn vv_agent::ModelProvider,
                [resolve, client, default_settings, default_model_ref,]
            )
        }
        ("host_cost_meter", "vv_agent::HostCostMeter", "method") => {
            compile_methods!(name, dyn vv_agent::HostCostMeter, [read])
        }
        ("runtime_backend", "vv_agent::RuntimeExecutionBackend", "method") => match name {
            "execute" => {
                type CycleFn = fn(
                    u32,
                    &mut Vec<vv_agent::Message>,
                    &mut Vec<vv_agent::CycleRecord>,
                    &mut BTreeMap<String, Value>,
                    Option<&vv_agent::CancellationToken>,
                ) -> Option<vv_agent::AgentResult>;
                let _ = vv_agent::RuntimeExecutionBackend::execute::<CycleFn>;
            }
            "parallel_map" => {
                let _ = vv_agent::RuntimeExecutionBackend::parallel_map::<
                    String,
                    String,
                    fn(String) -> String,
                >;
            }
            other => panic!("uncompiled Rust runtime backend method: {other}"),
        },
        other => panic!("public API member is not compiled by Rust test: {other:?} name={name}"),
    }
}

#[test]
fn public_api_manifest_compiles_real_rust_exports() {
    let fixture = load_fixture("public_api.json");
    assert_eq!(fixture["contract"], "vv-agent-public-api-v1");
    assert_eq!(fixture["schema_version"], 1);

    let domains = fixture["domains"].as_array().expect("public API domains");
    let domain_ids = domains
        .iter()
        .map(|domain| domain["id"].as_str().expect("domain id"))
        .collect::<Vec<_>>();
    assert_eq!(domain_ids, EXPECTED_DOMAINS);

    let mut capability_ids = BTreeSet::new();
    for domain in domains {
        let capabilities = domain["capabilities"]
            .as_array()
            .expect("domain capabilities");
        assert!(!capabilities.is_empty(), "empty domain: {}", domain["id"]);
        for capability in capabilities {
            let id = capability["id"].as_str().expect("capability id");
            assert!(capability_ids.insert(id));
            assert_eq!(
                capability["rust"].as_str().expect("Rust export path"),
                public_export_path(id),
                "{id}"
            );
        }
    }
    assert_eq!(capability_ids.len(), 149);

    let surfaces = fixture["surfaces"].as_array().expect("public API surfaces");
    let surface_map = surfaces
        .iter()
        .map(|surface| (surface["id"].as_str().expect("surface id"), surface))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(surface_map.len(), surfaces.len());

    let member_ids = |surface: &&Value, group: &str| {
        surface[group]
            .as_array()
            .map(|members| {
                members
                    .iter()
                    .map(|member| member["id"].as_str().expect("member id").to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    assert_eq!(
        member_ids(&surface_map["runner"], "members"),
        EXPECTED_RUNNER_OPERATIONS
    );
    assert_eq!(
        member_ids(&surface_map["run_handle"], "members"),
        EXPECTED_RUN_HANDLE_OPERATIONS
    );
    assert_eq!(
        member_ids(&surface_map["interactive_session"], "members"),
        EXPECTED_INTERACTIVE_SESSION_MEMBERS
    );
    assert_eq!(
        member_ids(&surface_map["app_server_client"], "protocol_operations"),
        EXPECTED_APP_SERVER_PROTOCOL_OPERATIONS
    );
    assert_eq!(
        member_ids(&surface_map["app_server_client"], "supporting_operations"),
        EXPECTED_APP_SERVER_SUPPORTING_OPERATIONS
    );

    for surface in surfaces {
        let surface_id = surface["id"].as_str().expect("surface id");
        let default_target = surface["rust_target"]
            .as_str()
            .expect("Rust surface target");
        let mut ids = BTreeSet::new();
        for group in ["members", "protocol_operations", "supporting_operations"] {
            let Some(members) = surface[group].as_array() else {
                continue;
            };
            for member in members {
                let id = member["id"].as_str().expect("member id");
                assert!(ids.insert(id), "duplicate member: {surface_id}.{id}");
                let rust = member["rust"].as_object().expect("Rust member evidence");
                compile_rust_member(
                    surface_id,
                    rust.get("target")
                        .and_then(Value::as_str)
                        .unwrap_or(default_target),
                    rust["name"].as_str().expect("Rust member name"),
                    rust["kind"].as_str().expect("Rust member kind"),
                );
            }
        }
        assert!(!ids.is_empty(), "empty public API surface: {surface_id}");
    }
}
