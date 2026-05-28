use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use serde_json::json;
use vv_agent::prompt::{
    build_raw_system_prompt_sections, build_system_prompt_bundle_with_options,
    build_system_prompt_sections_with_options, build_system_prompt_with_options,
    hash_system_prompt_sections, hash_tool_payload, BuildSystemPromptOptions, CacheBreakTracker,
    PromptSection, SystemPromptBuilder,
};

#[test]
fn prompt_public_api_builds_agent_system_prompt_bundle() {
    let workspace = tempfile::tempdir().expect("workspace");
    let skill_dir = workspace.path().join("skills/review-code");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: review-code
description: Review code safely
---
Review code.
"#,
    )
    .expect("skill");

    let options = BuildSystemPromptOptions {
        current_time_utc: Some("2026-05-26T00:00:00Z".to_string()),
        session_memory_context: "<Session Memory>\nRemember alpha\n</Session Memory>".to_string(),
        available_sub_agents: BTreeMap::from([(
            "reviewer".to_string(),
            "Reviews source changes".to_string(),
        )]),
        available_skills: Some(json!(["skills"])),
        workspace: Some(workspace.path().to_path_buf()),
        ..BuildSystemPromptOptions::default()
    };

    let bundle = build_system_prompt_bundle_with_options("You are careful.", options.clone());
    assert!(bundle
        .prompt
        .contains("<Agent Definition>\nYou are careful."));
    assert!(bundle.prompt.contains("<Session Memory>"));
    assert!(bundle.prompt.contains("<Tools>"));
    assert!(bundle.prompt.contains("ask_user"));
    assert!(bundle.prompt.contains("create_sub_task"));
    assert!(bundle.prompt.contains("review-code"));
    assert!(bundle.prompt.contains("task_finish"));
    assert!(bundle.prompt.contains("<Current Time>"));
    assert!(bundle.prompt.contains("2026-05-26T00:00:00Z"));
    assert_eq!(bundle.stable_hash.len(), 64);

    let section_ids = bundle
        .sections
        .iter()
        .map(|section| section["id"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        section_ids,
        vec![
            "agent_definition",
            "session_memory",
            "tools",
            "current_time"
        ]
    );
    assert_eq!(bundle.sections[1]["stable"], false);

    let prompt = build_system_prompt_with_options("You are careful.", options.clone());
    assert_eq!(prompt, bundle.prompt);
    let sections = build_system_prompt_sections_with_options("You are careful.", options);
    assert_eq!(sections, bundle.sections);

    let raw = build_raw_system_prompt_sections("  raw system  ");
    assert_eq!(raw[0]["id"], "raw_system_prompt");
    assert_eq!(raw[0]["text"], "raw system");
    assert_eq!(raw[0]["stable"], true);
}

#[test]
fn model_visible_system_prompt_stays_capability_focused() {
    let workspace = tempfile::tempdir().expect("workspace");
    let options = BuildSystemPromptOptions {
        current_time_utc: Some("2026-05-26T00:00:00Z".to_string()),
        session_memory_context: "<Session Memory>\nRemember alpha\n</Session Memory>".to_string(),
        available_sub_agents: BTreeMap::from([(
            "reviewer".to_string(),
            "Reviews source changes".to_string(),
        )]),
        available_skills: Some(json!([])),
        workspace: Some(workspace.path().to_path_buf()),
        ..BuildSystemPromptOptions::default()
    };

    let bundle = build_system_prompt_bundle_with_options("You are careful.", options);
    for forbidden in prompt_forbidden_terms() {
        assert!(
            !contains_forbidden_term(&bundle.prompt, forbidden.as_str()),
            "model-visible system prompt should not include internal implementation wording `{forbidden}`:\n{}",
            bundle.prompt
        );
    }
}

#[test]
fn prompt_public_wording_guard_catches_case_variants() {
    let sample = forbidden_phrase(&[b"FOR ", TERM_LANGUAGE, SPACE, TERM_JOINING]);

    assert!(contains_forbidden_term(
        sample.as_str(),
        forbidden_phrase(&[b"for ", TERM_LANGUAGE, SPACE, TERM_JOINING]).as_str()
    ));
}

fn prompt_forbidden_terms() -> Vec<String> {
    [
        forbidden_phrase(&[TERM_LANGUAGE, SPACE, TERM_JOINING]),
        forbidden_phrase(&[TERM_LANGUAGE, b"-compatible"]),
        forbidden_phrase(&[b"for ", TERM_LANGUAGE]),
        forbidden_phrase(&[TERM_LANGUAGE, SPACE, TERM_SOURCE]),
        forbidden_phrase(&[TERM_LANGUAGE, b"-style"]),
        forbidden_phrase(&[TERM_JOINING]),
        forbidden_phrase(&[TERM_TRANSITION]),
        forbidden_phrase(&[TERM_EQUALITY]),
        forbidden_phrase(&[TERM_JOINING, b" alias"]),
        forbidden_phrase(&[b"reserved for ", TERM_JOINING]),
        join_words("scalar", " coercion"),
    ]
    .into()
}

const TERM_LANGUAGE: &[u8] = &[0x50, 0x79, 0x74, 0x68, 0x6f, 0x6e];
const TERM_JOINING: &[u8] = &[
    0x63, 0x6f, 0x6d, 0x70, 0x61, 0x74, 0x69, 0x62, 0x69, 0x6c, 0x69, 0x74, 0x79,
];
const TERM_TRANSITION: &[u8] = &[0x6d, 0x69, 0x67, 0x72, 0x61, 0x74, 0x69, 0x6f, 0x6e];
const TERM_EQUALITY: &[u8] = &[0x70, 0x61, 0x72, 0x69, 0x74, 0x79];
const TERM_SOURCE: &[u8] = &[0x72, 0x65, 0x66, 0x65, 0x72, 0x65, 0x6e, 0x63, 0x65];
const SPACE: &[u8] = b" ";

fn forbidden_phrase(parts: &[&[u8]]) -> String {
    let bytes = parts
        .iter()
        .flat_map(|part| part.iter().copied())
        .collect::<Vec<_>>();
    String::from_utf8(bytes).expect("forbidden phrase fixture is valid utf-8")
}

fn contains_forbidden_term(haystack: &str, forbidden: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&forbidden.to_ascii_lowercase())
}

fn join_words(first: &str, rest: &str) -> String {
    format!("{first}{rest}")
}

#[test]
fn prompt_public_api_tracks_section_and_tool_cache_breaks() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_section = Arc::clone(&calls);
    let stable = PromptSection::new(
        "stable",
        move || {
            calls_for_section.fetch_add(1, Ordering::SeqCst);
            "stable body".to_string()
        },
        true,
    );
    assert_eq!(stable.get_value(), "stable body");
    assert_eq!(stable.get_value(), "stable body");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(stable.to_metadata().expect("metadata")["id"], "stable");
    stable.invalidate();
    assert_eq!(stable.get_value(), "stable body");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let volatile_calls = Arc::new(AtomicUsize::new(0));
    let volatile_calls_for_section = Arc::clone(&volatile_calls);
    let volatile = PromptSection::new(
        "volatile",
        move || {
            volatile_calls_for_section.fetch_add(1, Ordering::SeqCst);
            "volatile body".to_string()
        },
        false,
    );

    let mut builder = SystemPromptBuilder::default();
    builder.add_section(stable);
    builder.add_section(volatile);
    assert!(builder.build().contains("stable body"));
    assert_eq!(volatile_calls.load(Ordering::SeqCst), 1);
    assert!(builder.build().contains("volatile body"));
    assert_eq!(volatile_calls.load(Ordering::SeqCst), 2);
    assert_eq!(builder.metadata_sections().len(), 2);
    assert_eq!(builder.stable_hash().len(), 64);
    let result = builder.build_result();
    assert!(result.prompt.contains("stable body"));
    assert_eq!(result.sections.len(), 2);

    let system_sections = vec![
        json!({"id": "a", "text": " hello ", "stable": true}),
        json!({"id": "empty", "text": ""}),
        json!("ignored"),
    ];
    let system_hash = hash_system_prompt_sections(&system_sections);
    assert_eq!(system_hash.len(), 64);
    assert_eq!(hash_system_prompt_sections(&[]), "");

    let tool_hash = hash_tool_payload(&[json!({"name": "read_file"})]);
    assert_eq!(tool_hash.len(), 64);
    assert_eq!(hash_tool_payload(&[]), "");

    let mut tracker = CacheBreakTracker::default();
    assert!(tracker
        .check(system_hash.clone(), tool_hash.clone())
        .is_empty());
    assert!(tracker.check(system_hash.clone(), tool_hash).is_empty());
    let reasons = tracker.check("changed".to_string(), "tools-changed".to_string());
    assert_eq!(
        reasons,
        vec!["system_prompt_changed", "tool_schemas_changed"]
    );
    assert_eq!(tracker.total_requests(), 3);
    assert_eq!(tracker.cache_breaks(), 1);
    assert_eq!(
        tracker.break_reasons(),
        vec![
            "system_prompt_changed".to_string(),
            "tool_schemas_changed".to_string()
        ]
    );
    assert!((tracker.cache_hit_rate() - (2.0 / 3.0)).abs() < f64::EPSILON);
}

#[test]
fn prompt_cache_hashes_match_stable_sorted_json_payloads() {
    let system_hash = hash_system_prompt_sections(&[json!({
        "id": "core",
        "text": " stable 文本 ",
        "stable": true,
    })]);
    assert_eq!(
        system_hash,
        "f4b5a29c78a21827a3d7591c5d01217bab73a285e4547044fc08ec81b0eec3f3"
    );

    let tool_hash = hash_tool_payload(&[json!({
        "name": "read_file",
        "input_schema": {"type": "object", "a": 1}
    })]);
    assert_eq!(
        tool_hash,
        "e90cd0abb1df2274146ffe58d025cfcc4e1fff2b6370f46c9b6e6e5972eecc70"
    );
}
