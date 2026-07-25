use super::section::{PromptBundle, PromptSection};

#[derive(Clone, Default)]
pub struct SystemPromptBuilder {
    sections: Vec<PromptSection>,
}

impl SystemPromptBuilder {
    pub fn add_section(&mut self, section: PromptSection) {
        self.sections.push(section);
    }

    pub fn build(&self) -> String {
        self.build_result().flatten()
    }

    pub fn build_result(&self) -> PromptBundle {
        PromptBundle::new(
            self.sections
                .iter()
                .filter(|section| !section.text.is_empty())
                .cloned()
                .collect(),
        )
        .expect("SystemPromptBuilder contains valid non-empty sections")
    }
}
