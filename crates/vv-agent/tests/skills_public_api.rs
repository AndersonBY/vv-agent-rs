use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::{json, Value};
use vv_agent::skills::{
    discover_skill_dirs, find_skill_md, metadata_to_prompt_entries, normalize_skill_list,
    normalize_validation_mode, parse_frontmatter, read_properties, read_skill, render_skills_xml,
    to_available_skills_xml, validate_metadata, validate_metadata_with_diagnostics, SkillEntry,
    SkillProperties, ValidationMode,
};

#[test]
fn skills_public_api_validates_metadata_like_python() {
    assert_eq!(
        normalize_validation_mode(Some("compat")).expect("mode"),
        ValidationMode::Compat
    );
    assert!(normalize_validation_mode(Some("unknown")).is_err());

    let metadata = BTreeMap::from([
        ("name".to_string(), Value::String("Bad_Name".to_string())),
        (
            "description".to_string(),
            Value::String("Useful skill".to_string()),
        ),
        ("unexpected".to_string(), json!(true)),
    ]);
    let diagnostics = validate_metadata_with_diagnostics(
        &metadata,
        Some(Path::new("different-dir")),
        Some("strict"),
    )
    .expect("diagnostics");

    assert!(diagnostics
        .errors
        .iter()
        .any(|error| error.contains("Unexpected fields")));
    assert!(diagnostics
        .errors
        .iter()
        .any(|error| error.contains("invalid characters")));
    assert!(diagnostics
        .errors
        .iter()
        .any(|error| error.contains("Directory name")));

    let minimal_errors = validate_metadata(&metadata, None, Some("minimal")).expect("minimal");
    assert!(minimal_errors
        .iter()
        .any(|error| error.contains("invalid characters")));
}

#[test]
fn skills_public_api_renders_available_skills_xml_with_budget() {
    let entry = SkillEntry {
        name: "review-code".to_string(),
        description: "Review <code> safely".to_string(),
        location: Some("skills/review-code/SKILL.md".to_string()),
        ..SkillEntry::default()
    };
    let xml = render_skills_xml(std::slice::from_ref(&entry), 8000);

    assert!(xml.contains("<available_skills>"));
    assert!(xml.contains("review-code"));
    assert!(xml.contains("Review &lt;code&gt; safely"));
    assert!(xml.contains("skills/review-code/SKILL.md"));

    let compact = render_skills_xml(&[entry], 120);
    assert!(compact.contains("<available_skills>"));
    assert!(!compact.contains("<location>"));

    let properties = SkillProperties {
        name: "review-code".to_string(),
        description: "Review code".to_string(),
        license: Some("MIT".to_string()),
        compatibility: Some("vv-agent".to_string()),
        allowed_tools: Some("read_file".to_string()),
        metadata: BTreeMap::from([("owner".to_string(), "agent".to_string())]),
    };
    let payload = properties.to_value();
    assert_eq!(payload["allowed-tools"], "read_file");
    assert_eq!(payload["metadata"]["owner"], "agent");
}

#[test]
fn skills_public_api_parses_and_loads_skill_dirs_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let skill_dir = workspace.path().join("skills/review-code");
    fs::create_dir_all(&skill_dir).expect("skill dir");
    let skill_md = skill_dir.join("SKILL.md");
    let content = r#"---
name: review-code
description: Review code safely
license: MIT
compatibility: vv-agent
allowed-tools: read_file, workspace_grep
metadata:
  owner: agent
  retries: 2
---
Use these instructions.
"#;
    fs::write(&skill_md, content).expect("write skill");

    assert_eq!(find_skill_md(&skill_dir).expect("skill md"), skill_md);
    let discovered = discover_skill_dirs(workspace.path().join("skills"));
    assert_eq!(
        discovered,
        vec![skill_dir.canonicalize().expect("canonical")]
    );

    let (metadata, body) = parse_frontmatter(content).expect("frontmatter");
    assert_eq!(metadata["name"], "review-code");
    assert_eq!(metadata["metadata"]["retries"], "2");
    assert_eq!(body, "Use these instructions.");

    let properties = read_properties(&skill_dir).expect("properties");
    assert_eq!(properties.name, "review-code");
    assert_eq!(properties.license.as_deref(), Some("MIT"));
    assert_eq!(
        properties.allowed_tools.as_deref(),
        Some("read_file, workspace_grep")
    );
    assert_eq!(properties.metadata["owner"], "agent");
    assert_eq!(properties.metadata["retries"], "2");

    let loaded = read_skill(&skill_dir, Some("strict")).expect("loaded skill");
    assert_eq!(loaded.name(), "review-code");
    assert_eq!(
        loaded.skill_md_path,
        skill_md.canonicalize().expect("canonical skill")
    );
    assert_eq!(loaded.instructions, "Use these instructions.");
    assert!(loaded.warnings.is_empty());

    let xml = to_available_skills_xml(&[skill_dir]).expect("skills xml");
    assert!(xml.contains("<available_skills>"));
    assert!(xml.contains("review-code"));
    assert!(xml.contains("SKILL.md"));
}

#[test]
fn skills_public_api_normalizes_mixed_skill_metadata_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let skill_dir = workspace.path().join("skills/review-code");
    fs::create_dir_all(&skill_dir).expect("skill dir");
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: review-code
description: Review code safely
metadata:
  owner: agent
---
Loaded instructions.
"#,
    )
    .expect("write skill");

    let raw_skills = json!([
        "skills",
        {
            "name": "inline-skill",
            "description": "Inline description",
            "allowed_tools": "bash",
            "metadata": {"count": 2}
        },
        {
            "name": "inline-skill",
            "description": "Duplicate should be ignored"
        },
        {
            "location": "skills/review-code"
        }
    ]);

    let entries = normalize_skill_list(Some(&raw_skills), Some(workspace.path()), false);
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["review-code", "inline-skill"]
    );
    assert_eq!(
        entries[0].location.as_deref(),
        Some("skills/review-code/SKILL.md")
    );
    assert!(entries[0].instructions.is_none());
    assert_eq!(entries[1].allowed_tools.as_deref(), Some("bash"));
    assert_eq!(entries[1].metadata["count"], "2");

    let loaded_entries = normalize_skill_list(
        Some(&json!([{
            "name": "review-code",
            "description": "ignored",
            "location": "skills/review-code"
        }])),
        Some(workspace.path()),
        true,
    );
    assert_eq!(
        loaded_entries[0].instructions.as_deref(),
        Some("Loaded instructions.")
    );

    let prompt_entries = metadata_to_prompt_entries(Some(&raw_skills), Some(workspace.path()));
    assert_eq!(prompt_entries.len(), 2);
    assert_eq!(prompt_entries[0].name, "review-code");
}

#[test]
fn skills_public_api_stringifies_inline_scalar_fields_like_python() {
    let raw_skills = json!([
        {
            "name": 123,
            "description": 456,
            "instructions": 789,
            "compatibility": true,
            "metadata": {
                "priority": 5,
                "enabled": true,
                "missing": null,
                "tags": ["x", 2, true]
            }
        },
        {
            "name": 0,
            "description": "Falsy Python name should be skipped"
        }
    ]);

    let entries = normalize_skill_list(Some(&raw_skills), None, true);

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "123");
    assert_eq!(entries[0].description, "456");
    assert_eq!(entries[0].instructions.as_deref(), Some("789"));
    assert_eq!(entries[0].compatibility.as_deref(), Some("True"));
    assert_eq!(entries[0].metadata["priority"], "5");
    assert_eq!(entries[0].metadata["enabled"], "True");
    assert_eq!(entries[0].metadata["missing"], "None");
    assert_eq!(entries[0].metadata["tags"], "['x', 2, True]");
}
