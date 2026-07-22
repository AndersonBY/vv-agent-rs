use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

pub const TOKEN_USAGE_SCHEMA_VERSION: &str = "vv-agent.token-usage.v1";
pub const TASK_TOKEN_USAGE_SCHEMA_VERSION: &str = "vv-agent.task-token-usage.v1";

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

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheUsage {
    pub status: CacheUsageStatus,
    #[serde(deserialize_with = "required_option")]
    pub read_input_tokens: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    pub write_input_tokens: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    pub uncached_input_tokens: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    pub source: Option<String>,
}

impl Default for CacheUsage {
    fn default() -> Self {
        Self {
            status: CacheUsageStatus::AccountingMissing,
            read_input_tokens: None,
            write_input_tokens: None,
            uncached_input_tokens: None,
            source: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub usage_source: UsageSource,
    pub cache_usage: CacheUsage,
    pub provider_usage: Map<String, Value>,
}

impl Default for TokenUsage {
    fn default() -> Self {
        Self {
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            reasoning_tokens: None,
            usage_source: UsageSource::AccountingMissing,
            cache_usage: CacheUsage::default(),
            provider_usage: Map::new(),
        }
    }
}

impl TokenUsage {
    pub fn has_usage(&self) -> bool {
        self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.total_tokens.is_some()
            || self.reasoning_tokens.is_some()
            || self.usage_source != UsageSource::AccountingMissing
            || self.cache_usage.status != CacheUsageStatus::AccountingMissing
    }
}

#[derive(Serialize)]
struct TokenUsageWireRef<'a> {
    schema_version: &'static str,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    usage_source: UsageSource,
    cache_usage: &'a CacheUsage,
    provider_usage: &'a Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenUsageWire {
    schema_version: String,
    #[serde(deserialize_with = "required_option")]
    input_tokens: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    output_tokens: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    total_tokens: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    reasoning_tokens: Option<u64>,
    usage_source: UsageSource,
    cache_usage: CacheUsage,
    provider_usage: Map<String, Value>,
}

impl Serialize for TokenUsage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        TokenUsageWireRef {
            schema_version: TOKEN_USAGE_SCHEMA_VERSION,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            total_tokens: self.total_tokens,
            reasoning_tokens: self.reasoning_tokens,
            usage_source: self.usage_source,
            cache_usage: &self.cache_usage,
            provider_usage: &self.provider_usage,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TokenUsage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TokenUsageWire::deserialize(deserializer)?;
        if wire.schema_version != TOKEN_USAGE_SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "unsupported TokenUsage schema: {:?}",
                wire.schema_version
            )));
        }
        Ok(Self {
            input_tokens: wire.input_tokens,
            output_tokens: wire.output_tokens,
            total_tokens: wire.total_tokens,
            reasoning_tokens: wire.reasoning_tokens,
            usage_source: wire.usage_source,
            cache_usage: wire.cache_usage,
            provider_usage: wire.provider_usage,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CycleTokenUsage {
    pub cycle_index: u32,
    pub usage: TokenUsage,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CycleTokenUsageWire {
    cycle_index: u32,
    usage: TokenUsage,
}

impl<'de> Deserialize<'de> for CycleTokenUsage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CycleTokenUsageWire::deserialize(deserializer)?;
        if wire.cycle_index == 0 {
            return Err(serde::de::Error::custom("cycle_index must be positive"));
        }
        Ok(Self {
            cycle_index: wire.cycle_index,
            usage: wire.usage,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cache_usage: CacheUsage,
    pub cycles: Vec<CycleTokenUsage>,
}

impl Default for TaskTokenUsage {
    fn default() -> Self {
        Self {
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            reasoning_tokens: None,
            cache_usage: CacheUsage {
                source: Some("aggregate".to_string()),
                ..CacheUsage::default()
            },
            cycles: Vec::new(),
        }
    }
}

impl TaskTokenUsage {
    pub fn add_cycle(&mut self, cycle_index: u32, usage: TokenUsage) {
        assert!(cycle_index > 0, "cycle_index must be positive");
        self.cycles.push(CycleTokenUsage { cycle_index, usage });
        self.input_tokens = complete_sum(&self.cycles, |usage| usage.input_tokens);
        self.output_tokens = complete_sum(&self.cycles, |usage| usage.output_tokens);
        self.total_tokens = complete_sum(&self.cycles, |usage| usage.total_tokens);
        self.reasoning_tokens = complete_sum(&self.cycles, |usage| usage.reasoning_tokens);
        self.cache_usage = aggregate_cache_usage(&self.cycles);
    }
}

fn complete_sum(cycles: &[CycleTokenUsage], read: fn(&TokenUsage) -> Option<u64>) -> Option<u64> {
    if cycles.is_empty() {
        return None;
    }
    cycles
        .iter()
        .try_fold(0_u64, |total, cycle| total.checked_add(read(&cycle.usage)?))
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

    let complete_cache_sum = |read: fn(&CacheUsage) -> Option<u64>| -> Option<u64> {
        if status != CacheUsageStatus::ProviderReported {
            return None;
        }
        cycles.iter().try_fold(0_u64, |total, cycle| {
            total.checked_add(read(&cycle.usage.cache_usage)?)
        })
    };

    CacheUsage {
        status,
        read_input_tokens: complete_cache_sum(|usage| usage.read_input_tokens),
        write_input_tokens: complete_cache_sum(|usage| usage.write_input_tokens),
        uncached_input_tokens: complete_cache_sum(|usage| usage.uncached_input_tokens),
        source: Some("aggregate".to_string()),
    }
}

#[derive(Serialize)]
struct TaskTokenUsageWireRef<'a> {
    schema_version: &'static str,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    cache_usage: &'a CacheUsage,
    cycles: &'a [CycleTokenUsage],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskTokenUsageWire {
    schema_version: String,
    #[serde(deserialize_with = "required_option")]
    input_tokens: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    output_tokens: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    total_tokens: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    reasoning_tokens: Option<u64>,
    cache_usage: CacheUsage,
    cycles: Vec<CycleTokenUsage>,
}

impl Serialize for TaskTokenUsage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        TaskTokenUsageWireRef {
            schema_version: TASK_TOKEN_USAGE_SCHEMA_VERSION,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            total_tokens: self.total_tokens,
            reasoning_tokens: self.reasoning_tokens,
            cache_usage: &self.cache_usage,
            cycles: &self.cycles,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TaskTokenUsage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TaskTokenUsageWire::deserialize(deserializer)?;
        if wire.schema_version != TASK_TOKEN_USAGE_SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "unsupported TaskTokenUsage schema: {:?}",
                wire.schema_version
            )));
        }
        let mut expected = Self::default();
        for cycle in wire.cycles {
            expected.add_cycle(cycle.cycle_index, cycle.usage);
        }
        if wire.input_tokens != expected.input_tokens
            || wire.output_tokens != expected.output_tokens
            || wire.total_tokens != expected.total_tokens
            || wire.reasoning_tokens != expected.reasoning_tokens
            || wire.cache_usage != expected.cache_usage
        {
            return Err(serde::de::Error::custom(
                "TaskTokenUsage aggregate does not match cycle usage",
            ));
        }
        Ok(expected)
    }
}
