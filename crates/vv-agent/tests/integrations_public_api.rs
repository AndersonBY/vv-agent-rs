use vv_agent::integrations::protocols::SkillIntegration as ProtocolSkillIntegration;
use vv_agent::integrations::SkillIntegration;

struct ToggleIntegration(bool);

impl SkillIntegration for ToggleIntegration {
    fn enabled(&self) -> bool {
        self.0
    }
}

#[test]
fn skill_integration_protocol_matches_enabled_contract() {
    let enabled: Box<dyn SkillIntegration> = Box::new(ToggleIntegration(true));
    let disabled: Box<dyn SkillIntegration> = Box::new(ToggleIntegration(false));

    assert!(enabled.enabled());
    assert!(!disabled.enabled());
}

#[test]
fn integrations_protocols_submodule_matches_import_path() {
    let enabled: Box<dyn ProtocolSkillIntegration> = Box::new(ToggleIntegration(true));

    assert!(enabled.enabled());
}

#[test]
fn integrations_module_is_split_package() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    assert!(
        root.join("integrations").join("mod.rs").is_file(),
        "integrations should be split into src/integrations/mod.rs"
    );
    assert!(
        root.join("integrations").join("protocols.rs").is_file(),
        "integrations protocols should live in src/integrations/protocols.rs"
    );
    assert!(
        !root.join("integrations.rs").exists(),
        "src/integrations.rs should be split into an integrations/ module directory"
    );
}
