use super::*;

#[derive(Clone, Copy)]
enum ProviderPanicStage {
    Resolve,
    Client,
}

#[derive(Clone)]
struct PanickingChildModelProvider {
    stage: ProviderPanicStage,
}

impl ModelProvider for PanickingChildModelProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        if matches!(self.stage, ProviderPanicStage::Resolve) {
            panic!("configured child provider resolve panicked");
        }
        Ok(ResolvedModelConfig::new(
            "child-backend",
            model.model(),
            model.model(),
            model.model(),
            Vec::new(),
        ))
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        if matches!(self.stage, ProviderPanicStage::Client) {
            panic!("configured child provider client panicked");
        }
        Ok(Arc::new(ScriptedLlmClient::new(Vec::new())))
    }
}

#[test]
fn provider_resolve_and_client_panics_emit_one_failed_completion_and_cleanup() {
    for (label, stage, expected_error) in [
        (
            "resolve",
            ProviderPanicStage::Resolve,
            "configured child provider resolve panicked",
        ),
        (
            "client",
            ProviderPanicStage::Client,
            "configured child provider client panicked",
        ),
    ] {
        let lifecycle = Arc::new(Mutex::new(Vec::<(String, BTreeMap<String, Value>)>::new()));
        let lifecycle_for_handler = lifecycle.clone();
        let event_handler: vv_agent::RunEventHandler = Arc::new(move |run_event| {
            let (name, payload) = super::typed_event_parts(run_event);
            if matches!(name.as_str(), "sub_run_started" | "sub_run_completed") {
                lifecycle_for_handler
                    .lock()
                    .expect("provider panic lifecycle")
                    .push((name.to_string(), payload.clone()));
            }
        });
        let parent_llm = ScriptedLlmClient::from_steps(vec![
            ScriptStep::response(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    format!("delegate-{label}"),
                    "create_sub_task",
                    json!({
                        "agent_id": "researcher",
                        "task_description": format!("panic during {label}"),
                        "wait_for_completion": false
                    }),
                )],
            )),
            ScriptStep::response(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    format!("parent-finish-{label}"),
                    "task_finish",
                    json!({"message": "parent done"}),
                )],
            )),
        ]);
        let mut parent = AgentTask::new(
            format!("provider-panic-parent-{label}"),
            "parent-model",
            "Parent prompt",
            "Delegate",
        );
        parent.max_cycles = 3;
        parent.sub_agents.insert(
            "researcher".to_string(),
            SubAgentConfig::new("child-model", "Research"),
        );
        let manager = SubTaskManager::default();
        let provider: Arc<dyn ModelProvider> = Arc::new(PanickingChildModelProvider { stage });

        let parent_result = AgentRuntime::new(parent_llm)
            .run_with_controls(
                parent,
                RuntimeRunControls {
                    event_handler: Some(event_handler),
                    model_provider: Some(provider),
                    sub_task_manager: Some(manager.clone()),
                    ..RuntimeRunControls::default()
                },
            )
            .expect("parent survives provider panic");
        let payload: Value = serde_json::from_str(&parent_result.cycles[0].tool_results[0].content)
            .expect("provider panic async payload");
        let task_id = payload["task_id"].as_str().expect("provider panic task id");
        let session_id = payload["session_id"]
            .as_str()
            .expect("provider panic session id");
        assert!(
            manager.wait(task_id, Some(Duration::from_secs(3))),
            "{label}"
        );

        let snapshot = manager.get(task_id).expect("provider panic snapshot");
        let outcome = snapshot.outcome.as_ref().expect("provider panic outcome");
        let lifecycle = lifecycle.lock().expect("provider panic lifecycle");
        assert_eq!(parent_result.status, AgentStatus::Completed, "{label}");
        assert_eq!(outcome.status, AgentStatus::Failed, "{label}");
        assert_eq!(outcome.error.as_deref(), Some(expected_error), "{label}");
        assert_eq!(outcome.error_code.as_deref(), Some("sub_task_failed"));
        assert!(!snapshot.running, "{label}");
        assert_eq!(
            manager.has_attached_session(task_id),
            Some(false),
            "{label}"
        );
        assert!(
            vv_agent::get_sub_agent_session(session_id).is_none(),
            "{label}"
        );
        assert_eq!(
            lifecycle
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["sub_run_started", "sub_run_completed"],
            "{label}"
        );
        assert_eq!(lifecycle[1].1["status"], "failed", "{label}");
        assert_eq!(lifecycle[0].1["run_id"], lifecycle[1].1["run_id"]);
    }
}
