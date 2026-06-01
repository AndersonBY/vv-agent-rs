#[derive(Clone, Default)]
pub struct ToolPolicy {
    pub allowed_tools: Option<Vec<String>>,
    pub disallowed_tools: Vec<String>,
    pub approval: ApprovalPolicy,
    pub max_concurrency: Option<usize>,
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
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ApprovalPolicy {
    #[default]
    Never,
    Always,
    OnRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Denied(String),
    TimedOut(String),
    NeedsApproval,
}

impl ApprovalDecision {
    pub fn allow() -> Self {
        Self::Approved
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Denied(reason.into())
    }

    pub fn timeout(reason: impl Into<String>) -> Self {
        Self::TimedOut(reason.into())
    }
}
