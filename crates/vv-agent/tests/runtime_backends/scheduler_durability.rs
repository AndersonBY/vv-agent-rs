#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreFault {
    LoadError,
    FinalizeFalse,
    FinalizeError,
    AckFalse,
    AckDeleteThenFalse,
    AckError,
}

#[derive(Debug)]
struct FaultInjectingStore {
    inner: InMemoryStateStore,
    fault: StoreFault,
}

impl FaultInjectingStore {
    fn new(fault: StoreFault) -> Self {
        Self {
            inner: InMemoryStateStore::new(),
            fault,
        }
    }
}

impl StateStore for FaultInjectingStore {
    fn create_checkpoint(&self, checkpoint: Checkpoint) -> IoResult<bool> {
        self.inner.create_checkpoint(checkpoint)
    }

    fn save_checkpoint(&self, checkpoint: Checkpoint) -> IoResult<()> {
        self.inner.save_checkpoint(checkpoint)
    }

    fn load_checkpoint(&self, task_id: &str) -> IoResult<Option<Checkpoint>> {
        if self.fault == StoreFault::LoadError {
            return Err(Error::other("injected load failure"));
        }
        self.inner.load_checkpoint(task_id)
    }

    fn claim_checkpoint(
        &self,
        task_id: &str,
        cycle_index: u32,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> IoResult<Option<Checkpoint>> {
        self.inner.claim_checkpoint(
            task_id,
            cycle_index,
            claim_token,
            lease_expires_at_ms,
            now_ms,
        )
    }

    fn commit_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> IoResult<bool> {
        self.inner
            .commit_checkpoint(checkpoint, claim_token, expected_revision)
    }

    fn renew_checkpoint_claim(
        &self,
        task_id: &str,
        claim_token: &str,
        expected_revision: u64,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> IoResult<bool> {
        self.inner.renew_checkpoint_claim(
            task_id,
            claim_token,
            expected_revision,
            lease_expires_at_ms,
            now_ms,
        )
    }

    fn finalize_checkpoint(
        &self,
        checkpoint: Checkpoint,
        expected_revision: u64,
    ) -> IoResult<bool> {
        match self.fault {
            StoreFault::FinalizeFalse => Ok(false),
            StoreFault::FinalizeError => Err(Error::other("injected finalize failure")),
            _ => self
                .inner
                .finalize_checkpoint(checkpoint, expected_revision),
        }
    }

    fn delete_checkpoint(&self, task_id: &str) -> IoResult<()> {
        self.inner.delete_checkpoint(task_id)
    }

    fn acknowledge_terminal(&self, task_id: &str, expected_revision: u64) -> IoResult<bool> {
        match self.fault {
            StoreFault::AckFalse => Ok(false),
            StoreFault::AckDeleteThenFalse => {
                self.inner.delete_checkpoint(task_id)?;
                Ok(false)
            }
            StoreFault::AckError => Err(Error::other("injected acknowledgement failure")),
            _ => self.inner.acknowledge_terminal(task_id, expected_revision),
        }
    }

    fn list_checkpoints(&self) -> IoResult<Vec<String>> {
        self.inner.list_checkpoints()
    }

    fn state_store_spec(&self) -> Option<StateStoreSpec> {
        None
    }
}

#[derive(Debug)]
struct ImmediateResultDispatcher {
    result: AgentResult,
}

impl CycleDispatcher for ImmediateResultDispatcher {
    fn dispatch_cycle(
        &self,
        _task: &AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_name: &str,
        _cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        Ok(CycleDispatchResult::finished(self.result.clone()))
    }
}

#[derive(Debug)]
struct UnfinishedWithoutProgressDispatcher;

impl CycleDispatcher for UnfinishedWithoutProgressDispatcher {
    fn dispatch_cycle(
        &self,
        _task: &AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_name: &str,
        _cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        Ok(CycleDispatchResult::unfinished())
    }
}

#[derive(Debug)]
struct PanicDispatcher;

impl CycleDispatcher for PanicDispatcher {
    fn dispatch_cycle(
        &self,
        _task: &AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_name: &str,
        _cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        panic!("dispatcher must not run")
    }
}

#[derive(Debug)]
struct ResumeDispatcher {
    store: Arc<InMemoryStateStore>,
    cycles: Arc<Mutex<Vec<u32>>>,
}

impl CycleDispatcher for ResumeDispatcher {
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_name: &str,
        cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        self.cycles.lock().expect("cycles").push(cycle_index);
        let mut checkpoint = self
            .store
            .load_checkpoint(&task.task_id)
            .expect("load checkpoint")
            .expect("checkpoint");
        checkpoint.cycle_index = cycle_index;
        checkpoint
            .messages
            .push(Message::assistant(format!("resumed cycle {cycle_index}")));
        let result = AgentResult::completed_with_shared_state(
            checkpoint.messages.clone(),
            checkpoint.cycles.clone(),
            "resumed",
            checkpoint.shared_state.clone(),
        );
        self.store
            .save_checkpoint(checkpoint)
            .expect("save resumed checkpoint");
        Ok(CycleDispatchResult::finished(result))
    }
}

#[derive(Debug)]
struct PersistTerminalThenFailDispatcher {
    store: Arc<InMemoryStateStore>,
}

#[derive(Debug)]
struct ReclaimExpiredDispatcher {
    store: Arc<InMemoryStateStore>,
}

impl CycleDispatcher for ReclaimExpiredDispatcher {
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_name: &str,
        cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        let mut claimed = self
            .store
            .claim_checkpoint(&task.task_id, cycle_index, "replacement", u64::MAX, 2)
            .expect("reclaim checkpoint")
            .expect("checkpoint");
        claimed.cycle_index = cycle_index;
        let result = AgentResult::completed_with_shared_state(
            claimed.messages.clone(),
            claimed.cycles.clone(),
            "reclaimed",
            claimed.shared_state.clone(),
        );
        claimed.status = result.status;
        claimed.terminal_result = Some(result.clone());
        let revision = claimed.revision;
        assert!(self
            .store
            .commit_checkpoint(claimed, "replacement", revision)
            .expect("commit reclaimed terminal"));
        Ok(CycleDispatchResult::finished_at_revision(
            result,
            Some(revision + 1),
        ))
    }
}

impl CycleDispatcher for PersistTerminalThenFailDispatcher {
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_name: &str,
        cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        let mut claimed = self
            .store
            .claim_checkpoint(&task.task_id, cycle_index, "worker", u64::MAX, 0)
            .expect("claim checkpoint")
            .expect("checkpoint");
        claimed.cycle_index = cycle_index;
        let result = AgentResult::completed_with_shared_state(
            claimed.messages.clone(),
            claimed.cycles.clone(),
            "worker committed",
            claimed.shared_state.clone(),
        );
        claimed.status = result.status;
        claimed.terminal_result = Some(result);
        let revision = claimed.revision;
        assert!(self
            .store
            .commit_checkpoint(claimed, "worker", revision)
            .expect("commit terminal"));
        Err("transport failed after worker commit".to_string())
    }
}

#[derive(Debug)]
struct ClaimThenCancelDispatcher {
    store: Arc<InMemoryStateStore>,
}

impl CycleDispatcher for ClaimThenCancelDispatcher {
    fn dispatch_cycle(
        &self,
        _task: &AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_name: &str,
        _cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        panic!("cancellation-aware dispatch method must be used")
    }

    fn dispatch_envelope_with_cancellation(
        &self,
        envelope: &vv_agent::runtime::backends::DistributedRunEnvelope,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<CycleDispatchResult, String> {
        self.store
            .claim_checkpoint(
                &envelope.task.task_id,
                envelope.cycle_index,
                "worker",
                u64::MAX,
                0,
            )
            .expect("claim checkpoint")
            .expect("checkpoint");
        cancellation_token.expect("cancellation token").cancel();
        Err("scheduler stopped waiting after cancellation".to_string())
    }
}

fn terminal_checkpoint_for_test(task_id: &str, answer: &str, revision: u64) -> Checkpoint {
    let messages = vec![Message::user("hello"), Message::assistant(answer)];
    let result = AgentResult::completed(messages.clone(), Vec::new(), answer);
    Checkpoint {
        task_id: task_id.to_string(),
        cycle_index: 1,
        status: result.status,
        messages,
        cycles: Vec::new(),
        shared_state: Default::default(),
        budget_usage: None,
        revision,
        claim_token: None,
        claimed_cycle: None,
        lease_expires_at_ms: None,
        terminal_result: Some(result),
    }
}

fn run_distributed_scheduler(
    task: &AgentTask,
    store: Arc<dyn StateStore>,
    dispatcher: Arc<dyn CycleDispatcher>,
    cancellation_token: Option<&CancellationToken>,
    max_cycles: u32,
) -> AgentResult {
    let backend = DistributedBackend::distributed_with_dispatcher(
        RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", "."),
        store,
        dispatcher,
    );
    backend.execute(
        task,
        vec![Message::user("hello")],
        [("seed".to_string(), json!("state"))].into_iter().collect(),
        |_cycle_index, _messages, _cycles, _shared_state, _cancellation| {
            panic!("distributed scheduler must not execute cycles inline")
        },
        cancellation_token,
        max_cycles,
    )
}

#[test]
fn distributed_scheduler_load_error_is_explicit_and_preserves_primary_context() {
    let store = Arc::new(FaultInjectingStore::new(StoreFault::LoadError));
    let task = AgentTask::new("fault-load", "model", "system", "prompt");

    let result = run_distributed_scheduler(
        &task,
        store.clone(),
        Arc::new(UnfinishedWithoutProgressDispatcher),
        None,
        1,
    );

    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(result.messages[0].content, "hello");
    assert_eq!(result.shared_state["seed"], json!("state"));
    assert!(result
        .error
        .as_deref()
        .is_some_and(|error| error.contains("injected load failure")));
    assert!(store
        .inner
        .load_checkpoint(&task.task_id)
        .unwrap()
        .is_some());
}

#[test]
fn distributed_scheduler_finalize_false_or_error_is_not_a_normal_terminal() {
    for (index, fault) in [StoreFault::FinalizeFalse, StoreFault::FinalizeError]
        .into_iter()
        .enumerate()
    {
        let store = Arc::new(FaultInjectingStore::new(fault));
        let task = AgentTask::new(
            format!("fault-finalize-{index}"),
            "model",
            "system",
            "prompt",
        );
        let payload = AgentResult::completed(vec![Message::assistant("done")], Vec::new(), "done");

        let result = run_distributed_scheduler(
            &task,
            store.clone(),
            Arc::new(ImmediateResultDispatcher { result: payload }),
            None,
            1,
        );

        assert_eq!(result.status, AgentStatus::Failed);
        assert!(result
            .error
            .as_deref()
            .is_some_and(|error| error.contains("terminal finalize")));
        let checkpoint = store
            .inner
            .load_checkpoint(&task.task_id)
            .unwrap()
            .expect("checkpoint remains");
        assert!(checkpoint.terminal_result.is_none());
    }
}

#[test]
fn distributed_scheduler_ack_false_or_error_is_not_a_normal_terminal() {
    for (index, fault) in [StoreFault::AckFalse, StoreFault::AckError]
        .into_iter()
        .enumerate()
    {
        let store = Arc::new(FaultInjectingStore::new(fault));
        let task = AgentTask::new(format!("fault-ack-{index}"), "model", "system", "prompt");
        let payload = AgentResult::completed(vec![Message::assistant("done")], Vec::new(), "done");

        let result = run_distributed_scheduler(
            &task,
            store.clone(),
            Arc::new(ImmediateResultDispatcher { result: payload }),
            None,
            1,
        );

        assert_eq!(result.status, AgentStatus::Failed);
        assert!(result
            .error
            .as_deref()
            .is_some_and(|error| error.contains("terminal acknowledgement")));
        let checkpoint = store
            .inner
            .load_checkpoint(&task.task_id)
            .unwrap()
            .expect("terminal checkpoint remains");
        assert_eq!(checkpoint.cycle_index, 1);
    }
}

#[test]
fn distributed_scheduler_ack_false_with_missing_checkpoint_is_concurrent_success() {
    let store = Arc::new(FaultInjectingStore::new(StoreFault::AckDeleteThenFalse));
    let task = AgentTask::new("ack-race", "model", "system", "prompt");
    let payload = AgentResult::completed(vec![Message::assistant("done")], Vec::new(), "done");

    let result = run_distributed_scheduler(
        &task,
        store.clone(),
        Arc::new(ImmediateResultDispatcher { result: payload }),
        None,
        1,
    );

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("done"));
    assert!(store
        .inner
        .load_checkpoint(&task.task_id)
        .unwrap()
        .is_none());
}

#[test]
fn distributed_scheduler_replays_and_acknowledges_create_conflict_terminal() {
    let store = Arc::new(InMemoryStateStore::new());
    let task = AgentTask::new("scheduler-replay", "model", "system", "prompt");
    store
        .save_checkpoint(terminal_checkpoint_for_test(&task.task_id, "persisted", 7))
        .unwrap();

    let result =
        run_distributed_scheduler(&task, store.clone(), Arc::new(PanicDispatcher), None, 2);

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("persisted"));
    assert!(store.load_checkpoint(&task.task_id).unwrap().is_none());
}

#[test]
fn distributed_scheduler_resumes_unclaimed_create_conflict_from_next_cycle() {
    let store = Arc::new(InMemoryStateStore::new());
    let task = AgentTask::new("scheduler-resume", "model", "system", "prompt");
    let mut checkpoint = Checkpoint {
        task_id: task.task_id.clone(),
        cycle_index: 1,
        status: AgentStatus::Running,
        messages: vec![Message::user("durable")],
        cycles: Vec::new(),
        shared_state: Default::default(),
        budget_usage: None,
        revision: 4,
        claim_token: None,
        claimed_cycle: None,
        lease_expires_at_ms: None,
        terminal_result: None,
    };
    checkpoint
        .shared_state
        .insert("durable".to_string(), json!(true));
    store.save_checkpoint(checkpoint).unwrap();
    let cycles = Arc::new(Mutex::new(Vec::new()));

    let result = run_distributed_scheduler(
        &task,
        store.clone(),
        Arc::new(ResumeDispatcher {
            store: store.clone(),
            cycles: cycles.clone(),
        }),
        None,
        3,
    );

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("resumed"));
    assert_eq!(*cycles.lock().unwrap(), vec![2]);
    assert_eq!(result.messages[0].content, "durable");
    assert_eq!(result.shared_state["durable"], json!(true));
}

#[test]
fn distributed_scheduler_create_conflict_with_claim_is_in_progress_failure() {
    let store = Arc::new(InMemoryStateStore::new());
    let task = AgentTask::new("scheduler-in-progress", "model", "system", "prompt");
    store
        .create_checkpoint(Checkpoint {
            task_id: task.task_id.clone(),
            cycle_index: 0,
            status: AgentStatus::Running,
            messages: vec![Message::user("durable")],
            cycles: Vec::new(),
            shared_state: Default::default(),
            budget_usage: None,
            revision: 0,
            claim_token: None,
            claimed_cycle: None,
            lease_expires_at_ms: None,
            terminal_result: None,
        })
        .unwrap();
    store
        .claim_checkpoint(&task.task_id, 1, "worker", u64::MAX, 0)
        .unwrap()
        .expect("claim");

    let result =
        run_distributed_scheduler(&task, store.clone(), Arc::new(PanicDispatcher), None, 2);

    assert_eq!(result.status, AgentStatus::Failed);
    assert!(result
        .error
        .as_deref()
        .is_some_and(|error| error.contains("already claimed and in progress")));
    assert!(store
        .load_checkpoint(&task.task_id)
        .unwrap()
        .is_some_and(|checkpoint| checkpoint.claim_token.as_deref() == Some("worker")));
}

#[test]
fn distributed_scheduler_reclaims_an_expired_create_conflict_claim() {
    let store = Arc::new(InMemoryStateStore::new());
    let task = AgentTask::new("scheduler-expired-claim", "model", "system", "prompt");
    store
        .create_checkpoint(Checkpoint {
            task_id: task.task_id.clone(),
            cycle_index: 0,
            status: AgentStatus::Running,
            messages: vec![Message::user("durable")],
            cycles: Vec::new(),
            shared_state: Default::default(),
            budget_usage: None,
            revision: 0,
            claim_token: None,
            claimed_cycle: None,
            lease_expires_at_ms: None,
            terminal_result: None,
        })
        .unwrap();
    store
        .claim_checkpoint(&task.task_id, 1, "expired", 1, 0)
        .unwrap()
        .expect("expired claim");

    let result = run_distributed_scheduler(
        &task,
        store.clone(),
        Arc::new(ReclaimExpiredDispatcher {
            store: store.clone(),
        }),
        None,
        2,
    );

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("reclaimed"));
    assert!(store.load_checkpoint(&task.task_id).unwrap().is_none());
}

#[test]
fn distributed_scheduler_preserves_the_cancellation_reason() {
    let store = Arc::new(InMemoryStateStore::new());
    let task = AgentTask::new("scheduler-cancel-reason", "model", "system", "prompt");
    let cancellation = CancellationToken::default();
    cancellation.cancel_with_reason("host shutdown");

    let result = run_distributed_scheduler(
        &task,
        store.clone(),
        Arc::new(PanicDispatcher),
        Some(&cancellation),
        2,
    );

    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(result.error.as_deref(), Some("host shutdown"));
    assert!(store.load_checkpoint(&task.task_id).unwrap().is_none());
}

#[test]
fn distributed_scheduler_rejects_an_inconsistent_durable_terminal() {
    let store = Arc::new(InMemoryStateStore::new());
    let task = AgentTask::new("scheduler-terminal-mismatch", "model", "system", "prompt");
    let mut checkpoint = terminal_checkpoint_for_test(&task.task_id, "terminal", 7);
    checkpoint.messages = vec![Message::user("different checkpoint history")];
    store.save_checkpoint(checkpoint).unwrap();

    let result =
        run_distributed_scheduler(&task, store.clone(), Arc::new(PanicDispatcher), None, 2);

    assert_eq!(result.status, AgentStatus::Failed);
    assert!(result.error.as_deref().is_some_and(|error| {
        error.contains("checkpoint fields do not match its terminal result")
    }));
    assert!(store.load_checkpoint(&task.task_id).unwrap().is_some());
}

#[test]
fn distributed_scheduler_rejects_unfinished_without_exact_progress() {
    let store = Arc::new(InMemoryStateStore::new());
    let task = AgentTask::new("unfinished-no-progress", "model", "system", "prompt");

    let result = run_distributed_scheduler(
        &task,
        store.clone(),
        Arc::new(UnfinishedWithoutProgressDispatcher),
        None,
        1,
    );

    assert_eq!(result.status, AgentStatus::Failed);
    assert!(result
        .error
        .as_deref()
        .is_some_and(|error| { error.contains("expected durable cycle_index 1, found 0") }));
    assert!(store.load_checkpoint(&task.task_id).unwrap().is_some());
}

#[test]
fn distributed_scheduler_returns_worker_terminal_after_dispatch_error() {
    let store = Arc::new(InMemoryStateStore::new());
    let task = AgentTask::new("dispatch-error-terminal", "model", "system", "prompt");

    let result = run_distributed_scheduler(
        &task,
        store.clone(),
        Arc::new(PersistTerminalThenFailDispatcher {
            store: store.clone(),
        }),
        None,
        1,
    );

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("worker committed"));
    assert!(store.load_checkpoint(&task.task_id).unwrap().is_none());
}

#[test]
fn distributed_scheduler_cancellation_does_not_overwrite_a_worker_claim() {
    let store = Arc::new(InMemoryStateStore::new());
    let task = AgentTask::new("cancel-claimed", "model", "system", "prompt");
    let cancellation = CancellationToken::default();

    let result = run_distributed_scheduler(
        &task,
        store.clone(),
        Arc::new(ClaimThenCancelDispatcher {
            store: store.clone(),
        }),
        Some(&cancellation),
        1,
    );

    assert_eq!(result.status, AgentStatus::Failed);
    assert!(result.error.as_deref().is_some_and(|error| {
        error.contains("outcome is uncertain") && error.contains("was not overwritten")
    }));
    let checkpoint = store
        .load_checkpoint(&task.task_id)
        .unwrap()
        .expect("claimed checkpoint remains");
    assert_eq!(checkpoint.claim_token.as_deref(), Some("worker"));
    assert!(checkpoint.terminal_result.is_none());
}
