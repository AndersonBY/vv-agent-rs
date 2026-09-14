use super::*;

#[tokio::test]
async fn checkpoint_history_preserves_public_results_and_cumulative_hook_totals() {
    let store = InMemoryCheckpointStore::new();
    let key = "checkpoint-history-totals";
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some(key.to_string());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    checkpoint.capability_refs.insert(
        "after_cycle_hook:0".to_string(),
        CapabilityRef::new("history-totals-hook", "1").unwrap(),
    );
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let observed = snapshots.clone();
    let hook = Arc::new(move |snapshot: &AfterCycleSnapshot| {
        let value = serde_json::to_value(snapshot).unwrap();
        assert!(value["cumulative_token_usage"].get("model_calls").is_none());
        assert_eq!(
            serde_json::from_value::<AfterCycleSnapshot>(value).unwrap(),
            *snapshot
        );
        observed
            .lock()
            .unwrap()
            .push(snapshot.cumulative_token_usage.clone());
        Ok(Some(AfterCycleDecision::continue_run()))
    });
    let mut response = LLMResponse::new("continue");
    response.token_usage = TokenUsage {
        input_tokens: Some(10),
        output_tokens: Some(1),
        total_tokens: Some(11),
        reasoning_tokens: Some(0),
        usage_source: UsageSource::ProviderReported,
        ..TokenUsage::default()
    };
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "history-model",
            vec![response; 4],
        ))
        .workspace(".")
        .build()
        .unwrap();
    let agent = Agent::builder("history-agent")
        .instructions("Answer.")
        .model(ModelRef::named("history-model"))
        .build()
        .unwrap();
    let config = RunConfig::builder()
        .max_cycles(4)
        .no_tool_policy(NoToolPolicy::Continue)
        .checkpoint_config(checkpoint)
        .after_cycle_hook_arc(hook)
        .build();
    let result = runner
        .run_with_config(&agent, "answer", config.clone())
        .await
        .unwrap();
    assert_eq!(result.result().cycles.len(), 4);
    assert_eq!(result.result().token_usage.model_calls.len(), 4);
    assert_eq!(result.result().token_usage.total_tokens, Some(44));
    let max_cycles_event = result.events().iter().find(|event| {
        matches!(event.payload(), RunEventPayload::Diagnostic { code, .. } if code == "run_max_cycles")
    }).expect("max cycles diagnostic");
    assert_eq!(max_cycles_event.cycle_index(), Some(4));
    assert_eq!(
        snapshots
            .lock()
            .unwrap()
            .iter()
            .map(|usage| usage.total_tokens)
            .collect::<Vec<_>>(),
        vec![Some(11), Some(22), Some(33), Some(44)]
    );
    let stored = store.load_checkpoint(key).unwrap().unwrap();
    assert_eq!(stored.cycles.len(), 1);
    assert_eq!(stored.model_calls.len(), 1);
    assert_eq!(stored.history.cycle_count, 3);
    let replay = runner
        .run_with_config(&agent, "answer", config)
        .await
        .unwrap();
    assert_eq!(result.result(), replay.result());
    assert_eq!(snapshots.lock().unwrap().len(), 4);
}
