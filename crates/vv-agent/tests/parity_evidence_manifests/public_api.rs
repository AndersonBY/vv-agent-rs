fn public_export_path(id: &str) -> &'static str {
    match id {
        "agent.definition" => export_type!(vv_agent::Agent, "vv_agent::Agent"),
        "agent.run_context" => export_type!(vv_agent::RunContext, "vv_agent::RunContext"),
        "agent.tool_use_behavior" => export_type!(
            vv_agent::agent::ToolUseBehavior,
            "vv_agent::agent::ToolUseBehavior"
        ),
        "agent.handoff" => export_type!(vv_agent::Handoff, "vv_agent::Handoff"),
        "agent.background_task" => export_type!(
            vv_agent::BackgroundAgentTask,
            "vv_agent::BackgroundAgentTask"
        ),
        "agent.guardrail_outcome" => {
            export_type!(
                vv_agent::GuardrailOutcome<String>,
                "vv_agent::GuardrailOutcome"
            )
        }
        "runner.facade" => export_type!(vv_agent::Runner, "vv_agent::Runner"),
        "runner.configured" => export_type!(
            vv_agent::runner::RunnerBuilder,
            "vv_agent::runner::RunnerBuilder"
        ),
        "run_config.options" => export_type!(vv_agent::RunConfig, "vv_agent::RunConfig"),
        "run_config.tool_policy" => export_type!(vv_agent::ToolPolicy, "vv_agent::ToolPolicy"),
        "run_config.approval_policy" => {
            export_type!(vv_agent::ApprovalPolicy, "vv_agent::ApprovalPolicy")
        }
        "run_config.cancellation" => {
            export_type!(vv_agent::CancellationToken, "vv_agent::CancellationToken")
        }
        "run_config.approval_broker" => {
            export_type!(vv_agent::ApprovalBroker, "vv_agent::ApprovalBroker")
        }
        "run_config.context_provider" => {
            export_type!(dyn vv_agent::ContextProvider, "vv_agent::ContextProvider")
        }
        "result.public" => export_type!(vv_agent::RunResult, "vv_agent::RunResult"),
        "result.resume_state" => export_type!(vv_agent::RunState, "vv_agent::RunState"),
        "result.approval_snapshot" => {
            export_type!(vv_agent::ApprovalSnapshot, "vv_agent::ApprovalSnapshot")
        }
        "result.runtime_result" => export_type!(vv_agent::AgentResult, "vv_agent::AgentResult"),
        "result.status" => export_type!(vv_agent::AgentStatus, "vv_agent::AgentStatus"),
        "run_handle.live" => export_type!(vv_agent::RunHandle, "vv_agent::RunHandle"),
        "run_handle.snapshot" => {
            export_type!(vv_agent::RunHandleState, "vv_agent::RunHandleState")
        }
        "run_handle.status" => {
            export_type!(vv_agent::RunHandleStatus, "vv_agent::RunHandleStatus")
        }
        "interactive.session" => {
            export_type!(vv_agent::InteractiveSession, "vv_agent::InteractiveSession")
        }
        "interactive.options" => export_type!(
            vv_agent::InteractiveSessionOptions,
            "vv_agent::InteractiveSessionOptions"
        ),
        "interactive.state" => export_type!(
            vv_agent::InteractiveSessionState,
            "vv_agent::InteractiveSessionState"
        ),
        "interactive.client" => export_type!(
            vv_agent::InteractiveAgentClient,
            "vv_agent::InteractiveAgentClient"
        ),
        "interactive.event" => export_type!(
            vv_agent::InteractiveSessionEvent,
            "vv_agent::InteractiveSessionEvent"
        ),
        "interactive.error" => export_type!(
            vv_agent::InteractiveSessionError,
            "vv_agent::InteractiveSessionError"
        ),
        "app_server.server" => export_type!(
            vv_agent::app_server::AppServer<
                vv_agent::app_server::transport::stdio::StdioJsonlTransport,
            >,
            "vv_agent::app_server::AppServer"
        ),
        "app_server.host" => {
            export_type!(dyn vv_agent::AppServerHost, "vv_agent::AppServerHost")
        }
        "app_server.default_host" => export_type!(
            vv_agent::DefaultAppServerHost,
            "vv_agent::DefaultAppServerHost"
        ),
        "app_server.client" => export_type!(
            vv_agent::app_server::client::AppServerClient,
            "vv_agent::app_server::client::AppServerClient"
        ),
        "app_server.client_error" => export_type!(
            vv_agent::app_server::client::AppServerClientError,
            "vv_agent::app_server::client::AppServerClientError"
        ),
        "app_server.processor" => export_type!(
            vv_agent::app_server::processor::MessageProcessor,
            "vv_agent::app_server::processor::MessageProcessor"
        ),
        "app_server.outgoing_router" => export_type!(
            vv_agent::app_server::outgoing::OutgoingMessageSender,
            "vv_agent::app_server::outgoing::OutgoingMessageSender"
        ),
        "app_server.transport" => export_type!(
            dyn vv_agent::app_server::transport::AppServerTransport,
            "vv_agent::app_server::transport::AppServerTransport"
        ),
        "app_server.channel_transport" => export_type!(
            vv_agent::app_server::transport::channel::ChannelTransport,
            "vv_agent::app_server::transport::channel::ChannelTransport"
        ),
        "app_server.stdio_transport" => export_type!(
            vv_agent::app_server::transport::stdio::StdioJsonlTransport,
            "vv_agent::app_server::transport::stdio::StdioJsonlTransport"
        ),
        "app_server.jsonrpc_message" => export_type!(
            vv_agent::app_server::protocol::JsonRpcMessage,
            "vv_agent::app_server::protocol::JsonRpcMessage"
        ),
        "tools.public_tool" => export_type!(dyn vv_agent::Tool, "vv_agent::Tool"),
        "tools.function_tool" => {
            export_type!(vv_agent::FunctionTool, "vv_agent::FunctionTool")
        }
        "tools.registry" => export_type!(vv_agent::ToolRegistry, "vv_agent::ToolRegistry"),
        "tools.executor" => export_type!(dyn vv_agent::ToolExecutor, "vv_agent::ToolExecutor"),
        "tools.spec" => export_type!(vv_agent::ToolSpec, "vv_agent::ToolSpec"),
        "tools.context" => export_type!(vv_agent::ToolContext, "vv_agent::ToolContext"),
        "tools.call_context" => {
            export_type!(vv_agent::ToolCallContext, "vv_agent::ToolCallContext")
        }
        "tools.orchestrator" => {
            export_type!(vv_agent::ToolOrchestrator, "vv_agent::ToolOrchestrator")
        }
        "tools.exposure" => export_type!(vv_agent::ToolExposure, "vv_agent::ToolExposure"),
        "tools.output" => export_type!(vv_agent::ToolOutput, "vv_agent::ToolOutput"),
        "tools.spec_executor" => {
            export_type!(vv_agent::ToolSpecExecutor, "vv_agent::ToolSpecExecutor")
        }
        "tools.not_found_error" => {
            export_type!(vv_agent::ToolNotFoundError, "vv_agent::ToolNotFoundError")
        }
        "workspace.backend" => {
            export_type!(dyn vv_agent::WorkspaceBackend, "vv_agent::WorkspaceBackend")
        }
        "workspace.local" => export_type!(
            vv_agent::LocalWorkspaceBackend,
            "vv_agent::LocalWorkspaceBackend"
        ),
        "workspace.memory" => export_type!(
            vv_agent::MemoryWorkspaceBackend,
            "vv_agent::MemoryWorkspaceBackend"
        ),
        "workspace.s3" => {
            export_type!(vv_agent::S3WorkspaceBackend, "vv_agent::S3WorkspaceBackend")
        }
        "workspace.file_info" => export_type!(vv_agent::FileInfo, "vv_agent::FileInfo"),
        "workspace.discovery_filter" => export_type!(
            vv_agent::DiscoveryFilteredWorkspaceBackend,
            "vv_agent::DiscoveryFilteredWorkspaceBackend"
        ),
        "workspace.portable_regex_error" => {
            export_type!(vv_agent::PortableRegexError, "vv_agent::PortableRegexError")
        }
        "memory.manager" => export_type!(vv_agent::MemoryManager, "vv_agent::MemoryManager"),
        "memory.provider" => {
            export_type!(dyn vv_agent::MemoryProvider, "vv_agent::MemoryProvider")
        }
        "memory.provider_result" => export_type!(
            vv_agent::MemoryProviderResult,
            "vv_agent::MemoryProviderResult"
        ),
        "memory.session" => export_type!(vv_agent::SessionMemory, "vv_agent::SessionMemory"),
        "memory.session_config" => export_type!(
            vv_agent::SessionMemoryConfig,
            "vv_agent::SessionMemoryConfig"
        ),
        "memory.session_entry" => {
            export_type!(vv_agent::SessionMemoryEntry, "vv_agent::SessionMemoryEntry")
        }
        "memory.session_state" => {
            export_type!(vv_agent::SessionMemoryState, "vv_agent::SessionMemoryState")
        }
        "memory.search_request" => export_type!(
            vv_agent::MemorySearchRequest,
            "vv_agent::MemorySearchRequest"
        ),
        "memory.search_result" => {
            export_type!(vv_agent::MemorySearchResult, "vv_agent::MemorySearchResult")
        }
        "memory.save_request" => {
            export_type!(vv_agent::MemorySaveRequest, "vv_agent::MemorySaveRequest")
        }
        "memory.save_result" => {
            export_type!(vv_agent::MemorySaveResult, "vv_agent::MemorySaveResult")
        }
        "memory.compaction_exhausted" => export_type!(
            vv_agent::CompactionExhaustedError,
            "vv_agent::CompactionExhaustedError"
        ),
        "skills.properties" => export_type!(
            vv_agent::skills::SkillProperties,
            "vv_agent::skills::SkillProperties"
        ),
        "skills.loaded" => export_type!(
            vv_agent::skills::LoadedSkill,
            "vv_agent::skills::LoadedSkill"
        ),
        "skills.entry" => {
            export_type!(vv_agent::skills::SkillEntry, "vv_agent::skills::SkillEntry")
        }
        "skills.error" => {
            export_type!(vv_agent::skills::SkillError, "vv_agent::skills::SkillError")
        }
        "skills.parse_error" => export_type!(
            vv_agent::skills::SkillParseError,
            "vv_agent::skills::SkillParseError"
        ),
        "skills.validation_error" => export_type!(
            vv_agent::skills::SkillValidationError,
            "vv_agent::skills::SkillValidationError"
        ),
        "skills.validation_diagnostics" => export_type!(
            vv_agent::skills::ValidationDiagnostics,
            "vv_agent::skills::ValidationDiagnostics"
        ),
        "skills.validation_mode" => export_type!(
            vv_agent::skills::ValidationMode,
            "vv_agent::skills::ValidationMode"
        ),
        "tracing.span" => export_type!(vv_agent::Span, "vv_agent::Span"),
        "tracing.sink" => export_type!(dyn vv_agent::TraceSink, "vv_agent::TraceSink"),
        "tracing.jsonl_exporter" => {
            export_type!(vv_agent::JsonlTraceExporter, "vv_agent::JsonlTraceExporter")
        }
        "llm_bridge.client" => export_type!(dyn vv_agent::LlmClient, "vv_agent::LlmClient"),
        "llm_bridge.request" => export_type!(vv_agent::LlmRequest, "vv_agent::LlmRequest"),
        "llm_bridge.error" => export_type!(vv_agent::LlmError, "vv_agent::LlmError"),
        "llm_bridge.endpoint" => {
            export_type!(vv_agent::EndpointTarget, "vv_agent::EndpointTarget")
        }
        "llm_bridge.scripted" => {
            export_type!(vv_agent::ScriptedLlmClient, "vv_agent::ScriptedLlmClient")
        }
        "llm_bridge.vv_llm_client" => {
            export_type!(vv_agent::VvLlmClient, "vv_agent::VvLlmClient")
        }
        "llm_bridge.model_provider" => {
            export_type!(dyn vv_agent::ModelProvider, "vv_agent::ModelProvider")
        }
        "llm_bridge.model_ref" => export_type!(vv_agent::ModelRef, "vv_agent::ModelRef"),
        "llm_bridge.model_settings" => {
            export_type!(vv_agent::ModelSettings, "vv_agent::ModelSettings")
        }
        "llm_bridge.response_format" => {
            export_type!(vv_agent::ResponseFormat, "vv_agent::ResponseFormat")
        }
        "llm_bridge.retry_settings" => {
            export_type!(vv_agent::RetrySettings, "vv_agent::RetrySettings")
        }
        "llm_bridge.tool_choice" => export_type!(vv_agent::ToolChoice, "vv_agent::ToolChoice"),
        "runtime_backend.execution" => export_type!(
            vv_agent::RuntimeExecutionBackend,
            "vv_agent::RuntimeExecutionBackend"
        ),
        "runtime_backend.inline" => {
            export_type!(vv_agent::InlineBackend, "vv_agent::InlineBackend")
        }
        "runtime_backend.thread" => {
            export_type!(vv_agent::ThreadBackend, "vv_agent::ThreadBackend")
        }
        "runtime_backend.distributed" => {
            export_type!(vv_agent::DistributedBackend, "vv_agent::DistributedBackend")
        }
        "runtime_backend.envelope" => export_type!(
            vv_agent::DistributedRunEnvelope,
            "vv_agent::DistributedRunEnvelope"
        ),
        "runtime_backend.capability_registry" => export_type!(
            vv_agent::DistributedCapabilityRegistry,
            "vv_agent::DistributedCapabilityRegistry"
        ),
        "runtime_backend.capability_ref" => {
            export_type!(vv_agent::CapabilityRef, "vv_agent::CapabilityRef")
        }
        "runtime_backend.recipe" => {
            export_type!(vv_agent::RuntimeRecipe, "vv_agent::RuntimeRecipe")
        }
        "runtime_backend.state_store" => {
            export_type!(dyn vv_agent::StateStore, "vv_agent::StateStore")
        }
        "runtime_backend.in_memory_state_store" => {
            export_type!(vv_agent::InMemoryStateStore, "vv_agent::InMemoryStateStore")
        }
        "runtime_backend.sqlite_state_store" => {
            export_type!(vv_agent::SqliteStateStore, "vv_agent::SqliteStateStore")
        }
        "runtime_backend.redis_state_store" => {
            export_type!(vv_agent::RedisStateStore, "vv_agent::RedisStateStore")
        }
        "runtime_backend.checkpoint" => {
            export_type!(vv_agent::Checkpoint, "vv_agent::Checkpoint")
        }
        "runtime_backend.agent_runtime" => {
            export_type!(
                vv_agent::AgentRuntime<vv_agent::ScriptedLlmClient>,
                "vv_agent::AgentRuntime"
            )
        }
        "runtime_backend.cycle_runner" => {
            export_type!(
                vv_agent::CycleRunner<vv_agent::ScriptedLlmClient>,
                "vv_agent::CycleRunner"
            )
        }
        "runtime_backend.tool_call_runner" => {
            export_type!(vv_agent::ToolCallRunner, "vv_agent::ToolCallRunner")
        }
        other => panic!("public API capability is not compiled by Rust test: {other}"),
    }
}

macro_rules! compile_methods {
    ($name:expr, $type:ty, [$($method:ident),+ $(,)?]) => {
        match $name {
            $(stringify!($method) => { let _ = <$type>::$method; })+
            other => panic!("uncompiled Rust method `{other}` on `{}`", stringify!($type)),
        }
    };
}

macro_rules! compile_fields {
    ($name:expr, $type:ty, [$($field:ident),+ $(,)?]) => {
        match $name {
            $(stringify!($field) => {
                let accessor = |value: &$type| { let _ = &value.$field; };
                let _ = accessor;
            })+
            other => panic!("uncompiled Rust field `{other}` on `{}`", stringify!($type)),
        }
    };
}

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
                max_handoffs,
                tool_policy,
                execution_backend,
                cancellation_token,
                hooks,
                event_store,
                event_store_fail_closed,
                approval_provider,
                approval_timeout,
                approval_broker,
                context_providers,
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
                runtime_log_handler,
                runtime_stream_callback,
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
                is_enabled,
                as_tool_spec,
            ]
        ),
        ("tool", "vv_agent::ToolExecutor", "method") => {
            compile_methods!(name, dyn vv_agent::ToolExecutor, [metadata, run])
        }
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
    let fixture = load_fixture("public_api_v1.json");
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
    assert_eq!(capability_ids.len(), 109);

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
