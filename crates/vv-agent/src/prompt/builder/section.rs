use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

#[derive(Clone)]
pub struct PromptSection {
    pub id: String,
    stable: bool,
    source: String,
    cache_hint: Option<String>,
    metadata: Map<String, Value>,
    compute: Arc<dyn Fn() -> String + Send + Sync>,
    cached_value: Arc<Mutex<Option<String>>>,
}

impl PromptSection {
    pub fn new(
        id: impl Into<String>,
        compute: impl Fn() -> String + Send + Sync + 'static,
        stable: bool,
    ) -> Self {
        Self {
            id: id.into(),
            stable,
            source: String::new(),
            cache_hint: None,
            metadata: Map::new(),
            compute: Arc::new(compute),
            cached_value: Arc::new(Mutex::new(None)),
        }
    }

    pub fn constant(id: impl Into<String>, text: impl Into<String>, stable: bool) -> Self {
        let text = text.into();
        Self::new(id, move || text.clone(), stable)
    }

    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    pub fn cache_hint(mut self, cache_hint: impl Into<String>) -> Self {
        self.cache_hint = Some(cache_hint.into());
        self
    }

    pub fn metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    pub fn get_value(&self) -> String {
        if self.stable {
            let mut cached = self.cached_value.lock().expect("prompt section cache");
            if let Some(value) = cached.as_ref() {
                return value.clone();
            }
            let value = (self.compute)();
            *cached = Some(value.clone());
            return value;
        }
        (self.compute)()
    }

    pub fn invalidate(&self) {
        let mut cached = self.cached_value.lock().expect("prompt section cache");
        *cached = None;
    }

    pub fn to_metadata(&self) -> Option<Value> {
        let text = self.get_value().trim().to_string();
        if text.is_empty() {
            return None;
        }
        let mut payload = Map::from_iter([
            ("id".to_string(), Value::String(self.id.clone())),
            ("text".to_string(), Value::String(text)),
            ("stable".to_string(), Value::Bool(self.stable)),
        ]);
        if !self.source.is_empty() {
            payload.insert("source".to_string(), Value::String(self.source.clone()));
        }
        if let Some(cache_hint) = &self.cache_hint {
            payload.insert("cache_hint".to_string(), Value::String(cache_hint.clone()));
        }
        if !self.metadata.is_empty() {
            payload.insert("metadata".to_string(), Value::Object(self.metadata.clone()));
        }
        Some(Value::Object(payload))
    }

    pub fn stable(&self) -> bool {
        self.stable
    }
}
