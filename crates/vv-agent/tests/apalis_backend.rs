#![cfg(feature = "apalis")]

use apalis::prelude::{Extensions, RandomId, Task};
use vv_agent::runtime::backends::distributed::apalis::{run_apalis_cycle_job, ApalisCycleJob};
use vv_agent::runtime::backends::{CycleDispatchResult, RuntimeRecipe};
use vv_agent::{AgentTask, Message};

#[tokio::test]
async fn apalis_cycle_job_round_trips_through_apalis_task() {
    let job = ApalisCycleJob::new(
        AgentTask::new("apalis-cycle", "model", "system", "prompt"),
        RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", "."),
        "vv_agent.distributed.run_single_cycle",
        7,
    );

    let task: Task<ApalisCycleJob, Extensions, RandomId> = Task::new(job.clone());
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
        assert_eq!(job.cycle_index, 2);
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
