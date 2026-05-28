use serde_json::{json, Value};

const COMPRESS_MEMORY_DESCRIPTION: &str = r#"Store key summary notes to reduce future context load.

Store a durable memory note that should survive future compaction.

When to use:
- Preserve stable decisions, constraints, file paths, API names, test evidence, user preferences, or implementation facts that would be expensive or risky to rediscover.
- Capture facts needed by later turns after a long investigation, a live incident, or a multi-step implementation.
- Use this before context compaction when losing a detail would make the Agent repeat work or make an unsafe assumption.

Good memory notes:
- Include concrete names, paths, commands, identifiers, model names, error text, and final decisions.
- State whether the information is verified current state or only an inference.
- Keep it short enough to be useful but specific enough to resume work.

Do not store transient chatter, obvious facts already present in the latest messages, speculation without labels, secrets, or disposable command output."#;

pub(super) fn compress_memory_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "compress_memory",
            "description": COMPRESS_MEMORY_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "core_information": {"type": "string", "description": "Key information that should be preserved after compression. Include concrete names, paths, commands, and decisions when relevant."}
                },
                "required": ["core_information"]
            }
        }
    })
}
