use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use vv_agent::{
    AfterToolCallEvent, Agent, AgentStatus, ModelRef, ModelSettings, RunConfig, RunResult, Runner,
    RuntimeHook, ToolChoice, ToolExecutionResult, ToolResultStatus, VvLlmModelProvider,
};

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_edit_file -- --ignored --test-threads=1"]
fn live_edit_file_recovers_after_edit_before_read_error() {
    if !live_enabled() {
        eprintln!("Live tests are disabled. Set VV_AGENT_RUN_LIVE_TESTS=1 to run.");
        return;
    }

    let workspace = tempfile::tempdir().expect("workspace");
    let path = workspace.path().join("before_read.txt");
    std::fs::write(&path, "color = \"red\"\n").expect("seed file");

    let (runner, model) = live_runner(workspace.path()).expect("live runner");
    let agent = live_agent("live-edit-before-read", &model);
    let result = runner
        .run_blocking(
            &agent,
            r#"You are testing edit_file safety.

Work only on before_read.txt.
Follow this exact order:
1. First call edit_file without calling read_file. Replace old_string `color = "red"` with new_string `color = "blue"`.
2. If edit_file reports file_not_read or says the file must be read first, call read_file for the full file.
3. Then call edit_file again to make the replacement.
4. Finish by calling task_finish with message exactly `LIVE_EDIT_BEFORE_READ_OK`.

Do not use bash. Do not use write_file."#
                .into(),
            live_run_config(),
            None,
        )
        .expect("run live edit-before-read test");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert!(
        result
            .final_output()
            .unwrap_or_default()
            .contains("LIVE_EDIT_BEFORE_READ_OK"),
        "unexpected final output: {:?}",
        result.final_output()
    );
    let events = tool_events(&result);
    assert_eq!(
        events.first().map(|event| event.name.as_str()),
        Some("edit_file"),
        "model did not first try edit_file: {events:#?}"
    );
    assert!(
        events.iter().any(|event| {
            event.name == "edit_file"
                && event.error_code.as_deref() == Some("file_not_read")
                && event.status == ToolResultStatus::Error
        }),
        "missing file_not_read edit_file error: {events:#?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event.name == "read_file" && event.status == ToolResultStatus::Success),
        "model did not read after file_not_read: {events:#?}"
    );
    assert!(
        events.iter().any(|event| event.name == "edit_file"
            && event.error_code.is_none()
            && event.status == ToolResultStatus::Success),
        "model did not retry edit_file successfully: {events:#?}"
    );
    assert_eq!(
        std::fs::read_to_string(path).expect("final file"),
        "color = \"blue\"\n"
    );
}

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_edit_file -- --ignored --test-threads=1"]
fn live_edit_file_recovers_after_file_changed_since_read_error() {
    if !live_enabled() {
        eprintln!("Live tests are disabled. Set VV_AGENT_RUN_LIVE_TESTS=1 to run.");
        return;
    }

    let workspace = tempfile::tempdir().expect("workspace");
    let path = workspace.path().join("stale.txt");
    std::fs::write(&path, "status = \"stale-base\"\n").expect("seed file");

    let mutation_hook = Arc::new(MutateAfterFirstReadHook {
        relative_path: "stale.txt".to_string(),
        absolute_path: path.clone(),
        replacement: "status = \"external-change\"\n".to_string(),
        mutated: AtomicBool::new(false),
    });
    let (runner, model) = live_runner(workspace.path()).expect("live runner");
    let agent = live_agent("live-edit-stale-baseline", &model);
    let result = runner
        .run_blocking(
            &agent,
            r#"You are testing edit_file stale-file safety.

Work only on stale.txt.
Follow this exact order:
1. First call read_file for the full file.
2. Then call edit_file replacing old_string `status = "stale-base"` with new_string `status = "agent-final"`.
3. If edit_file reports file_changed_since_read, call read_file again for the full file.
4. Then call edit_file replacing the current latest text `status = "external-change"` with `status = "agent-final"`.
5. Finish by calling task_finish with message exactly `LIVE_STALE_BASELINE_OK`.

Do not use bash. Do not use write_file."#
                .into(),
            live_run_config_with_hook(mutation_hook.clone()),
            None,
        )
        .expect("run live stale-baseline test");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert!(
        mutation_hook.mutated.load(Ordering::SeqCst),
        "test hook did not modify the file after read_file"
    );
    assert!(
        result
            .final_output()
            .unwrap_or_default()
            .contains("LIVE_STALE_BASELINE_OK"),
        "unexpected final output: {:?}",
        result.final_output()
    );

    let events = tool_events(&result);
    let first_read = events
        .iter()
        .position(|event| event.name == "read_file")
        .expect("missing initial read_file");
    let first_edit = events
        .iter()
        .position(|event| event.name == "edit_file")
        .expect("missing edit_file");
    assert!(
        first_read < first_edit,
        "model did not read before first stale edit attempt: {events:#?}"
    );

    let stale_error = events
        .iter()
        .position(|event| {
            event.name == "edit_file"
                && event.error_code.as_deref() == Some("file_changed_since_read")
                && event.status == ToolResultStatus::Error
        })
        .expect("missing file_changed_since_read error");
    assert!(
        events
            .iter()
            .skip(stale_error + 1)
            .any(|event| event.name == "read_file" && event.status == ToolResultStatus::Success),
        "model did not re-read after file_changed_since_read: {events:#?}"
    );
    assert!(
        events
            .iter()
            .skip(stale_error + 1)
            .any(|event| event.name == "edit_file"
                && event.error_code.is_none()
                && event.status == ToolResultStatus::Success),
        "model did not retry edit_file successfully after stale error: {events:#?}"
    );
    assert_eq!(
        std::fs::read_to_string(path).expect("final file"),
        "status = \"agent-final\"\n"
    );
}

#[derive(Debug)]
struct ToolEvent {
    name: String,
    status: ToolResultStatus,
    error_code: Option<String>,
}

fn tool_events(result: &RunResult) -> Vec<ToolEvent> {
    result
        .result()
        .cycles
        .iter()
        .flat_map(|cycle| {
            cycle.tool_calls.iter().map(|call| {
                let matching_result = cycle
                    .tool_results
                    .iter()
                    .find(|result| result.tool_call_id == call.id);
                ToolEvent {
                    name: call.name.clone(),
                    status: matching_result
                        .map(|result| result.status)
                        .unwrap_or(ToolResultStatus::Error),
                    error_code: matching_result.and_then(|result| result.error_code.clone()),
                }
            })
        })
        .collect()
}

struct MutateAfterFirstReadHook {
    relative_path: String,
    absolute_path: PathBuf,
    replacement: String,
    mutated: AtomicBool,
}

impl RuntimeHook for MutateAfterFirstReadHook {
    fn after_tool_call(&self, event: AfterToolCallEvent<'_>) -> Option<ToolExecutionResult> {
        if event.call.name != "read_file" || event.result.status != ToolResultStatus::Success {
            return None;
        }
        let is_target_path = event
            .call
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path == self.relative_path);
        if !is_target_path {
            return None;
        }
        if self
            .mutated
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            std::fs::write(&self.absolute_path, &self.replacement)
                .expect("external stale-file mutation");
        }
        None
    }
}

fn live_runner(workspace: &Path) -> Result<(Runner, ModelRef), String> {
    let backend = live_backend();
    let model_name = live_model(&backend);
    let model = ModelRef::backend(backend.clone(), model_name);
    let provider =
        VvLlmModelProvider::from_settings_file(live_settings_path()).with_default_backend(backend);
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace)
        .build()?;
    Ok((runner, model))
}

fn live_agent(name: &str, model: &ModelRef) -> Agent {
    Agent::builder(name)
        .instructions(
            "You are a precise live integration-test agent. Follow the user's requested tool order exactly. Use workspace tools only as requested, then finish with task_finish.",
        )
        .model(model.clone())
        .model_settings(
            ModelSettings::builder()
                .temperature(0.0)
                .max_output_tokens(2048)
                .parallel_tool_calls(false)
                .tool_choice(ToolChoice::Required)
                .timeout(Duration::from_secs(180))
                .build(),
        )
        .build()
        .expect("agent")
}

fn live_run_config() -> RunConfig {
    RunConfig::builder()
        .max_cycles(10)
        .model_settings(live_model_settings())
        .build()
}

fn live_run_config_with_hook(hook: Arc<dyn RuntimeHook>) -> RunConfig {
    RunConfig::builder()
        .max_cycles(10)
        .model_settings(live_model_settings())
        .hook(hook)
        .build()
}

fn live_model_settings() -> ModelSettings {
    ModelSettings::builder()
        .temperature(0.0)
        .max_output_tokens(2048)
        .parallel_tool_calls(false)
        .tool_choice(ToolChoice::Required)
        .timeout(Duration::from_secs(180))
        .build()
}

fn live_backend() -> String {
    env::var("VV_AGENT_LIVE_BACKEND").unwrap_or_else(|_| "moonshot".to_string())
}

fn live_model(backend: &str) -> String {
    env::var("VV_AGENT_LIVE_MODEL").unwrap_or_else(|_| match backend {
        "deepseek" => "deepseek-v4-pro".to_string(),
        "moonshot" => "kimi-k2.6".to_string(),
        _ => panic!(
            "set VV_AGENT_LIVE_MODEL for unsupported live backend default: {}",
            backend
        ),
    })
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
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/dev_settings.json")
        })
}
