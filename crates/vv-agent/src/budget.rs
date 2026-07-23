use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

mod wire;

use crate::types::{CacheUsageStatus, TokenUsage, UsageSource};

pub const MAX_WIRE_INTEGER: u64 = (1_u64 << 53) - 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableMetricPolicy {
    #[default]
    ContinueAndMark,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDimension {
    TotalTokens,
    UncachedInputTokens,
    ToolCalls,
    ToolCallsByName,
    WallTime,
    HostCost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetEnforcementBoundary {
    RunStart,
    CycleStart,
    ModelCallComplete,
    ToolBatchPreflight,
    ToolBatchComplete,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetExhaustionReason {
    LimitReached,
    LimitExceeded,
    MetricUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetUnavailableReason {
    UsageMissing,
    MeterMissing,
    MeterUnavailable,
    MeterError,
    UnitMismatch,
    CurrencyMismatch,
    NonMonotonic,
    IntegerOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostCost {
    pub unit: String,
    pub currency: Option<String>,
    pub amount_microunits: u64,
}

impl HostCost {
    pub fn new(unit: impl Into<String>, amount_microunits: u64) -> Result<Self, String> {
        let cost = Self {
            unit: unit.into(),
            currency: None,
            amount_microunits,
        };
        cost.validate()?;
        Ok(cost)
    }

    pub fn with_currency(mut self, currency: impl Into<String>) -> Result<Self, String> {
        self.currency = Some(currency.into());
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> Result<(), String> {
        validate_non_empty(&self.unit, "host cost unit")?;
        if let Some(currency) = &self.currency {
            validate_non_empty(currency, "host cost currency")?;
        }
        validate_wire_integer(self.amount_microunits, "host cost amount_microunits")
    }
}

pub trait HostCostMeter: Send + Sync {
    fn read(&self) -> Result<Option<HostCost>, String>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BudgetUnavailableDimension {
    pub dimension: BudgetDimension,
    pub reason: BudgetUnavailableReason,
    pub expected_unit: Option<String>,
    pub observed_unit: Option<String>,
    pub expected_currency: Option<String>,
    pub observed_currency: Option<String>,
}

impl BudgetUnavailableDimension {
    fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("expected_unit", self.expected_unit.as_deref()),
            ("observed_unit", self.observed_unit.as_deref()),
            ("expected_currency", self.expected_currency.as_deref()),
            ("observed_currency", self.observed_currency.as_deref()),
        ] {
            if let Some(value) = value {
                validate_non_empty(value, field)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RunBudgetLimits {
    pub max_total_tokens: Option<u64>,
    pub max_uncached_input_tokens: Option<u64>,
    pub max_tool_calls: Option<u64>,
    pub max_tool_calls_by_name: BTreeMap<String, u64>,
    pub max_wall_time_ms: Option<u64>,
    pub max_host_cost: Option<HostCost>,
    pub unavailable_metric_policy: UnavailableMetricPolicy,
}

impl RunBudgetLimits {
    pub fn builder() -> RunBudgetLimitsBuilder {
        RunBudgetLimitsBuilder::default()
    }

    pub fn has_limits(&self) -> bool {
        self.max_total_tokens.is_some()
            || self.max_uncached_input_tokens.is_some()
            || self.max_tool_calls.is_some()
            || !self.max_tool_calls_by_name.is_empty()
            || self.max_wall_time_ms.is_some()
            || self.max_host_cost.is_some()
    }

    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("max_total_tokens", self.max_total_tokens),
            ("max_uncached_input_tokens", self.max_uncached_input_tokens),
            ("max_tool_calls", self.max_tool_calls),
            ("max_wall_time_ms", self.max_wall_time_ms),
        ] {
            if let Some(value) = value {
                validate_wire_integer(value, field)?;
            }
        }
        for (name, value) in &self.max_tool_calls_by_name {
            validate_non_empty(name, "named tool budget key")?;
            validate_wire_integer(*value, &format!("max_tool_calls_by_name[{name:?}]"))?;
        }
        if let Some(cost) = &self.max_host_cost {
            cost.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct RunBudgetLimitsBuilder {
    limits: RunBudgetLimits,
}

impl RunBudgetLimitsBuilder {
    pub fn max_total_tokens(mut self, value: u64) -> Self {
        self.limits.max_total_tokens = Some(value);
        self
    }

    pub fn max_uncached_input_tokens(mut self, value: u64) -> Self {
        self.limits.max_uncached_input_tokens = Some(value);
        self
    }

    pub fn max_tool_calls(mut self, value: u64) -> Self {
        self.limits.max_tool_calls = Some(value);
        self
    }

    pub fn max_tool_calls_by_name(
        mut self,
        values: impl IntoIterator<Item = (impl Into<String>, u64)>,
    ) -> Self {
        self.limits.max_tool_calls_by_name = values
            .into_iter()
            .map(|(name, value)| (name.into(), value))
            .collect();
        self
    }

    pub fn max_wall_time_ms(mut self, value: u64) -> Self {
        self.limits.max_wall_time_ms = Some(value);
        self
    }

    pub fn max_host_cost(mut self, value: HostCost) -> Self {
        self.limits.max_host_cost = Some(value);
        self
    }

    pub fn unavailable_metric_policy(mut self, value: UnavailableMetricPolicy) -> Self {
        self.limits.unavailable_metric_policy = value;
        self
    }

    pub fn build(self) -> Result<RunBudgetLimits, String> {
        self.limits.validate()?;
        Ok(self.limits)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BudgetUsageSnapshot {
    pub cycles: u64,
    pub total_tokens: Option<u64>,
    pub uncached_input_tokens: Option<u64>,
    pub tool_calls: u64,
    pub tool_calls_by_name: BTreeMap<String, u64>,
    pub elapsed_ms: u64,
    pub host_cost: Option<HostCost>,
    pub unavailable_dimensions: Vec<BudgetUnavailableDimension>,
}

impl Default for BudgetUsageSnapshot {
    fn default() -> Self {
        Self {
            cycles: 0,
            total_tokens: Some(0),
            uncached_input_tokens: Some(0),
            tool_calls: 0,
            tool_calls_by_name: BTreeMap::new(),
            elapsed_ms: 0,
            host_cost: None,
            unavailable_dimensions: Vec::new(),
        }
    }
}

impl BudgetUsageSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        validate_wire_integer(self.cycles, "budget usage cycles")?;
        if let Some(value) = self.total_tokens {
            validate_wire_integer(value, "budget usage total_tokens")?;
        }
        if let Some(value) = self.uncached_input_tokens {
            validate_wire_integer(value, "budget usage uncached_input_tokens")?;
        }
        validate_wire_integer(self.tool_calls, "budget usage tool_calls")?;
        validate_wire_integer(self.elapsed_ms, "budget usage elapsed_ms")?;
        for (name, value) in &self.tool_calls_by_name {
            validate_non_empty(name, "budget usage tool name")?;
            validate_wire_integer(
                *value,
                &format!("budget usage tool_calls_by_name[{name:?}]"),
            )?;
        }
        if let Some(cost) = &self.host_cost {
            cost.validate()?;
        }
        let mut dimensions = self
            .unavailable_dimensions
            .iter()
            .map(|observation| observation.dimension)
            .collect::<Vec<_>>();
        dimensions.sort();
        dimensions.dedup();
        if dimensions.len() != self.unavailable_dimensions.len() {
            return Err("budget unavailable dimensions must be unique".to_string());
        }
        for observation in &self.unavailable_dimensions {
            observation.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BudgetExhaustion {
    pub dimension: BudgetDimension,
    pub tool_name: Option<String>,
    pub reason: BudgetExhaustionReason,
    pub limit: u64,
    pub observed: Option<u64>,
    pub attempted_increment: Option<u64>,
    pub overshoot: Option<u64>,
    pub unit: String,
    pub currency: Option<String>,
    pub enforcement_boundary: BudgetEnforcementBoundary,
    pub unavailable_reason: Option<BudgetUnavailableReason>,
}

impl BudgetExhaustion {
    fn validate(&self) -> Result<(), String> {
        validate_wire_integer(self.limit, "budget exhaustion limit")?;
        for (field, value) in [
            ("observed", self.observed),
            ("attempted_increment", self.attempted_increment),
            ("overshoot", self.overshoot),
        ] {
            if let Some(value) = value {
                validate_wire_integer(value, &format!("budget exhaustion {field}"))?;
            }
        }
        validate_non_empty(&self.unit, "budget exhaustion unit")?;
        if let Some(tool_name) = &self.tool_name {
            validate_non_empty(tool_name, "tool_name")?;
        }
        if let Some(currency) = &self.currency {
            validate_non_empty(currency, "currency")?;
        }
        Ok(())
    }
}

pub type MonotonicClock = Arc<dyn Fn() -> u128 + Send + Sync>;

pub struct BudgetEvaluator {
    pub limits: RunBudgetLimits,
    host_cost_meter: Option<Arc<dyn HostCostMeter>>,
    clock_ns: MonotonicClock,
    started_ns: u128,
    cycles: u64,
    total_tokens: Option<u64>,
    uncached_input_tokens: Option<u64>,
    tool_calls: u64,
    tool_calls_by_name: BTreeMap<String, u64>,
    base_elapsed_ms: u64,
    elapsed_ms: u64,
    host_cost: Option<HostCost>,
    unavailable: BTreeMap<BudgetDimension, BudgetUnavailableDimension>,
}

impl BudgetEvaluator {
    pub fn new(
        limits: RunBudgetLimits,
        host_cost_meter: Option<Arc<dyn HostCostMeter>>,
        initial_usage: Option<BudgetUsageSnapshot>,
    ) -> Result<Self, String> {
        let started = Instant::now();
        Self::with_clock(
            limits,
            host_cost_meter,
            initial_usage,
            Arc::new(move || started.elapsed().as_nanos()),
        )
    }

    pub fn with_clock(
        limits: RunBudgetLimits,
        host_cost_meter: Option<Arc<dyn HostCostMeter>>,
        initial_usage: Option<BudgetUsageSnapshot>,
        clock_ns: MonotonicClock,
    ) -> Result<Self, String> {
        limits.validate()?;
        if !limits.has_limits() {
            return Err("BudgetEvaluator requires at least one configured limit".to_string());
        }
        let usage = initial_usage.unwrap_or_default();
        usage.validate()?;
        let unavailable = usage
            .unavailable_dimensions
            .iter()
            .cloned()
            .map(|item| (item.dimension, item))
            .collect();
        let started_ns = clock_ns();
        Ok(Self {
            limits,
            host_cost_meter,
            clock_ns,
            started_ns,
            cycles: usage.cycles,
            total_tokens: usage.total_tokens,
            uncached_input_tokens: usage.uncached_input_tokens,
            tool_calls: usage.tool_calls,
            tool_calls_by_name: usage.tool_calls_by_name,
            base_elapsed_ms: usage.elapsed_ms,
            elapsed_ms: usage.elapsed_ms,
            host_cost: usage.host_cost,
            unavailable,
        })
    }

    pub fn snapshot(&self) -> BudgetUsageSnapshot {
        let mut unavailable_dimensions = self.unavailable.values().cloned().collect::<Vec<_>>();
        unavailable_dimensions.sort_by_key(|item| dimension_precedence(item.dimension));
        BudgetUsageSnapshot {
            cycles: self.cycles,
            total_tokens: self.total_tokens,
            uncached_input_tokens: self.uncached_input_tokens,
            tool_calls: self.tool_calls,
            tool_calls_by_name: self.tool_calls_by_name.clone(),
            elapsed_ms: self.elapsed_ms,
            host_cost: self.host_cost.clone(),
            unavailable_dimensions,
        }
    }

    pub fn run_start(&mut self) -> Option<BudgetExhaustion> {
        let boundary = BudgetEnforcementBoundary::RunStart;
        self.observe_boundary();
        self.strict_unavailable(boundary).or_else(|| {
            self.check_admission_limits(
                boundary,
                &[BudgetDimension::WallTime, BudgetDimension::HostCost],
            )
        })
    }

    pub fn cycle_start(&mut self) -> Option<BudgetExhaustion> {
        let boundary = BudgetEnforcementBoundary::CycleStart;
        self.observe_boundary();
        self.strict_unavailable(boundary).or_else(|| {
            self.check_admission_limits(
                boundary,
                &[
                    BudgetDimension::WallTime,
                    BudgetDimension::TotalTokens,
                    BudgetDimension::UncachedInputTokens,
                    BudgetDimension::HostCost,
                ],
            )
        })
    }

    pub fn model_call_complete(&mut self, usage: &TokenUsage) -> Option<BudgetExhaustion> {
        let boundary = BudgetEnforcementBoundary::ModelCallComplete;
        self.cycles = self.cycles.saturating_add(1);
        self.observe_token_usage(usage);
        self.observe_boundary();
        self.strict_unavailable(boundary).or_else(|| {
            self.check_exceeded_limits(
                boundary,
                &[
                    BudgetDimension::WallTime,
                    BudgetDimension::TotalTokens,
                    BudgetDimension::UncachedInputTokens,
                    BudgetDimension::HostCost,
                ],
            )
        })
    }

    pub fn preflight_tools(&mut self, tool_names: &[String]) -> Option<BudgetExhaustion> {
        let boundary = BudgetEnforcementBoundary::ToolBatchPreflight;
        self.observe_boundary();
        if let Some(exhaustion) = self.strict_unavailable(boundary).or_else(|| {
            self.check_admission_limits(
                boundary,
                &[BudgetDimension::WallTime, BudgetDimension::HostCost],
            )
        }) {
            return Some(exhaustion);
        }

        let increment = u64::try_from(tool_names.len()).unwrap_or(u64::MAX);
        if let Some(limit) = self.limits.max_tool_calls {
            let projected = self.tool_calls.saturating_add(increment);
            if projected > limit {
                return Some(count_preflight_exhaustion(
                    BudgetDimension::ToolCalls,
                    None,
                    limit,
                    self.tool_calls,
                    increment,
                    boundary,
                ));
            }
        }

        let mut batch_by_name = BTreeMap::<String, u64>::new();
        for name in tool_names {
            *batch_by_name.entry(name.clone()).or_default() += 1;
        }
        for (name, limit) in &self.limits.max_tool_calls_by_name {
            let increment = batch_by_name.get(name).copied().unwrap_or_default();
            if increment == 0 {
                continue;
            }
            let observed = self
                .tool_calls_by_name
                .get(name)
                .copied()
                .unwrap_or_default();
            if observed.saturating_add(increment) > *limit {
                return Some(count_preflight_exhaustion(
                    BudgetDimension::ToolCallsByName,
                    Some(name.clone()),
                    *limit,
                    observed,
                    increment,
                    boundary,
                ));
            }
        }

        self.tool_calls = self.tool_calls.saturating_add(increment);
        for (name, increment) in batch_by_name {
            let value = self.tool_calls_by_name.entry(name).or_default();
            *value = value.saturating_add(increment);
        }
        None
    }

    pub fn tool_batch_complete(&mut self, operation_failed: bool) -> Option<BudgetExhaustion> {
        let boundary = BudgetEnforcementBoundary::ToolBatchComplete;
        self.observe_boundary();
        if operation_failed {
            return None;
        }
        self.strict_unavailable(boundary).or_else(|| {
            self.check_exceeded_limits(
                boundary,
                &[BudgetDimension::WallTime, BudgetDimension::HostCost],
            )
        })
    }

    pub fn terminal(&mut self) -> Option<BudgetExhaustion> {
        let boundary = BudgetEnforcementBoundary::Terminal;
        self.observe_boundary();
        self.strict_unavailable(boundary)
            .or_else(|| self.check_exceeded_limits(boundary, &DIMENSION_PRECEDENCE))
    }

    fn observe_boundary(&mut self) {
        self.observe_elapsed();
        self.observe_host_cost();
    }

    fn observe_elapsed(&mut self) {
        if self.unavailable.contains_key(&BudgetDimension::WallTime) {
            return;
        }
        let now_ns = (self.clock_ns)();
        let delta_ns = now_ns.saturating_sub(self.started_ns);
        let elapsed_ms = u128::from(self.base_elapsed_ms).saturating_add(delta_ns / 1_000_000);
        if elapsed_ms > u128::from(MAX_WIRE_INTEGER) {
            self.latch_unavailable(
                BudgetDimension::WallTime,
                BudgetUnavailableReason::IntegerOverflow,
                Some("milliseconds"),
                None,
                None,
                None,
            );
            return;
        }
        self.elapsed_ms = self.elapsed_ms.max(elapsed_ms as u64);
    }

    fn observe_host_cost(&mut self) {
        let Some(limit) = self.limits.max_host_cost.clone() else {
            return;
        };
        if self.unavailable.contains_key(&BudgetDimension::HostCost) {
            return;
        }
        let Some(meter) = &self.host_cost_meter else {
            self.latch_host_unavailable(&limit, BudgetUnavailableReason::MeterMissing, None);
            return;
        };
        let reading = match meter.read() {
            Ok(Some(reading)) => reading,
            Ok(None) => {
                self.latch_host_unavailable(
                    &limit,
                    BudgetUnavailableReason::MeterUnavailable,
                    None,
                );
                return;
            }
            Err(_) => {
                self.latch_host_unavailable(&limit, BudgetUnavailableReason::MeterError, None);
                return;
            }
        };
        if reading.validate().is_err() {
            self.latch_host_unavailable(&limit, BudgetUnavailableReason::MeterError, None);
            return;
        }
        if reading.unit != limit.unit {
            self.latch_host_unavailable(
                &limit,
                BudgetUnavailableReason::UnitMismatch,
                Some(&reading),
            );
            return;
        }
        if reading.currency != limit.currency {
            self.latch_host_unavailable(
                &limit,
                BudgetUnavailableReason::CurrencyMismatch,
                Some(&reading),
            );
            return;
        }
        if self
            .host_cost
            .as_ref()
            .is_some_and(|previous| reading.amount_microunits < previous.amount_microunits)
        {
            self.host_cost = None;
            self.latch_host_unavailable(
                &limit,
                BudgetUnavailableReason::NonMonotonic,
                Some(&reading),
            );
            return;
        }
        self.host_cost = Some(reading);
    }

    fn observe_token_usage(&mut self, usage: &TokenUsage) {
        let total_tokens = usage.total_tokens;
        if !matches!(
            usage.usage_source,
            UsageSource::ProviderReported | UsageSource::Estimated
        ) || total_tokens.is_none()
        {
            self.total_tokens = None;
            self.latch_unavailable(
                BudgetDimension::TotalTokens,
                BudgetUnavailableReason::UsageMissing,
                Some("tokens"),
                None,
                None,
                None,
            );
        } else if let (Some(current), Some(increment)) = (self.total_tokens, total_tokens) {
            self.total_tokens =
                self.safe_add_or_latch(BudgetDimension::TotalTokens, current, increment, "tokens");
        }

        let uncached = if usage.cache_usage.status == CacheUsageStatus::ProviderReported {
            usage.cache_usage.uncached_input_tokens
        } else {
            None
        };
        if let Some(increment) = uncached {
            if let Some(current) = self.uncached_input_tokens {
                self.uncached_input_tokens = self.safe_add_or_latch(
                    BudgetDimension::UncachedInputTokens,
                    current,
                    increment,
                    "tokens",
                );
            }
        } else {
            self.uncached_input_tokens = None;
            self.latch_unavailable(
                BudgetDimension::UncachedInputTokens,
                BudgetUnavailableReason::UsageMissing,
                Some("tokens"),
                None,
                None,
                None,
            );
        }
    }

    fn safe_add_or_latch(
        &mut self,
        dimension: BudgetDimension,
        current: u64,
        increment: u64,
        expected_unit: &str,
    ) -> Option<u64> {
        match current.checked_add(increment) {
            Some(total) if total <= MAX_WIRE_INTEGER => Some(total),
            _ => {
                self.latch_unavailable(
                    dimension,
                    BudgetUnavailableReason::IntegerOverflow,
                    Some(expected_unit),
                    None,
                    None,
                    None,
                );
                None
            }
        }
    }

    fn latch_host_unavailable(
        &mut self,
        limit: &HostCost,
        reason: BudgetUnavailableReason,
        reading: Option<&HostCost>,
    ) {
        self.latch_unavailable(
            BudgetDimension::HostCost,
            reason,
            Some(&limit.unit),
            reading.map(|value| value.unit.as_str()),
            limit.currency.as_deref(),
            reading.and_then(|value| value.currency.as_deref()),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn latch_unavailable(
        &mut self,
        dimension: BudgetDimension,
        reason: BudgetUnavailableReason,
        expected_unit: Option<&str>,
        observed_unit: Option<&str>,
        expected_currency: Option<&str>,
        observed_currency: Option<&str>,
    ) {
        self.unavailable
            .entry(dimension)
            .or_insert_with(|| BudgetUnavailableDimension {
                dimension,
                reason,
                expected_unit: expected_unit.map(str::to_string),
                observed_unit: observed_unit.map(str::to_string),
                expected_currency: expected_currency.map(str::to_string),
                observed_currency: observed_currency.map(str::to_string),
            });
    }

    fn strict_unavailable(&self, boundary: BudgetEnforcementBoundary) -> Option<BudgetExhaustion> {
        if self.limits.unavailable_metric_policy != UnavailableMetricPolicy::Stop {
            return None;
        }
        DIMENSION_PRECEDENCE.iter().find_map(|dimension| {
            let unavailable = self.unavailable.get(dimension)?;
            let (limit, unit, currency) = self.limit_descriptor(*dimension)?;
            Some(BudgetExhaustion {
                dimension: *dimension,
                tool_name: None,
                reason: BudgetExhaustionReason::MetricUnavailable,
                limit,
                observed: None,
                attempted_increment: None,
                overshoot: None,
                unit,
                currency,
                enforcement_boundary: boundary,
                unavailable_reason: Some(unavailable.reason),
            })
        })
    }

    fn check_admission_limits(
        &self,
        boundary: BudgetEnforcementBoundary,
        dimensions: &[BudgetDimension],
    ) -> Option<BudgetExhaustion> {
        DIMENSION_PRECEDENCE.iter().find_map(|dimension| {
            if !dimensions.contains(dimension) || self.unavailable.contains_key(dimension) {
                return None;
            }
            let (limit, unit, currency) = self.limit_descriptor(*dimension)?;
            let observed = self.observed_value(*dimension)?;
            (observed >= limit).then(|| BudgetExhaustion {
                dimension: *dimension,
                tool_name: None,
                reason: BudgetExhaustionReason::LimitReached,
                limit,
                observed: Some(observed),
                attempted_increment: None,
                overshoot: Some(observed.saturating_sub(limit)),
                unit,
                currency,
                enforcement_boundary: boundary,
                unavailable_reason: None,
            })
        })
    }

    fn check_exceeded_limits(
        &self,
        boundary: BudgetEnforcementBoundary,
        dimensions: &[BudgetDimension],
    ) -> Option<BudgetExhaustion> {
        DIMENSION_PRECEDENCE.iter().find_map(|dimension| {
            if !dimensions.contains(dimension) || self.unavailable.contains_key(dimension) {
                return None;
            }
            let (limit, unit, currency) = self.limit_descriptor(*dimension)?;
            let observed = self.observed_value(*dimension)?;
            (observed > limit).then(|| BudgetExhaustion {
                dimension: *dimension,
                tool_name: None,
                reason: BudgetExhaustionReason::LimitExceeded,
                limit,
                observed: Some(observed),
                attempted_increment: None,
                overshoot: Some(observed - limit),
                unit,
                currency,
                enforcement_boundary: boundary,
                unavailable_reason: None,
            })
        })
    }

    fn limit_descriptor(
        &self,
        dimension: BudgetDimension,
    ) -> Option<(u64, String, Option<String>)> {
        match dimension {
            BudgetDimension::TotalTokens => self
                .limits
                .max_total_tokens
                .map(|limit| (limit, "tokens".to_string(), None)),
            BudgetDimension::UncachedInputTokens => self
                .limits
                .max_uncached_input_tokens
                .map(|limit| (limit, "tokens".to_string(), None)),
            BudgetDimension::ToolCalls => self
                .limits
                .max_tool_calls
                .map(|limit| (limit, "calls".to_string(), None)),
            BudgetDimension::WallTime => self
                .limits
                .max_wall_time_ms
                .map(|limit| (limit, "milliseconds".to_string(), None)),
            BudgetDimension::HostCost => self.limits.max_host_cost.as_ref().map(|cost| {
                (
                    cost.amount_microunits,
                    cost.unit.clone(),
                    cost.currency.clone(),
                )
            }),
            BudgetDimension::ToolCallsByName => None,
        }
    }

    fn observed_value(&self, dimension: BudgetDimension) -> Option<u64> {
        match dimension {
            BudgetDimension::TotalTokens => self.total_tokens,
            BudgetDimension::UncachedInputTokens => self.uncached_input_tokens,
            BudgetDimension::ToolCalls => Some(self.tool_calls),
            BudgetDimension::WallTime => Some(self.elapsed_ms),
            BudgetDimension::HostCost => self.host_cost.as_ref().map(|cost| cost.amount_microunits),
            BudgetDimension::ToolCallsByName => None,
        }
    }
}

const DIMENSION_PRECEDENCE: [BudgetDimension; 6] = [
    BudgetDimension::WallTime,
    BudgetDimension::TotalTokens,
    BudgetDimension::UncachedInputTokens,
    BudgetDimension::HostCost,
    BudgetDimension::ToolCalls,
    BudgetDimension::ToolCallsByName,
];

fn dimension_precedence(dimension: BudgetDimension) -> usize {
    DIMENSION_PRECEDENCE
        .iter()
        .position(|candidate| *candidate == dimension)
        .expect("all budget dimensions have stable precedence")
}

fn count_preflight_exhaustion(
    dimension: BudgetDimension,
    tool_name: Option<String>,
    limit: u64,
    observed: u64,
    attempted_increment: u64,
    boundary: BudgetEnforcementBoundary,
) -> BudgetExhaustion {
    BudgetExhaustion {
        dimension,
        tool_name,
        reason: BudgetExhaustionReason::LimitReached,
        limit,
        observed: Some(observed),
        attempted_increment: Some(attempted_increment),
        overshoot: Some(
            observed
                .saturating_add(attempted_increment)
                .saturating_sub(limit),
        ),
        unit: "calls".to_string(),
        currency: None,
        enforcement_boundary: boundary,
        unavailable_reason: None,
    }
}

fn validate_wire_integer(value: u64, field: &str) -> Result<(), String> {
    if value <= MAX_WIRE_INTEGER {
        Ok(())
    } else {
        Err(format!("{field} must be between 0 and {MAX_WIRE_INTEGER}"))
    }
}

fn validate_non_empty(value: &str, field: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field} must be a non-empty string"))
    } else {
        Ok(())
    }
}
