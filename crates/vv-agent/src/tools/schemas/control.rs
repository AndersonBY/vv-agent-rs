use serde_json::{json, Value};

const TASK_FINISH_DESCRIPTION: &str = r#"When task goals are fully complete, call this tool to end the task and return final message.

Finish the current task and return the final response.

When to use:
- Only call this when the user's requested work is genuinely complete, verified, and no unfinished TODO remains.
- Use it after implementation, review, test output, and any required artifact paths are ready to report.
- Use `exposed_files` to list concrete deliverables the user should inspect.

Completion protocol:
- Do not call this tool if work is partially complete, blocked, waiting for user input, or still needs verification.
- If `todo_write` has pending or in-progress work, the runtime rejects premature finish by default.
- The message should include concise outcome, important verification evidence, and any remaining caveats."#;

const ASK_USER_DESCRIPTION: &str = r#"Pause execution and ask the user for required clarification or decision.

When to use:
- The task cannot be completed safely because a real user preference, permission, credential, destructive action, or ambiguous scope decision is missing.
- A reasonable default would risk doing the wrong work or violating the user's stated constraints.
- Multiple clear options exist and user choice changes the implementation or operational outcome.

Do not use this for facts you can discover with available tools, files, command output, documentation, or local configuration. This blocks the runtime until the user responds, so keep the question concrete, include 2-3 options when possible, and ask only for the decision needed to proceed."#;

pub(super) fn task_finish_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "task_finish",
            "description": TASK_FINISH_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "Final response shown to user. Include the result, important verification evidence, and any remaining caveats."
                    },
                    "exposed_files": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional workspace-relative paths for created or modified deliverables the user should inspect. Include concrete artifact paths, not transient logs, prose descriptions, or unrelated files."
                    }
                },
                "required": []
            }
        }
    })
}

pub(super) fn ask_user_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "ask_user",
            "description": ASK_USER_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "Question text to ask the user. Ask the smallest decision needed to unblock progress, include relevant context, and avoid bundling unrelated questions."
                    },
                    "options": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional answer options shown to the user. Prefer 2-3 concise, mutually exclusive choices when the decision has clear outcomes."
                    },
                    "selection_type": {
                        "type": "string",
                        "enum": ["single", "multi"],
                        "description": "Single or multi-choice mode when options are provided. Use `multi` only when several choices may validly apply at the same time."
                    },
                    "allow_custom_options": {
                        "type": "boolean",
                        "description": "Whether users can add custom options. Set true when preset options may be incomplete or the user may need to provide a custom path, credential label, or preference."
                    }
                },
                "required": ["question"]
            }
        }
    })
}

pub(super) fn activate_skill_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "activate_skill",
            "description": "Activate a skill from the current task's available skill list.\n\nWhen to use:\n- A listed skill directly applies to the current task, workflow, domain, or required process discipline.\n- The skill may contain repository-specific instructions, validation steps, tool constraints, or templates that should guide the next action.\n\nProtocol:\n- Use this tool only for skills explicitly listed in <available_skills>.\n- Do not invent skill names or activate unrelated skills.\n- Read the returned SKILL.md instructions before acting, then follow any mandatory workflow.\n\nThe skill metadata follows the Agent Skills specification (https://github.com/agentskills/agentskills): name/description are exposed in <available_skills>, and skill instructions are loaded from SKILL.md when location is provided.",
            "parameters": {
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "Skill identifier from available skill list. The exact `name` from the available skill list. Do not pass a path, title, or inferred alias."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Optional reason for activating this skill. Explain briefly why this skill applies before acting."
                    }
                },
                "required": ["skill_name"]
            }
        }
    })
}
