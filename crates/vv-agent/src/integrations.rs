pub trait SkillIntegration: Send + Sync {
    fn enabled(&self) -> bool;
}

pub mod protocols {
    pub use super::SkillIntegration;
}
