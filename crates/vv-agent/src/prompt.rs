use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct PromptSection {
    pub title: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BuiltSystemPrompt {
    pub prompt: String,
    pub sections: Vec<Value>,
}

pub fn build_system_prompt_bundle(base: impl Into<String>) -> BuiltSystemPrompt {
    let prompt = base.into();
    BuiltSystemPrompt {
        prompt: prompt.clone(),
        sections: vec![json!({ "title": "base", "content": prompt })],
    }
}

pub fn build_raw_system_prompt_sections(system_prompt: impl Into<String>) -> Vec<Value> {
    vec![json!({
        "title": "system",
        "content": system_prompt.into(),
    })]
}
