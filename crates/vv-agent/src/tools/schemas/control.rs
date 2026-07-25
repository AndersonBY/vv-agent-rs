use serde_json::{json, Value};

const TASK_FINISH_DESCRIPTION: &str = "Explicitly finish the run with a user-facing result when the requested work is complete. This tool is optional when the configured no-tool policy allows natural completion; the runtime still enforces unfinished-TODO checks.";

const ASK_USER_DESCRIPTION: &str = "Pause for a required user decision that cannot be discovered safely with available tools. Ask one concrete question and provide concise options when useful.";

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
                        "description": "Final user-facing result."
                    },
                    "require_all_todos_completed": {
                        "type": "boolean",
                        "description": "Reject finish while TODOs remain unless false."
                    },
                    "exposed_files": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Workspace-relative deliverable paths to expose to the user."
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
                        "description": "Question text to ask the user."
                    },
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
            "description": "Load one skill listed in the current available-skills metadata. Use the exact skill name and follow the returned instructions; unlisted names are rejected.",
            "parameters": {
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "Skill identifier from available skill list."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Optional reason for activating this skill."
                    }
                },
                "required": ["skill_name"]
            }
        }
    })
}
