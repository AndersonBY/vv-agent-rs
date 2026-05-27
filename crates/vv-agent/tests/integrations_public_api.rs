use vv_agent::integrations::protocols::SkillIntegration as ProtocolSkillIntegration;
use vv_agent::integrations::SkillIntegration;

struct ToggleIntegration(bool);

impl SkillIntegration for ToggleIntegration {
    fn enabled(&self) -> bool {
        self.0
    }
}

#[test]
fn skill_integration_protocol_matches_python_enabled_contract() {
    let enabled: Box<dyn SkillIntegration> = Box::new(ToggleIntegration(true));
    let disabled: Box<dyn SkillIntegration> = Box::new(ToggleIntegration(false));

    assert!(enabled.enabled());
    assert!(!disabled.enabled());
}

#[test]
fn integrations_protocols_submodule_matches_python_import_path() {
    let enabled: Box<dyn ProtocolSkillIntegration> = Box::new(ToggleIntegration(true));

    assert!(enabled.enabled());
}
