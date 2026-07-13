use super::*;

fn assert_error_metadata_matches_content(result: &ToolExecutionResult) {
    let payload: Value = serde_json::from_str(&result.content).expect("tool error payload");
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(json!(result.metadata), payload);
}

struct RawPathWorkspaceBackend {
    paths: Vec<String>,
    fallback: MemoryWorkspaceBackend,
}

impl RawPathWorkspaceBackend {
    fn new(paths: Vec<String>) -> Self {
        Self {
            paths,
            fallback: MemoryWorkspaceBackend::default(),
        }
    }
}

impl WorkspaceBackend for RawPathWorkspaceBackend {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn list_files(&self, _base: &str, _glob: &str) -> std::io::Result<Vec<String>> {
        Ok(self.paths.clone())
    }

    fn read_text(&self, path: &str) -> std::io::Result<String> {
        self.fallback.read_text(path)
    }

    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.fallback.read_bytes(path)
    }

    fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize> {
        self.fallback.write_text(path, content, append)
    }

    fn file_info(&self, path: &str) -> std::io::Result<Option<vv_agent::FileInfo>> {
        self.fallback.file_info(path)
    }

    fn exists(&self, path: &str) -> bool {
        self.fallback.exists(path)
    }

    fn is_file(&self, path: &str) -> bool {
        self.fallback.is_file(path)
    }

    fn mkdir(&self, path: &str) -> std::io::Result<()> {
        self.fallback.mkdir(path)
    }
}

fn visible_raw_paths(pattern: &str, paths: Vec<String>) -> Vec<String> {
    DiscoveryFilteredWorkspaceBackend::new(Arc::new(RawPathWorkspaceBackend::new(paths)), pattern)
        .expect("portable raw-path filter")
        .list_files(".", "**/*")
        .expect("raw-path listing")
}

#[test]
fn create_sub_task_error_corpus_matches_full_envelopes() {
    let fixture = manager_tool_contract();
    let registry = build_default_registry();
    let mut context = ToolContext::new(".");
    context.sub_task_runner = Some(Arc::new(completed_outcome));

    for case in fixture["create_error_cases"]
        .as_array()
        .expect("create error cases")
    {
        let arguments =
            serde_json::from_value(case["arguments"].clone()).expect("create error arguments");
        let result = registry
            .execute(
                &ToolCall::new(
                    case["name"].as_str().expect("case name"),
                    "create_sub_task",
                    arguments,
                ),
                &mut context,
            )
            .expect("create_sub_task error result");
        let payload: Value = serde_json::from_str(&result.content).expect("create error payload");
        assert_eq!(payload, case["expected"], "case {}", case["name"]);
        assert_eq!(
            result.error_code.as_deref(),
            case["expected"]["error_code"].as_str(),
            "case {}",
            case["name"]
        );
        assert_error_metadata_matches_content(&result);
    }
}

#[test]
fn sub_task_status_error_corpus_matches_full_envelopes() {
    let fixture = manager_tool_contract();
    let registry = build_default_registry();
    let mut context = ToolContext::new(".");
    context.sub_task_manager = Some(SubTaskManager::default());

    for case in fixture["status_error_cases"]
        .as_array()
        .expect("status error cases")
    {
        let arguments =
            serde_json::from_value(case["arguments"].clone()).expect("status error arguments");
        let result = registry
            .execute(
                &ToolCall::new(
                    case["name"].as_str().expect("case name"),
                    "sub_task_status",
                    arguments,
                ),
                &mut context,
            )
            .expect("sub_task_status error result");
        let payload: Value = serde_json::from_str(&result.content).expect("status error payload");
        assert_eq!(payload, case["expected"], "case {}", case["name"]);
        assert_eq!(
            result.error_code.as_deref(),
            case["expected"]["error_code"].as_str(),
            "case {}",
            case["name"]
        );
        assert_error_metadata_matches_content(&result);
    }
}

#[test]
fn sub_task_status_success_corpus_matches_full_envelopes() {
    let fixture = manager_tool_contract();
    let registry = build_default_registry();
    let mut context = ToolContext::new(".");
    context.sub_task_manager = Some(SubTaskManager::default());

    for case in fixture["status_success_cases"]
        .as_array()
        .expect("status success cases")
    {
        let arguments =
            serde_json::from_value(case["arguments"].clone()).expect("status success arguments");
        let result = registry
            .execute(
                &ToolCall::new(
                    case["name"].as_str().expect("case name"),
                    "sub_task_status",
                    arguments,
                ),
                &mut context,
            )
            .expect("sub_task_status success result");
        let payload: Value = serde_json::from_str(&result.content).expect("status success payload");
        assert_eq!(payload, case["expected"], "case {}", case["name"]);
        assert_eq!(result.status, ToolResultStatus::Success);
        assert!(result.error_code.is_none());
        assert_eq!(json!(result.metadata), payload);
    }
}

#[test]
fn synchronous_failed_outcome_normalizes_blank_error_code() {
    let fixture = manager_tool_contract();
    let contract = &fixture["sync_failed_outcome"];
    let registry = build_default_registry();
    let input_error_code = contract["input_error_code"]
        .as_str()
        .expect("blank input error code")
        .to_string();
    let mut context = ToolContext::new(".");
    context.sub_task_runner = Some(Arc::new(move |_request| SubTaskOutcome {
        task_id: "failed-child".to_string(),
        agent_name: "researcher".to_string(),
        status: AgentStatus::Failed,
        session_id: None,
        final_answer: None,
        wait_reason: None,
        error: Some("child failed".to_string()),
        error_code: Some(input_error_code.clone()),
        cycles: 0,
        todo_list: Vec::new(),
        resolved: BTreeMap::new(),
    }));

    let result = registry
        .execute(
            &ToolCall::new(
                "sync-failed-blank-code",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("researcher")),
                    ("task_description".to_string(), json!("fail")),
                ]),
            ),
            &mut context,
        )
        .expect("sync failed result");

    let payload: Value = serde_json::from_str(&result.content).expect("sync failed payload");
    assert_eq!(payload, contract["expected"]);
    assert_eq!(
        result.error_code.as_deref(),
        contract["expected"]["error_code"].as_str()
    );
    assert_error_metadata_matches_content(&result);
}

struct PendingContinuationSession;

impl SubAgentSession for PendingContinuationSession {
    fn steer(&self, _prompt: &str) -> Result<(), String> {
        Ok(())
    }

    fn continue_run(&self, _prompt: &str) -> Result<SubTaskOutcome, String> {
        Ok(SubTaskOutcome {
            task_id: "pending-task".to_string(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Completed,
            session_id: Some("pending-session".to_string()),
            final_answer: Some("done".to_string()),
            wait_reason: None,
            error: None,
            error_code: None,
            cycles: 0,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        })
    }
}

#[test]
fn pending_interaction_previous_status_matches_shared_contract() {
    let fixture = manager_tool_contract();
    let registry = build_default_registry();
    let manager = SubTaskManager::default();
    manager.attach_session(
        "pending-task",
        "pending-session",
        "researcher",
        "pending",
        Arc::new(MemoryWorkspaceBackend::default()),
        Arc::new(PendingContinuationSession),
    );
    let mut context = ToolContext::new(".");
    context.sub_task_manager = Some(manager.clone());

    let result = registry
        .execute(
            &ToolCall::new(
                "pending-interaction",
                "sub_task_status",
                BTreeMap::from([
                    ("task_ids".to_string(), json!(["pending-task"])),
                    ("message".to_string(), json!("continue")),
                ]),
            ),
            &mut context,
        )
        .expect("pending interaction result");
    let payload: Value =
        serde_json::from_str(&result.content).expect("pending interaction payload");

    assert_eq!(
        payload["interaction"]["previous_status"],
        fixture["pending_interaction_previous_status"]
    );
    assert!(manager.wait("pending-task", Some(Duration::from_secs(2))));
}

#[test]
fn configured_tool_early_errors_mirror_payload_into_metadata() {
    let fixture = manager_tool_contract();
    let registry = build_default_registry();
    let mut create_context = ToolContext::new(".");
    let create_error = registry
        .execute(
            &ToolCall::new(
                "missing-runner",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("researcher")),
                    ("task_description".to_string(), json!("Research")),
                ]),
            ),
            &mut create_context,
        )
        .expect("create_sub_task early error");
    assert_error_metadata_matches_content(&create_error);

    let mut status_context = ToolContext::new(".");
    let status_error = registry
        .execute(
            &ToolCall::new(
                "missing-manager",
                "sub_task_status",
                BTreeMap::from([("task_ids".to_string(), json!(["task"]))]),
            ),
            &mut status_context,
        )
        .expect("sub_task_status early error");
    assert_error_metadata_matches_content(&status_error);
    assert_eq!(fixture["early_error_metadata_matches_content"], true);
}

#[test]
fn create_sub_task_validates_payload_structure_before_exclude_pattern() {
    let fixture = manager_tool_contract();
    let registry = build_default_registry();
    let mut context = ToolContext::new(".");
    context.sub_task_runner = Some(Arc::new(completed_outcome));

    let cases = [
        (
            BTreeMap::from([
                ("agent_id".to_string(), json!("researcher")),
                ("task_description".to_string(), json!("single")),
                ("tasks".to_string(), json!([{"task_description": "batch"}])),
                ("exclude_files_pattern".to_string(), json!(r"(?=secret)")),
            ]),
            "sub_task_payload_conflict",
        ),
        (
            BTreeMap::from([
                ("agent_id".to_string(), json!("researcher")),
                ("tasks".to_string(), json!("not an array")),
                ("exclude_files_pattern".to_string(), json!(r"(?=secret)")),
            ]),
            "invalid_tasks_payload",
        ),
        (
            BTreeMap::from([
                ("agent_id".to_string(), json!("researcher")),
                ("tasks".to_string(), json!([])),
                ("exclude_files_pattern".to_string(), json!(r"(?=secret)")),
            ]),
            "invalid_tasks_payload",
        ),
        (
            BTreeMap::from([
                ("agent_id".to_string(), json!("researcher")),
                ("tasks".to_string(), json!([42])),
                ("exclude_files_pattern".to_string(), json!(r"(?=secret)")),
            ]),
            "invalid_tasks_payload",
        ),
    ];

    for (index, (arguments, expected_code)) in cases.into_iter().enumerate() {
        let result = registry
            .execute(
                &ToolCall::new(
                    format!("payload-priority-{index}"),
                    "create_sub_task",
                    arguments,
                ),
                &mut context,
            )
            .expect("create_sub_task validation result");
        assert_eq!(result.error_code.as_deref(), Some(expected_code));
        assert_error_metadata_matches_content(&result);
    }
    assert_eq!(
        fixture["validation"]["payload_validation_precedes_exclude_pattern"],
        true
    );
}

#[test]
fn configured_tools_reject_non_string_schema_values_without_stringifying() {
    let fixture = manager_tool_contract();
    let registry = build_default_registry();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_runner = calls.clone();
    let mut create_context = ToolContext::new(".");
    create_context.sub_task_runner = Some(Arc::new(move |request| {
        calls_for_runner.fetch_add(1, Ordering::SeqCst);
        completed_outcome(request)
    }));
    let create_cases = [
        BTreeMap::from([
            ("agent_id".to_string(), json!(["researcher"])),
            ("task_description".to_string(), json!("Research")),
        ]),
        BTreeMap::from([
            ("agent_id".to_string(), json!("researcher")),
            ("task_description".to_string(), json!({"task": "Research"})),
        ]),
        BTreeMap::from([
            ("agent_id".to_string(), json!("researcher")),
            ("task_description".to_string(), json!("Research")),
            ("output_requirements".to_string(), json!(["json"])),
        ]),
        BTreeMap::from([
            ("agent_id".to_string(), json!("researcher")),
            ("task_description".to_string(), json!("Research")),
            ("exclude_files_pattern".to_string(), json!(42)),
        ]),
        BTreeMap::from([
            ("agent_id".to_string(), json!("researcher")),
            (
                "tasks".to_string(),
                json!([{"task_description": ["Research"]}]),
            ),
        ]),
        BTreeMap::from([
            ("agent_id".to_string(), json!("researcher")),
            (
                "tasks".to_string(),
                json!([{"task_description": "Research", "output_requirements": {"format": "json"}}]),
            ),
        ]),
    ];

    for (index, arguments) in create_cases.into_iter().enumerate() {
        let result = registry
            .execute(
                &ToolCall::new(
                    format!("non-string-create-{index}"),
                    "create_sub_task",
                    arguments,
                ),
                &mut create_context,
            )
            .expect("create_sub_task type validation");
        assert_error_metadata_matches_content(&result);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let mut status_context = ToolContext::new(".");
    status_context.sub_task_manager = Some(SubTaskManager::default());
    for (index, arguments) in [
        BTreeMap::from([("task_ids".to_string(), json!([42]))]),
        BTreeMap::from([
            ("task_ids".to_string(), json!(["unknown"])),
            ("message".to_string(), json!({"prompt": "continue"})),
        ]),
        BTreeMap::from([
            ("task_ids".to_string(), json!(["unknown"])),
            ("detail_level".to_string(), json!(["snapshot"])),
        ]),
    ]
    .into_iter()
    .enumerate()
    {
        let result = registry
            .execute(
                &ToolCall::new(
                    format!("non-string-status-{index}"),
                    "sub_task_status",
                    arguments,
                ),
                &mut status_context,
            )
            .expect("sub_task_status type validation");
        assert_error_metadata_matches_content(&result);
    }
    assert_eq!(fixture["validation"]["non_string_schema_values"], "reject");
}

#[test]
fn portable_workspace_regex_cases_match_shared_contract() {
    let fixture = contract();
    let cases = &fixture["workspace_filter"]["portable_cases"];

    for case in cases["accepted"]
        .as_array()
        .expect("accepted portable regex cases")
    {
        let pattern = case["pattern"].as_str().expect("accepted pattern");
        validate_portable_exclude_pattern(pattern).expect("portable pattern");
        let matches = case["matches"]
            .as_array()
            .expect("matching paths")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        let misses = case["misses"]
            .as_array()
            .expect("non-matching paths")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        let paths = matches.iter().chain(&misses).cloned().collect::<Vec<_>>();
        assert_eq!(visible_raw_paths(pattern, paths), misses);
    }

    for pattern in cases["rejected"]
        .as_array()
        .expect("rejected portable regex cases")
        .iter()
        .filter_map(Value::as_str)
    {
        assert!(
            validate_portable_exclude_pattern(pattern).is_err(),
            "pattern should be rejected: {pattern}"
        );
    }
}

#[test]
fn portable_workspace_regex_complements_use_ascii_semantics() {
    let fixture = contract();
    let accepted = fixture["workspace_filter"]["portable_cases"]["accepted"]
        .as_array()
        .expect("accepted portable regex cases");

    for (positive, complement) in [(r"^\w$", r"^\W$"), (r"^\s$", r"^\S$")] {
        let case = accepted
            .iter()
            .find(|case| case["pattern"] == positive)
            .unwrap_or_else(|| panic!("missing portable case {positive}"));
        let matches = case["matches"]
            .as_array()
            .expect("positive matches")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        let misses = case["misses"]
            .as_array()
            .expect("positive misses")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        let paths = matches.iter().chain(&misses).cloned().collect::<Vec<_>>();

        assert_eq!(visible_raw_paths(complement, paths), matches);
    }
}

#[test]
fn public_sub_task_request_shape_and_wire_remain_unchanged() {
    let request = SubTaskRequest {
        agent_name: "researcher".to_string(),
        task_description: "Collect facts".to_string(),
        output_requirements: "Return JSON".to_string(),
        include_main_summary: true,
        exclude_files_pattern: Some("^target/".to_string()),
        metadata: BTreeMap::from([("custom".to_string(), json!(true))]),
    };

    let wire = serde_json::to_value(&request).expect("serialize public request");
    let restored: SubTaskRequest =
        serde_json::from_value(wire.clone()).expect("deserialize public request");

    assert_eq!(restored, request);
    assert_eq!(
        wire.as_object()
            .expect("request object")
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            "agent_name",
            "exclude_files_pattern",
            "include_main_summary",
            "metadata",
            "output_requirements",
            "task_description",
        ]
    );
}

#[test]
fn workspace_filter_matches_fixture_and_keeps_known_paths_accessible() {
    let fixture = contract();
    let backend = Arc::new(MemoryWorkspaceBackend::default());
    for path in fixture["workspace_filter"]["visible_paths"]
        .as_array()
        .expect("visible paths")
        .iter()
        .chain(
            fixture["workspace_filter"]["excluded_paths"]
                .as_array()
                .expect("excluded paths"),
        )
        .filter_map(Value::as_str)
    {
        backend
            .write_text(path, path, false)
            .expect("seed workspace");
    }
    let filtered = DiscoveryFilteredWorkspaceBackend::new(
        backend,
        fixture["workspace_filter"]["pattern"]
            .as_str()
            .expect("filter pattern"),
    )
    .expect("portable filter");

    assert_eq!(
        filtered.list_files(".", "**/*").expect("filtered listing"),
        fixture["workspace_filter"]["visible_paths"]
            .as_array()
            .expect("visible paths")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>()
    );
    let known_path = filtered.read_text("generated/cache.bin");
    assert_eq!(
        known_path.is_ok(),
        fixture["workspace_filter"]["known_path_accessible"]
    );
    assert_eq!(
        known_path.expect("known excluded path remains readable"),
        "generated/cache.bin"
    );
}

#[test]
fn workspace_filter_normalizes_custom_backend_paths_only_for_matching() {
    let fixture = contract();
    let path_contract = &fixture["workspace_filter"]["custom_backend_path_normalization"];
    let raw_paths = path_contract["raw_paths"]
        .as_array()
        .expect("custom backend raw paths")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    let visible = visible_raw_paths(
        path_contract["pattern"]
            .as_str()
            .expect("custom backend pattern"),
        raw_paths,
    );

    assert_eq!(
        visible,
        path_contract["visible_paths"]
            .as_array()
            .expect("custom backend visible paths")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        visible.first().map(String::as_str) == Some("src/lib.py"),
        path_contract["preserve_non_matching_raw_paths"]
    );
}

#[test]
fn filtered_local_backend_preserves_explicit_outside_path_capability() {
    let fixture = contract();
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside directory");
    let outside_file = outside.path().join("outside.txt");
    std::fs::write(&outside_file, "outside content").expect("write outside file");
    let filtered = DiscoveryFilteredWorkspaceBackend::new(
        Arc::new(LocalWorkspaceBackend::new(workspace.path())),
        r"^generated/",
    )
    .expect("filtered local backend");
    let mut context = ToolContext::new(workspace.path());
    context.workspace_backend = Arc::new(filtered);
    context
        .metadata
        .insert("allow_outside_workspace_paths".to_string(), json!(true));

    let outside_read = context
        .effective_workspace_backend()
        .read_text(&outside_file.display().to_string());
    assert_eq!(
        outside_read.is_err(),
        fixture["workspace_filter"]["security_boundary"]
    );
    assert_eq!(
        outside_read.expect("read explicitly allowed outside path"),
        "outside content"
    );
}

#[test]
fn create_sub_task_rejects_non_portable_patterns_before_dispatch() {
    let registry = build_default_registry();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_runner = calls.clone();
    let mut context = ToolContext::new(".");
    context.sub_task_runner = Some(Arc::new(move |request| {
        calls_for_runner.fetch_add(1, Ordering::SeqCst);
        completed_outcome(request)
    }));
    let fixture = contract();

    for (index, pattern) in [r"(?=secret)", r"(a)\1", r"\p{Greek}"]
        .into_iter()
        .enumerate()
    {
        let result = registry
            .execute(
                &ToolCall::new(
                    format!("invalid-{index}"),
                    "create_sub_task",
                    BTreeMap::from([
                        ("agent_id".to_string(), json!("researcher")),
                        ("task_description".to_string(), json!("Research")),
                        ("exclude_files_pattern".to_string(), json!(pattern)),
                    ]),
                ),
                &mut context,
            )
            .expect("create_sub_task result");
        let payload: Value = serde_json::from_str(&result.content).expect("error payload");
        assert_eq!(result.status, ToolResultStatus::Error);
        assert_eq!(
            payload["error_code"],
            fixture["workspace_filter"]["invalid_error_code"]
        );
        assert_eq!(
            payload["error"],
            fixture["workspace_filter"]["invalid_message"]
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn blank_exclude_pattern_is_treated_as_not_provided() {
    let registry = build_default_registry();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_for_runner = captured.clone();
    let mut context = ToolContext::new(".");
    context.sub_task_runner = Some(Arc::new(move |request| {
        captured_for_runner
            .lock()
            .expect("captured blank patterns")
            .push(request.clone());
        completed_outcome(request)
    }));

    for (index, pattern) in ["", "  \n\t  "].into_iter().enumerate() {
        let result = registry
            .execute(
                &ToolCall::new(
                    format!("blank-{index}"),
                    "create_sub_task",
                    BTreeMap::from([
                        ("agent_id".to_string(), json!("researcher")),
                        ("task_description".to_string(), json!("Research")),
                        ("exclude_files_pattern".to_string(), json!(pattern)),
                    ]),
                ),
                &mut context,
            )
            .expect("blank pattern result");
        assert_eq!(result.status, ToolResultStatus::Success);
    }

    let captured = captured.lock().expect("captured blank patterns");
    assert_eq!(captured.len(), 2);
    assert!(captured
        .iter()
        .all(|request| request.exclude_files_pattern.is_none()));
}
