use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::stores::sqlite::SqliteStateStore;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeRecipe {
    pub settings_file: String,
    pub backend: String,
    pub model: String,
    pub workspace: String,
    pub timeout_seconds: f64,
    pub log_preview_chars: Option<usize>,
}

impl RuntimeRecipe {
    pub fn new(
        settings_file: impl Into<String>,
        backend: impl Into<String>,
        model: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            settings_file: settings_file.into(),
            backend: backend.into(),
            model: model.into(),
            workspace: workspace.into(),
            timeout_seconds: 90.0,
            log_preview_chars: None,
        }
    }

    pub fn to_dict(&self) -> Value {
        serde_json::json!({
            "settings_file": self.settings_file,
            "backend": self.backend,
            "model": self.model,
            "workspace": self.workspace,
            "timeout_seconds": self.timeout_seconds,
            "log_preview_chars": self.log_preview_chars,
        })
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = data
            .as_object()
            .ok_or_else(|| "RuntimeRecipe payload must be an object".to_string())?;
        Ok(Self {
            settings_file: read_required_string(object, "settings_file")?.to_string(),
            backend: read_required_string(object, "backend")?.to_string(),
            model: read_required_string(object, "model")?.to_string(),
            workspace: read_required_string(object, "workspace")?.to_string(),
            timeout_seconds: object
                .get("timeout_seconds")
                .and_then(Value::as_f64)
                .unwrap_or(90.0),
            log_preview_chars: object
                .get("log_preview_chars")
                .filter(|value| !value.is_null())
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
        })
    }

    pub fn default_sqlite_checkpoint_path(&self) -> PathBuf {
        PathBuf::from(&self.workspace)
            .join(".vv-agent-state")
            .join("checkpoints.db")
    }

    pub fn build_default_state_store(&self) -> io::Result<SqliteStateStore> {
        let db_path = self.default_sqlite_checkpoint_path();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        SqliteStateStore::new(db_path)
    }
}

fn read_required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required string field {key:?}"))
}
