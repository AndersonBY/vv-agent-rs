use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;
use vv_agent::cli::{
    build_cli_task_from_resolved, parse_cli_args_from_with_default_settings,
    parse_cli_command_from_with_default_settings, result_payload, CliCommand, DebugCliCommand,
};
use vv_agent::{AgentResult, AgentStatus, ResolvedModelConfig};

const CONTRACT_SOURCE: &str = include_str!("fixtures/parity/cli_contract.json");

fn contract() -> Value {
    serde_json::from_str(CONTRACT_SOURCE).expect("CLI contract fixture")
}

fn resolved() -> ResolvedModelConfig {
    ResolvedModelConfig::new(
        "deepseek",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        vec![],
    )
    .with_token_limits(Some(1_000_000), Some(384_000))
    .with_capabilities(true, true, true)
}

fn result(status: AgentStatus, error: Option<&str>) -> AgentResult {
    AgentResult {
        status,
        completion_reason: match status {
            AgentStatus::Completed => Some(vv_agent::CompletionReason::ToolFinish),
            AgentStatus::WaitUser => Some(vv_agent::CompletionReason::WaitUser),
            AgentStatus::Failed => Some(vv_agent::CompletionReason::Failed),
            AgentStatus::MaxCycles => Some(vv_agent::CompletionReason::MaxCycles),
            AgentStatus::Pending | AgentStatus::Running | AgentStatus::ReconciliationRequired => {
                None
            }
        },
        completion_tool_name: None,
        partial_output: None,
        final_answer: (status == AgentStatus::Completed).then(|| "done".to_string()),
        wait_reason: None,
        error: error.map(str::to_string),
        error_code: None,
        messages: vec![],
        cycles: vec![],
        shared_state: Default::default(),
        token_usage: Default::default(),
        budget_usage: None,
        budget_exhaustion: None,
        checkpoint_key: None,
        resume_observation: None,
    }
}

fn run_with_settings_environment(args: &[&str], settings_file: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vv-agent"));
    command.args(args);
    command.env_remove("VV_AGENT_LOCAL_SETTINGS");
    if let Some(value) = settings_file {
        command.env("VV_AGENT_LOCAL_SETTINGS", value);
    }
    command.output().expect("run vv-agent")
}

#[test]
fn cli_contract_fixture_is_reviewable() {
    let value = contract();
    assert_eq!(value["contract"], "vv-agent-cli");
    assert_eq!(value["scope"], "direct-task");
}

#[test]
fn multiword_prompt_model_settings_and_resolved_limits_project_to_task() {
    let contract = contract();
    let argv = contract["argument_projection"]["argv"]
        .as_array()
        .expect("argv")
        .iter()
        .map(|value| value.as_str().expect("string arg"))
        .collect::<Vec<_>>();
    let args = parse_cli_args_from_with_default_settings(
        std::iter::once("vv-agent").chain(argv),
        "local_settings.json",
    )
    .expect("parse contract args");

    let task = build_cli_task_from_resolved(&args, &resolved(), "task_fixed").expect("task");

    let expected = &contract["argument_projection"]["task"];
    assert_eq!(task.user_prompt, expected["user_prompt"]);
    assert_eq!(task.max_cycles, expected["max_cycles"]);
    assert_eq!(task.agent_type.as_deref(), expected["agent_type"].as_str());
    assert_eq!(
        serde_json::to_value(task.model_settings.expect("model settings")).expect("settings value"),
        expected["model_settings"]
    );
    let projection = &contract["resolved_model_projection"]["task"];
    assert_eq!(task.native_multimodal, projection["native_multimodal"]);
    for (key, value) in projection["metadata"].as_object().expect("metadata") {
        assert_eq!(task.metadata.get(key), Some(value));
    }
}

#[test]
fn explicit_settings_argument_wins_over_environment() {
    let explicit = "/definitely/missing-explicit-cli.json";
    let output = run_with_settings_environment(
        &["--prompt", "task", "--settings-file", explicit],
        Some("/definitely/missing-environment-cli.json"),
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains(explicit));
}

#[test]
fn environment_wins_and_blank_environment_uses_language_default() {
    let environment = "/definitely/missing-environment-cli.json";
    let environment_output =
        run_with_settings_environment(&["--prompt", "task"], Some(environment));
    let blank_output = run_with_settings_environment(&["--prompt", "task"], Some("  "));

    assert_eq!(environment_output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&environment_output.stderr).contains(environment));
    assert_eq!(blank_output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&blank_output.stderr).contains("local_settings.json"));
}

#[test]
fn result_json_covers_success_failure_and_cancellation() {
    let success = result_payload(&result(AgentStatus::Completed, None), &resolved());
    let failure = result_payload(
        &result(AgentStatus::Failed, Some("request failed")),
        &resolved(),
    );
    let cancellation = result_payload(
        &result(AgentStatus::Failed, Some("Operation was cancelled")),
        &resolved(),
    );

    assert_eq!(success["status"], "completed");
    assert_eq!(success["final_answer"], "done");
    assert_eq!(failure["status"], "failed");
    assert_eq!(failure["error"], "request failed");
    assert_eq!(cancellation["status"], "failed");
    assert_eq!(cancellation["error"], "Operation was cancelled");
}

#[test]
fn process_uses_contract_exit_codes_channels_and_multiword_prompt() {
    let usage = run_with_settings_environment(&[], None);
    let missing_path = "/definitely/missing-vv-agent-cli.json";
    let configuration = run_with_settings_environment(
        &[
            "--prompt",
            "inspect",
            "this",
            "repository",
            "--settings-file",
            missing_path,
        ],
        None,
    );
    let help = run_with_settings_environment(&["--help"], None);
    let outcomes = &contract()["process_outcomes"];

    assert_eq!(
        usage.status.code(),
        outcomes["usage_error"]["exit_code"]
            .as_i64()
            .map(|value| value as i32)
    );
    assert!(usage.stdout.is_empty());
    assert!(String::from_utf8_lossy(&usage.stderr).contains("--prompt"));
    assert_eq!(
        configuration.status.code(),
        outcomes["configuration_or_runtime_error"]["exit_code"]
            .as_i64()
            .map(|value| value as i32)
    );
    assert!(configuration.stdout.is_empty());
    let configuration_error = String::from_utf8_lossy(&configuration.stderr);
    assert!(configuration_error.contains(missing_path));
    assert!(!configuration_error.contains("unknown argument"));
    assert_eq!(help.status.code(), Some(0));
    assert!(!help.stdout.is_empty());
    assert!(help.stderr.is_empty());
}

#[test]
fn language_default_remains_json_for_rust() {
    let args = parse_cli_args_from_with_default_settings(
        ["vv-agent", "--prompt", "task"],
        contract()["settings_file_resolution"]["language_defaults"]["rust"]
            .as_str()
            .expect("rust default"),
    )
    .expect("parse args");

    assert_eq!(args.settings_file, PathBuf::from("local_settings.json"));
}

#[test]
fn debug_send_message_preserves_all_message_words() {
    let command = parse_cli_command_from_with_default_settings(
        [
            "vv-agent",
            "debug",
            "app-server",
            "send-message",
            "inspect",
            "this",
            "repository",
        ],
        "local_settings.json",
    )
    .expect("parse debug message");

    assert_eq!(
        command,
        CliCommand::Debug(DebugCliCommand::AppServerSendMessage {
            message: "inspect this repository".to_string(),
        })
    );
}
