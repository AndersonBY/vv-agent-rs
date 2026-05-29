use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

#[derive(Clone)]
pub struct PromptSection {
    pub id: String,
    stable: bool,
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
            compute: Arc::new(compute),
            cached_value: Arc::new(Mutex::new(None)),
        }
    }

    pub fn constant(id: impl Into<String>, text: impl Into<String>, stable: bool) -> Self {
        let text = text.into();
        Self::new(id, move || text.clone(), stable)
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
        Some(json!({
            "id": self.id,
            "text": text,
            "stable": self.stable,
        }))
    }

    pub fn stable(&self) -> bool {
        self.stable
    }
}
