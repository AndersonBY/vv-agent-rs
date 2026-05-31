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
    NeedsApproval,
}
