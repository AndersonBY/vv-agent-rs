use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use vv_agent::{
    constants::{FIND_FILES_TOOL_NAME, SEARCH_FILES_TOOL_NAME, TASK_FINISH_TOOL_NAME},
    Agent, AgentStatus, ModelRef, ModelSettings, RunConfig, RunResult, Runner, ToolChoice,
    ToolPolicy, ToolResultStatus, VvLlmModelProvider,
};

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_search_tools -- --ignored --test-threads=1"]
fn live_model_uses_find_files_then_search_files() {
    if !live_enabled() {
        eprintln!("Live tests are disabled. Set VV_AGENT_RUN_LIVE_TESTS=1 to run.");
        return;
    }

    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::create_dir(workspace.path().join("notes")).expect("notes dir");
    std::fs::write(workspace.path().join("notes/alpha.txt"), "alpha only\n").expect("alpha");
    std::fs::write(
        workspace.path().join("notes/target.txt"),
        "line one\nCALYPSO_NEEDLE_7421 lives here\nline three\n",
    )
    .expect("target");
    std::fs::write(
        workspace.path().join("ignore.md"),
        "CALYPSO_NEEDLE_7421 in markdown should not match txt glob\n",
    )
    .expect("markdown");

    let (runner, model) = live_runner(workspace.path()).expect("live runner");
    let agent = live_agent("live-search-tools", &model);
    let result = runner
        .run_blocking(
            &agent,
            r#"Validate the renamed search tools against the workspace.
Step 1: call find_files with path ".", glob "**/*.txt", sort "path_asc", and max_results 10.
Step 2: call search_files with pattern "CALYPSO_NEEDLE_7421", glob "**/*.txt", output_mode "content", and n true.
Step 3: call task_finish with message exactly "LIVE_SEARCH_TOOLS_OK".
Do not call read_file, bash, workspace_grep, or list_files."#
                .into(),
            live_run_config(),
            None,
        )
        .expect("run live search-tools test");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert!(
        result
            .final_output()
            .unwrap_or_default()
            .contains("LIVE_SEARCH_TOOLS_OK"),
        "unexpected final output: {:?}",
        result.final_output()
    );

    let events = tool_events(&result);
    let event_names = events
        .iter()
        .map(|event| event.name.as_str())
        .collect::<Vec<_>>();

    assert!(
        !event_names.contains(&"workspace_grep") && !event_names.contains(&"list_files"),
        "old tool names were called: {events:#?}"
    );
    assert_eq!(
        event_names.get(0..2),
        Some(&[FIND_FILES_TOOL_NAME, SEARCH_FILES_TOOL_NAME][..]),
        "model did not call find_files then search_files: {events:#?}"
    );
    assert_eq!(event_names.last().copied(), Some(TASK_FINISH_TOOL_NAME));

    let find_event = &events[0];
    assert_eq!(find_event.status, ToolResultStatus::Success);
    let find_payload: Value = serde_json::from_str(&find_event.content).expect("find payload");
    assert!(
        find_payload["files"]
            .as_array()
            .expect("find files")
            .contains(&json!("notes/target.txt")),
        "target file missing from find_files payload: {find_payload}"
    );

    let search_event = &events[1];
    assert_eq!(search_event.status, ToolResultStatus::Success);
    assert_eq!(
        search_event.arguments.get("output_mode"),
        Some(&json!("content"))
    );
    assert_eq!(search_event.metadata["summary"]["total_matches"], 1);
    assert_eq!(
        search_event.metadata["matches"][0]["path"],
        "notes/target.txt"
    );
    assert!(
        search_event.metadata["matches"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("CALYPSO_NEEDLE_7421 lives here"),
        "unexpected search metadata: {:?}",
        search_event.metadata
    );
}

#[derive(Debug)]
struct ToolEvent {
    name: String,
    arguments: serde_json::Map<String, Value>,
    status: ToolResultStatus,
    content: String,
    metadata: serde_json::Map<String, Value>,
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
                    arguments: call.arguments.clone().into_iter().collect(),
                    status: matching_result
                        .map(|result| result.status)
                        .unwrap_or(ToolResultStatus::Error),
                    content: matching_result
                        .map(|result| result.content.clone())
                        .unwrap_or_default(),
                    metadata: matching_result
                        .map(|result| result.metadata.clone().into_iter().collect())
                        .unwrap_or_default(),
                }
            })
        })
        .collect()
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
            "You are a precise live integration-test agent. Follow the user's requested tool order exactly. Use only the available search tools, then finish with task_finish.",
        )
        .model(model.clone())
        .model_settings(live_model_settings())
        .tool_policy(
            ToolPolicy::default().allow_only([
                FIND_FILES_TOOL_NAME,
                SEARCH_FILES_TOOL_NAME,
                TASK_FINISH_TOOL_NAME,
            ]),
        )
        .build()
        .expect("agent")
}

fn live_run_config() -> RunConfig {
    RunConfig::builder()
        .max_cycles(8)
        .model_settings(live_model_settings())
        .tool_policy(ToolPolicy::default().allow_only([
            FIND_FILES_TOOL_NAME,
            SEARCH_FILES_TOOL_NAME,
            TASK_FINISH_TOOL_NAME,
        ]))
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
