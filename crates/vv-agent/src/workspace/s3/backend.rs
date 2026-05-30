use std::any::Any;
use std::io::{Error, ErrorKind};
use std::sync::Arc;

use futures_util::TryStreamExt;
use object_store::aws::AmazonS3Builder;
use object_store::memory::InMemory;
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use tokio::runtime::Runtime;

use crate::workspace::{
    glob_match, non_empty_option, normalize_workspace_path, normalized_glob_pattern,
    object_store_error_to_io, suffix_with_dot, FileInfo, WorkspaceBackend,
};

use super::config::S3WorkspaceConfig;
use super::paths::{list_prefix, object_key, relative_key};
use super::runtime::{block_on_object_store, build_runtime};

#[derive(Clone)]
pub struct S3WorkspaceBackend {
    store: Arc<dyn ObjectStore>,
    prefix: String,
    runtime: Arc<Runtime>,
}

impl std::fmt::Debug for S3WorkspaceBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("S3WorkspaceBackend")
            .field("store", &self.store.to_string())
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl Default for S3WorkspaceBackend {
    fn default() -> Self {
        Self::from_object_store(InMemory::new(), "")
            .expect("in-memory S3 workspace backend must initialize")
    }
}

impl S3WorkspaceBackend {
    pub fn new(bucket: impl Into<String>) -> std::io::Result<Self> {
        Self::from_config(S3WorkspaceConfig::new(bucket))
    }

    pub fn from_config(config: S3WorkspaceConfig) -> std::io::Result<Self> {
        let bucket = config.bucket.trim();
        if bucket.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "S3 bucket cannot be empty",
            ));
        }
        let mut builder = AmazonS3Builder::from_env().with_bucket_name(bucket.to_string());
        if let Some(endpoint) = non_empty_option(config.endpoint_url) {
            if endpoint.starts_with("http://") {
                builder = builder.with_allow_http(true);
            }
            builder = builder.with_endpoint(endpoint);
        }
        if let Some(region) = non_empty_option(config.region_name) {
            builder = builder.with_region(region);
        }
        if let Some(access_key_id) = non_empty_option(config.aws_access_key_id) {
            builder = builder.with_access_key_id(access_key_id);
        }
        if let Some(secret_access_key) = non_empty_option(config.aws_secret_access_key) {
            builder = builder.with_secret_access_key(secret_access_key);
        }
        if let Some(token) = non_empty_option(config.aws_session_token) {
            builder = builder.with_token(token);
        }
        let virtual_hosted = match config.addressing_style.trim().to_ascii_lowercase().as_str() {
            "" | "virtual" => true,
            "path" => false,
            other => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("unsupported S3 addressing_style: {other}"),
                ))
            }
        };
        builder = builder.with_virtual_hosted_style_request(virtual_hosted);
        let store = builder.build().map_err(object_store_error_to_io)?;
        Self::from_object_store(store, config.prefix)
    }

    pub fn from_object_store(
        store: impl ObjectStore + 'static,
        prefix: impl Into<String>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            store: Arc::new(store),
            prefix: normalize_workspace_path(&prefix.into()),
            runtime: Arc::new(build_runtime()?),
        })
    }

    fn object_key(&self, path: &str) -> String {
        object_key(&self.prefix, path)
    }

    fn relative_key(&self, key: &str) -> Option<String> {
        relative_key(&self.prefix, key)
    }

    fn list_prefix(&self) -> Option<ObjectPath> {
        list_prefix(&self.prefix)
    }

    fn block_on<T>(
        &self,
        future: impl std::future::Future<Output = object_store::Result<T>>,
    ) -> std::io::Result<T> {
        block_on_object_store(&self.runtime, future)
    }
}

impl WorkspaceBackend for S3WorkspaceBackend {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn list_files(&self, base: &str, glob: &str) -> std::io::Result<Vec<String>> {
        let base = normalize_workspace_path(base);
        let pattern = if base.is_empty() {
            normalized_glob_pattern(glob)
        } else {
            format!("{base}/{}", normalized_glob_pattern(glob))
        };
        let prefix = self.list_prefix();
        let objects = self.block_on(async {
            self.store
                .list(prefix.as_ref())
                .try_collect::<Vec<_>>()
                .await
        })?;
        let mut files = objects
            .into_iter()
            .filter_map(|object| self.relative_key(object.location.as_ref()))
            .filter(|path| !path.is_empty() && !path.ends_with('/'))
            .filter(|path| glob_match(path, &pattern))
            .collect::<Vec<_>>();
        files.sort();
        Ok(files)
    }

    fn read_text(&self, path: &str) -> std::io::Result<String> {
        let bytes = self.read_bytes(path)?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
        let key = ObjectPath::from(self.object_key(path));
        self.block_on(async { Ok(self.store.get(&key).await?.bytes().await?.to_vec()) })
    }

    fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize> {
        let key = ObjectPath::from(self.object_key(path));
        let content = if append {
            match self.read_text(path) {
                Ok(existing) => existing + content,
                Err(error) if error.kind() == ErrorKind::NotFound => content.to_string(),
                Err(error) => return Err(error),
            }
        } else {
            content.to_string()
        };
        let len = content.len();
        self.block_on(async {
            self.store
                .put(&key, PutPayload::from(content.into_bytes()))
                .await
                .map(|_| ())
        })?;
        Ok(len)
    }

    fn file_info(&self, path: &str) -> std::io::Result<Option<FileInfo>> {
        let key = ObjectPath::from(self.object_key(path));
        let metadata = match self.block_on(async { self.store.head(&key).await }) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let object_path = metadata.location.as_ref();
        Ok(Some(FileInfo {
            path: self
                .relative_key(object_path)
                .unwrap_or_else(|| normalize_workspace_path(path)),
            is_file: true,
            is_dir: false,
            size: metadata.size,
            modified_at: metadata.last_modified.to_rfc3339(),
            suffix: suffix_with_dot(path),
        }))
    }

    fn exists(&self, path: &str) -> bool {
        self.file_info(path).ok().flatten().is_some()
    }

    fn is_file(&self, path: &str) -> bool {
        self.exists(path)
    }

    fn mkdir(&self, _path: &str) -> std::io::Result<()> {
        Ok(())
    }
}
