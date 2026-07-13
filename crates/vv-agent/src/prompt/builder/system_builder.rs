use serde_json::Value;

use super::hash::sha256_hex;
use super::options::BuiltSystemPrompt;
use super::section::PromptSection;

#[derive(Clone, Default)]
pub struct SystemPromptBuilder {
    sections: Vec<PromptSection>,
}

impl SystemPromptBuilder {
    pub fn add_section(&mut self, section: PromptSection) {
        self.sections.push(section);
    }

    pub fn build(&self) -> String {
        self.sections
            .iter()
            .filter_map(|section| {
                let value = section.get_value().trim().to_string();
                (!value.is_empty()).then_some(value)
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn metadata_sections(&self) -> Vec<Value> {
        self.sections
            .iter()
            .filter_map(PromptSection::to_metadata)
            .collect()
    }

    pub fn invalidate_all(&self) {
        for section in &self.sections {
            section.invalidate();
        }
    }

    pub fn invalidate_volatile(&self) {
        for section in &self.sections {
            if !section.stable() {
                section.invalidate();
            }
        }
    }

    pub fn stable_hash(&self) -> String {
        let stable_text = self
            .sections
            .iter()
            .filter(|section| section.stable())
            .map(|section| section.get_value().trim().to_string())
            .collect::<String>();
        sha256_hex(stable_text.as_bytes())
    }

    pub fn build_result(&self) -> BuiltSystemPrompt {
        let mut prompt_parts = Vec::new();
        let mut sections = Vec::new();
        let mut stable_parts = Vec::new();
        for section in &self.sections {
            let value = section.get_value().trim().to_string();
            if value.is_empty() {
                continue;
            }
            prompt_parts.push(value.clone());
            sections.push(
                section
                    .to_metadata()
                    .expect("non-empty prompt section has metadata"),
            );
            if section.stable() {
                stable_parts.push(value);
            }
        }
        BuiltSystemPrompt {
            prompt: prompt_parts.join("\n\n"),
            sections,
            stable_hash: sha256_hex(stable_parts.join("").as_bytes()),
        }
    }
}
