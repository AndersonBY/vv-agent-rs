pub mod errors;
pub mod models;
pub mod normalize;
pub mod parser;
pub mod prompt;
pub mod validator;

pub use errors::{SkillError, SkillParseError, SkillValidationError};
pub use models::{LoadedSkill, SkillEntry, SkillProperties};
pub use normalize::normalize_skill_list;
pub use parser::{
    discover_skill_dirs, find_skill_md, parse_frontmatter, read_properties, read_skill,
};
pub use prompt::{
    metadata_to_prompt_entries, render_skills_xml, skill_entry_to_xml, to_available_skills_xml,
    MAX_SKILLS_PROMPT_CHARS,
};
pub use validator::{
    normalize_validation_mode, validate, validate_metadata, validate_metadata_with_diagnostics,
    validate_with_diagnostics, ValidationDiagnostics, ValidationMode, DEFAULT_VALIDATION_MODE,
    VALIDATION_MODES,
};
