use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{json, Value};
use vv_agent::{
    memory::{
        token_utils::{count_messages_tokens, count_tokens},
        LocalSummary, TOOL_RESULT_COMPACT_MARKER,
    },
    LocalWorkspaceBackend, MemoryManager, MemoryManagerConfig, MemoryWorkspaceBackend, Message,
    MessageRole, MicrocompactionPolicy, SessionMemory, SessionMemoryConfig, ToolArtifactRef,
    ToolCall, ToolResultRetention, WorkspaceBackend,
};

const FIXTURE_TEXT: &str = include_str!("fixtures/parity/memory_local.json");

#[derive(Debug, Deserialize)]
struct MemoryLocalFixture {
    contract: String,
    character_unit: String,
    token_counts: Vec<TokenCountCase>,
    message_tokens: MessageTokenFixture,
    microcompact: MicrocompactFixture,
    session_prompt_truncation: SessionPromptFixture,
    summary: SummaryFixture,
    recompression_originals: RecompressionFixture,
    unicode_excerpt: UnicodeExcerptFixture,
    session_extraction: SessionExtractionFixture,
    summary_parse: SummaryParseFixture,
}

#[derive(Debug, Deserialize)]
struct TokenCountCase {
    model: String,
    text: Option<String>,
    text_unit: Option<String>,
    repeat: Option<usize>,
    tokens: u64,
}

#[derive(Debug, Deserialize)]
struct MessageTokenFixture {
    model: String,
    messages: Vec<FixtureMessage>,
    tokens: u64,
}

#[derive(Debug, Deserialize)]
struct FixtureMessage {
    role: String,
    content: String,
    #[serde(default)]
    tool_calls: Vec<FixtureToolCall>,
    tool_call_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FixtureToolCall {
    id: String,
    name: String,
    arguments: Value,
}

#[derive(Debug, Deserialize)]
struct MicrocompactFixture {
    schema_version: String,
    content_unit: String,
    trigger_ratio_default: f64,
    target_ratio_default: f64,
    min_result_chars_default: u32,
    keep_recent_cycles_default: u32,
    cases: Vec<MicrocompactCase>,
}

#[derive(Debug, Deserialize)]
struct MicrocompactCase {
    name: String,
    tool_name: Option<String>,
    result_retention: Option<ToolResultRetention>,
    repeat: Option<usize>,
    replaced_with_compact_marker: Option<bool>,
    artifact_required: Option<bool>,
    artifact_write_succeeds: Option<bool>,
    existing_artifact_ref: Option<ToolArtifactRef>,
    persisted_utf8_text: Option<String>,
    original_message_preserved: Option<bool>,
    planned_candidate_reclaim_tokens: Option<Vec<u64>>,
    actual_replacement_reclaim_tokens: Option<Vec<u64>>,
    tokens_before_application: Option<u64>,
    target_tokens: Option<u64>,
    applied_candidate_count: Option<usize>,
    tokens_after_application: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct SessionPromptFixture {
    content_unit: String,
    limit_chars: usize,
    head_chars: usize,
    tail_chars: usize,
    notice: String,
    cases: Vec<SessionPromptCase>,
}

#[derive(Debug, Deserialize)]
struct SessionPromptCase {
    repeat: usize,
    truncated: bool,
    content_chars: usize,
    unit_chars: usize,
}

#[derive(Debug, Deserialize)]
struct SummaryFixture {
    event_limit: usize,
    messages: Vec<FixtureMessage>,
    expected: Value,
}

#[derive(Debug, Deserialize)]
struct RecompressionFixture {
    messages: Vec<FixtureMessage>,
    expected: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct UnicodeExcerptFixture {
    content_unit: String,
    repeat: usize,
    limit_chars: usize,
    expected_unit_chars: usize,
    suffix: String,
}

#[derive(Debug, Deserialize)]
struct SessionExtractionFixture {
    cycle: i32,
    raw: String,
    expected: Value,
}

#[derive(Debug, Deserialize)]
struct SummaryParseFixture {
    raw: String,
    expected: Value,
}

fn fixture() -> MemoryLocalFixture {
    serde_json::from_str(FIXTURE_TEXT).expect("memory local fixture")
}

fn fixture_messages(messages: &[FixtureMessage]) -> Vec<Message> {
    messages
        .iter()
        .map(|fixture| {
            let mut message = match fixture.role.as_str() {
                "system" => Message::system(&fixture.content),
                "user" => Message::user(&fixture.content),
                "assistant" => Message::assistant(&fixture.content),
                "tool" => Message::new(MessageRole::Tool, &fixture.content),
                role => panic!("unsupported fixture role: {role}"),
            };
            message.tool_call_id = fixture.tool_call_id.clone();
            message.tool_calls = fixture
                .tool_calls
                .iter()
                .map(|call| {
                    ToolCall::from_raw_arguments(&call.id, &call.name, call.arguments.clone())
                })
                .collect();
            message
        })
        .collect()
}

fn extract_first_object(raw: &str) -> Option<Value> {
    raw.char_indices()
        .filter(|(_, character)| *character == '{')
        .find_map(|(index, _)| {
            serde_json::Deserializer::from_str(&raw[index..])
                .into_iter::<Value>()
                .next()
                .and_then(Result::ok)
                .filter(Value::is_object)
        })
}

#[test]
fn canonical_fixture_and_token_counts_match() {
    let fixture = fixture();
    assert_eq!(fixture.contract, "memory_local");
    assert_eq!(fixture.character_unit, "unicode_code_point");

    for case in fixture.token_counts {
        let text = match (case.text, case.text_unit, case.repeat) {
            (Some(text), None, None) => text,
            (None, Some(unit), Some(repeat)) => unit.repeat(repeat),
            fields => panic!("invalid token fixture fields: {fields:?}"),
        };
        assert_eq!(
            count_tokens(&text, &case.model),
            case.tokens,
            "{}",
            case.model
        );
    }
}

#[test]
fn message_token_count_matches_text_block_and_image_rules() {
    let fixture = fixture().message_tokens;
    let messages = fixture_messages(&fixture.messages);
    assert_eq!(
        count_messages_tokens(&messages, &fixture.model),
        fixture.tokens
    );

    let mut image_message = Message::user("look");
    image_message.image_url = Some("https://example.test/image.png".to_string());
    assert_eq!(
        count_messages_tokens(&[image_message], "gpt-4o"),
        count_tokens("look", "gpt-4o") + 765
    );
}

#[test]
fn microcompact_uses_unicode_code_point_boundaries() {
    let fixture = fixture().microcompact;
    assert_eq!(fixture.schema_version, "vv-agent.microcompaction.v3");
    assert_eq!(fixture.content_unit.chars().count(), 1);
    assert_eq!(fixture.trigger_ratio_default, 0.75);
    assert_eq!(fixture.target_ratio_default, 0.60);
    assert_eq!(fixture.keep_recent_cycles_default, 3);
    assert_eq!(fixture.min_result_chars_default, 500);

    for case in fixture.cases {
        let Some(repeat) = case.repeat else {
            assert_eq!(case.name, "actual_replacement_delta_reaches_target");
            let planned = case
                .planned_candidate_reclaim_tokens
                .expect("planned reclaim fixture");
            let actual = case
                .actual_replacement_reclaim_tokens
                .expect("actual reclaim fixture");
            let before = case
                .tokens_before_application
                .expect("tokens before application");
            let target = case.target_tokens.expect("target tokens");
            let after = case
                .tokens_after_application
                .expect("tokens after application");
            assert!(planned.iter().sum::<u64>() > before - target);
            assert_eq!(
                case.applied_candidate_count
                    .expect("applied candidate count"),
                actual.len()
            );
            assert_eq!(after, before - actual.iter().sum::<u64>());
            assert!(after <= target);
            continue;
        };
        let expected = case
            .replaced_with_compact_marker
            .expect("repeat case replacement expectation");
        let retention = case.result_retention.expect("repeat case result retention");
        if retention == ToolResultRetention::Preserve {
            assert_eq!(case.name, "preserved_tool_remains_inline");
            assert!(!expected);
            continue;
        }

        let backend = Arc::new(MemoryWorkspaceBackend::default());
        let messages = vec![
            Message::system("system"),
            Message {
                tool_calls: vec![ToolCall::new(
                    "call_old",
                    case.tool_name.clone().expect("repeat case tool name"),
                    Default::default(),
                )],
                ..Message::assistant("old tool call")
            },
            Message::tool(fixture.content_unit.repeat(repeat), "call_old"),
            Message::assistant("recent reply 1"),
            Message::assistant("recent reply 2"),
            Message::assistant("recent reply 3"),
        ];
        let usage = count_messages_tokens(&messages, "");
        let mut manager = MemoryManager::new(MemoryManagerConfig {
            compact_threshold: usage + 10,
            model_context_window: usage + 10,
            reserved_output_tokens: 0,
            autocompact_buffer_tokens: 0,
            microcompaction_policy: MicrocompactionPolicy::new(
                0.01,
                0.005,
                fixture.keep_recent_cycles_default,
                fixture.min_result_chars_default,
            )
            .expect("policy"),
            ..MemoryManagerConfig::default()
        })
        .with_recovery_tool_available(true);
        let mut messages = messages;
        if let Some(artifact) = case.existing_artifact_ref.clone() {
            backend
                .write_text_exclusive(
                    &artifact.path,
                    case.persisted_utf8_text
                        .as_deref()
                        .expect("persisted artifact text"),
                )
                .expect("fixture artifact");
            messages[2].artifact_ref = Some(artifact);
        }
        if case.artifact_write_succeeds != Some(false) {
            manager = manager.with_workspace_backend(backend.clone());
        }

        let original = messages[2].clone();
        let (compacted, changed) = manager.compact_for_cycle(&messages, 5, false);
        let archived = compacted[2].content.starts_with(TOOL_RESULT_COMPACT_MARKER);

        assert_eq!(archived, expected, "{}", case.name);
        assert_eq!(changed, expected, "{}", case.name);
        if case.original_message_preserved == Some(true) {
            assert_eq!(compacted[2], original, "{}", case.name);
        }
        if archived {
            let artifact = compacted[2]
                .artifact_ref
                .as_ref()
                .expect("compacted message artifact");
            assert!(
                artifact.path.starts_with(".vv-agent/artifacts/"),
                "{}",
                case.name
            );
            assert_eq!(
                backend.read_text(&artifact.path).expect("archived content"),
                case.persisted_utf8_text
                    .as_deref()
                    .unwrap_or(original.content.as_str()),
                "{}",
                case.name
            );
            assert!(compacted[2]
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
                assert!(!compacted[2].content.contains(forbidden), "{forbidden}");
            }
        }
        if case.artifact_required == Some(true) {
            assert!(compacted[2].artifact_ref.is_some(), "{}", case.name);
        }
    }
}

#[test]
fn session_prompt_truncation_uses_fixture_code_point_limits() {
    let fixture = fixture().session_prompt_truncation;
    assert_eq!(fixture.content_unit.chars().count(), 1);

    for case in fixture.cases {
        let captured = Arc::new(Mutex::new(String::new()));
        let callback_capture = Arc::clone(&captured);
        let mut memory = SessionMemory::new(SessionMemoryConfig {
            extraction_callback: Some(Arc::new(move |prompt, _, _| {
                *callback_capture.lock().expect("prompt capture") = prompt.to_string();
                Some("[]".to_string())
            })),
            ..SessionMemoryConfig::default()
        });
        let content = fixture.content_unit.repeat(case.repeat);
        memory.extract(&[Message::user(content)], 1, 1);

        let prompt = captured.lock().expect("captured prompt").clone();
        let serialized = prompt
            .split_once("Messages:\n")
            .expect("messages section")
            .1;
        let messages: Vec<Value> = serde_json::from_str(serialized).expect("prompt messages");
        let rendered = messages[0]["content"].as_str().expect("message content");
        let expected = if case.truncated {
            format!(
                "{}{}{}",
                fixture.content_unit.repeat(fixture.head_chars),
                fixture.notice,
                fixture.content_unit.repeat(fixture.tail_chars)
            )
        } else {
            fixture.content_unit.repeat(case.repeat)
        };

        assert_eq!(case.truncated, case.repeat > fixture.limit_chars);
        assert_eq!(rendered, expected);
        assert_eq!(rendered.chars().count(), case.content_chars);
        assert_eq!(
            rendered
                .chars()
                .filter(|character| fixture.content_unit.contains(*character))
                .count(),
            case.unit_chars
        );
    }
}

#[test]
fn local_summary_and_recompression_match_the_fixture() {
    let fixture = fixture();
    let summary = LocalSummary::from_messages(
        &fixture_messages(&fixture.summary.messages),
        fixture.summary.event_limit,
    );
    assert_eq!(
        serde_json::to_value(summary).expect("summary value"),
        fixture.summary.expected
    );

    let recompressed = LocalSummary::from_messages(
        &fixture_messages(&fixture.recompression_originals.messages),
        fixture.summary.event_limit,
    );
    assert_eq!(
        recompressed.original_user_messages,
        fixture.recompression_originals.expected
    );

    let excerpt = fixture.unicode_excerpt;
    let summarized = LocalSummary::summarize_content(
        &excerpt.content_unit.repeat(excerpt.repeat),
        excerpt.limit_chars,
    );
    assert_eq!(
        summarized,
        format!(
            "{}{}",
            excerpt.content_unit.repeat(excerpt.expected_unit_chars),
            excerpt.suffix
        )
    );
}

#[test]
fn session_parser_keeps_right_brackets_inside_strings() {
    let fixture = fixture().session_extraction;
    let entries = SessionMemory::new(SessionMemoryConfig::default())
        .parse_extraction_result(&fixture.raw, fixture.cycle);

    assert_eq!(
        serde_json::to_value(entries).expect("entries"),
        fixture.expected
    );
}

#[test]
fn memory_manager_scans_prefixed_summary_json_for_restore() {
    let fixture = fixture().summary_parse;
    assert_eq!(
        extract_first_object(&fixture.raw),
        Some(fixture.expected.clone())
    );

    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("demo.py"), "print('restored')\n").expect("fixture file");
    let mut summary = fixture.expected;
    summary["files_examined_or_modified"] = json!([{
        "path": "demo.py",
        "action": "modified",
        "summary": "Modified demo.py"
    }]);
    let start = fixture.raw.find('{').expect("json object start");
    let end = fixture.raw.rfind('}').expect("json object end");
    let wrapped = format!(
        "{}{}{}",
        &fixture.raw[..start],
        serde_json::to_string(&summary).expect("summary json"),
        &fixture.raw[end + 1..]
    );
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        workspace: Some(workspace.path().to_path_buf()),
        summary_callback: Some(Arc::new(move |_, _, _| Some(wrapped.clone()))),
        ..MemoryManagerConfig::default()
    });
    let (compacted, changed) = manager.compact(
        &[
            Message::system("system"),
            Message::user("restore demo.py"),
            Message::assistant("done"),
        ],
        true,
    );

    assert!(changed);
    assert!(compacted[1]
        .content
        .contains("<Post-Compaction File Context>"));
    assert!(compacted[1].content.contains("print('restored')"));
}

#[test]
fn memory_manager_local_summary_keeps_artifact_facts() {
    let workspace = tempfile::tempdir().expect("workspace");
    let backend = Arc::new(LocalWorkspaceBackend::new(workspace.path()));
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        workspace: Some(workspace.path().to_path_buf()),
        tool_result_compact_threshold: 10,
        tool_result_keep_last: 0,
        ..MemoryManagerConfig::default()
    })
    .with_workspace_backend(backend)
    .with_recovery_tool_available(true);
    let messages = vec![
        Message::system("system"),
        Message::user("read data.txt"),
        Message {
            tool_calls: vec![ToolCall::new(
                "call_artifact",
                "read_file",
                [("path".to_string(), json!("data.txt"))]
                    .into_iter()
                    .collect(),
            )],
            ..Message::assistant("reading")
        },
        Message::tool("large result ".repeat(20), "call_artifact"),
        Message::assistant("done"),
    ];

    let (compacted, changed) = manager.compact_for_cycle(&messages, 3, true);
    assert!(changed);
    let summary = extract_first_object(&compacted[1].content).expect("local summary");
    let facts = summary["key_facts"].as_array().expect("key facts");
    assert!(facts.iter().any(|fact| {
        fact.as_str().is_some_and(|fact| {
            fact.contains(".vv-agent/artifacts/") && fact.ends_with("(tool=read_file)")
        })
    }));
}
