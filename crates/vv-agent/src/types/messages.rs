use serde::{Deserialize, Serialize};

use super::{Metadata, TokenUsage, ToolCall};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub reasoning_content: Option<String>,
    pub image_url: Option<String>,
    pub metadata: Metadata,
}

impl Message {
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            reasoning_content: None,
            image_url: None,
            metadata: Metadata::new(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::new(MessageRole::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(MessageRole::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(MessageRole::Assistant, content)
    }

    pub fn tool(content: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        let mut message = Self::new(MessageRole::Tool, content);
        message.tool_call_id = Some(tool_call_id.into());
        message
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LLMResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub raw: Metadata,
    pub token_usage: TokenUsage,
}

impl LLMResponse {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            tool_calls: Vec::new(),
            raw: Metadata::new(),
            token_usage: TokenUsage::default(),
        }
    }

    pub fn with_tool_calls(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            content: content.into(),
            tool_calls,
            raw: Metadata::new(),
            token_usage: TokenUsage::default(),
        }
    }
}
