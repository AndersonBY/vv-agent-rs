use super::*;

impl Runner {
    pub(super) fn run_single_agent(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        config: RunConfig,
        event_collector: Option<Arc<std::sync::Mutex<Vec<RunEvent>>>>,
        event_sender: Option<broadcast::Sender<RunEvent>>,
        mut checkpoint_admission_sender: Option<CheckpointAdmissionSender>,
    ) -> Result<SingleRunOutcome, String> {
        let checkpoint_config = config
            .checkpoint_config
            .clone()
            .or_else(|| self.default_run_config.checkpoint_config.clone());
        let checkpoint_extensions = self
            .default_run_config
            .checkpoint_extensions
            .iter()
            .chain(config.checkpoint_extensions.iter())
            .cloned()
            .collect::<Vec<_>>();
        let reconciliation_provider = config
            .reconciliation_provider
            .clone()
            .or_else(|| self.default_run_config.reconciliation_provider.clone());
        let max_cycles = validate_max_cycles(
            config
                .max_cycles
                .or(self.default_run_config.max_cycles)
                .or(agent.max_cycles())
                .unwrap_or(10),
        )?;
        let no_tool_policy = config
            .no_tool_policy
            .or(self.default_run_config.no_tool_policy)
            .or(agent.no_tool_policy())
            .unwrap_or_default();
        let provider = config
            .model_provider
            .clone()
            .or_else(|| self.default_run_config.model_provider.clone())
            .unwrap_or_else(|| self.model_provider.clone());
        let model_ref = effective_model_ref(agent, &self.default_run_config, &config, &provider)
            .ok_or_else(|| "agent model is not configured".to_string())?;
        let (event_store, event_store_fail_closed) =
            effective_event_store(&self.default_run_config, &config);
        let workspace = config
            .workspace
            .clone()
            .or_else(|| self.default_run_config.workspace.clone())
            .unwrap_or_else(|| self.workspace.clone());
        let session = config
            .session
            .clone()
            .or_else(|| self.default_run_config.session.clone());
        let (preloaded_checkpoint, checkpoint_resume) =
            prepare_checkpoint_resume(agent, session.as_ref(), checkpoint_config.as_ref())?;
        let tool_policy = merged_tool_policy(
            agent.tool_policy(),
            &self.default_run_config.tool_policy,
            &config.tool_policy,
        );
        let approval_provider = config
            .approval_provider
            .clone()
            .or_else(|| self.default_run_config.approval_provider.clone());
        let approval_broker = config
            .approval_broker
            .clone()
            .or_else(|| self.default_run_config.approval_broker.clone())
            .or_else(|| {
                approval_provider
                    .as_ref()
                    .map(|_| ApprovalBroker::default())
            });
        let approval_timeout = config
            .approval_timeout
            .or(self.default_run_config.approval_timeout);
        let cancellation_token = config
            .cancellation_token
            .clone()
            .or_else(|| self.default_run_config.cancellation_token.clone());
        let budget_limits = config
            .budget_limits
            .clone()
            .or_else(|| self.default_run_config.budget_limits.clone());
        let host_cost_meter = config
            .host_cost_meter
            .clone()
            .or_else(|| self.default_run_config.host_cost_meter.clone());
        let initial_budget_usage =
            initial_budget_usage(&self.default_run_config.metadata, &config.metadata)?;
        let memory_providers = self
            .default_run_config
            .memory_providers
            .iter()
            .chain(config.memory_providers.iter())
            .cloned()
            .collect::<Vec<_>>();
        let run_id = preloaded_checkpoint
            .as_ref()
            .filter(|_| checkpoint_resume)
            .map(|checkpoint| checkpoint.root_run_id.clone())
            .unwrap_or_else(|| format!("run_{}", uuid::Uuid::new_v4().simple()));
        let mut run_metadata = if checkpoint_resume {
            preloaded_checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.run_definition.get("run_metadata"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect()
        } else {
            let mut metadata = agent.metadata().clone();
            metadata.extend(self.default_run_config.metadata.clone());
            metadata.extend(config.metadata.clone());
            metadata
        };
        run_metadata.remove(INITIAL_BUDGET_USAGE_METADATA_KEY);
        let trace_id = preloaded_checkpoint
            .as_ref()
            .filter(|_| checkpoint_resume)
            .map(|checkpoint| checkpoint.trace_id.clone())
            .unwrap_or_else(|| {
                effective_trace_id(&self.default_run_config, &config, &run_metadata)
            });
        let workflow_name = effective_workflow_name(&self.default_run_config, &config);
        run_metadata.insert("trace_id".to_string(), Value::String(trace_id.clone()));
        let app_state = config
            .app_state
            .clone()
            .or_else(|| self.default_run_config.app_state.clone());
        let mut run_context = RunContext {
            run_id: run_id.clone(),
            agent_name: agent.name().to_string(),
            model: Some(model_ref.clone()),
            workspace: Some(workspace.clone()),
            metadata: run_metadata,
            app_state: app_state.clone(),
        };
        let trace_sink = config
            .trace_sink
            .clone()
            .or_else(|| self.default_run_config.trace_sink.clone());
        let trace = RunTrace::start(
            trace_sink.clone(),
            &trace_id,
            &run_context.run_id,
            agent.name(),
            workflow_name.as_deref(),
        );
        let event_session_id = session
            .as_ref()
            .map(|session| session.session_id().trim())
            .filter(|session_id| !session_id.is_empty())
            .map(str::to_string)
            .or_else(|| {
                run_context
                    .metadata
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|session_id| !session_id.is_empty())
                    .map(str::to_string)
            });
        let original_input = input.text.clone();
        let frozen_input = preloaded_checkpoint
            .as_ref()
            .filter(|_| checkpoint_resume)
            .and_then(|checkpoint| checkpoint.run_definition.get("root_input"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if frozen_input
            .as_ref()
            .is_some_and(|frozen| frozen != &original_input)
        {
            return Err(
                "checkpoint_definition_mismatch: resume input does not match the checkpoint run definition"
                    .to_string(),
            );
        }
        let input_text = if let Some(frozen_input) = frozen_input {
            frozen_input
        } else {
            match apply_input_guardrails(agent, &run_context, input) {
                GuardrailOutcome::Allow(input) => input.text,
                GuardrailOutcome::Block { message }
                | GuardrailOutcome::RequireApproval { message } => {
                    let mut failed_event = RunEvent::run_failed(
                        &run_context.run_id,
                        &trace_id,
                        agent.name(),
                        AgentErrorPayload::new(&message),
                    )
                    .with_completion_details(
                        Some(crate::types::CompletionReason::Failed),
                        None,
                        None,
                    );
                    if let Some(session_id) = event_session_id.as_ref() {
                        failed_event = failed_event.with_session_id(session_id);
                    }
                    capture_event(
                        event_collector.as_ref(),
                        event_sender.as_ref(),
                        event_store.as_ref(),
                        event_store_fail_closed,
                        failed_event,
                    )?;
                    let ended_run_span =
                        trace.finish("failed", Some(("error", Value::String(message.clone()))));
                    let events = event_collector
                        .as_ref()
                        .and_then(|collector| collector.lock().ok().map(|events| events.clone()))
                        .unwrap_or_default();
                    let mut result_metadata = run_context.metadata.clone();
                    result_metadata.insert(
                        "run_span".to_string(),
                        serde_json::to_value(ended_run_span)
                            .unwrap_or_else(|error| Value::String(error.to_string())),
                    );
                    return Ok(SingleRunOutcome {
                        result: RunResult::without_resolved_model(
                            agent.name().to_string(),
                            AgentResult::failed(message),
                        )
                        .with_ids(&run_context.run_id, &trace_id)
                        .with_input(original_input)
                        .with_events(events)
                        .with_metadata(result_metadata),
                        handoff: None,
                    });
                }
            }
        };
        let resolved = provider.resolve(&model_ref).map_err(format_model_error)?;
        run_context.model = Some(ModelRef::named(resolved.model_id.clone()));
        let tool_enablement_context =
            ToolEnablementContext::new(run_context.clone()).with_app_state(app_state.clone());
        let mut llm = provider.client(&resolved).map_err(format_model_error)?;
        if let Some(debug_dump_dir) = config
            .debug_dump_dir
            .as_ref()
            .or(self.default_run_config.debug_dump_dir.as_ref())
        {
            llm = llm
                .clone_with_debug_dump_dir(debug_dump_dir)
                .ok_or_else(|| {
                    "configured LLM client does not support debug_dump_dir".to_string()
                })?;
        }
        let provider_settings = provider.default_settings(&resolved);
        let settings = provider_settings
            .merge(
                self.default_run_config
                    .model_settings
                    .as_ref()
                    .unwrap_or(&crate::model_settings::ModelSettings::default()),
            )
            .merge(agent.model_settings())
            .merge(
                config
                    .model_settings
                    .as_ref()
                    .unwrap_or(&crate::model_settings::ModelSettings::default()),
            );
        settings
            .validate()
            .map_err(|error| format!("invalid model settings: {error}"))?;
        let session_items = if checkpoint_resume {
            Vec::new()
        } else if let Some(session) = session.as_ref() {
            block_on_session(session.get_items(None))?
        } else {
            Vec::new()
        };
        let event_context = RuntimeEventContext::new(
            &run_context.run_id,
            &trace_id,
            agent.name(),
            event_session_id.clone(),
            &input_text,
        );
        let (mut task, definition_initial_messages) = if checkpoint_resume {
            let checkpoint = preloaded_checkpoint.as_ref().ok_or_else(|| {
                "checkpoint_not_found: resume checkpoint disappeared before task restoration"
                    .to_string()
            })?;
            let task = build_frozen_task(agent, checkpoint, &settings)
                .map_err(|error| error.to_string())?;
            let initial_messages =
                frozen_definition_messages(checkpoint).map_err(|error| error.to_string())?;
            (task, initial_messages)
        } else {
            let (instructions, context_bundle) =
                self.build_instructions_with_context(InstructionBuildRequest {
                    agent,
                    run_context: &run_context,
                    input_text: &input_text,
                    config: &config,
                    model: &resolved.model_id,
                    trace_id: &trace_id,
                    session: session.clone(),
                    workspace: &workspace,
                })?;
            let mut task = AgentTask::new(
                run_id,
                resolved.model_id.clone(),
                instructions,
                input_text.clone(),
            );
            task.max_cycles = max_cycles;
            task.no_tool_policy = no_tool_policy;
            task.has_sub_agents = false;
            task.sub_agents = agent.sub_agents().clone();
            task.metadata = agent.metadata().clone();
            task.metadata
                .extend(self.default_run_config.metadata.clone());
            task.metadata.extend(config.metadata.clone());
            task.metadata.remove(INITIAL_BUDGET_USAGE_METADATA_KEY);
            if let Some(context_bundle) = context_bundle {
                insert_context_metadata(&mut task.metadata, &context_bundle);
            }
            task.model_settings = Some(settings.clone());
            task.initial_shared_state = self.default_run_config.initial_shared_state.clone();
            task.initial_shared_state
                .extend(config.initial_shared_state.clone());
            task.initial_messages = config
                .initial_messages
                .clone()
                .or_else(|| self.default_run_config.initial_messages.clone())
                .unwrap_or_else(|| {
                    session_items
                        .iter()
                        .map(SessionItem::to_message)
                        .collect::<Vec<_>>()
                });
            let initial_messages = task.initial_messages.clone();
            (task, initial_messages)
        };
        task.metadata
            .entry("agent_name".to_string())
            .or_insert_with(|| Value::String(agent.name().to_string()));
        task.metadata
            .insert("trace_id".to_string(), Value::String(trace_id.clone()));
        task.metadata.insert(
            "_vv_agent_run_id".to_string(),
            Value::String(run_context.run_id.clone()),
        );
        task.metadata.insert(
            "_vv_agent_trace_id".to_string(),
            Value::String(trace_id.clone()),
        );
        task.metadata.insert(
            "_vv_agent_agent_name".to_string(),
            Value::String(agent.name().to_string()),
        );
        task.metadata.insert(
            "_vv_agent_input".to_string(),
            Value::String(input_text.clone()),
        );
        if let Some(session_id) = event_session_id.as_ref() {
            task.metadata.insert(
                "_vv_agent_session_id".to_string(),
                Value::String(session_id.clone()),
            );
        }
        match agent.tool_use_behavior() {
            crate::agent::ToolUseBehavior::RunLlmAgain => {
                task.metadata.insert(
                    "_vv_agent_tool_use_behavior".to_string(),
                    Value::String("run_llm_again".to_string()),
                );
            }
            crate::agent::ToolUseBehavior::StopOnFirstTool => {
                task.metadata.insert(
                    "_vv_agent_tool_use_behavior".to_string(),
                    Value::String("stop_on_first_tool".to_string()),
                );
            }
            crate::agent::ToolUseBehavior::StopAtToolNames(names) => {
                task.metadata.insert(
                    "_vv_agent_tool_use_behavior".to_string(),
                    Value::String("stop_at_tool_names".to_string()),
                );
                task.metadata.insert(
                    "_vv_agent_stop_at_tool_names".to_string(),
                    Value::Array(names.iter().cloned().map(Value::String).collect()),
                );
            }
        }
        project_tool_policy(&mut task, &tool_policy);
        apply_resolved_model_limits(&mut task, &resolved);
        let session_result_prefix_len = definition_initial_messages.len()
            + usize::from(
                definition_initial_messages
                    .first()
                    .is_none_or(|message| message.role != MessageRole::System),
            );
        let mut registry = config
            .tool_registry_factory
            .as_ref()
            .or(self.default_run_config.tool_registry_factory.as_ref())
            .map(|factory| factory())
            .unwrap_or_else(|| self.tool_registry.clone());
        for tool_name in registry.list_planner_extra_tool_names() {
            if !task.extra_tool_names.contains(&tool_name) {
                task.extra_tool_names.push(tool_name);
            }
        }
        for handoff in agent.handoffs() {
            let spec = handoff.as_tool_spec(agent.name());
            if !task.extra_tool_names.contains(&spec.name) {
                task.extra_tool_names.push(spec.name.clone());
            }
            registry.register(spec)?;
        }
        for tool in agent.tools() {
            if !tool.is_enabled(&tool_enablement_context) {
                continue;
            }
            let spec = tool.as_tool_spec();
            if spec.exposure != crate::tools::ToolExposure::Hidden
                && !task.extra_tool_names.contains(&spec.name)
            {
                task.extra_tool_names.push(spec.name.clone());
            }
            registry.register(spec)?;
        }
        let definition_registry = registry.clone();
        let pending_tool_approval = Arc::new(Mutex::new(None));
        let mut runtime = AgentRuntime::new(ArcLlmClient(llm))
            .with_tool_registry(registry)
            .with_settings_file("__runner_model_provider__")
            .with_default_backend(resolved.backend.clone())
            .with_tool_policy(tool_policy.clone())
            .with_pending_tool_approval(pending_tool_approval.clone());
        if let Some(log_preview_chars) = config
            .log_preview_chars
            .or(self.default_run_config.log_preview_chars)
        {
            runtime.log_preview_chars = Some(log_preview_chars);
        }
        runtime.default_workspace = Some(workspace.clone());
        runtime.workspace_backend = config
            .workspace_backend
            .clone()
            .or_else(|| self.default_run_config.workspace_backend.clone())
            .unwrap_or_else(|| Arc::new(LocalWorkspaceBackend::new(workspace.clone())));
        if let Some(execution_backend) = config
            .execution_backend
            .clone()
            .or_else(|| self.default_run_config.execution_backend.clone())
        {
            runtime.execution_backend = execution_backend;
        }
        runtime.hooks.extend(agent.hooks().iter().cloned());
        runtime
            .hooks
            .extend(self.default_run_config.hooks.iter().cloned());
        runtime.hooks.extend(config.hooks.iter().cloned());
        runtime.after_cycle_hooks.extend(
            self.default_run_config
                .after_cycle_hooks
                .iter()
                .chain(config.after_cycle_hooks.iter())
                .cloned(),
        );
        runtime
            .hooks
            .push(Arc::new(ApprovalHook::new(tool_policy.clone())));
        let has_event_sink = event_collector.is_some()
            || event_sender.is_some()
            || event_store.is_some()
            || trace.is_enabled();
        let event_store_error = Arc::new(Mutex::new(None::<String>));
        let runtime_log_handler = config
            .runtime_log_handler
            .clone()
            .or_else(|| self.default_run_config.runtime_log_handler.clone());
        let log_handler = if has_event_sink || runtime_log_handler.is_some() {
            let collector = event_collector.clone();
            let event_sender = event_sender.clone();
            let event_store = event_store.clone();
            let event_context = event_context.clone();
            let event_store_error = event_store_error.clone();
            let trace_observer = trace.observer();
            let configured_runtime_log_handler = runtime_log_handler.clone();
            Some(Arc::new(
                move |event: &str, payload: &std::collections::BTreeMap<String, Value>| {
                    if has_event_sink {
                        trace_observer.on_event(event, payload);
                        if !is_runtime_terminal_log(event)
                            && event_store_error
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .is_none()
                        {
                            if let Some(mapped) = map_runtime_event(event, payload, &event_context)
                            {
                                if let Err(error) = capture_event(
                                    collector.as_ref(),
                                    event_sender.as_ref(),
                                    event_store.as_ref(),
                                    event_store_fail_closed,
                                    mapped,
                                ) {
                                    *event_store_error
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                        Some(error);
                                }
                            }
                        }
                    }
                    if let Some(handler) = configured_runtime_log_handler.as_ref() {
                        handler(event, payload);
                    }
                },
            ) as crate::runtime::RuntimeEventHandler)
        } else {
            None
        };
        let runtime_stream_callback = config
            .runtime_stream_callback
            .clone()
            .or_else(|| self.default_run_config.runtime_stream_callback.clone());
        let stream_callback = if has_event_sink || runtime_stream_callback.is_some() {
            let collector = event_collector.clone();
            let event_sender = event_sender.clone();
            let event_store = event_store.clone();
            let event_context = event_context.clone();
            let event_store_error = event_store_error.clone();
            let configured_runtime_stream_callback = runtime_stream_callback.clone();
            Some(
                Arc::new(move |payload: &std::collections::BTreeMap<String, Value>| {
                    if has_event_sink
                        && event_store_error
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .is_none()
                    {
                        if let Some(mapped) = map_stream_event(payload, &event_context) {
                            if let Err(error) = capture_event(
                                collector.as_ref(),
                                event_sender.as_ref(),
                                event_store.as_ref(),
                                event_store_fail_closed,
                                mapped,
                            ) {
                                *event_store_error
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                    Some(error);
                            }
                        }
                    }
                    if let Some(callback) = configured_runtime_stream_callback.as_ref() {
                        callback(payload);
                    }
                }) as crate::llm::LlmStreamCallback,
            )
        } else {
            None
        };
        let mut background_parent_run_config = config.clone();
        background_parent_run_config.model = Some(model_ref.clone());
        background_parent_run_config.model_provider = Some(provider.clone());
        background_parent_run_config.model_settings = Some(settings.clone());
        background_parent_run_config.workspace = Some(workspace.clone());
        background_parent_run_config.workspace_backend = Some(runtime.workspace_backend.clone());
        background_parent_run_config.session = session.clone();
        background_parent_run_config.initial_messages = Some(definition_initial_messages.clone());
        background_parent_run_config.max_cycles = Some(task.max_cycles);
        background_parent_run_config.max_handoffs = Some(
            config
                .max_handoffs
                .or(self.default_run_config.max_handoffs)
                .unwrap_or(10),
        );
        background_parent_run_config.tool_policy = tool_policy.clone();
        background_parent_run_config.execution_backend = Some(runtime.execution_backend.clone());
        background_parent_run_config.cancellation_token = cancellation_token.clone();
        background_parent_run_config.hooks = self
            .default_run_config
            .hooks
            .iter()
            .chain(config.hooks.iter())
            .cloned()
            .collect();
        background_parent_run_config.after_cycle_hooks = self
            .default_run_config
            .after_cycle_hooks
            .iter()
            .chain(config.after_cycle_hooks.iter())
            .cloned()
            .collect();
        background_parent_run_config.trace_sink = trace_sink;
        background_parent_run_config.trace_id = Some(trace_id.clone());
        background_parent_run_config.workflow_name = workflow_name.clone();
        background_parent_run_config.event_store = event_store.clone();
        background_parent_run_config.event_store_fail_closed = event_store_fail_closed;
        background_parent_run_config.approval_provider = approval_provider.clone();
        background_parent_run_config.approval_timeout = approval_timeout;
        background_parent_run_config.approval_broker = approval_broker.clone();
        background_parent_run_config.context_providers = self
            .default_run_config
            .context_providers
            .iter()
            .chain(config.context_providers.iter())
            .cloned()
            .collect();
        background_parent_run_config.max_context_chars = config
            .max_context_chars
            .or(self.default_run_config.max_context_chars);
        background_parent_run_config.memory_providers = memory_providers.clone();
        background_parent_run_config.app_state = app_state.clone();
        background_parent_run_config.initial_shared_state = task.initial_shared_state.clone();
        background_parent_run_config.tool_registry_factory = config
            .tool_registry_factory
            .clone()
            .or_else(|| self.default_run_config.tool_registry_factory.clone());
        background_parent_run_config.log_preview_chars = config
            .log_preview_chars
            .or(self.default_run_config.log_preview_chars);
        background_parent_run_config.debug_dump_dir = config
            .debug_dump_dir
            .clone()
            .or_else(|| self.default_run_config.debug_dump_dir.clone());
        background_parent_run_config.before_cycle_messages = config
            .before_cycle_messages
            .clone()
            .or_else(|| self.default_run_config.before_cycle_messages.clone());
        background_parent_run_config.interruption_messages = config
            .interruption_messages
            .clone()
            .or_else(|| self.default_run_config.interruption_messages.clone());
        background_parent_run_config.sub_task_manager = config
            .sub_task_manager
            .clone()
            .or_else(|| self.default_run_config.sub_task_manager.clone());
        background_parent_run_config.runtime_log_handler = runtime_log_handler;
        background_parent_run_config.runtime_stream_callback = runtime_stream_callback;
        background_parent_run_config.budget_limits = budget_limits.clone();
        background_parent_run_config.host_cost_meter = host_cost_meter.clone();
        background_parent_run_config.metadata = if checkpoint_resume {
            preloaded_checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.run_definition.get("run_metadata"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect()
        } else {
            let mut metadata = self.default_run_config.metadata.clone();
            metadata.extend(config.metadata.clone());
            metadata
        };
        background_parent_run_config
            .metadata
            .remove(INITIAL_BUDGET_USAGE_METADATA_KEY);
        background_parent_run_config.checkpoint_config = checkpoint_config.clone();
        background_parent_run_config.checkpoint_extensions = checkpoint_extensions.clone();
        background_parent_run_config.reconciliation_provider = reconciliation_provider.clone();
        let mut definition_run_config = background_parent_run_config.clone();
        definition_run_config.workspace = config
            .workspace
            .clone()
            .or_else(|| self.default_run_config.workspace.clone());
        definition_run_config.workspace_backend = config
            .workspace_backend
            .clone()
            .or_else(|| self.default_run_config.workspace_backend.clone());

        let CheckpointRuntimeState {
            controller: checkpoint_controller,
            mut terminal_replayed,
            replayed_result,
            initial_budget_usage,
            initial_messages: checkpoint_initial_messages,
            initial_cycles: checkpoint_initial_cycles,
            initial_shared_state: checkpoint_initial_shared_state,
            cycle_index_start: checkpoint_cycle_index_start,
            cycle_count: checkpoint_cycle_count,
        } = prepare_checkpoint_runtime(CheckpointRuntimeRequest {
            config: checkpoint_config,
            agent,
            input_text: &input_text,
            run_config: &definition_run_config,
            resolved: &resolved,
            model_settings: &settings,
            task: &task,
            registry: &definition_registry,
            definition_initial_messages: &definition_initial_messages,
            run_id: &run_context.run_id,
            trace_id: &trace_id,
            initial_budget_usage,
            extensions: checkpoint_extensions,
            reconciliation_provider,
            event_collector: event_collector.clone(),
            event_sender: event_sender.clone(),
            event_store: event_store.clone(),
            preloaded_checkpoint,
            checkpoint_resume,
            backend_manages_checkpoint_cycles: runtime
                .execution_backend
                .manages_checkpoint_cycles(),
            admission_sender: &mut checkpoint_admission_sender,
        })?;
        let controls = RuntimeRunControls {
            log_handler,
            before_cycle_messages: config
                .before_cycle_messages
                .clone()
                .or_else(|| self.default_run_config.before_cycle_messages.clone()),
            interruption_messages: config
                .interruption_messages
                .clone()
                .or_else(|| self.default_run_config.interruption_messages.clone()),
            cancellation_token: cancellation_token.clone(),
            execution_context: Some(ExecutionContext {
                stream_callback,
                metadata: task.metadata.clone(),
                approval_provider,
                approval_broker,
                approval_timeout,
                memory_providers,
                app_state,
                ..ExecutionContext::default()
            }),
            workspace: Some(workspace),
            workspace_backend: runtime.workspace_backend.clone().into(),
            model_provider: Some(provider.clone()),
            run_context: Some(run_context.clone()),
            background_parent_run_config: Some(background_parent_run_config),
            sub_task_manager: config
                .sub_task_manager
                .clone()
                .or_else(|| self.default_run_config.sub_task_manager.clone()),
            budget_limits,
            host_cost_meter,
            initial_budget_usage,
            initial_messages: checkpoint_initial_messages,
            initial_cycles: checkpoint_initial_cycles,
            initial_shared_state: checkpoint_initial_shared_state,
            cycle_index_start: checkpoint_cycle_index_start,
            cycle_count: checkpoint_cycle_count,
            checkpoint_controller: checkpoint_controller
                .clone()
                .map(CheckpointRuntimeControl::new),
            ..RuntimeRunControls::default()
        };
        let mut result = if let Some(replayed_result) = replayed_result {
            replayed_result
        } else {
            runtime
                .run_with_controls(task, controls)
                .map_err(|error| error.to_string())?
        };
        (result, terminal_replayed) =
            replay_checkpoint_terminal(checkpoint_controller.as_ref(), terminal_replayed, result)?;
        if let Some(error) = event_store_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            return Err(error);
        }
        result =
            prepare_checkpoint_terminal(checkpoint_controller.as_ref(), terminal_replayed, result)?;
        let reconciliation_required = result.status == AgentStatus::ReconciliationRequired;
        let operator_abort = result.status == AgentStatus::Failed
            && result.error.as_deref() == Some("operator_abort_with_unknown_outcome")
            && result.resume_observation.is_some();
        if !terminal_replayed && !reconciliation_required && !operator_abort {
            result = apply_output_guardrails(agent, &run_context, result);
            result = apply_cancellation_precedence(result, cancellation_token.as_ref());
        }
        let output_validation_error = if !reconciliation_required
            && !operator_abort
            && result.status == AgentStatus::Completed
        {
            result.final_answer.as_deref().and_then(|output| {
                agent.validate_output(output).err().map(|error| {
                    format!(
                        "failed to validate final output for agent `{}` as `{}`: {error}",
                        agent.name(),
                        agent.output_type_name().unwrap_or("configured output type")
                    )
                })
            })
        } else {
            None
        };
        let handoff = extract_handoff(&result);
        let new_items = if terminal_replayed || reconciliation_required || operator_abort {
            Vec::new()
        } else {
            result
                .messages
                .get(session_result_prefix_len..)
                .unwrap_or_default()
                .to_vec()
        };
        if !terminal_replayed && !reconciliation_required && !operator_abort {
            if let Some(session) = session.as_ref() {
                let approval_call_ids = result
                    .cycles
                    .iter()
                    .flat_map(|cycle| cycle.tool_results.iter())
                    .filter(|tool_result| {
                        tool_result.error_code.as_deref() == Some("tool_approval_required")
                    })
                    .map(|tool_result| tool_result.tool_call_id.as_str())
                    .collect::<std::collections::BTreeSet<_>>();
                let session_items = new_items
                    .iter()
                    .filter(|message| {
                        !(result.status == AgentStatus::WaitUser
                            && message.role == crate::types::MessageRole::Tool
                            && message
                                .tool_call_id
                                .as_deref()
                                .is_some_and(|call_id| approval_call_ids.contains(call_id)))
                    })
                    .filter_map(SessionItem::from_message)
                    .collect::<Vec<_>>();
                let session_commit_id = checkpoint_controller.as_ref().map(|controller| {
                    controller
                        .lock()
                        .map_err(|_| {
                            "checkpoint_store_lock_poisoned: checkpoint controller lock poisoned"
                                .to_string()
                        })
                        .and_then(|controller| {
                            controller
                                .checkpoint_key()
                                .map(checkpoint_session_commit_id)
                                .map_err(|error| error.to_string())
                        })
                });
                if let Some(commit_id) = session_commit_id.transpose()? {
                    let payload_digest = session_commit_payload_digest(&session_items)?;
                    block_on_session(session.add_items_once(
                        commit_id.clone(),
                        payload_digest,
                        session_items,
                    ))?;
                } else {
                    block_on_session(session.add_items(session_items))?;
                }
                if has_event_sink {
                    let payload = std::collections::BTreeMap::from([(
                        "session_id".to_string(),
                        Value::String(session.session_id().to_string()),
                    )]);
                    if let Some(event) =
                        map_runtime_event("session_persisted", &payload, &event_context)
                    {
                        if let Some(controller) = checkpoint_controller.as_ref() {
                            let commit_id = controller
                            .lock()
                            .map_err(|_| {
                                "checkpoint_store_lock_poisoned: checkpoint controller lock poisoned"
                                    .to_string()
                            })?
                            .checkpoint_key()
                            .map(checkpoint_session_commit_id)
                            .map_err(|error| error.to_string())?;
                            controller
                            .lock()
                            .map_err(|_| {
                                "checkpoint_store_lock_poisoned: checkpoint controller lock poisoned"
                                    .to_string()
                            })?
                            .persist_preterminal_event(event, &commit_id)
                            .map_err(|error| error.to_string())?;
                        } else {
                            capture_event(
                                event_collector.as_ref(),
                                event_sender.as_ref(),
                                event_store.as_ref(),
                                event_store_fail_closed,
                                event,
                            )?;
                        }
                    }
                }
            }
        }
        if !terminal_replayed && !reconciliation_required {
            let event = terminal_event(
                &result,
                &run_context.run_id,
                &trace_id,
                agent.name(),
                event_session_id.as_deref(),
                cancellation_token.as_ref(),
            );
            if let Some(controller) = checkpoint_controller.as_ref() {
                result = controller
                    .lock()
                    .map_err(|_| {
                        "checkpoint_store_lock_poisoned: checkpoint controller lock poisoned"
                            .to_string()
                    })?
                    .finalize(result, Some(event))
                    .map_err(|error| error.to_string())?;
            } else {
                capture_event(
                    event_collector.as_ref(),
                    event_sender.as_ref(),
                    event_store.as_ref(),
                    event_store_fail_closed,
                    event,
                )?;
            }
        }
        if let Some(controller) = checkpoint_controller.as_ref() {
            controller
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .close();
        }
        let ended_run_span = if let Some(error) = output_validation_error.as_ref() {
            trace.finish("failed", Some(("error", Value::String(error.clone()))))
        } else {
            trace.finish(
                &status_string(result.status),
                result
                    .final_answer
                    .clone()
                    .or_else(|| result.wait_reason.clone())
                    .or_else(|| result.error.clone())
                    .map(|output| ("final_output", Value::String(output))),
            )
        };
        let events = event_collector
            .as_ref()
            .and_then(|collector| collector.lock().ok().map(|events| events.clone()))
            .unwrap_or_default();
        let mut result_metadata = run_context.metadata.clone();
        result_metadata.insert(
            "resolved_model".to_string(),
            Value::String(resolved.model_id.clone()),
        );
        result_metadata.insert(
            "backend".to_string(),
            Value::String(resolved.backend.clone()),
        );
        result_metadata.insert(
            "run_span".to_string(),
            serde_json::to_value(ended_run_span)
                .unwrap_or_else(|error| Value::String(error.to_string())),
        );
        let pending_tool_approval = pending_tool_approval
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(error) = output_validation_error {
            return Err(error);
        }
        Ok(SingleRunOutcome {
            result: RunResult::new(agent.name().to_string(), result, resolved)
                .with_ids(&run_context.run_id, &trace_id)
                .with_input(&original_input)
                .with_new_items(new_items)
                .with_events(events)
                .with_metadata(result_metadata)
                .with_resume_context(RunResumeContext {
                    agent: agent.clone(),
                    input: NormalizedInput {
                        text: original_input.clone(),
                    },
                    config,
                    runner: self.clone(),
                    pending_tool_approval,
                }),
            handoff,
        })
    }
}
