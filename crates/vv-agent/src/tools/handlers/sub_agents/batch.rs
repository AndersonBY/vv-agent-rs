use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::tools::base::{SubTaskRunner, ToolContext};
use crate::types::{AgentStatus, SubTaskRequest, ToolExecutionResult};

use super::request::BatchRequestEntry;
use super::response;

pub(super) fn run_batch_sync(
    context: &mut ToolContext,
    runner: SubTaskRunner,
    total: usize,
    entries: Vec<BatchRequestEntry>,
) -> ToolExecutionResult {
    let mut prepared_requests = Vec::new();
    let mut invalid_results = BTreeMap::new();
    for entry in entries {
        if let Some(request) = entry.request {
            prepared_requests.push((entry.index, request));
        } else {
            invalid_results.insert(
                entry.index,
                json!({
                    "index": entry.index,
                    "status": "failed",
                    "error": entry.error.unwrap_or_else(|| "Invalid task item".to_string()),
                }),
            );
        }
    }

    let outcomes = run_prepared_requests(context, runner, prepared_requests);
    let outcome_map: BTreeMap<_, _> = outcomes.into_iter().collect();
    let mut results = Vec::new();
    let mut completed = 0usize;
    let mut failed = 0usize;
    for index in 0..total {
        if let Some(payload) = invalid_results.remove(&index) {
            failed += 1;
            results.push(payload);
            continue;
        }
        let outcome = outcome_map
            .get(&index)
            .expect("valid sub-task request should have an outcome");
        if outcome.status == AgentStatus::Completed {
            completed += 1;
        } else {
            failed += 1;
        }
        let mut payload = outcome.to_value();
        payload["index"] = Value::Number((index as u64).into());
        results.push(payload);
    }

    let payload = json!({
        "summary": {
            "total": total,
            "completed": completed,
            "failed": failed,
        },
        "results": results,
        "wait_for_completion": true,
    });
    if completed == 0 {
        return response::all_batch_tasks_failed(payload);
    }
    response::success(payload)
}

fn run_prepared_requests(
    context: &mut ToolContext,
    runner: SubTaskRunner,
    prepared_requests: Vec<(usize, SubTaskRequest)>,
) -> Vec<(usize, crate::types::SubTaskOutcome)> {
    if let Some(backend) = context.execution_backend.clone() {
        let runner = runner.clone();
        backend.parallel_map(
            move |(index, request)| (index, runner(request)),
            prepared_requests,
        )
    } else {
        prepared_requests
            .into_iter()
            .map(|(index, request)| (index, runner(request)))
            .collect()
    }
}
