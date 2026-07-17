use super::*;

pub(super) fn parse_params<T: serde::de::DeserializeOwned>(
    params: Option<Value>,
) -> Result<T, AppServerError> {
    let params = params.ok_or_else(|| AppServerError::invalid_params("Missing params"))?;
    serde_json::from_value(params)
        .map_err(|error| AppServerError::invalid_params(error.to_string()))
}

pub(super) fn parse_params_or_default<T: serde::de::DeserializeOwned + Default>(
    params: Option<Value>,
) -> Result<T, AppServerError> {
    match params {
        Some(params) => serde_json::from_value(params)
            .map_err(|error| AppServerError::invalid_params(error.to_string())),
        None => Ok(T::default()),
    }
}

pub(super) fn store_error(error: ThreadStoreError) -> AppServerError {
    AppServerError::internal(error.to_string())
}

pub(super) fn load_thread_resume_snapshot(
    store: &SqliteThreadStore,
    thread_id: &str,
) -> Result<ThreadResumeResponse, AppServerError> {
    let thread = store
        .get_thread(thread_id)
        .map_err(store_error)?
        .ok_or_else(AppServerError::thread_not_found)?;
    let items = store.replay_items(thread_id).map_err(store_error)?;
    let turns = store.list_turns(thread_id).map_err(store_error)?;
    Ok(ThreadResumeResponse {
        thread,
        turns,
        items,
    })
}
