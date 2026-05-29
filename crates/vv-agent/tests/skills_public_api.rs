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
fn skills_public_api_validates_metadata() {
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

    let typed_metadata = BTreeMap::from([
        ("name".to_string(), json!(123)),
        ("description".to_string(), json!(false)),
    ]);
    let typed_errors =
        validate_metadata(&typed_metadata, None, Some("strict")).expect("typed validation");
    assert!(typed_errors
        .iter()
        .any(|error| error == "Field 'name' must be a non-empty string"));
    assert!(typed_errors
        .iter()
        .any(|error| error == "Field 'description' must be a non-empty string"));

    let i18n_uppercase = BTreeMap::from([
        ("name".to_string(), Value::String("Мой-навык".to_string())),
        (
            "description".to_string(),
            Value::String("Useful skill".to_string()),
        ),
    ]);
    let i18n_errors = validate_metadata(
        &i18n_uppercase,
        Some(Path::new("Мой-навык")),
        Some("strict"),
    )
    .expect("i18n validation");
    assert!(
        i18n_errors
            .iter()
            .any(|error| error.contains("must be lowercase")),
        "non-ASCII uppercase names should be validated with Unicode lowercase semantics"
    );
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

    let entry_with_metadata = SkillEntry {
        name: "metadata-skill".to_string(),
        description: "Metadata should stay hidden".to_string(),
        location: Some("skills/metadata-skill/SKILL.md".to_string()),
        allowed_tools: Some("read_file".to_string()),
        metadata: BTreeMap::from([("owner".to_string(), "agent".to_string())]),
        ..SkillEntry::default()
    };
    let xml = render_skills_xml(&[entry_with_metadata], 8000);
    assert!(xml.contains("metadata-skill"));
    assert!(
        !xml.contains("owner") && !xml.contains("agent"),
        "<available_skills> is model-visible and should not expose internal metadata"
    );
    assert!(
        !xml.contains("allowed_tools") && !xml.contains("<metadata>"),
        "<available_skills> should stay focused on name, description, and location"
    );

    let properties = SkillProperties {
        name: "review-code".to_string(),
        description: "Review code".to_string(),
        license: Some("MIT".to_string()),
        compatibility: None,
        allowed_tools: Some("read_file".to_string()),
        metadata: BTreeMap::from([("owner".to_string(), "agent".to_string())]),
    };
    let payload = properties.to_value();
    assert_eq!(payload["allowed-tools"], "read_file");
    assert_eq!(payload["metadata"]["owner"], "agent");
}

#[test]
fn skills_public_api_parses_and_loads_skill_dirs() {
    let workspace = tempfile::tempdir().expect("workspace");
    let skill_dir = workspace.path().join("skills/review-code");
    fs::create_dir_all(&skill_dir).expect("skill dir");
    let skill_md = skill_dir.join("SKILL.md");
    let content = r#"---
name: review-code
description: Review code safely
license: MIT
allowed-tools: read_file, workspace_grep
metadata:
  owner: agent
  retries: 2
  enabled: true
  missing: null
  tags:
    - x
    - 2
    - true
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
    assert_eq!(metadata["metadata"]["enabled"], "True");
    assert_eq!(metadata["metadata"]["missing"], "None");
    assert_eq!(metadata["metadata"]["tags"], "['x', 2, True]");
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
    assert_eq!(properties.metadata["enabled"], "True");
    assert_eq!(properties.metadata["missing"], "None");
    assert_eq!(properties.metadata["tags"], "['x', 2, True]");

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
fn skills_public_api_reads_only_canonical_frontmatter_tool_field() {
    let workspace = tempfile::tempdir().expect("workspace");
    let skill_dir = workspace.path().join("skills/review-code");
    fs::create_dir_all(&skill_dir).expect("skill dir");
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: review-code
description: Review code safely
allowed_tools: read_file
---
Use these instructions.
"#,
    )
    .expect("write skill");

    let properties = read_properties(&skill_dir).expect("properties");
    assert_eq!(properties.allowed_tools, None);
}

#[test]
fn skills_public_api_accepts_skill_compatibility_without_public_payload_exposure() {
    let workspace = tempfile::tempdir().expect("workspace");
    let skill_dir = workspace.path().join("skills/runtime-ready");
    fs::create_dir_all(&skill_dir).expect("skill dir");
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: runtime-ready
description: Runtime-ready skill
compatibility: rust>=1.80
---
Use these instructions.
"#,
    )
    .expect("write skill");

    let diagnostics = validate_metadata_with_diagnostics(
        &BTreeMap::from([
            (
                "name".to_string(),
                Value::String("runtime-ready".to_string()),
            ),
            (
                "description".to_string(),
                Value::String("Runtime-ready skill".to_string()),
            ),
            (
                "compatibility".to_string(),
                Value::String("rust>=1.80".to_string()),
            ),
        ]),
        Some(&skill_dir),
        Some("strict"),
    )
    .expect("diagnostics");
    assert_eq!(diagnostics.errors, Vec::<String>::new());
    assert_eq!(diagnostics.warnings, Vec::<String>::new());

    let properties = read_properties(&skill_dir).expect("properties");
    assert_eq!(properties.compatibility.as_deref(), Some("rust>=1.80"));
    assert!(
        !properties
            .to_value()
            .as_object()
            .expect("properties payload")
            .contains_key("compatibility"),
        "serialized skill payloads should stay focused on execution guidance"
    );

    let entries = normalize_skill_list(Some(&json!(["skills"])), Some(workspace.path()), false);
    assert_eq!(entries.len(), 1);
    assert!(
        !format!("{:?}", entries[0]).contains("rust>=1.80"),
        "normalized skill entries feed Agent-visible prompts/results and should not carry runtime requirement metadata"
    );

    let xml = render_skills_xml(&entries, 8000);
    assert!(xml.contains("runtime-ready"));
    assert!(
        !xml.contains("rust>=1.80") && !xml.contains("compatibility"),
        "<available_skills> should stay focused on name, description, and location"
    );
}

#[test]
fn skills_public_api_keeps_optional_frontmatter_out_of_prompt_models() {
    let workspace = tempfile::tempdir().expect("workspace");
    let skill_dir = workspace.path().join("skills/review-code");
    fs::create_dir_all(&skill_dir).expect("skill dir");
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: review-code
description: Review code safely
license: MIT
allowed-tools: read_file
x-internal-note: hidden
metadata:
  owner: agent
---
Use these instructions.
"#,
    )
    .expect("write skill");

    let properties = read_properties(&skill_dir).expect("properties");
    let property_payload = properties.to_value();
    assert!(
        !property_payload
            .as_object()
            .expect("properties payload")
            .contains_key("x-internal-note"),
        "unknown frontmatter should not be part of the public skill model"
    );

    let entries = normalize_skill_list(Some(&json!(["skills"])), Some(workspace.path()), false);
    assert_eq!(entries.len(), 1);
    assert!(
        !format!("{:?}", entries[0]).contains("license"),
        "normalized skills feed model-visible prompts/results and should not carry package metadata"
    );
    assert!(
        !format!("{:?}", entries[0]).contains("x-internal-note"),
        "normalized skills feed model-visible prompts/results and should not carry unknown metadata"
    );
    assert!(
        !format!("{:?}", entries[0]).contains("MIT"),
        "normalized skills feed model-visible prompts/results and should not carry package metadata values"
    );
}

#[test]
fn skills_public_api_normalizes_mixed_skill_metadata() {
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
fn skills_public_api_stringifies_inline_scalar_fields() {
    let raw_skills = json!([
        {
            "name": 123,
            "description": 456,
            "instructions": 789,
            "x_internal_note": true,
            "metadata": {
                "priority": 5,
                "enabled": true,
                "missing": null,
                "tags": ["x", 2, true]
            }
        },
        {
            "name": 0,
            "description": "Falsy truthy name should be skipped"
        }
    ]);

    let entries = normalize_skill_list(Some(&raw_skills), None, true);

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "123");
    assert_eq!(entries[0].description, "456");
    assert_eq!(entries[0].instructions.as_deref(), Some("789"));
    assert!(
        !format!("{:?}", entries[0]).contains("x_internal_note"),
        "inline internal fields should be ignored before entries reach prompts/results"
    );
    assert_eq!(entries[0].metadata["priority"], "5");
    assert_eq!(entries[0].metadata["enabled"], "True");
    assert_eq!(entries[0].metadata["missing"], "None");
    assert_eq!(entries[0].metadata["tags"], "['x', 2, True]");
}
