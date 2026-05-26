use serde_json::json;
use vv_agent::runtime::backends::{CeleryBackend, InlineBackend, RuntimeRecipe, ThreadBackend};

#[test]
fn inline_backend_parallel_map_runs_serially_and_preserves_order() {
    let backend = InlineBackend;

    let results = backend.parallel_map(|value| value * 2, vec![1, 2, 3, 4]);

    assert_eq!(results, vec![2, 4, 6, 8]);
}

#[test]
fn thread_backend_submit_and_parallel_map_preserve_order() {
    let backend = ThreadBackend::new(2);

    let future = backend.submit(|| 42);
    let results = backend.parallel_map(|value| value * 2, vec![1, 2, 3, 4]);

    assert_eq!(future.join().expect("thread result"), 42);
    assert_eq!(results, vec![2, 4, 6, 8]);
}

#[test]
fn runtime_recipe_round_trips_through_json() {
    let recipe = RuntimeRecipe {
        settings_file: "/tmp/settings.py".to_string(),
        backend: "deepseek".to_string(),
        model: "deepseek-v4-pro".to_string(),
        workspace: "/tmp/workspace".to_string(),
        timeout_seconds: 120.0,
        hook_class_paths: vec!["my.hooks.LogHook".to_string()],
        log_preview_chars: Some(300),
    };

    let payload = serde_json::to_value(&recipe).expect("serialize");
    assert_eq!(payload["backend"], json!("deepseek"));

    let restored: RuntimeRecipe = serde_json::from_value(payload).expect("deserialize");
    assert_eq!(restored, recipe);
}

#[test]
fn celery_backend_without_dispatcher_keeps_inline_parallel_map_fallback() {
    let backend = CeleryBackend::inline_fallback();

    let results = backend.parallel_map(|value| value * 3, vec![1, 2, 3]);

    assert_eq!(results, vec![3, 6, 9]);
    assert!(backend.runtime_recipe().is_none());
}
