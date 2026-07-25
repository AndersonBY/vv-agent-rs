use std::collections::BTreeMap;

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
        session_memory_enabled: true,
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
    let flat_prompt = bundle.flatten();
    assert!(flat_prompt.contains("<Agent Definition>\nYou are careful."));
    assert!(flat_prompt.contains("<Session Memory>"));
    assert!(flat_prompt.contains("<Tools>"));
    assert!(flat_prompt.contains("Ask the user only for a required decision"));
    assert!(flat_prompt.contains("agent_id=`reviewer`"));
    assert!(flat_prompt.contains("review-code"));
    assert!(flat_prompt.contains("task_finish"));
    assert!(flat_prompt.contains("<Current Time>"));
    assert!(flat_prompt.contains("2026-05-26T00:00:00Z"));
    assert!(flat_prompt.contains("Prefer specialized workspace tools for direct file operations"));
    assert_eq!(bundle.stable_hash.len(), 64);

    let section_ids = bundle
        .sections
        .iter()
        .map(|section| section.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        section_ids,
        vec![
            "agent_definition",
            "tools",
            "session_memory",
            "current_time"
        ]
    );
    assert!(!bundle.sections[2].stable);

    let prompt = build_system_prompt_with_options("You are careful.", options.clone());
    assert_eq!(prompt, flat_prompt);
    let sections = build_system_prompt_sections_with_options("You are careful.", options);
    assert_eq!(sections, bundle.sections);

    let raw = build_raw_system_prompt_sections("  raw system  ");
    assert_eq!(raw[0].id, "raw_system_prompt");
    assert_eq!(raw[0].text, "raw system");
    assert!(raw[0].stable);
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
    let flat_prompt = bundle.flatten();
    assert!(!flat_prompt.contains("<Session Memory>"));
    for forbidden in prompt_forbidden_terms() {
        assert!(
            !contains_forbidden_term(&flat_prompt, forbidden.as_str()),
            "model-visible system prompt should not include internal implementation wording `{forbidden}`:\n{}",
            flat_prompt
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
        forbidden_phrase(&[TERM_LANGUAGE]),
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
    let stable = PromptSection::new(" stable ", " stable body ", true)
        .source("agent.instructions")
        .cache_hint("ephemeral")
        .metadata("priority", json!(0));
    assert_eq!(stable.id, "stable");
    assert_eq!(stable.text, "stable body");
    assert_eq!(stable.source.as_deref(), Some("agent.instructions"));
    assert_eq!(stable.cache_hint.as_deref(), Some("ephemeral"));
    assert_eq!(stable.metadata["priority"], 0);

    let volatile = PromptSection::new("volatile", "volatile body", false);

    let mut builder = SystemPromptBuilder::default();
    builder.add_section(stable);
    builder.add_section(volatile);
    assert_eq!(builder.build(), "stable body\n\nvolatile body");
    let result = builder.build_result();
    assert_eq!(result.flatten(), "stable body\n\nvolatile body");
    assert_eq!(result.sections.len(), 2);
    assert_eq!(result.stable_hash.len(), 64);
    assert_eq!(
        serde_json::from_value::<vv_agent::prompt::PromptBundle>(result.to_value())
            .expect("strict prompt bundle round trip"),
        result
    );

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
