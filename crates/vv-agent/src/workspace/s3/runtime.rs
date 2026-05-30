use std::future::Future;
use std::io::Error;

use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

pub(super) fn build_runtime() -> std::io::Result<Runtime> {
    RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::other(error.to_string()))
}

pub(super) fn block_on_object_store<T>(
    runtime: &Runtime,
    future: impl Future<Output = object_store::Result<T>>,
) -> std::io::Result<T> {
    runtime
        .block_on(future)
        .map_err(crate::workspace::object_store_error_to_io)
}
