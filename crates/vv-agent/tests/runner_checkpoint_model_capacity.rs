use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use vv_agent::{
    Agent, CapabilityRef, CheckpointConfig, CheckpointStore, InMemoryCheckpointStore, LLMResponse,
    LlmRequest, MemorySession, ModelRef, NoToolPolicy, ResumePolicy, RunConfig, Runner,
    ScriptedModelProvider,
};

fn checkpoint_config(store: InMemoryCheckpointStore) -> CheckpointConfig {
    let mut config = CheckpointConfig::with_store(store);
    config.key = Some("resume-capacity".to_string());
    config.resume_policy = ResumePolicy::ResumeIfPresent;
    config.capability_refs.insert(
        "before_cycle_messages".to_string(),
        CapabilityRef::new("runner.before-cycle", "1").expect("capability ref"),
    );
    config.capability_refs.insert(
        "session".to_string(),
        CapabilityRef::new("session.runner-checkpoint", "1").expect("capability ref"),
    );
    config
}

#[tokio::test]
async fn checkpoint_resume_preserves_model_capability_without_fabricating_output_reserve() {
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let requests_for_provider = requests.clone();
    let provider =
        ScriptedModelProvider::from_callback("scripted", "resume-capacity-model", move |request| {
            requests_for_provider
                .lock()
                .expect("resume requests")
                .push(request.clone());
            Ok(LLMResponse::new("resumed"))
        })
        .with_token_limits(Some(64_000), Some(8_192));
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("resume-capacity-agent")
        .instructions("Return a result.")
        .model(ModelRef::named("resume-capacity-model"))
        .build()
        .expect("agent");
    let store = InMemoryCheckpointStore::new();
    let session = MemorySession::new("resume-capacity-session");
    let crash_once = Arc::new(AtomicBool::new(true));
    let crash_for_hook = crash_once.clone();

    let first = runner
        .run_with_config(
            &agent,
            "resume capacity",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .session(session.clone())
                .checkpoint_config(checkpoint_config(store.clone()))
                .before_cycle_messages(move |_cycle, _messages, _state| {
                    if crash_for_hook.swap(false, Ordering::SeqCst) {
                        panic!("deterministic crash after checkpoint admission");
                    }
                    Vec::new()
                })
                .build(),
        )
        .await;
    assert!(first.is_err());
    assert!(requests.lock().expect("resume requests").is_empty());
    let mut crashed = store
        .load_checkpoint("resume-capacity")
        .expect("load crashed checkpoint")
        .expect("crashed checkpoint");
    crashed.lease_expires_at_ms = Some(1);
    store
        .save_checkpoint(crashed)
        .expect("expire crashed checkpoint lease");

    let resumed = runner
        .run_with_config(
            &agent,
            "resume capacity",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .session(session)
                .checkpoint_config(checkpoint_config(store))
                .before_cycle_messages(|_cycle, _messages, _state| Vec::new())
                .build(),
        )
        .await
        .expect("checkpoint resume");

    assert_eq!(resumed.final_output(), Some("resumed"));
    let request = requests
        .lock()
        .expect("resume requests")
        .first()
        .cloned()
        .expect("resumed model request");
    assert_eq!(request.metadata["model_context_window"], 64_000);
    assert_eq!(request.metadata["model_max_output_tokens"], 8_192);
    assert!(request.metadata.get("reserved_output_tokens").is_none());
}
