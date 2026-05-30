use std::collections::BTreeMap;

use crate::types::{Message, MessageRole};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ToolCallInfo {
    pub tool_name: Option<String>,
    pub arguments: Option<String>,
}

pub(super) fn build_tool_call_info(messages: &[Message]) -> BTreeMap<String, ToolCallInfo> {
    let mut info = BTreeMap::new();
    for message in messages {
        if message.role != MessageRole::Assistant {
            continue;
        }
        for tool_call in &message.tool_calls {
            let arguments = serde_json::to_string(&tool_call.arguments).ok();
            info.insert(
                tool_call.id.clone(),
                ToolCallInfo {
                    tool_name: Some(tool_call.name.clone()),
                    arguments,
                },
            );
        }
    }
    info
}
