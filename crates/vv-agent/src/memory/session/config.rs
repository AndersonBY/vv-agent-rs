use std::path::PathBuf;
use std::sync::Arc;

const DEFAULT_MIN_TOKENS: u64 = 10_000;
const DEFAULT_MAX_TOKENS: u64 = 40_000;
const DEFAULT_MIN_TEXT_MESSAGES: usize = 5;
const DEFAULT_GROWTH_RATIO: f64 = 0.5;

pub type SessionMemoryExtractionCallback =
    Arc<dyn Fn(&str, Option<&str>, Option<&str>) -> Option<String> + Send + Sync + 'static>;

#[derive(Clone)]
pub struct SessionMemoryConfig {
    pub min_tokens_before_extraction: u64,
    pub max_tokens: u64,
    pub min_text_messages: usize,
    pub growth_ratio: f64,
    pub storage_dir: PathBuf,
    pub extraction_callback: Option<SessionMemoryExtractionCallback>,
    pub extraction_backend: Option<String>,
    pub extraction_model: Option<String>,
    pub token_model: String,
}

impl std::fmt::Debug for SessionMemoryConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionMemoryConfig")
            .field(
                "min_tokens_before_extraction",
                &self.min_tokens_before_extraction,
            )
            .field("max_tokens", &self.max_tokens)
            .field("min_text_messages", &self.min_text_messages)
            .field("growth_ratio", &self.growth_ratio)
            .field("storage_dir", &self.storage_dir)
            .field(
                "extraction_callback",
                &self.extraction_callback.as_ref().map(|_| "<callback>"),
            )
            .field("extraction_backend", &self.extraction_backend)
            .field("extraction_model", &self.extraction_model)
            .field("token_model", &self.token_model)
            .finish()
    }
}

impl Default for SessionMemoryConfig {
    fn default() -> Self {
        Self {
            min_tokens_before_extraction: DEFAULT_MIN_TOKENS,
            max_tokens: DEFAULT_MAX_TOKENS,
            min_text_messages: DEFAULT_MIN_TEXT_MESSAGES,
            growth_ratio: DEFAULT_GROWTH_RATIO,
            storage_dir: PathBuf::from(".memory/session"),
            extraction_callback: None,
            extraction_backend: None,
            extraction_model: None,
            token_model: String::new(),
        }
    }
}
