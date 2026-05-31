pub(super) fn block_on_session<T>(
    future: std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send>>,
) -> Result<T, String>
where
    T: Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            tokio::task::block_in_place(|| handle.block_on(future))
        } else {
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?
                    .block_on(future)
            })
            .join()
            .map_err(|_| "session thread panicked".to_string())?
        }
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?
            .block_on(future)
    }
}
