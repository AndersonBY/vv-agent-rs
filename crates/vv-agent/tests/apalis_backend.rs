#![cfg(feature = "apalis")]

use apalis::prelude::{Extensions, RandomId, Task};
use serde_json::json;
use vv_agent::runtime::backends::distributed::{
    apalis::{run_apalis_cycle_job, ApalisCycleJob},
    DistributedRunEnvelope, DEFAULT_LEASE_DURATION_MS,
};
use vv_agent::runtime::backends::{CycleDispatchResult, RuntimeRecipe};
use vv_agent::{AgentTask, Message, RunBudgetLimits};

#[tokio::test]
async fn apalis_cycle_job_round_trips_through_apalis_task() {
    let job = ApalisCycleJob::new(
        AgentTask::new("apalis-cycle", "model", "system", "prompt"),
        RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", "."),
        "vv_agent.distributed.run_single_cycle",
        7,
    );
    let mut wire = serde_json::to_value(&job).expect("serialize Apalis job");
    wire["task"] = json!({
        "task_id": "apalis-cycle",
        "model": "model",
        "system_prompt": "system",
        "user_prompt": "prompt"
    });
    let decoded: ApalisCycleJob =
        serde_json::from_value(wire).expect("deserialize sparse Apalis job");

    let task: Task<ApalisCycleJob, Extensions, RandomId> = Task::new(decoded);
    let restored = ApalisCycleJob::from_apalis_task(task);

    assert_eq!(restored, job);
}

#[tokio::test]
async fn apalis_cycle_job_handler_returns_dispatch_result() {
    let job = ApalisCycleJob::new(
        AgentTask::new("apalis-handler", "model", "system", "prompt"),
        RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", "."),
        "vv_agent.distributed.run_single_cycle",
        2,
    );

    let result = run_apalis_cycle_job(job, |job| {
        assert_eq!(job.envelope.cycle_index, 2);
        Ok(CycleDispatchResult::finished(
            vv_agent::AgentResult::completed(vec![Message::assistant("done")], Vec::new(), "ok"),
        ))
    })
    .await
    .expect("apalis cycle handler");

    assert!(result.finished);
    assert_eq!(
        result.result.and_then(|result| result.final_answer),
        Some("ok".to_string())
    );
}

#[test]
fn apalis_task_round_trip_preserves_distributed_budget_limits() {
    let limits = RunBudgetLimits::builder()
        .max_total_tokens(4_096)
        .max_tool_calls(7)
        .build()
        .expect("valid run budget");
    let envelope = DistributedRunEnvelope::for_cycle(
        AgentTask::new("apalis-budget", "model", "system", "prompt"),
        RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", "."),
        3,
        "vv_agent.distributed.run_single_cycle",
        None,
        None,
        DEFAULT_LEASE_DURATION_MS,
        Some(limits.clone()),
    )
    .expect("valid distributed envelope");
    let task: Task<ApalisCycleJob, Extensions, RandomId> =
        Task::new(ApalisCycleJob::from_envelope(envelope));

    let restored = ApalisCycleJob::from_apalis_task(task);

    assert_eq!(restored.envelope.budget_limits, Some(limits));
}
