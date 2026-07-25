use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::constants::WORKSPACE_TOOLS;
use crate::skills::{normalize_skill_list, render_skills_xml, MAX_SKILLS_PROMPT_CHARS};

pub fn task_finish_prompt(language: &str) -> &'static str {
    match language {
        "zh-CN" => "可使用 task_finish 显式返回最终结果；若配置的 no-tool policy 允许，也可自然结束。",
        _ => "Use task_finish for an explicit final result. Natural completion is allowed when the configured no-tool policy permits it.",
    }
}

pub fn ask_user_prompt(language: &str) -> &'static str {
    match language {
        "zh-CN" => "只有缺少无法从上下文或可用工具中获得的必要决策时才询问用户。",
        _ => "Ask the user only for a required decision that cannot be resolved from context or available tools.",
    }
}

pub fn todo_prompt(language: &str) -> &'static str {
    match language {
        "zh-CN" => "多步骤工作中，同一时间只保留一个进行中的 TODO。",
        _ => "For multi-step work, keep the TODO state current with at most one item in progress.",
    }
}

pub fn tool_priority_prompt(language: &str) -> &'static str {
    match language {
        "zh-CN" => "直接操作文件时优先使用工作区专用工具；仅在专用工具不足时使用 bash。",
        _ => "Prefer specialized workspace tools for direct file operations; use bash when they are insufficient.",
    }
}

pub fn computer_agent_env_prompt(language: &str) -> String {
    let os = os_label();
    match language {
        "zh-CN" => format!("你运行在 {os} 工作区环境中, 可以用工具读取, 搜索, 修改文件."),
        _ => format!(
            "You are running in a {os} workspace environment and can use tools to inspect and modify files."
        ),
    }
}

pub fn current_time_prompt(language: &str) -> &'static str {
    match language {
        "zh-CN" => "任务开始时的真实 UTC 时间:",
        _ => "Actual task start time (UTC):",
    }
}

pub fn render_workspace_tools(language: &str) -> String {
    let tools = WORKSPACE_TOOLS.join(", ");
    match language {
        "zh-CN" => format!("你可以使用这些工具操作工作区文件: {tools}."),
        _ => format!("You can operate workspace files with tools: {tools}."),
    }
}

pub fn render_sub_agents(
    language: &str,
    available_sub_agents: &BTreeMap<String, String>,
) -> String {
    let header = match language {
        "zh-CN" => "已配置的子 Agent：",
        _ => "Configured sub-agents:",
    };
    let mut lines = vec![header.to_string()];
    for (name, description) in available_sub_agents {
        lines.push(format!("- agent_id=`{name}`: {description}"));
    }
    lines.join("\n")
}

pub fn render_available_skills(
    language: &str,
    available_skills: &Value,
    workspace: Option<&Path>,
) -> String {
    let header = if language == "zh-CN" {
        "可用技能元数据 (Agent Skills 标准格式):"
    } else {
        "Available skills metadata (Agent Skills format):"
    };
    let entries = normalize_skill_list(Some(available_skills), workspace, false);
    if entries.is_empty() {
        return String::new();
    }
    format!(
        "{header}\n{}",
        render_skills_xml(&entries, MAX_SKILLS_PROMPT_CHARS)
    )
}

fn os_label() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    }
}
