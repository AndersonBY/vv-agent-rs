use std::sync::Arc;

use crate::tools::metadata::{normalize_tool_metadata_labels, utf16_cmp};
use crate::tools::{ToolMetadata, ToolMetadataError, ToolSideEffect};
use crate::types::{Metadata, ToolArguments};

pub type CanUseToolPredicate = Arc<dyn Fn(&str, &ToolArguments) -> bool + Send + Sync + 'static>;

#[derive(Clone, Default)]
pub struct ToolPolicy {
    pub allowed_tools: Option<Vec<String>>,
    pub disallowed_tools: Vec<String>,
    pub approval: ApprovalPolicy,
    pub can_use_tool: Option<CanUseToolPredicate>,
    pub denied_side_effects: Vec<ToolSideEffect>,
    pub denied_capability_tags: Vec<String>,
    pub deny_terminal_tools: bool,
    pub denied_cost_dimensions: Vec<String>,
}

impl ToolPolicy {
    pub fn allow_only(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    pub fn disallow(mut self, tool: impl Into<String>) -> Self {
        self.disallowed_tools.push(tool.into());
        self
    }

    pub fn can_use_tool<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&str, &ToolArguments) -> bool + Send + Sync + 'static,
    {
        self.can_use_tool = Some(Arc::new(predicate));
        self
    }

    pub fn deny_side_effect(mut self, side_effect: ToolSideEffect) -> Self {
        self.denied_side_effects.push(side_effect);
        self.normalize_metadata_denials_in_place()
            .expect("a side-effect enum is always a valid denial");
        self
    }

    pub fn deny_capability_tag(
        mut self,
        tag: impl Into<String>,
    ) -> Result<Self, ToolMetadataError> {
        self.denied_capability_tags.push(tag.into());
        self.normalize_metadata_denials_in_place()?;
        Ok(self)
    }

    pub fn deny_terminal_tools(mut self) -> Self {
        self.deny_terminal_tools = true;
        self
    }

    pub fn deny_cost_dimension(
        mut self,
        dimension: impl Into<String>,
    ) -> Result<Self, ToolMetadataError> {
        self.denied_cost_dimensions.push(dimension.into());
        self.normalize_metadata_denials_in_place()?;
        Ok(self)
    }

    pub fn allows_arguments(&self, tool_name: &str, arguments: &ToolArguments) -> bool {
        self.can_use_tool
            .as_ref()
            .is_none_or(|predicate| predicate(tool_name, arguments))
    }

    pub fn normalized(&self) -> Result<Self, ToolMetadataError> {
        let mut normalized = self.clone();
        normalized.normalize_metadata_denials_in_place()?;
        Ok(normalized)
    }

    pub fn metadata_denial_source(&self, metadata: Option<&ToolMetadata>) -> Option<&'static str> {
        let metadata = metadata?;
        if self.denied_side_effects.contains(&metadata.side_effect) {
            return Some("metadata.side_effect");
        }
        if self.deny_terminal_tools && metadata.terminal {
            return Some("metadata.terminal");
        }
        if metadata
            .capability_tags
            .iter()
            .any(|tag| self.denied_capability_tags.contains(tag))
        {
            return Some("metadata.capability_tag");
        }
        if metadata
            .cost_dimensions
            .iter()
            .any(|dimension| self.denied_cost_dimensions.contains(dimension))
        {
            return Some("metadata.cost_dimension");
        }
        None
    }

    pub(crate) fn extend_metadata_denials(&mut self, other: &Self) {
        self.denied_side_effects
            .extend(other.denied_side_effects.iter().copied());
        self.denied_capability_tags
            .extend(other.denied_capability_tags.iter().cloned());
        self.deny_terminal_tools |= other.deny_terminal_tools;
        self.denied_cost_dimensions
            .extend(other.denied_cost_dimensions.iter().cloned());
        self.normalize_metadata_denials_in_place()
            .expect("normalized policies remain valid when unioned");
    }

    pub(crate) fn normalize_metadata_denials_in_place(&mut self) -> Result<(), ToolMetadataError> {
        self.denied_side_effects
            .sort_by(|left, right| utf16_cmp(left.as_str(), right.as_str()));
        self.denied_side_effects.dedup();
        self.denied_capability_tags =
            normalize_tool_metadata_labels(&self.denied_capability_tags, "denied_capability_tags")?;
        self.denied_cost_dimensions =
            normalize_tool_metadata_labels(&self.denied_cost_dimensions, "denied_cost_dimensions")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ApprovalPolicy {
    #[default]
    Default,
    Never,
    Always,
    OnRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    ApprovedForSession,
    Denied(String),
    TimedOut(String),
    NeedsApproval,
    Detailed {
        decision: Box<ApprovalDecision>,
        reason: Option<String>,
        metadata: Metadata,
    },
}

impl ApprovalDecision {
    pub fn allow() -> Self {
        Self::Approved
    }

    pub fn allow_session() -> Self {
        Self::ApprovedForSession
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Denied(reason.into())
    }

    pub fn timeout(reason: impl Into<String>) -> Self {
        Self::TimedOut(reason.into())
    }

    pub fn with_reason(self, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        match self {
            Self::Denied(_) => Self::Denied(reason),
            Self::TimedOut(_) => Self::TimedOut(reason),
            Self::Detailed {
                decision, metadata, ..
            } => Self::Detailed {
                decision,
                reason: Some(reason),
                metadata,
            },
            decision => Self::Detailed {
                decision: Box::new(decision),
                reason: Some(reason),
                metadata: Metadata::new(),
            },
        }
    }

    pub fn with_metadata(self, metadata: Metadata) -> Self {
        match self {
            Self::Detailed {
                decision,
                reason,
                metadata: mut existing,
            } => {
                existing.extend(metadata);
                Self::Detailed {
                    decision,
                    reason,
                    metadata: existing,
                }
            }
            decision => Self::Detailed {
                decision: Box::new(decision),
                reason: None,
                metadata,
            },
        }
    }

    pub fn is_approved(&self) -> bool {
        matches!(self.action(), "allow" | "allow_session")
    }

    pub fn action(&self) -> &'static str {
        match self {
            Self::Approved => "allow",
            Self::ApprovedForSession => "allow_session",
            Self::Denied(_) => "deny",
            Self::TimedOut(_) => "timeout",
            Self::NeedsApproval => "needs_approval",
            Self::Detailed { decision, .. } => decision.action(),
        }
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::Denied(reason) | Self::TimedOut(reason) => reason,
            Self::Detailed {
                decision, reason, ..
            } => reason.as_deref().unwrap_or_else(|| decision.reason()),
            _ => "",
        }
    }

    pub fn metadata(&self) -> Option<&Metadata> {
        match self {
            Self::Detailed { metadata, .. } => Some(metadata),
            _ => None,
        }
    }
}
