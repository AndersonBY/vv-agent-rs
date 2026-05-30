use serde_json::Value;
use vv_agent::constants::{ASK_USER_TOOL_NAME, READ_IMAGE_TOOL_NAME};
use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions, AgentStatus, NoToolPolicy};

use super::common::{live_enabled, live_settings_path};

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x01, 0xe2, 0x21, 0xbc, 0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
fn live_deepseek_v4_pro_uses_read_image_tool() {
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

    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("live_probe.png"), PNG_1X1).expect("seed image");

    let client = AgentSDKClient::new(AgentSDKOptions {
        settings_file: settings_path,
        default_backend: "deepseek".to_string(),
        workspace: workspace.path().to_path_buf(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("deepseek-v4-pro");
    agent.backend = Some("deepseek".to_string());
    agent.language = "en-US".to_string();
    agent.max_cycles = 6;
    agent.no_tool_policy = NoToolPolicy::WaitUser;
    agent.allow_interruption = false;
    agent.enable_todo_management = false;
    agent.use_workspace = false;
    agent.native_multimodal = false;
    agent.extra_tool_names = vec![READ_IMAGE_TOOL_NAME.to_string()];
    agent.exclude_tools = vec![ASK_USER_TOOL_NAME.to_string()];
    agent.system_prompt = Some(
        "You are running a deterministic integration test for the image loading tool.\n\
         Follow this protocol exactly.\n\
         1. First call `read_image` with path `live_probe.png`.\n\
         2. You do not need to describe the image content.\n\
         3. After the tool result says the image source is `workspace` and \
         image_path is `live_probe.png`, call `task_finish` with message exactly \
         `image tool observed`.\n\
         Do not answer in plain text before finishing. Do not use bash."
            .to_string(),
    );

    let run = client
        .run_with_agent(agent, "Execute the image tool protocol now.")
        .expect("run live read_image tool test");
    let tool_names = run
        .result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_calls.iter().map(|call| call.name.clone()))
        .collect::<Vec<_>>();
    let image_result_payloads = run
        .result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_results.iter())
        .filter(|result| result.image_path.as_deref() == Some("live_probe.png"))
        .map(|result| serde_json::from_str::<Value>(&result.content).expect("image result JSON"))
        .collect::<Vec<_>>();

    assert_eq!(run.resolved.backend, "deepseek");
    assert_eq!(run.resolved.requested_model, "deepseek-v4-pro");
    assert_eq!(run.result.status, AgentStatus::Completed, "{tool_names:?}");
    assert_eq!(
        run.result.final_answer.as_deref(),
        Some("image tool observed"),
        "{tool_names:?}"
    );
    assert!(
        tool_names.iter().any(|name| name == READ_IMAGE_TOOL_NAME),
        "expected live model to call read_image, got {tool_names:?}"
    );
    assert!(
        image_result_payloads.iter().any(|payload| {
            payload["source"] == "workspace"
                && payload["image_path"] == "live_probe.png"
                && payload["inline_transport"] == true
        }),
        "expected successful read_image payload, got {image_result_payloads:?}"
    );
}
