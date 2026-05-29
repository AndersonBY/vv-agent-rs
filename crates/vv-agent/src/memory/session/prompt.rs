use crate::types::{Message, MessageRole};

pub(super) fn build_extraction_prompt(messages: &[&Message]) -> String {
    let serialized_messages = messages
        .iter()
        .map(|message| message_to_text(message))
        .collect::<Vec<_>>();
    format!(
        "Analyze the following conversation messages and extract durable facts that should survive context compression.\n\n\
Categories:\n\
- user_intent: goals, constraints, preferences, explicit asks\n\
- decision: decisions or chosen approaches\n\
- file_change: files created/modified/deleted and why\n\
- error_fix: failures and their resolutions\n\
- key_fact: other important context that should not be forgotten\n\n\
Requirements:\n\
- Return JSON array only.\n\
- Keep each content field concise and deduplicatable.\n\
- Skip transient chatter and repeated information.\n\
- importance is 1-10 where 10 means critical.\n\n\
Output format:\n\
[{example}]\n\n\
Messages:\n{}",
        serde_json::to_string_pretty(&serialized_messages).unwrap_or_default(),
        example = r#"{"category":"...", "content":"...", "importance": 5}"#
    )
}

pub(super) fn should_skip_message(message: &Message) -> bool {
    message.role == MessageRole::System
        || (message.role == MessageRole::User
            && message.content.contains("<Compressed Agent Memory>"))
}

fn message_to_text(message: &Message) -> serde_json::Value {
    let role = match message.role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    };
    let mut object = serde_json::Map::new();
    object.insert("role".to_string(), serde_json::json!(role));
    object.insert(
        "content".to_string(),
        serde_json::json!(compact_long_content(&message.content)),
    );
    if let Some(name) = &message.name {
        object.insert("name".to_string(), serde_json::json!(name));
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        object.insert("tool_call_id".to_string(), serde_json::json!(tool_call_id));
    }
    if !message.tool_calls.is_empty() {
        object.insert(
            "tool_calls".to_string(),
            serde_json::to_value(&message.tool_calls).unwrap_or(serde_json::Value::Null),
        );
    }
    serde_json::Value::Object(object)
}

fn compact_long_content(content: &str) -> String {
    if content.len() <= 2_000 {
        return content.to_string();
    }
    let head = content.chars().take(1_200).collect::<String>();
    let tail = content
        .chars()
        .rev()
        .take(400)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{head}\n...[truncated]...\n{tail}")
}
