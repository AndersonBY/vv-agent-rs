use std::any::Any;
use std::collections::BTreeMap;
use std::io::Error;
use std::sync::{Arc, Mutex};

use serde_json::json;
use sha2::{Digest, Sha256};
use vv_agent::memory::TOOL_RESULT_COMPACT_MARKER;
use vv_agent::types::AgentTask;
use vv_agent::{
    AgentRuntime, AgentStatus, FileInfo, LLMResponse, LlmClient, LlmError, LlmRequest,
    MemoryCompactMode, MemoryCompactTrigger, MemoryManager, MemoryManagerConfig,
    MemoryWorkspaceBackend, Message, MicrocompactionPolicy, RunEvent, RunEventPayload,
    RuntimeRunControls, ToolArtifactRef, ToolCall, ToolExecutionResult, ToolMetadata, ToolRegistry,
    ToolResultRetention, ToolSpec, WorkspaceBackend,
};

const CUSTOM_TOOL: &str = "custom_search";

#[derive(Default)]
struct FailFirstArtifactBackend {
    inner: MemoryWorkspaceBackend,
    failed_once: Mutex<bool>,
}

impl WorkspaceBackend for FailFirstArtifactBackend {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn list_files(&self, base: &str, glob: &str) -> std::io::Result<Vec<String>> {
        self.inner.list_files(base, glob)
    }

    fn read_text(&self, path: &str) -> std::io::Result<String> {
        self.inner.read_text(path)
    }

    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.inner.read_bytes(path)
    }

    fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize> {
        self.inner.write_text(path, content, append)
    }

    fn write_text_exclusive(&self, path: &str, content: &str) -> std::io::Result<usize> {
        let mut failed_once = self.failed_once.lock().expect("failure state");
        if !*failed_once {
            *failed_once = true;
            return Err(Error::other("injected archive failure"));
        }
        drop(failed_once);
        self.inner.write_text_exclusive(path, content)
    }

    fn write_text_chunks_exclusive(
        &self,
        path: &str,
        chunks: &mut dyn Iterator<Item = std::io::Result<String>>,
    ) -> std::io::Result<usize> {
        self.inner.write_text_chunks_exclusive(path, chunks)
    }

    fn file_info(&self, path: &str) -> std::io::Result<Option<FileInfo>> {
        self.inner.file_info(path)
    }

    fn exists(&self, path: &str) -> bool {
        self.inner.exists(path)
    }

    fn is_file(&self, path: &str) -> bool {
        self.inner.is_file(path)
    }

    fn mkdir(&self, path: &str) -> std::io::Result<()> {
        self.inner.mkdir(path)
    }
}

#[derive(Clone)]
struct InspectingLlm {
    calls: Arc<Mutex<usize>>,
    observed_messages: Arc<Mutex<Vec<Message>>>,
}

impl InspectingLlm {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(0)),
            observed_messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn observed_messages(&self) -> Vec<Message> {
        self.observed_messages
            .lock()
            .expect("observed messages")
            .clone()
    }
}

impl LlmClient for InspectingLlm {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut calls = self.calls.lock().expect("calls");
        *calls += 1;
        if *calls == 1 {
            return Ok(LLMResponse::with_tool_calls(
                "search",
                vec![ToolCall::new("custom-call", CUSTOM_TOOL, BTreeMap::new())],
            ));
        }
        *self.observed_messages.lock().expect("observed messages") = request.messages;
        Ok(LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new(
                "finish-call",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("done"))]),
            )],
        ))
    }
}

fn registry(retention: Option<ToolResultRetention>) -> ToolRegistry {
    let mut registry = vv_agent::tools::build_default_registry();
    let mut spec = ToolSpec::new(
        CUSTOM_TOOL,
        "Return a large custom result.",
        Arc::new(|_context, _arguments| {
            ToolExecutionResult::success("", "custom result ".repeat(600))
        }),
    );
    if let Some(result_retention) = retention {
        spec.tool_metadata = Some(ToolMetadata {
            result_retention,
            ..ToolMetadata::default()
        });
    }
    registry.register(spec).expect("custom tool");
    registry
}

fn task() -> AgentTask {
    let mut task = AgentTask::new(
        "archive-micro",
        "demo",
        vv_agent::PromptBundle::from_instruction_text("system").expect("prompt bundle"),
        "search",
    );
    task.extra_tool_names.push(CUSTOM_TOOL.to_string());
    task.memory_compact_threshold = 10_000;
    task.microcompaction_policy = MicrocompactionPolicy::new(0.02, 0.01, 0, 200).expect("policy");
    task.metadata
        .insert("model_context_window".to_string(), json!(20_000));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(0));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task
}

fn runtime_case(
    retention: Option<ToolResultRetention>,
) -> (
    vv_agent::AgentResult,
    Vec<Message>,
    Vec<RunEvent>,
    Arc<MemoryWorkspaceBackend>,
) {
    runtime_case_with_task(retention, task())
}

fn runtime_case_with_task(
    retention: Option<ToolResultRetention>,
    task: AgentTask,
) -> (
    vv_agent::AgentResult,
    Vec<Message>,
    Vec<RunEvent>,
    Arc<MemoryWorkspaceBackend>,
) {
    let llm = InspectingLlm::new();
    let inspector = llm.clone();
    let backend = Arc::new(MemoryWorkspaceBackend::default());
    let mut runtime = AgentRuntime::new(llm).with_tool_registry(registry(retention));
    runtime.workspace_backend = backend.clone();
    let events = Arc::new(Mutex::new(Vec::new()));
    let event_sink = events.clone();
    let result = runtime
        .run_with_controls(
            task,
            RuntimeRunControls {
                workspace_backend: Some(backend.clone()),
                event_handler: Some(Arc::new(move |event| {
                    event_sink.lock().expect("events").push(event.clone());
                })),
                ..RuntimeRunControls::default()
            },
        )
        .expect("runtime");
    let observed = inspector.observed_messages();
    let events = events.lock().expect("events").clone();
    (result, observed, events, backend)
}

#[test]
fn default_custom_tool_archives_before_replacement_and_emits_v5_fields() {
    let (result, observed, events, backend) = runtime_case(None);
    assert_eq!(result.status, AgentStatus::Completed);
    let compacted = observed
        .iter()
        .find(|message| message.content.starts_with(TOOL_RESULT_COMPACT_MARKER))
        .expect("compacted custom result");
    assert!(compacted.content.contains("tool_name: custom_search"));
    assert!(compacted
        .content
        .contains("retrieval_hint: use read_file on artifact_path if needed"));
    for forbidden in [
        "original_bytes",
        "visible_bytes",
        "size_bytes",
        "sha256",
        "total_chars",
        "truncated_chars",
    ] {
        assert!(!compacted.content.contains(forbidden), "{forbidden}");
    }
    let artifact_path = compacted
        .content
        .lines()
        .find_map(|line| line.strip_prefix("artifact_path: "))
        .expect("artifact path");
    assert_eq!(
        backend.read_text(artifact_path).expect("archived result"),
        "custom result ".repeat(600)
    );

    let started = events
        .iter()
        .find_map(|event| match event.payload() {
            RunEventPayload::MemoryCompactStarted {
                trigger,
                microcompact_target,
                candidate_count,
                estimated_reclaimable_tokens,
                ..
            } => Some((
                *trigger,
                *microcompact_target,
                *candidate_count,
                *estimated_reclaimable_tokens,
            )),
            _ => None,
        })
        .expect("started");
    assert_eq!(started.0, MemoryCompactTrigger::MicroThreshold);
    assert_eq!(started.1, 100);
    assert_eq!(started.2, 1);
    assert!(started.3 > 0);
    assert!(events.iter().any(|event| {
        matches!(
            event.payload(),
            RunEventPayload::MemoryCompactCompleted {
                mode: MemoryCompactMode::Micro,
                archived_count: 1,
                reclaimed_tokens,
                artifact_failure_count: 0,
                ..
            } if *reclaimed_tokens > 0
        )
    }));
}

#[test]
fn preserve_retention_keeps_inline_result_and_emits_no_micro_lifecycle() {
    let (_, observed, events, _) = runtime_case(Some(ToolResultRetention::Preserve));
    assert!(observed
        .iter()
        .any(|message| message.content.contains("custom result")));
    assert!(observed
        .iter()
        .all(|message| !message.content.starts_with(TOOL_RESULT_COMPACT_MARKER)));
    assert!(events.iter().all(|event| {
        !matches!(
            event.payload(),
            RunEventPayload::MemoryCompactStarted { .. }
                | RunEventPayload::MemoryCompactCompleted { .. }
        )
    }));
}

fn assert_microcompaction_skipped_without_recovery_tool(task: AgentTask) {
    let (_, observed, events, backend) = runtime_case_with_task(None, task);
    assert!(observed
        .iter()
        .any(|message| message.content.contains("custom result")));
    assert!(observed
        .iter()
        .all(|message| !message.content.starts_with(TOOL_RESULT_COMPACT_MARKER)));
    assert!(events.iter().all(|event| {
        !matches!(
            event.payload(),
            RunEventPayload::MemoryCompactStarted { .. }
                | RunEventPayload::MemoryCompactCompleted { .. }
        )
    }));
    assert!(backend
        .list_files(".vv-agent/artifacts", "**/*")
        .expect("artifacts")
        .is_empty());
}

#[test]
fn workspace_disabled_keeps_results_recoverable_inline() {
    let mut task = task();
    task.use_workspace = false;
    assert_microcompaction_skipped_without_recovery_tool(task);
}

#[test]
fn excluded_read_file_keeps_results_recoverable_inline() {
    let mut task = task();
    task.exclude_tools.push("read_file".to_string());
    assert_microcompaction_skipped_without_recovery_tool(task);
}

#[test]
fn microcompaction_policy_enforces_canonical_bounds_and_closed_wire() {
    assert!(MicrocompactionPolicy::new(0.75, 0.60, 0, 1).is_ok());
    for invalid in [
        MicrocompactionPolicy::new(0.75, 0.0, 3, 500),
        MicrocompactionPolicy::new(0.75, 0.75, 3, 500),
        MicrocompactionPolicy::new(1.01, 0.60, 3, 500),
        MicrocompactionPolicy::new(f64::NAN, 0.60, 3, 500),
        MicrocompactionPolicy::new(0.75, 0.60, 3, 0),
    ] {
        assert!(invalid.is_err());
    }
    assert!(serde_json::from_value::<MicrocompactionPolicy>(json!({
        "trigger_ratio": 0.75,
        "target_ratio": 0.60,
        "keep_recent_cycles": -1,
        "min_result_chars": 500
    }))
    .is_err());
    assert!(serde_json::from_value::<MicrocompactionPolicy>(json!({
        "trigger_ratio": 0.75,
        "target_ratio": 0.60,
        "keep_recent_cycles": 3,
        "min_result_chars": 500,
        "legacy_alias": true
    }))
    .is_err());

    let mut task_wire = serde_json::to_value(task()).expect("task wire");
    task_wire
        .as_object_mut()
        .expect("task object")
        .remove("microcompaction_policy");
    assert!(serde_json::from_value::<AgentTask>(task_wire).is_err());
}

#[test]
fn tool_result_retention_defaults_to_archive_and_rejects_unknown_values() {
    assert_eq!(
        serde_json::to_value(ToolMetadata::default()).expect("metadata")["result_retention"],
        json!("archive")
    );
    let preserve = serde_json::from_value::<ToolMetadata>(json!({
        "result_retention": "preserve"
    }))
    .expect("preserve metadata");
    assert_eq!(preserve.result_retention, ToolResultRetention::Preserve);
    assert!(serde_json::from_value::<ToolMetadata>(json!({
        "result_retention": "drop"
    }))
    .is_err());
}

fn manager_for(messages: &[Message]) -> MemoryManager {
    let usage = vv_agent::memory::token_utils::count_messages_tokens(messages, "");
    MemoryManager::new(MemoryManagerConfig {
        compact_threshold: usage + 100,
        model_context_window: usage + 100,
        reserved_output_tokens: 0,
        autocompact_buffer_tokens: 0,
        tool_result_compact_threshold: usize::MAX,
        tool_result_excerpt_head: 20,
        tool_result_excerpt_tail: 20,
        microcompaction_policy: MicrocompactionPolicy::new(0.75, 0.60, 0, 100).expect("policy"),
        ..MemoryManagerConfig::default()
    })
}

#[test]
fn oldest_candidate_is_applied_first_and_planning_stops_at_target() {
    let messages = vec![
        Message::system("sys"),
        Message {
            tool_calls: vec![ToolCall::new("old", CUSTOM_TOOL, BTreeMap::new())],
            ..Message::assistant("old call")
        },
        Message::tool("old result ".repeat(1_200), "old"),
        Message {
            tool_calls: vec![ToolCall::new("newer", CUSTOM_TOOL, BTreeMap::new())],
            ..Message::assistant("newer call")
        },
        Message::tool("new result ".repeat(300), "newer"),
    ];
    let backend = Arc::new(MemoryWorkspaceBackend::default());
    let mut manager = manager_for(&messages)
        .with_workspace_backend(backend)
        .with_recovery_tool_available(true);
    let (compacted, changed) = manager.compact_for_cycle(&messages, 3, false);

    assert!(changed);
    assert!(compacted[2].content.starts_with(TOOL_RESULT_COMPACT_MARKER));
    assert_eq!(compacted[4].content, messages[4].content);
}

#[test]
fn low_token_density_result_is_not_replaced_by_a_larger_marker() {
    let backend = Arc::new(MemoryWorkspaceBackend::default());
    let messages = vec![
        Message::system("sys"),
        Message {
            tool_calls: vec![ToolCall::new("spaces", CUSTOM_TOOL, BTreeMap::new())],
            ..Message::assistant("call")
        },
        Message::tool(" ".repeat(501), "spaces"),
        Message::assistant("continue"),
    ];
    let manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10,
        model_context_window: 10,
        reserved_output_tokens: 0,
        autocompact_buffer_tokens: 0,
        microcompaction_policy: MicrocompactionPolicy::new(0.75, 0.60, 0, 500).expect("policy"),
        ..MemoryManagerConfig::default()
    })
    .with_workspace_backend(backend.clone())
    .with_recovery_tool_available(true);

    let (compacted, archived_count) = manager.microcompact_messages(&messages, Some(3));

    assert_eq!(archived_count, 0);
    assert_eq!(compacted, messages);
    assert!(backend
        .list_files(".vv-agent/artifacts", "**/*")
        .expect("artifacts")
        .is_empty());
}

#[test]
fn first_cycle_of_new_run_ages_existing_session_history() {
    let messages = vec![
        Message::system("sys"),
        Message::user("prior request"),
        Message {
            tool_calls: vec![ToolCall::new("prior", CUSTOM_TOOL, BTreeMap::new())],
            ..Message::assistant("prior call")
        },
        Message::tool("prior result ".repeat(1_000), "prior"),
        Message::assistant("prior answer"),
        Message::user("new run request"),
    ];
    let usage = vv_agent::memory::token_utils::count_messages_tokens(&messages, "");
    let backend = Arc::new(MemoryWorkspaceBackend::default());
    let manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: usage + 100,
        model_context_window: usage + 100,
        reserved_output_tokens: 0,
        autocompact_buffer_tokens: 0,
        microcompaction_policy: MicrocompactionPolicy::new(0.75, 0.60, 1, 100).expect("policy"),
        ..MemoryManagerConfig::default()
    })
    .with_workspace_backend(backend)
    .with_recovery_tool_available(true);

    let (compacted, archived_count) = manager.microcompact_messages(&messages, Some(0));

    assert_eq!(archived_count, 1);
    assert!(compacted[3].content.starts_with(TOOL_RESULT_COMPACT_MARKER));
}

#[test]
fn archive_failure_preserves_original_message() {
    let messages = vec![
        Message::system("sys"),
        Message {
            tool_calls: vec![ToolCall::new("old", CUSTOM_TOOL, BTreeMap::new())],
            ..Message::assistant("old call")
        },
        Message::tool("x".repeat(4_000), "old"),
    ];
    let mut manager = manager_for(&messages).with_recovery_tool_available(true);
    let (compacted, changed) = manager.compact_for_cycle(&messages, 2, false);

    assert!(!changed);
    assert_eq!(compacted, messages);
}

#[test]
fn archive_failure_falls_through_to_later_candidate_in_same_plan() {
    let messages = vec![
        Message::system("sys"),
        Message {
            tool_calls: vec![ToolCall::new("old", CUSTOM_TOOL, BTreeMap::new())],
            ..Message::assistant("old call")
        },
        Message::tool("old result ".repeat(1_200), "old"),
        Message {
            tool_calls: vec![ToolCall::new("newer", CUSTOM_TOOL, BTreeMap::new())],
            ..Message::assistant("newer call")
        },
        Message::tool("new result ".repeat(1_200), "newer"),
    ];
    let backend = Arc::new(FailFirstArtifactBackend::default());
    let mut manager = manager_for(&messages)
        .with_workspace_backend(backend)
        .with_recovery_tool_available(true);
    let (compacted, changed) = manager.compact_for_cycle(&messages, 3, false);

    assert!(changed);
    assert_eq!(compacted[2].content, messages[2].content);
    assert!(compacted[4].content.starts_with(TOOL_RESULT_COMPACT_MARKER));
}

#[test]
fn existing_bounded_result_artifact_is_reused() {
    let backend = Arc::new(MemoryWorkspaceBackend::default());
    let artifact_path = ".vv-agent/artifacts/existing/call.txt";
    let complete = "complete existing result";
    backend
        .write_text_exclusive(artifact_path, complete)
        .expect("existing artifact");
    let artifact = ToolArtifactRef {
        path: artifact_path.to_string(),
        media_type: "text/plain".to_string(),
        encoding: "utf-8".to_string(),
        size_bytes: complete.len() as u64,
        sha256: format!("{:x}", Sha256::digest(complete.as_bytes())),
    };
    let recovery = json!({
        "vv_agent_recovery": {
            "truncated": true,
            "truncation_reason": "output_limit",
            "original_bytes": complete.len(),
            "visible_bytes": 700,
            "artifact": artifact.clone(),
        }
    });
    let mut bounded_result =
        Message::tool(format!("{}\n{recovery}", "preview ".repeat(1_000)), "old");
    bounded_result.artifact_ref = Some(artifact.clone());
    let messages = vec![
        Message::system("sys"),
        Message {
            tool_calls: vec![ToolCall::new("old", CUSTOM_TOOL, BTreeMap::new())],
            ..Message::assistant("old call")
        },
        bounded_result,
    ];
    let mut manager = manager_for(&messages)
        .with_workspace_backend(backend.clone())
        .with_recovery_tool_available(true);
    let (compacted, changed) = manager.compact_for_cycle(&messages, 2, false);

    assert!(changed);
    assert!(compacted[2]
        .content
        .contains(&format!("artifact_path: {artifact_path}")));
    assert!(!compacted[2].content.contains("original_bytes"));
    assert_eq!(compacted[2].artifact_ref, Some(artifact));
    assert_eq!(
        backend.read_text(artifact_path).expect("existing artifact"),
        complete
    );
}

#[test]
fn damaged_or_missing_typed_artifact_preserves_original_without_rearchiving_preview() {
    for existing_bytes in [None, Some("tampered artifact")] {
        let backend = Arc::new(MemoryWorkspaceBackend::default());
        let artifact_path = ".vv-agent/artifacts/existing/damaged.txt";
        if let Some(content) = existing_bytes {
            backend
                .write_text_exclusive(artifact_path, content)
                .expect("damaged artifact");
        }
        let expected = "complete original output";
        let artifact = ToolArtifactRef {
            path: artifact_path.to_string(),
            media_type: "text/plain".to_string(),
            encoding: "utf-8".to_string(),
            size_bytes: expected.len() as u64,
            sha256: format!("{:x}", Sha256::digest(expected.as_bytes())),
        };
        let recovery = json!({
            "vv_agent_recovery": {
                "truncated": true,
                "truncation_reason": "output_limit",
                "original_bytes": expected.len(),
                "visible_bytes": 8_000,
                "artifact": artifact.clone(),
            }
        });
        let mut bounded = Message::tool(format!("{}\n{recovery}", "preview ".repeat(1_000)), "old");
        bounded.artifact_ref = Some(artifact);
        let messages = vec![
            Message::system("sys"),
            Message {
                tool_calls: vec![ToolCall::new("old", CUSTOM_TOOL, BTreeMap::new())],
                ..Message::assistant("old call")
            },
            bounded,
        ];
        let manager = manager_for(&messages)
            .with_workspace_backend(backend.clone())
            .with_recovery_tool_available(true);
        let files_before = backend
            .list_files(".vv-agent/artifacts", "**/*")
            .expect("files before");

        let (compacted, archived_count) = manager.microcompact_messages(&messages, Some(2));

        assert_eq!(archived_count, 0);
        assert_eq!(compacted, messages);
        assert_eq!(
            backend
                .list_files(".vv-agent/artifacts", "**/*")
                .expect("files after"),
            files_before
        );
    }
}

#[test]
fn recovery_envelope_without_typed_artifact_is_not_archived_as_complete_output() {
    let backend = Arc::new(MemoryWorkspaceBackend::default());
    let recovery = json!({
        "vv_agent_recovery": {
            "truncated": true,
            "truncation_reason": "output_limit",
            "original_bytes": 20_000,
            "visible_bytes": 8_000,
            "artifact": {
                "path": ".vv-agent/artifacts/missing/legacy.txt",
                "media_type": "text/plain",
                "encoding": "utf-8",
                "size_bytes": 20_000,
                "sha256": "a".repeat(64),
            },
        }
    });
    let messages = vec![
        Message::system("sys"),
        Message {
            tool_calls: vec![ToolCall::new("old", CUSTOM_TOOL, BTreeMap::new())],
            ..Message::assistant("old call")
        },
        Message::tool(format!("{}\n{recovery}", "preview ".repeat(1_000)), "old"),
    ];
    let manager = manager_for(&messages)
        .with_workspace_backend(backend.clone())
        .with_recovery_tool_available(true);

    let (compacted, archived_count) = manager.microcompact_messages(&messages, Some(2));

    assert_eq!(archived_count, 0);
    assert_eq!(compacted, messages);
    assert!(backend
        .list_files(".vv-agent/artifacts", "**/*")
        .expect("artifacts")
        .is_empty());
}
