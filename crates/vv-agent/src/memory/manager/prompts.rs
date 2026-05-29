use crate::types::Message;

const MEMORY_WARNING_EN: &str = "The current memory usage has exceeded {memory_threshold_percentage}%. It is recommended to immediately organize and record key information and materials from the conversation, and store them in the workspace to prevent data loss after memory compression.\n\n";
const MEMORY_WARNING_ZH: &str = "当前记忆已使用容量超过 {memory_threshold_percentage}%,建议立即整理、记录对话中的关键信息、资料, 并储存至工作区, 避免记忆压缩后资料丢失。\n\n";
const COMPRESS_MEMORY_PROMPT_EN: &str = r#"You are summarizing a conversation between a user and an AI coding assistant.
Provide your analysis in <analysis> tags first (this section will be stripped), then output a structured JSON summary.

<analysis>
Think step by step about what information is critical to preserve, especially the user's exact wording,
the current work state, file operations, and any errors that were resolved.
</analysis>

<Conversation History>
{messages}
</Conversation History>

Please compress the conversation into a structured JSON "Task Status Summary".
This summary should allow the Agent to quickly resume the task
while preserving user constraints, key decisions, file operations, and critical context.

Requirements:
- Output JSON only, no Markdown.
- Keep fields concise and searchable; use short sentences.
- If a field has no data, use [] or "" as appropriate.
- The "original_user_messages" field is critical. Preserve user messages verbatim or near-verbatim.

JSON Schema:
{
  "summary_version": "2.0",
  "original_user_messages": ["..."],
  "user_constraints": ["..."],
  "decisions": ["..."],
  "files_examined_or_modified": [
    {"path": "...", "action": "read|created|modified|deleted", "summary": "..."}
  ],
  "errors_and_fixes": [
    {"error": "...", "fix": "...", "file": "..."}
  ],
  "progress": ["Preserve up to {event_limit} critical events"],
  "key_facts": ["..."],
  "open_issues": ["..."],
  "current_work_state": "...",
  "next_steps": ["..."]
}
"#;
const COMPRESS_MEMORY_PROMPT_ZH: &str = r#"你正在总结一段用户与 AI 编程助手的对话。
请先在 <analysis> 标签中进行思考 (该部分后续会被剥离), 然后输出结构化 JSON 摘要。

<analysis>
请逐步思考: 哪些信息必须保留, 哪些用户原话不能丢, 哪些文件/错误/当前状态会影响后续继续执行。
</analysis>

<Conversation History>
{messages}
</Conversation History>

请将以上对话压缩为结构化 JSON「Task Status Summary」, 让 Agent 能快速恢复任务, 并保留用户约束、关键决策、文件操作与当前工作状态。

要求:
- 只输出 JSON, 不要 Markdown。
- 字段内容简洁、可检索, 短句表达。
- 没有信息的字段使用 [] 或 ""。
- `original_user_messages` 字段至关重要: 尽量保留用户原话, 不要做概括式改写。

JSON Schema:
{
  "summary_version": "2.0",
  "original_user_messages": ["..."],
  "user_constraints": ["..."],
  "decisions": ["..."],
  "files_examined_or_modified": [
    {"path": "...", "action": "read|created|modified|deleted", "summary": "..."}
  ],
  "errors_and_fixes": [
    {"error": "...", "fix": "...", "file": "..."}
  ],
  "progress": ["最多保留 {event_limit} 条关键进展"],
  "key_facts": ["..."],
  "open_issues": ["..."],
  "current_work_state": "...",
  "next_steps": ["..."]
}
"#;

pub(super) fn memory_warning_text(language: &str, warning_threshold_percentage: u8) -> String {
    let template = if language == "zh-CN" {
        MEMORY_WARNING_ZH
    } else {
        MEMORY_WARNING_EN
    };
    template.replace(
        "{memory_threshold_percentage}",
        &warning_threshold_percentage.to_string(),
    )
}

pub(super) fn build_compress_memory_prompt(
    language: &str,
    summary_event_limit: usize,
    messages: &[Message],
) -> String {
    let template = if language == "zh-CN" {
        COMPRESS_MEMORY_PROMPT_ZH
    } else {
        COMPRESS_MEMORY_PROMPT_EN
    };
    let serialized_messages = messages
        .iter()
        .map(|message| message.to_openai_message(true))
        .collect::<Vec<_>>();
    template
        .replace(
            "{messages}",
            &serde_json::to_string(&serialized_messages).unwrap_or_default(),
        )
        .replace("{event_limit}", &summary_event_limit.max(1).to_string())
}
