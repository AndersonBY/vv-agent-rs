use super::*;

struct AdmissionBlockingSession {
    task_id: String,
    session_id: String,
    sanitizer_entered: Arc<Barrier>,
    sanitizer_release: Arc<(Mutex<bool>, Condvar)>,
    sanitizer_calls: AtomicUsize,
    continuation_calls: AtomicUsize,
}

impl SubAgentSession for AdmissionBlockingSession {
    fn steer(&self, prompt: &str) -> Result<(), String> {
        self.continue_run(prompt).map(|_| ())
    }

    fn sanitize_for_resume(&self) -> usize {
        if self.sanitizer_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.sanitizer_entered.wait();
            let (released, wake) = &*self.sanitizer_release;
            let mut released = released.lock().expect("sanitizer release lock");
            while !*released {
                released = wake.wait(released).expect("sanitizer release wait");
            }
        }
        0
    }

    fn continue_run(&self, _prompt: &str) -> Result<SubTaskOutcome, String> {
        self.continuation_calls.fetch_add(1, Ordering::SeqCst);
        Ok(SubTaskOutcome {
            task_id: self.task_id.clone(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Completed,
            session_id: Some(self.session_id.clone()),
            final_answer: Some("continued".to_string()),
            wait_reason: None,
            error: None,
            error_code: None,
            cycles: 2,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        })
    }
}

struct RecoverableContinuationSession {
    task_id: String,
    panic_sanitizer_once: AtomicBool,
    continuation_calls: AtomicUsize,
}

impl SubAgentSession for RecoverableContinuationSession {
    fn steer(&self, prompt: &str) -> Result<(), String> {
        self.continue_run(prompt).map(|_| ())
    }

    fn sanitize_for_resume(&self) -> usize {
        if self.panic_sanitizer_once.swap(false, Ordering::SeqCst) {
            panic!("sanitizer panicked");
        }
        0
    }

    fn continue_run(&self, _prompt: &str) -> Result<SubTaskOutcome, String> {
        let call = self.continuation_calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(SubTaskOutcome {
            task_id: self.task_id.clone(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Completed,
            session_id: None,
            final_answer: Some(format!("continued {call}")),
            wait_reason: None,
            error: None,
            error_code: None,
            cycles: 1,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        })
    }
}

fn assert_continuation_admission_rolled_back(
    before: &vv_agent::ManagedSubTaskSnapshot,
    after: &vv_agent::ManagedSubTaskSnapshot,
) {
    assert_eq!(after.running, before.running);
    assert_eq!(after.status, before.status);
    assert_eq!(after.task_title, before.task_title);
    assert_eq!(after.recent_activity, before.recent_activity);
    assert_eq!(after.parent_run_id, before.parent_run_id);
    assert_eq!(after.parent_tool_call_id, before.parent_tool_call_id);
    assert_eq!(after.current_cycle_index, before.current_cycle_index);
    assert_eq!(after.latest_cycle, before.latest_cycle);
    assert_eq!(after.latest_tool_call, before.latest_tool_call);
    assert_eq!(after.outcome, before.outcome);
    assert_eq!(after.resolved, before.resolved);
    assert_eq!(after.updated_at, before.updated_at);
}

#[test]
fn continuation_admission_is_atomic_while_sanitizer_is_blocked() {
    let fixture = contract();
    let task_id = "atomic-continuation";
    let session_id = "atomic-continuation-session";
    let manager = SubTaskManager::default();
    let sanitizer_entered = Arc::new(Barrier::new(2));
    let sanitizer_release = Arc::new((Mutex::new(false), Condvar::new()));
    let session = Arc::new(AdmissionBlockingSession {
        task_id: task_id.to_string(),
        session_id: session_id.to_string(),
        sanitizer_entered: sanitizer_entered.clone(),
        sanitizer_release: sanitizer_release.clone(),
        sanitizer_calls: AtomicUsize::new(0),
        continuation_calls: AtomicUsize::new(0),
    });
    manager.attach_session(
        task_id,
        session_id,
        "researcher",
        "initial task",
        Arc::new(MemoryWorkspaceBackend::default()),
        session.clone(),
    );
    manager.record_outcome(task_id, completed_outcome_for_manager(task_id, session_id));

    let first_manager = manager.clone();
    let first = std::thread::spawn(move || first_manager.continue_task(task_id, "first follow-up"));
    sanitizer_entered.wait();

    let admitted_snapshot = manager.get(task_id);
    let registered = vv_agent::get_sub_agent_session(session_id)
        .expect("admitted continuation is registered before sanitizer returns");
    let expected_session: Arc<dyn SubAgentSession> = session.clone();
    assert!(Arc::ptr_eq(&registered, &expected_session));
    assert!(!manager.wait(task_id, Some(Duration::from_millis(40))));
    assert!(
        manager
            .get(task_id)
            .expect("snapshot remains queryable during admission")
            .running
    );
    let second = manager.continue_task(task_id, "second follow-up");
    let (released, wake) = &*sanitizer_release;
    *released.lock().expect("sanitizer release lock") = true;
    wake.notify_all();
    let first = first.join().expect("first continuation caller");

    assert_eq!(first, Ok(()));
    assert_eq!(
        second,
        Err(fixture["manager"]["continuation_errors"]["already_running"]
            .as_str()
            .expect("already-running error fixture")
            .replace("{task_id}", task_id))
    );
    let admitted_snapshot = admitted_snapshot.expect("admitted task snapshot");
    assert!(admitted_snapshot.running);
    assert_eq!(admitted_snapshot.status, "running");
    assert_eq!(
        admitted_snapshot.running,
        fixture["manager"]["continuation_admission_atomic"]
            .as_bool()
            .expect("atomic admission fixture")
    );
    assert!(manager.wait(task_id, Some(Duration::from_secs(2))));
    assert_eq!(session.sanitizer_calls.load(Ordering::SeqCst), 1);
    assert_eq!(session.continuation_calls.load(Ordering::SeqCst), 1);
    assert!(!manager.get(task_id).expect("completed snapshot").running);
    assert!(vv_agent::get_sub_agent_session(session_id).is_none());
}

#[test]
fn sanitizer_panic_rolls_back_admission_and_session_remains_resumable() {
    let task_id = "sanitizer-rollback";
    let session_id = "sanitizer-rollback-session";
    let lineage = SubTaskLineage {
        parent_run_id: Some("initial-parent-run".to_string()),
        parent_tool_call_id: Some("initial-parent-call".to_string()),
    };
    let manager = SubTaskManager::default();
    let session = Arc::new(RecoverableContinuationSession {
        task_id: task_id.to_string(),
        panic_sanitizer_once: AtomicBool::new(true),
        continuation_calls: AtomicUsize::new(0),
    });
    manager.attach_session_with_resolved_and_lineage(
        SubTaskSessionAttachment {
            task_id: task_id.to_string(),
            session_id: session_id.to_string(),
            agent_name: "researcher".to_string(),
            task_title: "initial task".to_string(),
            workspace_backend: Arc::new(MemoryWorkspaceBackend::default()),
            session: session.clone(),
            resolved: BTreeMap::new(),
        },
        lineage,
    );
    let sidecar_lineage = SubTaskLineage {
        parent_run_id: Some("sidecar-parent-run".to_string()),
        parent_tool_call_id: Some("sidecar-parent-call".to_string()),
    };
    manager.record_outcome_with_context(
        task_id,
        completed_outcome_for_manager(task_id, session_id),
        None,
        sidecar_lineage,
    );
    let before = manager
        .get(task_id)
        .expect("snapshot before sanitizer panic");

    let error = manager
        .continue_task(task_id, "failed admission")
        .expect_err("sanitizer panic must fail admission");

    assert!(error.contains("continuation setup failed"));
    assert!(error.contains("sanitizer panicked"));
    let after = manager
        .get(task_id)
        .expect("snapshot after sanitizer panic");
    assert_continuation_admission_rolled_back(&before, &after);
    assert_eq!(manager.has_attached_session(task_id), Some(true));
    assert!(vv_agent::get_sub_agent_session(session_id).is_none());

    manager
        .continue_task(task_id, "retry after sanitizer panic")
        .expect("retained session remains resumable");
    assert!(manager.wait(task_id, Some(Duration::from_secs(2))));
    let resumed = manager.get(task_id).expect("resumed snapshot");
    assert_eq!(resumed.parent_run_id.as_deref(), Some("initial-parent-run"));
    assert_eq!(
        resumed.parent_tool_call_id.as_deref(),
        Some("initial-parent-call")
    );
    assert_eq!(
        resumed
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.final_answer.as_deref()),
        Some("continued 1")
    );
    assert_eq!(session.continuation_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn thread_spawn_failure_rolls_back_admission_and_retains_session() {
    let task_id = "spawn-rollback";
    let invalid_session_id = "spawn\0rollback-session";
    let valid_session_id = "spawn-rollback-session";
    let lineage = SubTaskLineage {
        parent_run_id: Some("initial-parent-run".to_string()),
        parent_tool_call_id: Some("initial-parent-call".to_string()),
    };
    let manager = SubTaskManager::default();
    let session = Arc::new(RecoverableContinuationSession {
        task_id: task_id.to_string(),
        panic_sanitizer_once: AtomicBool::new(false),
        continuation_calls: AtomicUsize::new(0),
    });
    manager.attach_session_with_resolved_and_lineage(
        SubTaskSessionAttachment {
            task_id: task_id.to_string(),
            session_id: invalid_session_id.to_string(),
            agent_name: "researcher".to_string(),
            task_title: "initial task".to_string(),
            workspace_backend: Arc::new(MemoryWorkspaceBackend::default()),
            session: session.clone(),
            resolved: BTreeMap::new(),
        },
        lineage.clone(),
    );
    let sidecar_lineage = SubTaskLineage {
        parent_run_id: Some("sidecar-parent-run".to_string()),
        parent_tool_call_id: Some("sidecar-parent-call".to_string()),
    };
    manager.record_outcome_with_context(
        task_id,
        completed_outcome_for_manager(task_id, invalid_session_id),
        None,
        sidecar_lineage.clone(),
    );
    let before = manager.get(task_id).expect("snapshot before spawn failure");

    let error = manager
        .continue_task(task_id, "failed spawn admission")
        .expect_err("invalid thread name must fail spawn");

    assert!(error.contains("continuation thread failed to spawn"));
    let after = manager.get(task_id).expect("snapshot after spawn failure");
    assert_continuation_admission_rolled_back(&before, &after);
    assert_eq!(manager.has_attached_session(task_id), Some(true));
    assert!(vv_agent::get_sub_agent_session(invalid_session_id).is_none());

    manager.attach_session_with_resolved_and_lineage(
        SubTaskSessionAttachment {
            task_id: task_id.to_string(),
            session_id: valid_session_id.to_string(),
            agent_name: "researcher".to_string(),
            task_title: "initial task".to_string(),
            workspace_backend: Arc::new(MemoryWorkspaceBackend::default()),
            session: session.clone(),
            resolved: BTreeMap::new(),
        },
        sidecar_lineage,
    );
    manager
        .continue_task(task_id, "retry after spawn failure")
        .expect("retained session can be resumed after reattachment");
    assert!(manager.wait(task_id, Some(Duration::from_secs(2))));
    assert_eq!(session.continuation_calls.load(Ordering::SeqCst), 1);
    let resumed = manager.get(task_id).expect("resumed snapshot");
    assert_eq!(resumed.parent_run_id.as_deref(), Some("initial-parent-run"));
    assert_eq!(
        resumed.parent_tool_call_id.as_deref(),
        Some("initial-parent-call")
    );
    assert_eq!(
        resumed
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.final_answer.as_deref()),
        Some("continued 1")
    );
}
