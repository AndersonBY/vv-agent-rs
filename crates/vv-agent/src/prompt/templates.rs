use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::skills::{normalize_skill_list, render_skills_xml, MAX_SKILLS_PROMPT_CHARS};

pub fn task_finish_prompt(language: &str) -> &'static str {
    match language {
        "zh-CN" => "当你确认任务完成时, 必须调用 `task_finish`, 并在 `message` 字段里给出面向用户的最终结果.",
        _ => "When you confirm task completion, you must call `task_finish` and put the final user-facing result in its `message` field.",
    }
}

pub fn ask_user_prompt(language: &str) -> &'static str {
    match language {
        "zh-CN" => "当你需要用户补充信息或做出选择时, 调用 `ask_user`. 你可以提供 options 并设置选择模式.",
        _ => "When you need clarification or decision from the user, call `ask_user`. You may provide options and set selection mode.",
    }
}

pub fn todo_prompt(language: &str) -> &'static str {
    match language {
        "zh-CN" => "多步骤任务请使用 `todo_write` 管理任务清单并及时更新状态, 任意时刻仅保留一个 in_progress.",
        _ => "Use `todo_write` for multi-step tasks and keep progress updated. Only one item should be in progress at a time.",
    }
}

pub fn tool_priority_prompt(language: &str) -> &'static str {
    match language {
        "zh-CN" => {
            "工具优先级: 优先使用专用工具而不是 shell. 读取用 `read_file`, 写入用 `write_file`, 编辑用 `edit_file`, 搜索用 `workspace_grep`. 仅在专用工具不足时使用 `bash`."
        }
        _ => {
            "Tool priority: prefer specialized tools over shell commands. Read with `read_file`, write with `write_file`, edit with `edit_file`, search with `workspace_grep`. Use `bash` only when specialized tools are insufficient."
        }
    }
}

pub fn computer_agent_env_prompt(language: &str) -> String {
    let os = os_label();
    match language {
        "zh-CN" => format!("{os} 工作区环境中, 可以用工具读取, 搜索, 修改文件."),
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
    let tools = [
        "list_files",
        "file_info",
        "read_file",
        "write_file",
        "edit_file",
        "workspace_grep",
        "read_image",
        "bash",
        "check_background_command",
    ]
    .join(", ");
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
        "zh-CN" => {
            "如果已配置子 Agent, 可使用 `create_sub_task` 委派任务: 用 `agent_id` 指定目标子 Agent, 单任务用 `task_description`, 同一子 Agent 的并行任务用 `tasks`, 后台执行用 `wait_for_completion=false`; 需要查询进度或追加消息时使用 `sub_task_status`。如果后台任务完成前主任务无法继续, 请调用 `sub_task_status` 并设置 `wait_for_completion=true` 和较长的 `check_interval_seconds`, 不要连续轮询。"
        }
        _ => {
            "If sub-agents are configured, delegate work with `create_sub_task`. Use `agent_id` to select the target sub-agent, `task_description` for one task, `tasks` for multiple independent tasks of the same sub-agent, `wait_for_completion=false` for background execution, and `sub_task_status` to query progress or send follow-up messages. If background work must finish before you can continue, call `sub_task_status` with `wait_for_completion=true` and a longer `check_interval_seconds` instead of repeatedly polling."
        }
    };
    let list_header = if language == "zh-CN" {
        "可用子 Agent 列表 (调用时请直接使用下列 agent_id):"
    } else {
        "Available sub-agents (use the agent_id exactly as shown):"
    };
    let mut lines = vec![header.to_string(), list_header.to_string()];
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
