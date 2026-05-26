pub trait SkillIntegration: Send + Sync {
    fn name(&self) -> &str;
}
