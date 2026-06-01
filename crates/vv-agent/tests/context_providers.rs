use serde_json::json;
use vv_agent::{
    assemble_context_fragments, ContextError, ContextFragment, ContextProvider, ContextRequest,
};

struct StaticProvider;

impl ContextProvider for StaticProvider {
    fn fragments(
        &self,
        _request: &ContextRequest<'_>,
    ) -> Result<Vec<ContextFragment>, ContextError> {
        Ok(vec![
            ContextFragment::new("volatile", "second")
                .stable(false)
                .priority(20)
                .source("test"),
            ContextFragment::new("stable", "first")
                .stable(true)
                .priority(10)
                .cache_hint("cache"),
        ])
    }
}

#[test]
fn context_fragments_are_ordered_budgeted_and_hashed() {
    let request = ContextRequest::for_test("assistant", "input").max_prompt_chars(20);
    let fragments = StaticProvider.fragments(&request).expect("fragments");
    let bundle = assemble_context_fragments(&request, fragments).expect("bundle");

    assert_eq!(bundle.prompt, "first\n\nsecond");
    assert_eq!(bundle.sections[0].id, "stable");
    assert!(!bundle.stable_hash.is_empty());
    assert_eq!(bundle.sources["volatile"], "test");
    assert_eq!(
        bundle.sections[0].metadata.get("cache_hint"),
        Some(&json!("cache"))
    );
}
