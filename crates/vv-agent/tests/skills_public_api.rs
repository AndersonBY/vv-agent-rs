use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};
use vv_agent::skills::{
    normalize_validation_mode, render_skills_xml, validate_metadata,
    validate_metadata_with_diagnostics, SkillEntry, SkillProperties, ValidationMode,
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
