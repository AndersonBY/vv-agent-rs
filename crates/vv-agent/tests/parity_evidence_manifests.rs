use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use vv_agent::prompt::{
    build_system_prompt_bundle_with_options, BuildSystemPromptOptions, BuiltSystemPrompt,
    PromptSection, SystemPromptBuilder,
};
use vv_agent::{
    ApprovalRequirement, ToolApprovalRule, ToolExposure, ToolSpecContext, ToolSpecKind,
};

const FIXTURE_DIR: &str = "tests/fixtures/parity";
const CANONICAL_FIXTURES: [&str; 3] = [
    "public_api_v1.json",
    "prompt_bundle_v1.json",
    "builtin_tools_v1.json",
];
const EXPECTED_DOMAINS: [&str; 14] = [
    "agent",
    "runner",
    "run_config",
    "result",
    "run_handle",
    "interactive",
    "app_server",
    "tools",
    "workspace",
    "memory",
    "skills",
    "tracing",
    "llm_bridge",
    "runtime_backend",
];
const EXPECTED_RUNNER_OPERATIONS: [&str; 5] = ["run", "start", "stream", "resume", "configured"];
const EXPECTED_RUN_HANDLE_OPERATIONS: [&str; 8] = [
    "cancel",
    "events",
    "result",
    "state",
    "approve",
    "steer",
    "follow_up",
    "resume",
];
const EXPECTED_INTERACTIVE_SESSION_MEMBERS: [&str; 20] = [
    "messages",
    "session",
    "shared_state",
    "latest_run",
    "running",
    "closed",
    "active_run_handle",
    "subscribe",
    "close",
    "steer",
    "follow_up",
    "clear_queues",
    "cancel",
    "approve",
    "prompt",
    "continue_run",
    "query",
    "state",
    "replace_messages",
    "replace_shared_state",
];
const EXPECTED_APP_SERVER_PROTOCOL_OPERATIONS: [&str; 15] = [
    "initialize",
    "thread/start",
    "thread/resume",
    "thread/read",
    "thread/list",
    "thread/archive",
    "thread/unsubscribe",
    "turn/start",
    "turn/interrupt",
    "turn/steer",
    "turn/followUp",
    "approval/resolve",
    "model/list",
    "schema/export",
    "initialized",
];
const EXPECTED_APP_SERVER_SUPPORTING_OPERATIONS: [&str; 4] = [
    "resolve_server_request",
    "send_response",
    "next_message",
    "close",
];

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURE_DIR)
        .join(name)
}

fn load_fixture(name: &str) -> Value {
    serde_json::from_slice(&fs::read(fixture_path(name)).expect("read parity fixture"))
        .expect("parse parity fixture")
}

fn touch_type<T: ?Sized>() {
    let _ = std::any::type_name::<T>();
}

macro_rules! export_type {
    ($type:ty, $path:literal) => {{
        touch_type::<$type>();
        $path
    }};
}

include!("parity_evidence_manifests/public_api.rs");
include!("parity_evidence_manifests/prompt_tools.rs");
include!("parity_evidence_manifests/fixtures.rs");
