use serde_json::{json, Value};

pub(super) fn compress_memory_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "compress_memory",
            "description": "Store a durable memory note that should survive future compaction and help later turns continue accurately.\n\nUse this for stable decisions, constraints, file paths, user preferences, test evidence, or implementation facts that would be expensive or risky to rediscover. Do not store transient chatter or obvious information already encoded in the latest messages.",
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
