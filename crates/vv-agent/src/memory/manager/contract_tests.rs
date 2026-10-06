use super::*;
use crate::memory::token_utils::test_estimator;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
fn fixture(name: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/tests/fixtures/parity/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap()
}
fn messages(value: &Value) -> Vec<Message> {
    serde_json::from_value(value.clone()).unwrap()
}
struct NoFileReads;
impl crate::workspace::WorkspaceBackend for NoFileReads {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn list_files(&self, _: &str, _: &str) -> std::io::Result<Vec<String>> {
        panic!("automatic list")
    }
    fn read_text(&self, _: &str) -> std::io::Result<String> {
        panic!("automatic read")
    }
    fn read_bytes(&self, _: &str) -> std::io::Result<Vec<u8>> {
        panic!("automatic read")
    }
    fn write_text(&self, _: &str, _: &str, _: bool) -> std::io::Result<usize> {
        panic!("unexpected summary write")
    }
    fn write_text_exclusive(&self, _: &str, _: &str) -> std::io::Result<usize> {
        panic!("second pruner")
    }
    fn write_text_chunks_exclusive(
        &self,
        _: &str,
        _: &mut dyn Iterator<Item = std::io::Result<String>>,
    ) -> std::io::Result<usize> {
        panic!("second pruner")
    }
    fn file_info(&self, _: &str) -> std::io::Result<Option<crate::workspace::FileInfo>> {
        panic!("automatic stat")
    }
    fn exists(&self, _: &str) -> bool {
        panic!("automatic exists")
    }
    fn is_file(&self, _: &str) -> bool {
        panic!("automatic stat")
    }
    fn mkdir(&self, _: &str) -> std::io::Result<()> {
        panic!("unexpected summary mkdir")
    }
}
fn run(case: &Value, language: &str) -> (Vec<Message>, bool, Vec<String>) {
    let input = &case["input"];
    let original = messages(&input["messages"]);
    let _estimate = case.get("token_estimator").map(|e| {
        test_estimator::install(
            &original,
            e["input_messages"].as_u64().unwrap(),
            e["candidate_messages"].as_u64().unwrap(),
        )
    });
    let captured = Arc::new(Mutex::new(Vec::new()));
    let capture = captured.clone();
    let callback = input
        .get("callback")
        .cloned()
        .unwrap_or(json!({"kind":"return", "value": input["summary_response"]}));
    let absent = callback["kind"] == "absent";
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        keep_recent_messages: input["keep_recent_messages"].as_u64().unwrap() as usize,
        language: language.into(),
        summary_event_limit: 10,
        model_context_window: if input["candidate_fits"] == false {
            1
        } else {
            279000
        },
        reserved_output_tokens: 0,
        summary_callback: (!absent).then(|| {
            Arc::new(move |prompt: &str, _: Option<&str>, _: Option<&str>| {
                capture.lock().unwrap().push(prompt.to_owned());
                if callback["kind"] == "raise" {
                    panic!("scripted summary unavailable");
                }
                callback["value"].as_str().map(str::to_owned)
            }) as SummaryCallback
        }),
        ..MemoryManagerConfig::default()
    })
    .with_workspace_backend(Arc::new(NoFileReads))
    .with_recovery_tool_available(input["recovery_tool_available"].as_bool().unwrap_or(true));
    let (output, changed) = if let Some(ratio) = input["drop_ratio"].as_f64() {
        let output = manager.emergency_compact(&original, ratio);
        let changed = output != original;
        (output, changed)
    } else {
        manager.compact_for_cycle(
            &original,
            input["cycle_index"].as_u64().unwrap_or(5) as u32,
            true,
        )
    };
    let prompts = captured.lock().unwrap().clone();
    (output, changed, prompts)
}
fn assert_case(case: Value) {
    let (output, changed, prompts) = run(&case, "zh-CN");
    assert_eq!(
        changed,
        case["expected"]["changed"].as_bool().unwrap(),
        "{}",
        case["name"]
    );
    assert_eq!(
        serde_json::to_value(output).unwrap(),
        case["expected"]["messages"]
    );
    assert_eq!(
        prompts.len() as u64,
        case["expected"]["summary_calls"].as_u64().unwrap()
    );
    if let Some(expected) = case.get("expected_summary_input") {
        if let Some(prompt) = prompts.first() {
            for (label, key) in [
                ("Previous Summary", "previous_summary"),
                ("Conversation Prefix", "conversation_prefix"),
            ] {
                let raw = prompt
                    .split_once(&format!("<{label}>\n"))
                    .unwrap()
                    .1
                    .split_once(&format!("\n</{label}>"))
                    .unwrap()
                    .0;
                assert_eq!(serde_json::from_str::<Value>(raw).unwrap(), expected[key]);
            }
        }
    }
}
#[test]
fn old_tool_pairs() {
    assert_case(fixture("memory_local")["summary_compaction"]["cases"][0].clone());
}
#[test]
fn parallel_multi_call_block() {
    assert_case(fixture("memory_local")["summary_compaction"]["cases"][1].clone());
}
#[test]
fn duplicate_call_ids_across_blocks() {
    assert_case(fixture("memory_local")["summary_compaction"]["cases"][2].clone());
}
#[test]
fn no_system_message() {
    assert_case(fixture("memory_local")["summary_compaction"]["cases"][3].clone());
}
#[test]
fn nothing_removable() {
    assert_case(fixture("memory_local")["summary_compaction"]["cases"][4].clone());
}
#[test]
fn recompression_merges_and_deduplicates_evidence() {
    assert_case(fixture("memory_local")["summary_compaction"]["cases"][5].clone());
}
#[test]
fn summary_paths_do_not_read_files() {
    assert_case(fixture("memory_local")["summary_compaction"]["cases"][6].clone());
}
#[test]
fn candidate_does_not_shrink() {
    assert_case(fixture("memory_local")["summary_compaction"]["cases"][7].clone());
}
#[test]
fn failure_empty() {
    let mut case = fixture("memory_local")["summary_compaction"]["cases"][8].clone();
    let variant = case["variants"][0].clone();
    case["input"]
        .as_object_mut()
        .unwrap()
        .extend(variant.as_object().unwrap().clone());
    case["expected"]["summary_calls"] = variant["expected_summary_calls"].clone();
    assert_case(case);
}
#[test]
fn failure_analysis_only() {
    let mut case = fixture("memory_local")["summary_compaction"]["cases"][8].clone();
    let variant = case["variants"][1].clone();
    case["input"]
        .as_object_mut()
        .unwrap()
        .extend(variant.as_object().unwrap().clone());
    case["expected"]["summary_calls"] = variant["expected_summary_calls"].clone();
    assert_case(case);
}
#[test]
fn failure_invalid_json() {
    let mut case = fixture("memory_local")["summary_compaction"]["cases"][8].clone();
    let variant = case["variants"][2].clone();
    case["input"]
        .as_object_mut()
        .unwrap()
        .extend(variant.as_object().unwrap().clone());
    case["expected"]["summary_calls"] = variant["expected_summary_calls"].clone();
    assert_case(case);
}
#[test]
fn failure_no_effective_content() {
    let mut case = fixture("memory_local")["summary_compaction"]["cases"][8].clone();
    let variant = case["variants"][3].clone();
    case["input"]
        .as_object_mut()
        .unwrap()
        .extend(variant.as_object().unwrap().clone());
    case["expected"]["summary_calls"] = variant["expected_summary_calls"].clone();
    assert_case(case);
}
#[test]
fn failure_callback_exception() {
    let mut case = fixture("memory_local")["summary_compaction"]["cases"][8].clone();
    let variant = case["variants"][4].clone();
    case["input"]
        .as_object_mut()
        .unwrap()
        .extend(variant.as_object().unwrap().clone());
    case["expected"]["summary_calls"] = variant["expected_summary_calls"].clone();
    assert_case(case);
}
#[test]
fn failure_no_callback() {
    let mut case = fixture("memory_local")["summary_compaction"]["cases"][8].clone();
    let variant = case["variants"][5].clone();
    case["input"]
        .as_object_mut()
        .unwrap()
        .extend(variant.as_object().unwrap().clone());
    case["expected"]["summary_calls"] = variant["expected_summary_calls"].clone();
    assert_case(case);
}
#[test]
fn failure_manifest_over_budget() {
    let mut case = fixture("memory_local")["summary_compaction"]["cases"][8].clone();
    let variant = case["variants"][7].clone();
    case["input"]
        .as_object_mut()
        .unwrap()
        .extend(variant.as_object().unwrap().clone());
    case["expected"]["summary_calls"] = variant["expected_summary_calls"].clone();
    assert_case(case);
}
#[test]
fn failure_recovery_surface_unavailable() {
    let mut case = fixture("memory_local")["summary_compaction"]["cases"][8].clone();
    let variant = case["variants"][8].clone();
    case["input"]
        .as_object_mut()
        .unwrap()
        .extend(variant.as_object().unwrap().clone());
    case["expected"]["summary_calls"] = variant["expected_summary_calls"].clone();
    assert_case(case);
}
#[test]
fn prefix_image_uses_text_placeholder() {
    assert_case(fixture("memory_local")["summary_compaction"]["cases"][9].clone());
}
#[test]
fn computer_agent_image_prefix_uses_placeholders() {
    assert_case(fixture("memory_local")["summary_compaction"]["cases"][10].clone());
}
#[test]
fn trailing_incomplete_block_stays_in_tail() {
    assert_case(fixture("memory_local")["summary_compaction"]["cases"][11].clone());
}
#[test]
fn normalization_missing_fields() {
    let mut case =
        fixture("memory_local")["summary_compaction"]["accepted_normalization_cases"].clone();
    let variant = case["variants"][0].clone();
    case["input"]["summary_response"] = variant["summary_response"].clone();
    case["expected"] = variant["expected"].clone();
    assert_case(case);
}
#[test]
fn normalization_wrong_version() {
    let mut case =
        fixture("memory_local")["summary_compaction"]["accepted_normalization_cases"].clone();
    let variant = case["variants"][1].clone();
    case["input"]["summary_response"] = variant["summary_response"].clone();
    case["expected"] = variant["expected"].clone();
    assert_case(case);
}
#[test]
fn normalization_wrong_field_type() {
    let mut case =
        fixture("memory_local")["summary_compaction"]["accepted_normalization_cases"].clone();
    let variant = case["variants"][2].clone();
    case["input"]["summary_response"] = variant["summary_response"].clone();
    case["expected"] = variant["expected"].clone();
    assert_case(case);
}
#[test]
fn normalization_unknown_field() {
    let mut case =
        fixture("memory_local")["summary_compaction"]["accepted_normalization_cases"].clone();
    let variant = case["variants"][3].clone();
    case["input"]["summary_response"] = variant["summary_response"].clone();
    case["expected"] = variant["expected"].clone();
    assert_case(case);
}
#[test]
fn normalization_malformed_nested_records() {
    let mut case =
        fixture("memory_local")["summary_compaction"]["accepted_normalization_cases"].clone();
    let variant = case["variants"][4].clone();
    case["input"]["summary_response"] = variant["summary_response"].clone();
    case["expected"] = variant["expected"].clone();
    assert_case(case);
}
#[test]
fn normalization_tolerant_wrappers() {
    let mut case =
        fixture("memory_local")["summary_compaction"]["accepted_normalization_cases"].clone();
    let variant = case["variants"][5].clone();
    case["input"]["summary_response"] = variant["summary_response"].clone();
    case["expected"] = variant["expected"].clone();
    assert_case(case);
}
#[test]
fn normalization_first_json_object() {
    let mut case =
        fixture("memory_local")["summary_compaction"]["accepted_normalization_cases"].clone();
    let variant = case["variants"][6].clone();
    case["input"]["summary_response"] = variant["summary_response"].clone();
    case["expected"] = variant["expected"].clone();
    assert_case(case);
}
#[test]
fn invalid_missing_result_in_middle() {
    let case = fixture("memory_local")["summary_compaction"]["invalid_block_cases"][0].clone();
    assert_case(
        json!({"input":{"messages":case["messages"],"keep_recent_messages":1,"summary_response":"{\"progress\":[\"done\"]}"},"expected":case["expected"]}),
    );
}
#[test]
fn invalid_duplicate_result() {
    let case = fixture("memory_local")["summary_compaction"]["invalid_block_cases"][1].clone();
    assert_case(
        json!({"input":{"messages":case["messages"],"keep_recent_messages":1,"summary_response":"{\"progress\":[\"done\"]}"},"expected":case["expected"]}),
    );
}
#[test]
fn invalid_out_of_order_results() {
    let case = fixture("memory_local")["summary_compaction"]["invalid_block_cases"][2].clone();
    assert_case(
        json!({"input":{"messages":case["messages"],"keep_recent_messages":1,"summary_response":"{\"progress\":[\"done\"]}"},"expected":case["expected"]}),
    );
}
#[test]
fn localized_prompt_golden() {
    let f = fixture("memory_local");
    for language in ["zh-CN", "en-US"] {
        let (_, _, prompts) = run(&f["summary_compaction"]["cases"][0], language);
        assert_eq!(
            prompts,
            vec![
                f["summary_compaction"]["prompt_cases"][0]["expected_prompts"][language]
                    .as_str()
                    .unwrap()
            ]
        );
    }
}
#[test]
fn emergency_resummarizes_more_tail() {
    assert_case(fixture("memory_lifecycle")["emergency_cases"][0].clone());
}
#[test]
fn emergency_exhausted_atomic_tail() {
    assert_case(fixture("memory_lifecycle")["emergency_cases"][1].clone());
}
#[test]
fn emergency_failed_summary_exhausted() {
    assert_case(fixture("memory_lifecycle")["emergency_cases"][2].clone());
}

#[test]
fn relative_microcompact_age_and_tail_contract() {
    for case in fixture("memory_local")["microcompact"]["transcript_cases"]
        .as_array()
        .unwrap()
    {
        let input = &case["input"];
        let manager = MemoryManager::new(MemoryManagerConfig {
            compact_threshold: 1667,
            reserved_output_tokens: 0,
            autocompact_buffer_tokens: 0,
            keep_recent_messages: input["keep_recent_messages"].as_u64().unwrap_or(2) as usize,
            microcompaction_policy: serde_json::from_value(input["policy"].clone()).unwrap(),
            ..MemoryManagerConfig::default()
        })
        .with_recovery_tool_available(true);
        let original = messages(&input["messages"]);
        let plan = manager.plan_microcompaction(
            &original,
            input["cycle_index"].as_u64().unwrap() as u32,
            input["current_tokens"].as_u64().unwrap(),
        );
        assert_eq!(
            plan.candidate_count,
            case["expected"]["candidate_message_indices"]
                .as_array()
                .unwrap()
                .len(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn below_threshold_and_failed_image_summary_preserve_history() {
    let mut input =
        messages(&fixture("memory_local")["summary_compaction"]["cases"][0]["input"]["messages"]);
    let mut image = Message::user("Inspect image");
    image.image_url = Some("data:image/png;base64,AA==".into());
    input.insert(2, image);
    let mut manager = MemoryManager::new(MemoryManagerConfig::default());
    assert_eq!(manager.compact(&input, false), (input.clone(), false));
    manager.config.compact_threshold = 100;
    assert_eq!(manager.compact(&input, false), (input.clone(), false));
    let captured = Arc::new(Mutex::new(String::new()));
    let capture = captured.clone();
    manager.config.keep_recent_messages = 2;
    manager.config.summary_callback = Some(Arc::new(move |prompt, _, _| {
        *capture.lock().unwrap() = prompt.into();
        None
    }));
    assert_eq!(manager.compact(&input, true), (input, false));
    let prompt = captured.lock().unwrap();
    assert!(prompt.contains("[image omitted from summary input: Inspect image]"));
    assert!(!prompt.contains("data:image"));
}

#[test]
fn control_failure_propagates_without_replacement() {
    let case = fixture("memory_local")["summary_compaction"]["control_failure_case"].clone();
    let input = messages(&case["input"]["messages"]);
    for variant in case["variants"].as_array().unwrap() {
        let code = variant["callback"]["code"].as_str().unwrap().to_string();
        let thrown = code.clone();
        let mut manager = MemoryManager::new(MemoryManagerConfig {
            keep_recent_messages: 2,
            ..Default::default()
        })
        .with_runtime_callbacks(RuntimeMemoryCallbacks {
            memory_compaction: Some(Arc::new(move |_, _, _, _| {
                Err(RuntimeMemoryCallbackError::new(thrown.clone()))
            })),
            ..Default::default()
        });
        let error = manager
            .compact_for_cycle_with_usage_observed(&input, 5, true, None, None, None)
            .unwrap_err();
        assert_eq!(error.downcast::<String>().unwrap(), code);
        let error = manager
            .emergency_compact_observed(&input, 0.4, Some(5))
            .unwrap_err();
        assert_eq!(error.downcast::<String>().unwrap(), code);
    }
}

#[test]
fn old_tool_pairs_capture_and_literal_placeholders() {
    let f = fixture("memory_local");
    let case = &f["summary_compaction"]["cases"][0];
    let (output, changed, prompts) = run(case, "zh-CN");
    assert!(changed);
    if let Ok(directory) = std::env::var("FIX2_CAPTURE_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            format!("{directory}/old_tool_pairs-output.json"),
            serde_json::to_string_pretty(&output).unwrap(),
        )
        .unwrap();
        std::fs::write(
            format!("{directory}/old_tool_pairs-prompt-zh.txt"),
            &prompts[0],
        )
        .unwrap();
        let (_, _, english) = run(case, "en-US");
        std::fs::write(
            format!("{directory}/old_tool_pairs-prompt-en.txt"),
            &english[0],
        )
        .unwrap();
    }
    for language in ["zh-CN", "en-US"] {
        let mut case = case.clone();
        case["input"]["messages"][1]["content"] =
            json!("literal {event_limit} {conversation_prefix_jcs} {previous_summary_jcs}");
        let (_, _, prompts) = run(&case, language);
        assert!(prompts[0]
            .contains("literal {event_limit} {conversation_prefix_jcs} {previous_summary_jcs}"));
    }
}

#[test]
fn full_compaction_uses_no_second_pruner_and_keeps_event_limit_out_of_history() {
    let mut original = vec![Message::system("system"), Message::user("inspect")];
    for i in 0..5 {
        original.push(Message {
            tool_calls: vec![crate::types::ToolCall::new(
                format!("c{i}"),
                "read_file",
                Default::default(),
            )],
            ..Message::assistant("")
        });
        original.push(Message::tool(
            format!("result {i}: {}", "evidence ".repeat(600)),
            format!("c{i}"),
        ));
    }
    let captured = Arc::new(Mutex::new(String::new()));
    let capture = captured.clone();
    let workspace = Arc::new(crate::workspace::MemoryWorkspaceBackend::default());
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        keep_recent_messages: 2,
        summary_event_limit: 1,
        summary_callback: Some(Arc::new(move |prompt, _, _| {
            *capture.lock().unwrap() = prompt.into();
            Some("{\"progress\":[\"one\",\"two\",\"three\"]}".into())
        })),
        ..Default::default()
    })
    .with_workspace_backend(workspace)
    .with_archive_context(
        "single-pruner",
        [(
            "read_file".into(),
            crate::tools::ToolResultRetention::Preserve,
        )]
        .into_iter()
        .collect(),
        true,
    );
    let (output, changed) = manager.compact(&original, true);
    assert!(changed);
    assert_eq!(output[2..], original[original.len() - 2..]);
    let prompt = captured.lock().unwrap();
    for i in 0..4 {
        assert!(prompt.contains(&format!("result {i}: {}", "evidence ".repeat(600))));
    }
    assert!(!prompt.contains("<Tool Result Compact>"));
    assert!(output[1]
        .content
        .contains("\"progress\":[\"one\",\"two\",\"three\"]"));
}

#[test]
fn malformed_summary_evidence_never_authorizes_history_removal() {
    // Publicly constructed messages still encounter the same strict boundary at compaction.
    let mut original =
        messages(&fixture("memory_local")["summary_compaction"]["cases"][5]["input"]["messages"]);
    original[1]
        .metadata
        .get_mut("_vv_agent_compaction")
        .unwrap()["extra"] = json!(true);
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        keep_recent_messages: 2,
        summary_callback: Some(Arc::new(|_, _, _| Some("{\"progress\":[\"done\"]}".into()))),
        ..Default::default()
    })
    .with_recovery_tool_available(true);
    assert_eq!(manager.compact(&original, true), (original, false));
}

#[test]
fn compaction_accounting_contract_uses_real_dispatch_and_budget_observer() {
    use crate::llm::LlmRequest;
    use crate::runtime::model_calls::{ModelCallCoordinator, ModelCallLedger};
    use crate::types::{LLMResponse, ModelCallOperation};
    use std::sync::atomic::{AtomicU64, Ordering};
    for index in 0..3 {
        let accounting_case = fixture("token_usage")["compaction_cases"][index].clone();
        let transcript = match index {
            0 => fixture("memory_lifecycle")["summary_pipeline"]["prune_only_case"].clone(),
            1 => fixture("memory_local")["summary_compaction"]["cases"][0].clone(),
            _ => fixture("memory_lifecycle")["emergency_cases"][0].clone(),
        };
        let input = &transcript["input"];
        let original = messages(&input["messages"]);
        let estimator = &transcript["token_estimator"];
        let _guard = test_estimator::install(
            &original,
            estimator["input_messages"].as_u64().unwrap(),
            estimator["candidate_messages"].as_u64().unwrap(),
        );
        if index == 0 {
            test_estimator::install_result_counts(800, 400);
        }
        let ledger = ModelCallLedger::default();
        ledger
            .replace(
                serde_json::from_value(accounting_case["input"]["existing_model_calls"].clone())
                    .unwrap(),
            )
            .unwrap();
        let budget = Arc::new(AtomicU64::new(0));
        let observer_budget = budget.clone();
        let coordinator = ModelCallCoordinator::new(
            ledger.clone(),
            "run",
            "trace",
            "agent",
            None,
            None,
            None,
            Some(Arc::new(move |_, usage| {
                observer_budget.fetch_add(usage.total_tokens.unwrap_or(0), Ordering::SeqCst);
                Default::default()
            })),
        );
        let dispatches = Arc::new(AtomicU64::new(0));
        let calls = dispatches.clone();
        let callback_case = accounting_case.clone();
        let callback = Arc::new(
            move |prompt: &str, _: Option<&str>, _: Option<&str>, cycle: u32| {
                let response = &callback_case["input"]["provider_responses"][0];
                let mut request = LlmRequest::new(
                    "test-model",
                    vec![Message::user(prompt)],
                    crate::prompt::PromptBundle::from_instruction_text("system").unwrap(),
                );
                request.tools = vec![];
                let slot = if index == 2 {
                    "memory_compaction_2"
                } else {
                    "memory_compaction_1"
                };
                let result = coordinator
                    .dispatch(
                        ModelCallOperation::MemoryCompaction,
                        cycle,
                        slot,
                        "test",
                        "test-model",
                        &request,
                        || {
                            calls.fetch_add(1, Ordering::SeqCst);
                            let mut output =
                                LLMResponse::new(response["content"].as_str().unwrap());
                            output.raw.insert("usage".into(), response["usage"].clone());
                            Ok(output)
                        },
                    )
                    .map_err(RuntimeMemoryCallbackError::new)?;
                Ok(Some(result.response.content))
            },
        );
        let backend = Arc::new(crate::workspace::MemoryWorkspaceBackend::default());
        if index == 0 {
            let artifact = original[2].artifact_ref.as_ref().unwrap();
            backend
                .write_text_exclusive(
                    &artifact.path,
                    input["existing_artifact_text"].as_str().unwrap(),
                )
                .unwrap();
        }
        let mut manager = MemoryManager::new(MemoryManagerConfig {
            keep_recent_messages: input["keep_recent_messages"].as_u64().unwrap() as usize,
            compact_threshold: input["compact_threshold"].as_u64().unwrap_or(250000),
            microcompaction_policy: input
                .get("microcompaction_policy")
                .map(|p| serde_json::from_value(p.clone()).unwrap())
                .unwrap_or_default(),
            ..Default::default()
        })
        .with_workspace_backend(backend)
        .with_recovery_tool_available(true)
        .with_runtime_callbacks(RuntimeMemoryCallbacks {
            memory_compaction: Some(callback),
            ..Default::default()
        });
        let output = if index == 2 {
            manager
                .emergency_compact_observed(
                    &original,
                    input["drop_ratio"].as_f64().unwrap(),
                    Some(5),
                )
                .unwrap()
        } else {
            let result = manager
                .compact_for_cycle_with_usage_observed(&original, 5, index == 1, None, None, None)
                .unwrap();
            if index == 0 {
                assert_eq!(result.archived_count, 1);
                assert_eq!(result.reclaimed_tokens, 400);
            }
            result.messages
        };
        assert_eq!(
            serde_json::to_value(output).unwrap(),
            transcript["expected"]["messages"]
        );
        assert_eq!(
            serde_json::to_value(ledger.records()).unwrap(),
            accounting_case["expected"]["model_calls"]
        );
        assert_eq!(
            dispatches.load(Ordering::SeqCst),
            accounting_case["expected"]["new_model_dispatches"]
                .as_u64()
                .unwrap()
        );
        assert_eq!(
            budget.load(Ordering::SeqCst),
            accounting_case["expected"]["new_budget_total_tokens"]
                .as_u64()
                .unwrap()
        );
    }
}

#[test]
fn microcompact_plan_uses_original_indices_before_empty_assistant_filtering() {
    let mut summary = Message::user("previous memory");
    summary.name = Some("memory_summary".into());
    let original = vec![
        Message::system("system"),
        summary.clone(),
        Message::assistant(""),
        Message {
            tool_calls: vec![crate::types::ToolCall::new(
                "old",
                "lookup",
                Default::default(),
            )],
            ..Message::assistant("")
        },
        Message::tool("original evidence ".repeat(500), "old"),
        Message::assistant("recent"),
    ];
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10000,
        model_context_window: 100000,
        reserved_output_tokens: 0,
        autocompact_buffer_tokens: 0,
        microcompaction_policy: crate::memory::MicrocompactionPolicy::new(0.01, 0.005, 1, 500)
            .unwrap(),
        ..Default::default()
    })
    .with_workspace_backend(Arc::new(crate::workspace::MemoryWorkspaceBackend::default()))
    .with_recovery_tool_available(true);
    let plan = manager.plan_cycle_microcompaction(&original, 1000, 5000);
    assert_eq!(plan.candidate_count, 1);
    let output = manager
        .compact_for_cycle_with_usage_observed(&original, 1000, false, Some(5000), None, Some(plan))
        .unwrap();
    assert_eq!(output.archived_count, 1);
    assert_eq!(output.messages[1], summary);
    assert_eq!(output.messages[3].tool_call_id.as_deref(), Some("old"));
    assert!(output.messages[3]
        .content
        .starts_with("<Tool Result Compact>"));
    assert_eq!(output.messages.last(), original.last());
}

#[test]
fn emergency_updates_session_memory_baseline_only_on_acceptance() {
    let case = fixture("memory_lifecycle")["emergency_cases"][0].clone();
    let input = &case["input"];
    let original = messages(&input["messages"]);
    let estimate = &case["token_estimator"];
    let _guard = test_estimator::install(
        &original,
        estimate["input_messages"].as_u64().unwrap(),
        estimate["candidate_messages"].as_u64().unwrap(),
    );
    let response = input["summary_response"].as_str().unwrap().to_owned();
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        keep_recent_messages: input["keep_recent_messages"].as_u64().unwrap() as usize,
        session_memory: Some(crate::memory::SessionMemory::new(
            crate::memory::SessionMemoryConfig::default(),
        )),
        summary_callback: Some(Arc::new(move |_, _, _| Some(response.clone()))),
        ..Default::default()
    })
    .with_recovery_tool_available(true);
    let output = manager.emergency_compact(&original, input["drop_ratio"].as_f64().unwrap());
    assert_ne!(output, original);
    assert_eq!(
        manager
            .session_memory()
            .unwrap()
            .state
            .tokens_at_last_extraction,
        estimate["candidate_messages"].as_u64().unwrap()
    );
    manager.config.summary_callback = None;
    assert_eq!(manager.emergency_compact(&original, 0.4), original);
    assert_eq!(
        manager
            .session_memory()
            .unwrap()
            .state
            .tokens_at_last_extraction,
        estimate["candidate_messages"].as_u64().unwrap()
    );
}
