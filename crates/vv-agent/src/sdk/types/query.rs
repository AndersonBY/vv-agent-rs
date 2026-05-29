use crate::types::AgentStatus;

use super::run::{agent_status_value, AgentRun};

pub(crate) fn query_text_from_run(
    run: AgentRun,
    require_completed: bool,
    error_prefix: &str,
) -> Result<String, String> {
    if run.result.status == AgentStatus::Completed {
        return Ok(run.result.final_answer.unwrap_or_default());
    }
    if require_completed {
        let reason = run
            .result
            .error
            .clone()
            .or(run.result.wait_reason.clone())
            .or(run.result.final_answer.clone())
            .unwrap_or_else(|| "query did not complete successfully".to_string());
        return Err(format!(
            "{error_prefix} with status={}: {}",
            agent_status_value(run.result.status),
            reason
        ));
    }
    Ok(run
        .result
        .final_answer
        .or(run.result.wait_reason)
        .or(run.result.error)
        .unwrap_or_default())
}
