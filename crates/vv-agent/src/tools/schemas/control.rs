use serde_json::{json, Value};

const ASK_USER_DESCRIPTION: &str = "Pause for a required user decision that cannot be discovered safely with available tools. Ask one concrete question and provide concise options when useful.";

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
