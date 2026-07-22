use std::path::PathBuf;
use std::process::Command;

use vv_agent::cli::{
    build_cli_task, build_cli_task_from_resolved, parse_cli_args_from_with_default_settings,
    result_payload,
};
use vv_agent::{AgentResult, AgentStatus, ResolvedModelConfig};

#[test]
fn cli_parser_matches_entrypoint_flags() {
    let args = parse_cli_args_from_with_default_settings(
        [
            "vv-agent",
            "--prompt",
            "finish this",
            "--backend",
            "deepseek",
            "--model",
            "deepseek-v4-pro",
            "--settings-file",
            "settings.json",
            "--workspace",
            "/tmp/vv-agent-cli",
            "--max-cycles",
            "0",
            "--language",
            "en-US",
            "--agent-type",
            "computer",
            "--verbose",
        ],
        "local_settings.json",
    )
    .expect("parse args");

    assert_eq!(args.prompt, "finish this");
    assert_eq!(args.backend, "deepseek");
    assert_eq!(args.model, "deepseek-v4-pro");
    assert_eq!(args.settings_file, PathBuf::from("settings.json"));
    assert_eq!(args.workspace, PathBuf::from("/tmp/vv-agent-cli"));
    assert_eq!(args.max_cycles, 1);
    assert_eq!(args.language, "en-US");
    assert_eq!(args.agent_type.as_deref(), Some("computer"));
    assert!(args.verbose);
}

#[test]
fn cli_help_uses_json_settings_defaults() {
    let error =
        parse_cli_args_from_with_default_settings(["vv-agent", "--help"], "local_settings.json")
            .expect_err("help text");

    assert!(error.contains("VV_AGENT_LOCAL_SETTINGS"));
    assert!(error.contains("local_settings.json"));
    assert_eq!(error.matches("VV_AGENT_LOCAL_SETTINGS").count(), 1);
    assert!(!error.contains("local_settings.py"));
}

#[test]
fn cli_help_exits_successfully() {
    let output = Command::new(env!("CARGO_BIN_EXE_vv-agent"))
        .arg("--help")
        .output()
        .expect("run vv-agent --help");

    assert!(
        output.status.success(),
        "vv-agent --help exited with {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("Run a vv-agent task against a configured LLM endpoint."));
    assert!(String::from_utf8_lossy(&output.stderr).trim().is_empty());
}

#[test]
fn cli_task_uses_prompt_bundle_and_metadata_sections() {
    let args = parse_cli_args_from_with_default_settings(
        [
            "vv-agent",
            "--prompt",
            "inspect screenshot",
            "--workspace",
            ".",
        ],
        "dev_settings.json",
    )
    .expect("parse args");

    let task = build_cli_task(&args, "deepseek-v4-pro", "task_fixed").expect("task");

    assert_eq!(task.task_id, "task_fixed");
    assert_eq!(task.model, "deepseek-v4-pro");
    assert_eq!(task.max_cycles, 80);
    assert_eq!(task.user_prompt, "inspect screenshot");
    assert!(task
        .system_prompt
        .contains("Vector Vein agent runtime demo"));
    assert_eq!(task.metadata["language"], "zh-CN");
    assert!(task.metadata["system_prompt_sections"].is_array());
}

#[test]
fn cli_task_applies_resolved_vv_llm_token_limits_to_memory_metadata() {
    let args = parse_cli_args_from_with_default_settings(
        [
            "vv-agent",
            "--prompt",
            "inspect screenshot",
            "--workspace",
            ".",
        ],
        "dev_settings.json",
    )
    .expect("parse args");
    let resolved = ResolvedModelConfig::new(
        "deepseek",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        vec![],
    )
    .with_token_limits(Some(1_000_000), Some(384_000));

    let task = build_cli_task_from_resolved(&args, &resolved, "task_fixed").expect("task");

    assert_eq!(task.model, "deepseek-v4-pro");
    assert_eq!(task.metadata["model_context_window"], 1_000_000);
    assert_eq!(task.metadata["model_max_output_tokens"], 384_000);
    assert!(!task.metadata.contains_key("reserved_output_tokens"));
}

#[test]
fn cli_result_payload_matches_shape() {
    let result = AgentResult {
        status: AgentStatus::Completed,
        completion_reason: Some(vv_agent::CompletionReason::ToolFinish),
        completion_tool_name: Some("task_finish".to_string()),
        partial_output: None,
        final_answer: Some("done".to_string()),
        wait_reason: None,
        error: None,
        error_code: None,
        messages: vec![],
        cycles: vec![],
        shared_state: Default::default(),
        token_usage: Default::default(),
        budget_usage: None,
        budget_exhaustion: None,
        checkpoint_key: None,
        resume_observation: None,
    };
    let resolved = ResolvedModelConfig::new(
        "deepseek",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        vec![],
    );

    let payload = result_payload(&result, &resolved);

    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["final_answer"], "done");
    assert_eq!(payload["cycles"], 0);
    assert_eq!(payload["resolved"]["backend"], "deepseek");
    assert_eq!(payload["resolved"]["model_id"], "deepseek-v4-pro");
}
