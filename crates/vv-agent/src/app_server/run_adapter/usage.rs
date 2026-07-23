use crate::app_server::protocol::{AppCacheUsage, AppModelCallUsage, AppModelUsage, AppTokenUsage};
use crate::types::{
    CacheUsageStatus, ModelCallOperation, ModelCallStatus, TaskTokenUsage, TokenUsage, UsageSource,
};

pub(super) fn app_token_usage(usage: &TaskTokenUsage) -> AppTokenUsage {
    AppTokenUsage {
        schema_version: "vv-agent.task-token-usage.v2".to_string(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        cache_usage: AppCacheUsage {
            status: cache_usage_status(usage.cache_usage.status).to_string(),
            read_input_tokens: usage.cache_usage.read_input_tokens,
            write_input_tokens: usage.cache_usage.write_input_tokens,
            uncached_input_tokens: usage.cache_usage.uncached_input_tokens,
            source: usage.cache_usage.source.clone(),
        },
        model_calls: usage
            .model_calls
            .iter()
            .map(|model_call| AppModelCallUsage {
                call_id: model_call.call_id.clone(),
                operation_id: model_call.operation_id.clone(),
                attempt: model_call.attempt,
                operation: match model_call.operation {
                    ModelCallOperation::AgentCycle => "agent_cycle",
                    ModelCallOperation::SessionMemory => "session_memory",
                    ModelCallOperation::MemoryCompaction => "memory_compaction",
                }
                .to_string(),
                cycle_index: model_call.cycle_index,
                backend: model_call.backend.clone(),
                model: model_call.model.clone(),
                status: match model_call.status {
                    ModelCallStatus::Completed => "completed",
                    ModelCallStatus::Failed => "failed",
                    ModelCallStatus::Ambiguous => "ambiguous",
                }
                .to_string(),
                usage: app_model_usage(&model_call.usage),
                error_code: model_call.error_code.clone(),
            })
            .collect(),
    }
}

fn app_model_usage(usage: &TokenUsage) -> AppModelUsage {
    AppModelUsage {
        schema_version: "vv-agent.token-usage.v1".to_string(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        usage_source: match usage.usage_source {
            UsageSource::ProviderReported => "provider_reported",
            UsageSource::Estimated => "estimated",
            UsageSource::AccountingMissing => "accounting_missing",
        }
        .to_string(),
        cache_usage: AppCacheUsage {
            status: cache_usage_status(usage.cache_usage.status).to_string(),
            read_input_tokens: usage.cache_usage.read_input_tokens,
            write_input_tokens: usage.cache_usage.write_input_tokens,
            uncached_input_tokens: usage.cache_usage.uncached_input_tokens,
            source: usage.cache_usage.source.clone(),
        },
        provider_usage: usage.provider_usage.clone().into_iter().collect(),
    }
}

fn cache_usage_status(status: CacheUsageStatus) -> &'static str {
    match status {
        CacheUsageStatus::ProviderReported => "provider_reported",
        CacheUsageStatus::AccountingMissing => "accounting_missing",
        CacheUsageStatus::Unsupported => "unsupported",
    }
}
