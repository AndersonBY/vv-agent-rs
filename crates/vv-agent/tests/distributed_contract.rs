use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};
use vv_agent::runtime::backends::distributed::{
    toolset_schema_digest, CapabilityRef, DistributedCapabilities, DistributedCapabilityRegistry,
    DistributedCycleWorker, DistributedRunEnvelope, DistributedToolPolicy, ToolsetRef,
    DEFAULT_CYCLE_NAME, DEFAULT_LEASE_DURATION_MS, DEFAULT_TOOLSET_SCHEMA_DIGEST,
};
use vv_agent::runtime::state::{Checkpoint, StateStore};
use vv_agent::{
    build_default_registry, AgentStatus, AgentTask, LLMResponse, Message, NoToolPolicy,
    RuntimeRecipe, ScriptStep, ScriptedLlmClient, ToolCall, ToolDirective, ToolExecutionResult,
};

const FIXTURE: &str = include_str!("fixtures/parity/distributed_run_envelope_v1.json");

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).expect("distributed fixture")
}

fn lease_lifecycle() -> Value {
    fixture()["lease_lifecycle"].clone()
}

fn worker_case(name: &str) -> Value {
    lease_lifecycle()["worker_cases"]
        .as_array()
        .expect("worker cases")
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("missing worker case {name}"))
        .clone()
}

fn set_path(payload: &mut Value, path: &[Value], value: Value) {
    let mut target = payload;
    for key in &path[..path.len() - 1] {
        target = &mut target[key.as_str().expect("path key")];
    }
    target[path.last().and_then(Value::as_str).expect("final path key")] = value;
}

#[test]
fn distributed_envelope_fixture_round_trips_and_default_digest_matches() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("distributed fixture");
    let canonical = fixture["canonical_envelope"].clone();

    let envelope = DistributedRunEnvelope::from_dict(&canonical).expect("canonical envelope");

    assert_eq!(envelope.to_dict(), canonical);
    assert_eq!(fixture["schema_version"], envelope.schema_version);
    assert_eq!(
        fixture["default_toolset_schema_digest"],
        DEFAULT_TOOLSET_SCHEMA_DIGEST
    );
}

#[test]
fn distributed_envelope_invalid_cases_match_shared_contract() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("distributed fixture");
    for case in fixture["invalid_cases"].as_array().expect("invalid cases") {
        let mut payload = fixture["canonical_envelope"].clone();
        set_path(
            &mut payload,
            case["path"].as_array().expect("case path"),
            case["value"].clone(),
        );
        let error = DistributedRunEnvelope::from_dict(&payload).expect_err("invalid envelope");
        assert!(
            error.contains(case["error"].as_str().expect("expected error")),
            "case {} returned {error}",
            case["name"]
        );
    }
}

#[test]
fn python_and_rust_distributed_fixture_copies_are_byte_identical() {
    let rust_bytes = FIXTURE.as_bytes();
    assert_eq!(
        format!("{:x}", Sha256::digest(rust_bytes)),
        "c1eb11591c93e8ac880fd4688cf06e0fe60a8b4522f7707ea13e1cccf40208e0"
    );
    let explicit_python_root = std::env::var_os("VV_AGENT_PYTHON_REPO").map(PathBuf::from);
    let python_root = explicit_python_root
        .clone()
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../vv-agent"));
    let python_path = python_root.join("tests/fixtures/parity/distributed_run_envelope_v1.json");
    if explicit_python_root.is_none() {
        let rust_lock = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contract.lock.json");
        let python_lock = python_root.join("contract.lock.json");
        let locks_match = [rust_lock, python_lock]
            .map(std::fs::read_to_string)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .ok()
            .and_then(|locks| {
                locks
                    .into_iter()
                    .map(|lock| serde_json::from_str::<Value>(&lock).ok())
                    .collect::<Option<Vec<_>>>()
            })
            .is_some_and(|locks| {
                locks[0]["contract_version"] == locks[1]["contract_version"]
                    && locks[0]["contract_revision"] == locks[1]["contract_revision"]
            });
        if !python_path.exists() || !locks_match {
            return;
        }
    }
    assert_eq!(
        rust_bytes,
        std::fs::read(python_path).expect("Python distributed fixture")
    );
}

#[test]
fn default_distributed_capabilities_resolve_without_hidden_fallbacks() {
    let resolved = DistributedCapabilityRegistry::new()
        .resolve(&DistributedCapabilities::default())
        .expect("default distributed capabilities");

    assert_eq!(
        toolset_schema_digest(&resolved.tool_registry).expect("toolset digest"),
        DEFAULT_TOOLSET_SCHEMA_DIGEST
    );
    assert_eq!(
        toolset_schema_digest(&build_default_registry()).expect("default digest"),
        DEFAULT_TOOLSET_SCHEMA_DIGEST
    );
}

#[test]
fn distributed_capability_registry_fails_closed_for_unknown_reference() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("distributed fixture");
    let unknown = &fixture["unknown_capability"];
    let reference = CapabilityRef::from_dict(&unknown["reference"], "reference")
        .expect("unknown reference shape");
    let capabilities = DistributedCapabilities {
        hook_refs: vec![reference],
        ..DistributedCapabilities::default()
    };

    let error = DistributedCapabilityRegistry::new()
        .resolve(&capabilities)
        .err()
        .expect("unknown capability must fail");

    assert_eq!(error.to_string(), unknown["error"].as_str().unwrap());
}

#[test]
fn claim_expiry_boundary_matches_shared_contract() {
    let case = &lease_lifecycle()["claim_boundary_case"];
    let directory = tempfile::tempdir().expect("claim boundary tempdir");
    let stores: Vec<Box<dyn StateStore>> = vec![
        Box::new(vv_agent::InMemoryStateStore::new()),
        Box::new(
            vv_agent::SqliteStateStore::new(directory.path().join("claim-boundary.sqlite3"))
                .expect("SQLite state store"),
        ),
    ];

    for (index, store) in stores.into_iter().enumerate() {
        let task_id = format!("claim-boundary-{index}");
        assert!(store
            .create_checkpoint(Checkpoint {
                task_id: task_id.clone(),
                cycle_index: 0,
                status: AgentStatus::Running,
                messages: vec![Message::system("system"), Message::user("prompt")],
                cycles: Vec::new(),
                shared_state: Default::default(),
                revision: 0,
                claim_token: None,
                claimed_cycle: None,
                lease_expires_at_ms: None,
                terminal_result: None,
                budget_usage: None,
            })
            .expect("create checkpoint"));
        let owner = store
            .claim_checkpoint(
                &task_id,
                1,
                "owner",
                case["lease_expires_at_ms"].as_u64().expect("lease expiry"),
                case["claim_now_ms"].as_u64().expect("claim now"),
            )
            .expect("owner claim")
            .expect("owner checkpoint");
        let boundary_now_ms = case["boundary_now_ms"].as_u64().expect("boundary now");
        let owner_renewed = store
            .renew_checkpoint_claim(
                &task_id,
                "owner",
                owner.revision,
                boundary_now_ms + 100,
                boundary_now_ms,
            )
            .expect("owner renewal result");
        assert_eq!(
            owner_renewed,
            case["owner_renewed"].as_bool().expect("owner renewed")
        );
        let contender = store
            .claim_checkpoint(
                &task_id,
                1,
                "contender",
                boundary_now_ms + 100,
                boundary_now_ms,
            )
            .expect("contender claim");
        assert_eq!(
            contender.is_some(),
            case["contender_reclaimed"]
                .as_bool()
                .expect("contender reclaimed")
        );
    }
}

#[test]
fn distributed_worker_reconstructs_custom_tool_policy_and_app_state() {
    let temp = tempfile::tempdir().expect("distributed worker tempdir");
    let store_path = temp.path().join("checkpoints.sqlite3");
    let store = vv_agent::SqliteStateStore::new(&store_path).expect("state store");
    let mut task = AgentTask::new("worker-custom", "model-x", "system", "prompt");
    task.extra_tool_names.push("custom_probe".to_string());
    store
        .create_checkpoint(Checkpoint {
            task_id: task.task_id.clone(),
            cycle_index: 0,
            status: AgentStatus::Running,
            messages: vec![Message::system("system"), Message::user("prompt")],
            cycles: Vec::new(),
            shared_state: Default::default(),
            revision: 0,
            claim_token: None,
            claimed_cycle: None,
            lease_expires_at_ms: None,
            terminal_result: None,
            budget_usage: None,
        })
        .expect("initial checkpoint");

    let tool_ran = Arc::new(AtomicBool::new(false));
    let tool_ran_for_handler = tool_ran.clone();
    let mut tools = build_default_registry();
    tools
        .register_tool_with_parameters(
            "custom_probe",
            "Read worker app state and finish.",
            serde_json::json!({"type": "object", "properties": {}, "required": []}),
            Arc::new(move |context, _arguments| {
                assert_eq!(
                    context.app_state::<String>().map(String::as_str),
                    Some("tenant-7")
                );
                tool_ran_for_handler.store(true, Ordering::SeqCst);
                let mut result = ToolExecutionResult::success("", "custom worker done");
                result.directive = ToolDirective::Finish;
                result
            }),
        )
        .expect("register custom tool");
    let custom_toolset = ToolsetRef {
        id: "toolset.project".to_string(),
        version: "7".to_string(),
        schema_digest: toolset_schema_digest(&tools).expect("custom digest"),
    };
    let llm_ref = CapabilityRef::new("llm.scripted", "1").unwrap();
    let app_state_ref = CapabilityRef::new("app.tenant", "7").unwrap();
    let predicate_ref = CapabilityRef::new("policy.project", "3").unwrap();
    let predicate_ran = Arc::new(AtomicBool::new(false));
    let predicate_ran_for_policy = predicate_ran.clone();
    let registry = DistributedCapabilityRegistry::new();
    registry
        .register_toolset(custom_toolset.clone(), tools)
        .expect("custom toolset");
    registry.register_llm_client(
        llm_ref.clone(),
        Arc::new(ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
            "run probe",
            vec![ToolCall::new("probe-1", "custom_probe", Default::default())],
        )])),
    );
    registry.register_app_state(app_state_ref.clone(), Arc::new("tenant-7".to_string()));
    registry.register_tool_predicate(
        predicate_ref.clone(),
        Arc::new(move |name, _arguments| {
            predicate_ran_for_policy.store(true, Ordering::SeqCst);
            name == "custom_probe"
        }),
    );

    let mut recipe = RuntimeRecipe::new(
        temp.path()
            .join("unused-settings.json")
            .display()
            .to_string(),
        "backend-x",
        "model-x",
        temp.path().join("workspace").display().to_string(),
    );
    recipe.state_store = store.state_store_spec();
    recipe.capabilities = DistributedCapabilities {
        toolset_ref: custom_toolset,
        tool_policy: DistributedToolPolicy {
            allowed_tools: Some(vec!["custom_probe".to_string()]),
            disallowed_tools: Vec::new(),
            approval: "never".to_string(),
            predicate_ref: Some(predicate_ref),
        },
        llm_client_ref: Some(llm_ref),
        app_state_ref: Some(app_state_ref),
        ..DistributedCapabilities::default()
    };
    let envelope = DistributedRunEnvelope::for_cycle(
        task,
        recipe,
        1,
        DEFAULT_CYCLE_NAME,
        Some("run-worker-custom".to_string()),
        Some(2_000_000_000_000),
        DEFAULT_LEASE_DURATION_MS,
        None,
    )
    .expect("worker envelope");

    let dispatch = DistributedCycleWorker::new(registry)
        .run_cycle(envelope)
        .expect("distributed worker cycle");

    assert!(dispatch.finished);
    assert_eq!(
        dispatch.result.as_ref().map(|result| result.status),
        Some(AgentStatus::Completed)
    );
    assert_eq!(
        dispatch
            .result
            .as_ref()
            .and_then(|result| result.final_answer.as_deref()),
        Some("custom worker done")
    );
    assert!(tool_ran.load(Ordering::SeqCst));
    assert!(predicate_ran.load(Ordering::SeqCst));
    let terminal = store
        .load_checkpoint("worker-custom")
        .expect("load terminal")
        .expect("terminal checkpoint");
    assert!(terminal.terminal_result.is_some());
    assert_eq!(dispatch.checkpoint_revision, Some(terminal.revision));
}

#[test]
fn distributed_worker_resolves_every_capability_before_claiming_checkpoint() {
    let temp = tempfile::tempdir().expect("distributed worker tempdir");
    let store =
        vv_agent::SqliteStateStore::new(temp.path().join("state.sqlite3")).expect("state store");
    let task = AgentTask::new("worker-fail-closed", "model", "system", "prompt");
    store
        .create_checkpoint(Checkpoint {
            task_id: task.task_id.clone(),
            cycle_index: 0,
            status: AgentStatus::Running,
            messages: vec![Message::system("system"), Message::user("prompt")],
            cycles: Vec::new(),
            shared_state: Default::default(),
            revision: 0,
            claim_token: None,
            claimed_cycle: None,
            lease_expires_at_ms: None,
            terminal_result: None,
            budget_usage: None,
        })
        .unwrap();
    let mut recipe = RuntimeRecipe::new(
        temp.path()
            .join("missing-settings.json")
            .display()
            .to_string(),
        "missing",
        "missing",
        temp.path().join("workspace").display().to_string(),
    );
    recipe.state_store = store.state_store_spec();
    recipe.capabilities.hook_refs = vec![CapabilityRef::new("hook.missing", "1").unwrap()];
    let envelope = DistributedRunEnvelope::for_cycle(
        task,
        recipe,
        1,
        DEFAULT_CYCLE_NAME,
        None,
        Some(2_000_000_000_000),
        DEFAULT_LEASE_DURATION_MS,
        None,
    )
    .unwrap();

    let error = DistributedCycleWorker::default()
        .run_cycle(envelope)
        .expect_err("unknown capability must fail closed");

    assert_eq!(error, "unknown distributed capability hook hook.missing@1");
    let checkpoint = store
        .load_checkpoint("worker-fail-closed")
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.revision, 0);
    assert!(checkpoint.claim_token.is_none());
}

#[test]
fn distributed_worker_heartbeat_prevents_claim_theft_during_long_cycle() {
    let case = worker_case("commit_barrier_keeps_heartbeat_active");
    let expected = &case["expected"];
    const LEASE_DURATION_MS: u64 = 3_000;

    let temp = tempfile::tempdir().expect("heartbeat tempdir");
    let db_path = temp.path().join("heartbeat.sqlite3");
    let store = vv_agent::SqliteStateStore::new(&db_path).expect("state store");
    let mut task = AgentTask::new("worker-heartbeat", "model", "system", "prompt");
    task.no_tool_policy = NoToolPolicy::Finish;
    store
        .create_checkpoint(Checkpoint {
            task_id: task.task_id.clone(),
            cycle_index: 0,
            status: AgentStatus::Running,
            messages: vec![Message::system("system"), Message::user("prompt")],
            cycles: Vec::new(),
            shared_state: Default::default(),
            revision: 0,
            claim_token: None,
            claimed_cycle: None,
            lease_expires_at_ms: None,
            terminal_result: None,
            budget_usage: None,
        })
        .unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let llm_ref = CapabilityRef::new("llm.slow", "1").unwrap();
    let registry = DistributedCapabilityRegistry::new();
    let release_rx_for_llm = release_rx.clone();
    registry.register_llm_client(
        llm_ref.clone(),
        Arc::new(ScriptedLlmClient::from_steps(vec![ScriptStep::callback(
            move |_request| {
                started_tx.send(()).expect("signal slow LLM");
                release_rx_for_llm
                    .lock()
                    .expect("release receiver")
                    .recv_timeout(Duration::from_secs(30))
                    .expect("release slow LLM");
                Ok(LLMResponse::new("done"))
            },
        )])),
    );
    let mut recipe = RuntimeRecipe::new(
        temp.path().join("unused.json").display().to_string(),
        "test",
        "model",
        temp.path().join("workspace").display().to_string(),
    );
    recipe.state_store = store.state_store_spec();
    recipe.capabilities.llm_client_ref = Some(llm_ref);
    let envelope = DistributedRunEnvelope::for_cycle(
        task.clone(),
        recipe,
        1,
        DEFAULT_CYCLE_NAME,
        None,
        Some(2_000_000_000_000),
        LEASE_DURATION_MS,
        None,
    )
    .unwrap();
    let worker = DistributedCycleWorker::new(registry);
    let worker_thread = std::thread::spawn(move || worker.run_cycle(envelope));
    started_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("slow LLM started");

    let contender = vv_agent::SqliteStateStore::new(&db_path).expect("contender store");
    let initial_expiry = contender
        .load_checkpoint(&task.task_id)
        .expect("load claimed checkpoint")
        .expect("claimed checkpoint")
        .lease_expires_at_ms
        .expect("initial lease expiry");
    let wait_for_lease_after = |previous_expiry| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let checkpoint = contender
                .load_checkpoint(&task.task_id)
                .expect("load renewed checkpoint")
                .expect("renewed checkpoint");
            let lease_expires_at_ms = checkpoint
                .lease_expires_at_ms
                .expect("active heartbeat lease");
            if lease_expires_at_ms > previous_expiry {
                break lease_expires_at_ms;
            }
            assert!(
                Instant::now() < deadline,
                "heartbeat did not extend lease beyond {previous_expiry}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let first_renewed_expiry = wait_for_lease_after(initial_expiry);
    assert_eq!(
        expected["periodic_renewals_during_commit_min"].as_u64(),
        Some(1)
    );
    let contender_expiry = first_renewed_expiry
        .checked_add(LEASE_DURATION_MS)
        .expect("contender lease expiry");
    let claim_result = contender.claim_checkpoint(
        &task.task_id,
        1,
        "contender",
        contender_expiry,
        initial_expiry,
    );
    release_tx.send(()).expect("release slow LLM");
    let error = claim_result.expect_err("heartbeat must keep the original claim active");
    assert!(error.to_string().contains("already claimed"));

    let result = worker_thread
        .join()
        .expect("worker thread")
        .expect("worker result");
    assert_eq!(expected["contender_claimed"].as_bool(), Some(false));
    assert_eq!(expected["commit_calls"].as_u64(), Some(1));
    assert_eq!(expected["outcome"].as_str(), Some("success"));
    assert!(result.finished);
    assert_eq!(
        result.result.map(|result| result.status),
        Some(AgentStatus::Completed)
    );
}
