use serde_json::{json, Value};

pub(super) fn task_finish_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "task_finish",
            "description": "Finish the current task and return the final response.\n\nOnly call this when the user's requested work is genuinely complete, verified, and no unfinished TODO remains. If work is partially complete, blocked, waiting for user input, or still needs verification, do not call this tool.\n\nUse `exposed_files` to list concrete deliverables the user should inspect.",
            "parameters": {
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "Final response shown to the user. Include the result, important verification evidence, and any remaining caveats."
                    },
                    "exposed_files": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional output file paths that should be exposed as final deliverables."
                    },
                    "require_all_todos_completed": {
                        "type": "boolean",
                        "description": "Keep true unless explicitly finishing with known incomplete TODOs. When true, the runtime rejects finishing if todo_write still has pending or in_progress items."
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
            "description": "Pause execution and ask the user for required clarification or a decision.\n\nUse this only when you cannot safely choose a reasonable default from the current context. Keep the question concrete and include options when the decision space is clear.",
            "parameters": {
                "type": "object",
                "properties": {
                    "question": {"type": "string", "description": "Question text to ask the user."},
                    "options": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional answer options shown to the user."
                    },
                    "selection_type": {
                        "type": "string",
                        "enum": ["single", "multi"],
                        "description": "Single or multi-choice mode when options are provided."
                    },
                    "allow_custom_options": {
                        "type": "boolean",
                        "description": "Whether users can add custom options."
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
            "description": "Activate a skill from the current task's available skill list.\n\nThe skill metadata follows the Agent Skills specification (https://github.com/agentskills/agentskills):\n- name/description are exposed in <available_skills>\n- skill instructions are loaded from SKILL.md when location is provided\n\nUse this tool only for skills explicitly listed in <available_skills>.",
            "parameters": {
                "type": "object",
                "properties": {
                    "skill_name": {"type": "string", "description": "Skill identifier from available skill list."},
                    "reason": {"type": "string", "description": "Optional reason for activating this skill."}
                },
                "required": ["skill_name"]
            }
        }
    })
}
