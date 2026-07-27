use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MicrocompactionPolicy {
    pub trigger_ratio: f64,
    pub target_ratio: f64,
    pub keep_recent_cycles: u32,
    pub min_result_chars: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("microcompaction_policy_invalid: {message}")]
pub struct MicrocompactionPolicyError {
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MicrocompactionPolicyWire {
    trigger_ratio: f64,
    target_ratio: f64,
    keep_recent_cycles: u32,
    min_result_chars: u32,
}

impl Default for MicrocompactionPolicy {
    fn default() -> Self {
        Self {
            trigger_ratio: 0.75,
            target_ratio: 0.60,
            keep_recent_cycles: 3,
            min_result_chars: 500,
        }
    }
}

impl<'de> Deserialize<'de> for MicrocompactionPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = MicrocompactionPolicyWire::deserialize(deserializer)?;
        let policy = Self {
            trigger_ratio: wire.trigger_ratio,
            target_ratio: wire.target_ratio,
            keep_recent_cycles: wire.keep_recent_cycles,
            min_result_chars: wire.min_result_chars,
        };
        policy.validate().map_err(D::Error::custom)?;
        Ok(policy)
    }
}

impl MicrocompactionPolicy {
    pub fn new(
        trigger_ratio: f64,
        target_ratio: f64,
        keep_recent_cycles: u32,
        min_result_chars: u32,
    ) -> Result<Self, MicrocompactionPolicyError> {
        let policy = Self {
            trigger_ratio,
            target_ratio,
            keep_recent_cycles,
            min_result_chars,
        };
        policy.validate()?;
        Ok(policy)
    }

    pub fn validate(&self) -> Result<(), MicrocompactionPolicyError> {
        if !self.trigger_ratio.is_finite()
            || !self.target_ratio.is_finite()
            || self.target_ratio <= 0.0
            || self.target_ratio >= self.trigger_ratio
            || self.trigger_ratio > 1.0
        {
            return Err(MicrocompactionPolicyError {
                message: "expected 0 < target_ratio < trigger_ratio <= 1".to_string(),
            });
        }
        if self.min_result_chars == 0 {
            return Err(MicrocompactionPolicyError {
                message: "min_result_chars must be at least 1".to_string(),
            });
        }
        Ok(())
    }
}
