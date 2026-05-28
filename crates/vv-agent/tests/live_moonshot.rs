use std::env;
use std::path::PathBuf;

use vv_agent::{build_vv_llm_from_local_settings, LlmClient, LlmRequest, Message};

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_moonshot -- --ignored"]
fn live_moonshot_kimi_smoke_completion() {
    if !live_enabled() {
        eprintln!("Live tests are disabled. Set VV_AGENT_RUN_LIVE_TESTS=1 to run.");
        return;
    }

    let settings_path = live_settings_path();
    assert!(
        settings_path.exists(),
        "live settings file is missing: {}",
        settings_path.display()
    );

    let backend = env::var("VV_AGENT_LIVE_BACKEND").unwrap_or_else(|_| "moonshot".to_string());
    let model = env::var("VV_AGENT_LIVE_MODEL").unwrap_or_else(|_| "kimi-k2.5".to_string());
    let (llm, resolved) = build_vv_llm_from_local_settings(&settings_path, &backend, &model, 90.0)
        .expect("build Moonshot vv-llm client");

    let response = llm
        .complete(LlmRequest::new(
            resolved.model_id.clone(),
            vec![
                Message::system("You are a concise assistant."),
                Message::user("Reply with exactly one word: pong"),
            ],
        ))
        .expect("run live Moonshot smoke completion");

    assert_eq!(resolved.backend, backend);
    assert!(
        !response.content.trim().is_empty()
            || response
                .raw
                .get("choices")
                .is_some_and(|choices| !choices.is_null()),
        "unexpected empty Moonshot response: {:?}",
        response.raw
    );
}

fn live_enabled() -> bool {
    env::var("VV_AGENT_RUN_LIVE_TESTS")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn live_settings_path() -> PathBuf {
    env::var("VV_AGENT_LIVE_SETTINGS_JSON")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../third_party_service/vv-llm-rs/crates/vv-llm/tests/fixtures/dev_settings.json")
        })
}
