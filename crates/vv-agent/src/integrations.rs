pub trait SkillIntegration: Send + Sync {
    fn enabled(&self) -> bool;
}
