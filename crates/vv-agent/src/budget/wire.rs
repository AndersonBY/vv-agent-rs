use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};

use super::{
    dimension_precedence, BudgetDimension, BudgetEnforcementBoundary, BudgetExhaustion,
    BudgetExhaustionReason, BudgetUnavailableDimension, BudgetUnavailableReason,
    BudgetUsageSnapshot, HostCost, RunBudgetLimits, UnavailableMetricPolicy,
};

#[derive(Deserialize)]
struct HostCostWire {
    unit: String,
    #[serde(default)]
    currency: Option<String>,
    amount_microunits: u64,
}

impl<'de> Deserialize<'de> for HostCost {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = HostCostWire::deserialize(deserializer)?;
        let cost = Self {
            unit: wire.unit,
            currency: wire.currency,
            amount_microunits: wire.amount_microunits,
        };
        cost.validate().map_err(serde::de::Error::custom)?;
        Ok(cost)
    }
}

#[derive(Deserialize)]
struct BudgetUnavailableDimensionWire {
    dimension: BudgetDimension,
    reason: BudgetUnavailableReason,
    #[serde(default)]
    expected_unit: Option<String>,
    #[serde(default)]
    observed_unit: Option<String>,
    #[serde(default)]
    expected_currency: Option<String>,
    #[serde(default)]
    observed_currency: Option<String>,
}

impl<'de> Deserialize<'de> for BudgetUnavailableDimension {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BudgetUnavailableDimensionWire::deserialize(deserializer)?;
        let observation = Self {
            dimension: wire.dimension,
            reason: wire.reason,
            expected_unit: wire.expected_unit,
            observed_unit: wire.observed_unit,
            expected_currency: wire.expected_currency,
            observed_currency: wire.observed_currency,
        };
        observation.validate().map_err(serde::de::Error::custom)?;
        Ok(observation)
    }
}

#[derive(Default, Deserialize)]
struct RunBudgetLimitsWire {
    #[serde(default)]
    max_total_tokens: Option<u64>,
    #[serde(default)]
    max_uncached_input_tokens: Option<u64>,
    #[serde(default)]
    max_tool_calls: Option<u64>,
    #[serde(default)]
    max_tool_calls_by_name: BTreeMap<String, u64>,
    #[serde(default)]
    max_wall_time_ms: Option<u64>,
    #[serde(default)]
    max_host_cost: Option<HostCost>,
    #[serde(default)]
    unavailable_metric_policy: UnavailableMetricPolicy,
}

impl<'de> Deserialize<'de> for RunBudgetLimits {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RunBudgetLimitsWire::deserialize(deserializer)?;
        let limits = Self {
            max_total_tokens: wire.max_total_tokens,
            max_uncached_input_tokens: wire.max_uncached_input_tokens,
            max_tool_calls: wire.max_tool_calls,
            max_tool_calls_by_name: wire.max_tool_calls_by_name,
            max_wall_time_ms: wire.max_wall_time_ms,
            max_host_cost: wire.max_host_cost,
            unavailable_metric_policy: wire.unavailable_metric_policy,
        };
        limits.validate().map_err(serde::de::Error::custom)?;
        Ok(limits)
    }
}

#[derive(Deserialize)]
struct BudgetUsageSnapshotWire {
    cycles: u64,
    total_tokens: Option<u64>,
    uncached_input_tokens: Option<u64>,
    tool_calls: u64,
    tool_calls_by_name: BTreeMap<String, u64>,
    elapsed_ms: u64,
    host_cost: Option<HostCost>,
    unavailable_dimensions: Vec<BudgetUnavailableDimension>,
}

impl<'de> Deserialize<'de> for BudgetUsageSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BudgetUsageSnapshotWire::deserialize(deserializer)?;
        let mut snapshot = Self {
            cycles: wire.cycles,
            total_tokens: wire.total_tokens,
            uncached_input_tokens: wire.uncached_input_tokens,
            tool_calls: wire.tool_calls,
            tool_calls_by_name: wire.tool_calls_by_name,
            elapsed_ms: wire.elapsed_ms,
            host_cost: wire.host_cost,
            unavailable_dimensions: wire.unavailable_dimensions,
        };
        snapshot
            .unavailable_dimensions
            .sort_by_key(|item| dimension_precedence(item.dimension));
        snapshot.validate().map_err(serde::de::Error::custom)?;
        Ok(snapshot)
    }
}

#[derive(Deserialize)]
struct BudgetExhaustionWire {
    dimension: BudgetDimension,
    #[serde(default)]
    tool_name: Option<String>,
    reason: BudgetExhaustionReason,
    limit: u64,
    observed: Option<u64>,
    attempted_increment: Option<u64>,
    overshoot: Option<u64>,
    unit: String,
    #[serde(default)]
    currency: Option<String>,
    enforcement_boundary: BudgetEnforcementBoundary,
    #[serde(default)]
    unavailable_reason: Option<BudgetUnavailableReason>,
}

impl<'de> Deserialize<'de> for BudgetExhaustion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BudgetExhaustionWire::deserialize(deserializer)?;
        let exhaustion = Self {
            dimension: wire.dimension,
            tool_name: wire.tool_name,
            reason: wire.reason,
            limit: wire.limit,
            observed: wire.observed,
            attempted_increment: wire.attempted_increment,
            overshoot: wire.overshoot,
            unit: wire.unit,
            currency: wire.currency,
            enforcement_boundary: wire.enforcement_boundary,
            unavailable_reason: wire.unavailable_reason,
        };
        exhaustion.validate().map_err(serde::de::Error::custom)?;
        Ok(exhaustion)
    }
}
