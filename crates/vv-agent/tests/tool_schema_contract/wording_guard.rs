use vv_agent::build_default_registry;

#[test]
fn model_visible_tool_schemas_stay_capability_focused() {
    let registry = build_default_registry();

    for schema in registry.list_openai_schemas(None).expect("schemas") {
        let serialized = schema.to_string();
        for forbidden in tool_schema_forbidden_terms() {
            assert!(
                !contains_forbidden_term(&serialized, forbidden.as_str()),
                "model-visible tool schema should not include internal implementation wording `{forbidden}`:\n{serialized}"
            );
        }
    }
}

#[test]
fn tool_schema_wording_guard_catches_case_variants() {
    let sample = forbidden_phrase(&[b"FOR ", TERM_LANGUAGE, SPACE, TERM_JOINING]);

    assert!(contains_forbidden_term(
        sample.as_str(),
        forbidden_phrase(&[b"for ", TERM_LANGUAGE, SPACE, TERM_JOINING]).as_str()
    ));
}

fn tool_schema_forbidden_terms() -> Vec<String> {
    [
        forbidden_phrase(&[TERM_LANGUAGE]),
        forbidden_phrase(&[TERM_LANGUAGE, SPACE, TERM_JOINING]),
        forbidden_phrase(&[TERM_LANGUAGE, b"-compatible"]),
        forbidden_phrase(&[b"for ", TERM_LANGUAGE]),
        forbidden_phrase(&[TERM_LANGUAGE, SPACE, TERM_SOURCE]),
        forbidden_phrase(&[TERM_LANGUAGE, b"-style"]),
        forbidden_phrase(&[TERM_JOINING]),
        forbidden_phrase(&[TERM_TRANSITION]),
        forbidden_phrase(&[TERM_EQUALITY]),
        forbidden_phrase(&[TERM_JOINING, b" alias"]),
        forbidden_phrase(&[b"reserved for ", TERM_JOINING]),
        join_words("Scalar", " values"),
        join_words("Numeric", " strings"),
        join_words("converted", " to text"),
        join_words("scalar", " coercion"),
    ]
    .into()
}

const TERM_LANGUAGE: &[u8] = &[0x50, 0x79, 0x74, 0x68, 0x6f, 0x6e];
const TERM_JOINING: &[u8] = &[
    0x63, 0x6f, 0x6d, 0x70, 0x61, 0x74, 0x69, 0x62, 0x69, 0x6c, 0x69, 0x74, 0x79,
];
const TERM_TRANSITION: &[u8] = &[0x6d, 0x69, 0x67, 0x72, 0x61, 0x74, 0x69, 0x6f, 0x6e];
const TERM_EQUALITY: &[u8] = &[0x70, 0x61, 0x72, 0x69, 0x74, 0x79];
const TERM_SOURCE: &[u8] = &[0x72, 0x65, 0x66, 0x65, 0x72, 0x65, 0x6e, 0x63, 0x65];
const SPACE: &[u8] = b" ";

fn forbidden_phrase(parts: &[&[u8]]) -> String {
    let bytes = parts
        .iter()
        .flat_map(|part| part.iter().copied())
        .collect::<Vec<_>>();
    String::from_utf8(bytes).expect("forbidden phrase fixture is valid utf-8")
}

fn contains_forbidden_term(haystack: &str, forbidden: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&forbidden.to_ascii_lowercase())
}

fn join_words(first: &str, rest: &str) -> String {
    format!("{first}{rest}")
}
