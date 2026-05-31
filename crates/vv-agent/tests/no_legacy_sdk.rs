use std::path::{Path, PathBuf};

#[test]
fn examples_and_tests_use_current_sdk_surface() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let roots = [
        manifest_dir.join("src"),
        manifest_dir.join("examples"),
        manifest_dir.join("tests"),
    ];
    let forbidden = [
        "#![allow(deprecated)]",
        "AgentDefinition",
        "AgentSDKClient",
        "AgentSDKOptions",
        "AgentSession",
        "AgentSessionRunRequest",
        "AgentRun",
        "create_agent_session",
        "create_default_session",
        "run_with_options_and_agent",
        "query_with_options_and_agent",
        "CeleryBackend",
        "CycleTaskDispatchResult",
        "CycleTaskDispatcher",
        "ExecutionBackend",
        "LLMClient",
        "ScriptedLLM",
        "VVLlmClient",
        "AfterLLMEvent",
        "BeforeLLMEvent",
        "BeforeLLMPatch",
        "BaseRuntimeHook",
        "with_cycle_task_name",
        "cycle_task_name",
    ];

    let mut violations = Vec::new();
    for root in roots {
        for file in rust_files(&root) {
            if file
                .file_name()
                .is_some_and(|name| name == "no_legacy_sdk.rs")
            {
                continue;
            }
            let content = std::fs::read_to_string(&file).expect("read Rust file");
            for term in forbidden {
                let found = if term.starts_with("#!") {
                    content.contains(term)
                } else {
                    contains_identifier(&content, term)
                };
                if found {
                    violations.push(format!("{} contains {term}", file.display()));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "examples and tests should use the current Agent/Runner SDK surface:\n{}",
        violations.join("\n")
    );
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rust_files(root, &mut files);
    files
}

fn collect_rust_files(path: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(path).expect("read directory") {
        let path = entry.expect("read directory entry").path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn contains_identifier(content: &str, term: &str) -> bool {
    content.match_indices(term).any(|(index, _)| {
        let before = content[..index].chars().next_back();
        let after = content[index + term.len()..].chars().next();
        !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char)
    })
}

fn is_ident_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}
