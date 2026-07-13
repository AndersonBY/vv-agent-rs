use std::sync::Arc;

use crate::types::{Metadata, ToolArguments};

pub type CanUseToolPredicate = Arc<dyn Fn(&str, &ToolArguments) -> bool + Send + Sync + 'static>;

#[derive(Clone, Default)]
pub struct ToolPolicy {
    pub allowed_tools: Option<Vec<String>>,
    pub disallowed_tools: Vec<String>,
    pub approval: ApprovalPolicy,
    pub can_use_tool: Option<CanUseToolPredicate>,
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

    pub fn allows_arguments(&self, tool_name: &str, arguments: &ToolArguments) -> bool {
        self.can_use_tool
            .as_ref()
            .is_none_or(|predicate| predicate(tool_name, arguments))
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
