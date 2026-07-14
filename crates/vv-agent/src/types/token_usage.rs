use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    ProviderReported,
    Estimated,
    #[default]
    AccountingMissing,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheUsageStatus {
    ProviderReported,
    #[default]
    AccountingMissing,
    Unsupported,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheUsage {
    pub status: CacheUsageStatus,
    pub read_tokens: Option<u64>,
    pub write_tokens: Option<u64>,
    pub uncached_input_tokens: Option<u64>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub usage_source: UsageSource,
    pub cache_usage: CacheUsage,
    pub raw: Value,
}

impl Default for TokenUsage {
    fn default() -> Self {
        Self {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            cached_tokens: 0,
            reasoning_tokens: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            usage_source: UsageSource::AccountingMissing,
            cache_usage: CacheUsage::default(),
            raw: Value::Object(Map::new()),
        }
    }
}

impl TokenUsage {
    pub fn has_usage(&self) -> bool {
        self.prompt_tokens > 0
            || self.completion_tokens > 0
            || self.total_tokens > 0
            || self.cached_tokens > 0
            || self.reasoning_tokens > 0
            || self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_creation_tokens > 0
            || self.usage_source != UsageSource::AccountingMissing
            || self.cache_usage.status != CacheUsageStatus::AccountingMissing
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CycleTokenUsage {
    pub cycle_index: u32,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskTokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_usage: CacheUsage,
    pub cycles: Vec<CycleTokenUsage>,
}

impl TaskTokenUsage {
    pub fn add_cycle(&mut self, cycle_index: u32, usage: TokenUsage) {
        if !usage.has_usage() {
            return;
        }
        self.prompt_tokens += usage.prompt_tokens;
        self.completion_tokens += usage.completion_tokens;
        self.total_tokens += usage.total_tokens;
        self.cached_tokens += usage.cached_tokens;
        self.reasoning_tokens += usage.reasoning_tokens;
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.cache_creation_tokens += usage.cache_creation_tokens;
        self.cycles.push(CycleTokenUsage { cycle_index, usage });
        self.cache_usage = aggregate_cache_usage(&self.cycles);
    }
}

fn aggregate_cache_usage(cycles: &[CycleTokenUsage]) -> CacheUsage {
    if cycles.is_empty() {
        return CacheUsage::default();
    }
    let status = if cycles
        .iter()
        .all(|cycle| cycle.usage.cache_usage.status == CacheUsageStatus::ProviderReported)
    {
        CacheUsageStatus::ProviderReported
    } else if cycles
        .iter()
        .all(|cycle| cycle.usage.cache_usage.status == CacheUsageStatus::Unsupported)
    {
        CacheUsageStatus::Unsupported
    } else {
        CacheUsageStatus::AccountingMissing
    };

    let complete_sum = |read: fn(&CacheUsage) -> Option<u64>| -> Option<u64> {
        if status != CacheUsageStatus::ProviderReported {
            return None;
        }
        cycles.iter().try_fold(0_u64, |total, cycle| {
            total.checked_add(read(&cycle.usage.cache_usage)?)
        })
    };

    CacheUsage {
        status,
        read_tokens: complete_sum(|usage| usage.read_tokens),
        write_tokens: complete_sum(|usage| usage.write_tokens),
        uncached_input_tokens: complete_sum(|usage| usage.uncached_input_tokens),
        source: Some("aggregate".to_string()),
    }
}
