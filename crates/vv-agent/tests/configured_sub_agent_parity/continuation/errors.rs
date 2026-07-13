use super::*;
use vv_agent::{_unregister_sub_agent_session, get_sub_agent_session, register_sub_agent_session};

struct NoopSession;

impl SubAgentSession for NoopSession {
    fn steer(&self, _prompt: &str) -> Result<(), String> {
        Ok(())
    }
}

struct FailingContinuationSession {
    should_panic: bool,
}

struct RegistryReplacingContinuationSession {
    task_id: String,
    session_id: String,
    replacement: Arc<dyn SubAgentSession>,
}

impl SubAgentSession for RegistryReplacingContinuationSession {
    fn steer(&self, _prompt: &str) -> Result<(), String> {
        Ok(())
    }

    fn continue_run(&self, _prompt: &str) -> Result<SubTaskOutcome, String> {
        register_sub_agent_session(self.session_id.clone(), self.replacement.clone());
        Ok(completed_outcome_for_manager(
            &self.task_id,
            &self.session_id,
        ))
    }
}

impl SubAgentSession for FailingContinuationSession {
    fn steer(&self, prompt: &str) -> Result<(), String> {
        self.continue_run(prompt).map(|_| ())
    }

    fn continue_run(&self, _prompt: &str) -> Result<SubTaskOutcome, String> {
        if self.should_panic {
            panic!("continuation panicked");
        }
        Err("continuation failed".to_string())
    }
}

#[test]
fn continuation_errors_match_fixture_exactly() {
    let fixture = contract();
    let errors = &fixture["manager"]["continuation_errors"];
    let expected = |key: &str, task_id: &str| {
        errors[key]
            .as_str()
            .expect("continuation error fixture")
            .replace("{task_id}", task_id)
    };

    assert_eq!(
        SubTaskManager::default()
            .continue_task("ignored", " \t ")
            .expect_err("empty prompt must fail"),
        expected("empty_prompt", "ignored")
    );
    assert_eq!(
        SubTaskManager::default()
            .continue_task("missing-task", "continue")
            .expect_err("missing task must fail"),
        expected("not_found", "missing-task")
    );

    let detached_manager = SubTaskManager::default();
    detached_manager.record_outcome(
        "detached-task",
        completed_outcome_for_manager("detached-task", "detached-session"),
    );
    assert_eq!(
        detached_manager
            .continue_task("detached-task", "continue")
            .expect_err("detached session must fail"),
        expected("session_not_attached", "detached-task")
    );

    let max_cycles_manager = SubTaskManager::default();
    max_cycles_manager.record_outcome(
        "max-cycles-task",
        SubTaskOutcome {
            status: AgentStatus::MaxCycles,
            final_answer: None,
            error: Some("max cycles".to_string()),
            cycles: 8,
            ..completed_outcome_for_manager("max-cycles-task", "max-cycles-session")
        },
    );
    assert_eq!(
        max_cycles_manager
            .continue_task("max-cycles-task", "continue")
            .expect_err("max-cycles task must fail"),
        expected("max_cycles", "max-cycles-task")
    );
}

#[test]
fn continuation_generated_failures_keep_resolved_and_fixture_error_code() {
    let fixture = contract();
    let expected_error_code = fixture["continuation"]["failure_error_code"]
        .as_str()
        .expect("continuation failure error code");

    for (suffix, should_panic) in [("error", false), ("panic", true)] {
        let task_id = format!("continuation-{suffix}");
        let session_id = format!("continuation-{suffix}-session");
        let manager = SubTaskManager::default();
        manager.attach_session_with_resolved(SubTaskSessionAttachment {
            task_id: task_id.clone(),
            session_id: session_id.clone(),
            agent_name: "researcher".to_string(),
            task_title: "initial task".to_string(),
            workspace_backend: Arc::new(MemoryWorkspaceBackend::default()),
            session: Arc::new(FailingContinuationSession { should_panic }),
            resolved: BTreeMap::from([("model_id".to_string(), "fixture-model".to_string())]),
        });
        manager.record_outcome(
            &task_id,
            completed_outcome_for_manager(&task_id, &session_id),
        );

        manager
            .continue_task(&task_id, "continue")
            .expect("admit failing continuation");
        assert!(manager.wait(&task_id, Some(Duration::from_secs(2))));
        let entries = manager.status_entries(std::slice::from_ref(&task_id), "basic", 10);

        assert_eq!(entries[0]["status"], "failed");
        assert_eq!(entries[0]["error_code"], expected_error_code);
        assert_eq!(entries[0]["resolved"]["model_id"], "fixture-model");
        assert!(!manager.get(&task_id).expect("failed snapshot").running);
    }
}

#[test]
fn continuation_cleanup_preserves_same_id_replacement_session() {
    let task_id = format!("registry-replacement-{}", uuid::Uuid::new_v4().simple());
    let session_id = format!("{task_id}-session");
    let manager = SubTaskManager::default();
    let replacement: Arc<dyn SubAgentSession> = Arc::new(NoopSession);
    let original: Arc<dyn SubAgentSession> = Arc::new(RegistryReplacingContinuationSession {
        task_id: task_id.clone(),
        session_id: session_id.clone(),
        replacement: replacement.clone(),
    });
    manager.attach_session(
        task_id.clone(),
        session_id.clone(),
        "researcher",
        "initial task",
        Arc::new(MemoryWorkspaceBackend::default()),
        original,
    );
    manager.record_outcome(
        &task_id,
        completed_outcome_for_manager(&task_id, &session_id),
    );

    manager
        .continue_task(&task_id, "replace global registry entry")
        .expect("admit replacement continuation");
    assert!(manager.wait(&task_id, Some(Duration::from_secs(2))));

    let registered = get_sub_agent_session(&session_id).expect("replacement remains registered");
    assert!(Arc::ptr_eq(&registered, &replacement));
    assert!(_unregister_sub_agent_session(
        &session_id,
        Some(replacement)
    ));
}
