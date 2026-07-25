use serde_json::json;
use vv_agent::llm::{apply_claude_prompt_cache, CACHE_CONTROL_EPHEMERAL, PROMPT_CACHE_ENABLED_KEY};
use vv_agent::prompt::{PromptBundle, PromptSection};

fn prompt_bundle(text: impl Into<String>) -> PromptBundle {
    PromptBundle::new(vec![PromptSection::new("core_identity", text, true)]).expect("prompt bundle")
}

#[test]
fn claude_prompt_cache_exports_agent_ephemeral_cache_control() {
    assert_eq!(CACHE_CONTROL_EPHEMERAL(), json!({"type": "ephemeral"}));
}

#[test]
fn claude_prompt_cache_vertex_marks_history_boundary_and_skips_thinking() {
    let prompt_bundle = prompt_bundle("stable section ".repeat(400));
    let (planned_messages, planned_tools, planned_extra_body) = apply_claude_prompt_cache(
        "anthropic_vertex",
        "claude-sonnet-4-5-20250929",
        &[
            json!({"role": "system", "content": "sys"}),
            json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "assistant reply"},
                    {"type": "thinking", "thinking": "private chain"}
                ]
            }),
            json!({"role": "user", "content": "latest user turn ".repeat(300)}),
        ],
        &[],
        None,
        &prompt_bundle,
        Some(&json!({PROMPT_CACHE_ENABLED_KEY: true})),
    );

    assert_eq!(planned_extra_body, None);
    assert!(planned_tools.is_empty());
    assert_eq!(
        planned_messages[0]["content"]
            .as_array()
            .expect("system blocks")
            .last()
            .expect("system cache block")["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(planned_messages[1]["content"][0].get("cache_control"), None);
    assert_eq!(planned_messages[1]["content"][1].get("cache_control"), None);
    assert_eq!(
        planned_messages[2]["content"]
            .as_array()
            .expect("history blocks")
            .last()
            .expect("history cache block")["cache_control"],
        json!({"type": "ephemeral"})
    );
}

#[test]
fn claude_prompt_cache_uses_sonnet_4_6_threshold() {
    let prompt_bundle = prompt_bundle("stable system ".repeat(350));
    let (planned_messages, planned_tools, planned_extra_body) = apply_claude_prompt_cache(
        "anthropic",
        "claude-sonnet-4-6",
        &[
            json!({"role": "system", "content": "stable system ".repeat(350)}),
            json!({"role": "user", "content": "latest user turn ".repeat(40)}),
        ],
        &[],
        None,
        &prompt_bundle,
        Some(&json!({PROMPT_CACHE_ENABLED_KEY: true})),
    );

    assert_eq!(planned_extra_body, None);
    assert!(planned_tools.is_empty());
    assert_eq!(
        planned_messages[0]["content"]
            .as_array()
            .expect("system blocks")
            .last()
            .expect("system cache block")["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(
        planned_messages[1]["content"]
            .as_array()
            .expect("history blocks")
            .last()
            .expect("history cache block")["cache_control"],
        json!({"type": "ephemeral"})
    );
}

#[test]
fn claude_prompt_cache_is_skipped_for_disabled_or_non_claude_requests() {
    let messages = vec![json!({"role": "system", "content": "stable system ".repeat(350)})];
    let tools = vec![json!({"type": "function", "function": {"name": "search_docs"}})];
    let extra_body = json!({"extra_body": {"trace": true}});
    let prompt_bundle = prompt_bundle("stable system ".repeat(350));

    let (planned_messages, planned_tools, planned_extra_body) = apply_claude_prompt_cache(
        "anthropic",
        "claude-sonnet-4-6",
        &messages,
        &tools,
        Some(&extra_body),
        &prompt_bundle,
        Some(&json!({PROMPT_CACHE_ENABLED_KEY: false})),
    );

    assert_eq!(planned_messages, messages);
    assert_eq!(planned_tools, tools);
    assert_eq!(planned_extra_body, Some(extra_body.clone()));

    let (planned_messages, planned_tools, planned_extra_body) = apply_claude_prompt_cache(
        "openai",
        "gpt-5",
        &messages,
        &tools,
        Some(&extra_body),
        &prompt_bundle,
        None,
    );

    assert_eq!(planned_messages, messages);
    assert_eq!(planned_tools, tools);
    assert_eq!(planned_extra_body, Some(extra_body));
}
