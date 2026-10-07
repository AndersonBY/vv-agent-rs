use super::*;
use crate::memory::token_utils::test_estimator;
use crate::memory::{
    MemoryManager, MemoryManagerConfig, RuntimeMemoryCallbackError, RuntimeMemoryCallbacks,
};
use crate::runtime::model_calls::ModelCallLedger;
use crate::types::ModelCallOperation;

fn fixture(name: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/tests/fixtures/parity/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap()
}
fn controller(
    store: Arc<dyn CheckpointStore>,
    key: &str,
    cycle: u64,
) -> CheckpointResumeController {
    let before = store.load_checkpoint(key).unwrap().unwrap();
    let now = before
        .lease_expires_at_ms
        .unwrap_or(0)
        .max(now_ms().unwrap())
        + 1;
    let token = format!("summary-owner-{}", before.revision);
    let mode = if before.claim_token.is_some() {
        ClaimMode::Recovery
    } else {
        ClaimMode::Continue
    };
    let claimed = store
        .claim_checkpoint(key, cycle, &token, now + 600000, now, mode)
        .unwrap()
        .unwrap();
    let mut controller = CheckpointResumeController::new(CheckpointControllerRequest {
        config: CheckpointConfig {
            store: Some(store),
            key: Some(key.into()),
            resume_policy: ResumePolicy::RequireExisting,
            ..CheckpointConfig::default()
        },
        task_id: claimed.task_id.clone(),
        run_id: claimed.root_run_id.clone(),
        trace_id: claimed.trace_id.clone(),
        agent_name: "summary-test".into(),
        run_definition: claimed.run_definition.clone(),
        run_definition_digest: claimed.run_definition_digest.clone(),
        initial_messages: claimed.messages.clone(),
        initial_shared_state: BTreeMap::new(),
        initial_budget_usage: None,
        extensions: vec![],
        reconciliation_provider: None,
        event_sink: Arc::new(|_| Ok(())),
        event_store: None,
        preloaded_checkpoint: None,
    })
    .unwrap();
    controller.checkpoint = Some(claimed);
    controller.owned_claim_token = Some(token);
    controller
}
fn seed(store: &Arc<dyn CheckpointStore>, key: &str, input: &Value) {
    let codec = fixture("checkpoint_codec");
    let mut payload = codec["valid_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "minimal_running")
        .unwrap()["payload"]
        .clone();
    payload["checkpoint_key"] = json!(key);
    payload["cycle_index"] = json!(input["cycle_index"].as_u64().unwrap() - 1);
    payload["messages"] = input["messages"].clone();
    let checkpoint =
        crate::runtime::checkpoint_codec::checkpoint_from_value(&payload, 262144).unwrap();
    assert!(store.create_checkpoint(checkpoint).unwrap());
}
fn run_summary(
    controller: Arc<Mutex<CheckpointResumeController>>,
    input: &Value,
    original: &[Message],
    fail_after_receipt: bool,
    dispatches: Arc<AtomicU64>,
) -> Result<(Vec<Message>, bool), RuntimeMemoryCallbackError> {
    let ledger = ModelCallLedger::default();
    ledger
        .replace_checkpoint(controller.lock().unwrap().checkpoint().unwrap())
        .unwrap();
    let budget = Arc::new(AtomicU64::new(0));
    let budget_observer = budget.clone();
    let accounting = ModelCallCoordinator::new(
        ledger.clone(),
        "run",
        "trace",
        "summary-test",
        None,
        None,
        None,
        Some(Arc::new(move |_, usage| {
            budget_observer.fetch_add(usage.total_tokens.unwrap_or(0), Ordering::SeqCst);
            Default::default()
        })),
    );
    let callback_input = input.clone();
    let callback_controller = controller.clone();
    let callback_ledger = ledger.clone();
    let callback = Arc::new(
        move |prompt: &str, _: Option<&str>, _: Option<&str>, cycle: u32| {
            let mut request = LlmRequest::new(
                "test-model",
                vec![Message::user(prompt)],
                crate::prompt::PromptBundle::from_instruction_text("system").unwrap(),
            );
            request.model_settings = Some(crate::model_settings::ModelSettings {
                temperature: Some(0.0),
                top_p: Some(1.0),
                ..Default::default()
            });
            let mut current = callback_controller.lock().unwrap();
            let golden =
                &fixture("checkpoint_resume")["summary_receipt_replay"]["summary_request_golden"];
            let projection = current
                .model_request_projection(&request, "test", "test-model")
                .unwrap();
            if original_prompt_matches(prompt, &callback_input) {
                assert_eq!(
                    crate::canonical_json_bytes(&projection, "request").unwrap(),
                    crate::canonical_json_bytes(&golden["request"], "golden").unwrap()
                );
            }
            let before_dispatches = dispatches.load(Ordering::SeqCst);
            let outcome = current
                .complete_model(
                    ModelCallDispatchRequest {
                        cycle_index: cycle,
                        operation_slot: "memory_compaction_1",
                        operation: ModelCallOperation::MemoryCompaction,
                        backend: "test",
                        model: "test-model",
                        request: &request,
                        accounting: &accounting,
                    },
                    None,
                    || {
                        dispatches.fetch_add(1, Ordering::SeqCst);
                        let mut response = LLMResponse::new(
                            callback_input["retained_summary_response"]
                                .as_str()
                                .unwrap(),
                        );
                        response.raw.insert(
                            "usage".into(),
                            callback_input["retained_model_call"]["usage"]["provider_usage"]
                                .clone(),
                        );
                        Ok(response)
                    },
                )
                .map_err(|error| {
                    eprintln!("summary checkpoint error: {error}");
                    RuntimeMemoryCallbackError::new(error)
                })?;
            assert_eq!(
                serde_json::to_value(callback_ledger.records()).unwrap(),
                json!([callback_input["retained_model_call"]])
            );
            let new_dispatches = dispatches.load(Ordering::SeqCst) - before_dispatches;
            assert_eq!(
                budget.load(Ordering::SeqCst),
                new_dispatches
                    * callback_input["retained_model_call"]["usage"]["total_tokens"]
                        .as_u64()
                        .unwrap()
            );
            if fail_after_receipt {
                return Err(RuntimeMemoryCallbackError::new("fault after receipt"));
            }
            match outcome {
                ModelOperationOutcome::Response(result) => Ok(Some(result.response.content)),
                _ => panic!("unexpected model outcome"),
            }
        },
    );
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        keep_recent_messages: input["keep_recent_messages"].as_u64().unwrap() as usize,
        language: input["language"].as_str().unwrap().into(),
        summary_event_limit: input["summary_event_limit"].as_u64().unwrap() as usize,
        ..MemoryManagerConfig::default()
    })
    .with_recovery_tool_available(true)
    .with_runtime_callbacks(RuntimeMemoryCallbacks {
        memory_compaction: Some(callback),
        ..Default::default()
    });
    let _estimate = test_estimator::install(
        original,
        input["token_estimator"]["input_messages"].as_u64().unwrap(),
        input["token_estimator"]["candidate_messages"]
            .as_u64()
            .unwrap(),
    );
    manager
        .compact_for_cycle_with_usage_observed(
            original,
            input["cycle_index"].as_u64().unwrap() as u32,
            true,
            None,
            None,
            None,
        )
        .map(|o| (o.messages, o.changed))
}
fn original_prompt_matches(prompt: &str, _input: &Value) -> bool {
    !prompt.contains("changed request")
}

#[test]
fn summary_receipt_replay_is_identical_and_does_not_charge_twice() {
    let case = fixture("checkpoint_resume")["summary_receipt_replay"].clone();
    let input = &case["input"];
    for sqlite in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn CheckpointStore> = if sqlite {
            Arc::new(
                crate::SqliteCheckpointStore::new(directory.path().join("summary.sqlite3"))
                    .unwrap(),
            )
        } else {
            Arc::new(crate::InMemoryCheckpointStore::new())
        };
        let key = "summary-replay";
        seed(&store, key, input);
        let original: Vec<Message> = serde_json::from_value(input["messages"].clone()).unwrap();
        let dispatches = Arc::new(AtomicU64::new(0));
        let first = Arc::new(Mutex::new(controller(store.clone(), key, 5)));
        assert!(run_summary(first.clone(), input, &original, true, dispatches.clone()).is_err());
        let retained = store.load_checkpoint(key).unwrap().unwrap();
        assert_eq!(retained.messages, original);
        assert_eq!(
            retained.model_call_journal[0].to_value(),
            input["retained_receipt"]
        );
        first.lock().unwrap().close();
        for _ in 0..2 {
            let current = Arc::new(Mutex::new(controller(store.clone(), key, 5)));
            let (output, changed) =
                run_summary(current.clone(), input, &original, false, dispatches.clone()).unwrap();
            assert!(changed);
            assert_eq!(
                serde_json::to_value(&output).unwrap(),
                case["expected"]["messages"]
            );
            assert_eq!(dispatches.load(Ordering::SeqCst), 1);
            let mut altered = original.clone();
            altered
                .iter_mut()
                .find(|m| m.content == "继续验证")
                .unwrap()
                .content = "changed request".into();
            let error = run_summary(current.clone(), input, &altered, false, dispatches.clone())
                .unwrap_err()
                .downcast::<CheckpointError>()
                .unwrap();
            assert_eq!(
                error.code(),
                case["changed_request"]["expected_error"].as_str().unwrap()
            );
            let snapshot = current.lock().unwrap().checkpoint().unwrap().clone();
            let mut committed = snapshot.clone();
            committed.messages = output.clone();
            assert!(store
                .progress_checkpoint(
                    committed,
                    snapshot.claim_token.as_deref().unwrap(),
                    snapshot.revision
                )
                .unwrap());
            let (unchanged, changed) =
                run_summary(current.clone(), input, &output, false, dispatches.clone()).unwrap();
            assert!(!changed);
            assert_eq!(unchanged, output);
            assert_eq!(dispatches.load(Ordering::SeqCst), 1);
            current.lock().unwrap().close();
        }
    }
}

#[test]
#[ignore = "requires a paired summary SQLite checkpoint via FIX2_EXCHANGE_DB"]
fn cross_runtime_summary_receipt_exchange() {
    let location = std::env::var("FIX2_EXCHANGE_DB").unwrap();
    let key = std::env::var("FIX2_EXCHANGE_KEY").unwrap_or("summary-receipt-sqlite".into());
    let store: Arc<dyn CheckpointStore> =
        Arc::new(crate::SqliteCheckpointStore::new(&location).unwrap());
    let case = fixture("checkpoint_resume")["summary_receipt_replay"].clone();
    let input = &case["input"];
    if store.load_checkpoint(&key).unwrap().is_none() {
        seed(&store, &key, input);
    }
    let original: Vec<Message> = serde_json::from_value(input["messages"].clone()).unwrap();
    let prior = store.load_checkpoint(&key).unwrap().unwrap();
    let expected_dispatches = u64::from(prior.model_call_journal.is_empty());
    let dispatches = Arc::new(AtomicU64::new(0));
    for _ in 0..2 {
        let current = Arc::new(Mutex::new(controller(store.clone(), &key, 5)));
        let (output, changed) =
            run_summary(current.clone(), input, &original, false, dispatches.clone()).unwrap();
        assert!(changed);
        assert_eq!(
            serde_json::to_value(&output).unwrap(),
            case["expected"]["messages"]
        );
        let mut snapshot = current.lock().unwrap().checkpoint().unwrap().clone();
        snapshot.messages = output;
        assert!(store
            .progress_checkpoint(
                snapshot.clone(),
                snapshot.claim_token.as_deref().unwrap(),
                snapshot.revision
            )
            .unwrap());
        current.lock().unwrap().close();
    }
    assert_eq!(dispatches.load(Ordering::SeqCst), expected_dispatches);
    let after = store.load_checkpoint(&key).unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(after.messages).unwrap(),
        case["after_transcript_commit"]["messages"]
    );
    assert_eq!(
        serde_json::to_value(after.model_calls).unwrap(),
        case["expected"]["model_calls"]
    );
    assert!(after.tool_journal.is_empty());
}
