use std::path::Path;

#[test]
fn rust_examples_cover_agent_example_numbering() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let examples_dir = manifest_dir.join("examples");
    let expected = [
        "01_quick_start.rs",
        "02_agent_profiles.rs",
        "03_sdk_client.rs",
        "04_session_api.rs",
        "05_ask_user_resume.rs",
        "06_runtime_hooks.rs",
        "07_token_budget_guard.rs",
        "08_custom_tool.rs",
        "09_resource_loader.rs",
        "10_read_image.rs",
        "11_sub_agent_pipeline.rs",
        "12_skill_activation.rs",
        "13_arxiv_pipeline.rs",
        "14_batch_sub_tasks.rs",
        "15_memory_compact_hook.rs",
        "16_hook_composition.rs",
        "17_error_recovery.rs",
        "18_cancellation.rs",
        "19_streaming.rs",
        "20_thread_backend.rs",
        "21_state_checkpoint.rs",
        "22_sdk_advanced.rs",
        "23_celery_backend.rs",
        "24_workspace_backends.rs",
        "25_temporary_tool_injection.rs",
    ];

    let missing = expected
        .iter()
        .filter(|name| !examples_dir.join(name).is_file())
        .copied()
        .collect::<Vec<_>>();

    assert!(missing.is_empty(), "missing Rust examples: {missing:?}");
}
