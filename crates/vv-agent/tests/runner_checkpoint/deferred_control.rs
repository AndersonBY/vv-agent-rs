use super::*;

#[tokio::test]
async fn deferred_controller_preserves_suspension_receipts_and_cancel_evidence() {
    use vv_agent::{
        ControllerCommand, ControllerCommandResolution, ControllerCommandVariant, ControllerHandle,
        DeferredResolveDecision, RedisCheckpointStore, SqliteCheckpointStore,
    };
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/parity/controller_command.json")).unwrap();
    for store_kind in ["memory", "sqlite", "redis"] {
        let redis_url = std::env::var("VV_AGENT_TEST_REDIS_URL").ok();
        if store_kind == "redis" && redis_url.is_none() {
            continue;
        }
        for case in fixture["deferred_control_cases"].as_array().unwrap() {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("deferred-control.sqlite");
            let mut store: Arc<dyn CheckpointStore> = match store_kind {
                "sqlite" => Arc::new(SqliteCheckpointStore::new(&path).unwrap()),
                "redis" => {
                    Arc::new(RedisCheckpointStore::new(redis_url.as_deref().unwrap()).unwrap())
                }
                _ => Arc::new(InMemoryCheckpointStore::new()),
            };
            let key = format!("deferred-control-{}", uuid::Uuid::new_v4());
            let calls = Arc::new(AtomicUsize::new(0));
            let calls_observed = calls.clone();
            let tool = StaticTool::new(
                "remote",
                "Start remote work.",
                json!({"type":"object", "properties":{}, "required":[], "additionalProperties":false}),
                Arc::new(move |context, _| {
                    calls_observed.fetch_add(1, Ordering::SeqCst);
                    let _ = context.defer();
                    ToolExecutionResult::success(context.tool_call_id.clone(), "pending")
                }),
            );
            let runner = Runner::builder()
                .model_provider(ScriptedModelProvider::new(
                    "scripted",
                    "test-model",
                    vec![LLMResponse::with_tool_calls(
                        "run",
                        (0..2)
                            .map(|index| {
                                ToolCall::new(format!("call-{index}"), "remote", BTreeMap::new())
                            })
                            .collect(),
                    )],
                ))
                .workspace(".")
                .build()
                .unwrap();
            let agent = Agent::builder("deferred-control")
                .instructions("Run remote work.")
                .model(ModelRef::named("test-model"))
                .tool(tool)
                .build()
                .unwrap();
            let mut config = CheckpointConfig::new(store.clone());
            config.key = Some(key.clone());
            config.resume_policy = ResumePolicy::ResumeIfPresent;
            let result = runner
                .run_with_config(
                    &agent,
                    "run",
                    RunConfig::builder()
                        .max_cycles(1)
                        .checkpoint_config(config)
                        .build(),
                )
                .await
                .unwrap();
            assert_eq!(result.status(), AgentStatus::Deferred);
            let initial = store.load_checkpoint(&key).unwrap().unwrap();
            let handles = initial
                .tool_journal
                .iter()
                .map(|entry| entry.deferred_handle.clone().unwrap())
                .collect::<Vec<_>>();
            let replies = (0..2)
                .map(|index| {
                    ToolExecutionResult::success(
                        format!("call-{index}"),
                        format!("结果 {index} https://example.invalid/?token=保留"),
                    )
                })
                .collect::<Vec<_>>();
            let resolved = case["resolve_while_suspended"].as_u64().unwrap() as usize;
            let mut commands = Vec::new();
            let mut last_wake = false;
            for kind in case["commands"].as_array().unwrap() {
                let kind = kind.as_str().unwrap();
                let current = store.load_checkpoint(&key).unwrap().unwrap();
                let variant = match kind {
                    "suspend" => ControllerCommandVariant::Suspend,
                    "resume" => ControllerCommandVariant::Resume,
                    "cancel" => ControllerCommandVariant::Cancel,
                    _ => panic!("invalid control fixture"),
                };
                let command = ControllerCommand::new(
                    format!("{key}-{kind}"),
                    ControllerHandle::new(&key, &current.root_run_id, &current.trace_id).unwrap(),
                    current.resume_attempt,
                    current.revision,
                    variant,
                )
                .unwrap();
                let ControllerCommandResolution::Applied { wake, .. } =
                    store.resolve_controller_command(command.clone()).unwrap()
                else {
                    panic!("control must apply")
                };
                last_wake = wake.action == "recovery_dispatch";
                commands.push(command.clone());
                store = match store_kind {
                    "sqlite" => Arc::new(SqliteCheckpointStore::new(&path).unwrap()),
                    "redis" => {
                        Arc::new(RedisCheckpointStore::new(redis_url.as_deref().unwrap()).unwrap())
                    }
                    _ => store,
                };
                let saved = store.load_checkpoint(&key).unwrap().unwrap();
                assert!(matches!(
                    store.resolve_controller_command(command).unwrap(),
                    ControllerCommandResolution::Replayed { .. }
                ));
                assert_eq!(store.load_checkpoint(&key).unwrap().unwrap(), saved);
                if kind == "suspend" {
                    assert_eq!(saved.suspended_origin.as_ref().unwrap().status, "deferred");
                    assert!(!last_wake);
                    for index in 0..resolved {
                        assert!(matches!(
                            store
                                .resolve_deferred(handles[index].clone(), replies[index].clone())
                                .unwrap(),
                            DeferredResolveDecision::AppliedWaiting { .. }
                        ));
                        let waiting = store.load_checkpoint(&key).unwrap().unwrap();
                        assert_eq!(waiting.status, CheckpointStatus::Suspended);
                        assert_eq!(waiting.suspended_origin, saved.suspended_origin);
                        assert!(waiting.claim_token.is_none());
                        assert!(matches!(
                            store
                                .resolve_deferred(handles[index].clone(), replies[index].clone())
                                .unwrap(),
                            DeferredResolveDecision::Replayed { .. }
                        ));
                        assert_eq!(store.load_checkpoint(&key).unwrap().unwrap(), waiting);
                    }
                }
            }
            let current = store.load_checkpoint(&key).unwrap().unwrap();
            assert_eq!(
                current.status.as_str(),
                case["resulting_status"].as_str().unwrap()
            );
            assert_eq!(last_wake, case["wake"].as_bool().unwrap());
            assert_eq!(current.cycle_index, initial.cycle_index);
            assert!(current.claim_token.is_none());
            if case["commands"].as_array().unwrap().last().unwrap() == "cancel" {
                let terminal = current.terminal_result.as_ref().unwrap();
                assert_eq!(terminal["completion_reason"], "cancelled");
                let observations = terminal["resume_observations"].as_array().unwrap();
                assert_eq!(observations.len(), 2 - resolved);
                assert!(observations
                    .iter()
                    .all(|value| value["state"] == "ambiguous"
                        && value["risk"] == "unknown_tool_side_effect"));
                assert_eq!(
                    current
                        .event_outbox
                        .iter()
                        .filter(|entry| entry.event["type"] == "cycle_aborted")
                        .count(),
                    1
                );
                for index in 0..2 {
                    let resolution =
                        store.resolve_deferred(handles[index].clone(), replies[index].clone());
                    if index < resolved {
                        assert!(matches!(
                            resolution.unwrap(),
                            DeferredResolveDecision::Replayed { .. }
                        ));
                    } else {
                        assert_eq!(resolution.unwrap_err().code(), "deferred_resolution_stale");
                    }
                }
                assert_eq!(store.load_checkpoint(&key).unwrap().unwrap(), current);
            } else if last_wake {
                for (entry, reply) in current.tool_journal.iter().zip(&replies) {
                    assert_eq!(entry.result.as_ref().unwrap()["content"], reply.content);
                }
            }
            for command in commands {
                assert!(matches!(
                    store.resolve_controller_command(command).unwrap(),
                    ControllerCommandResolution::Replayed { .. }
                ));
            }
            assert_eq!(store.load_checkpoint(&key).unwrap().unwrap(), current);
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        }
    }
}
