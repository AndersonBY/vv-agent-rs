use std::sync::Arc;

use crate::guardrails::{InputGuardrail, OutputGuardrail};
use crate::handoffs::Handoff;
use crate::model::ModelRef;
use crate::model_settings::ModelSettings;
use crate::runtime::RuntimeHook;
use crate::tools::{AgentToolBuilder, BackgroundAgentTaskBuilder};
use crate::tools::{Tool, ToolPolicy};
use crate::types::Metadata;

#[derive(Clone)]
pub struct Agent {
    name: String,
    instructions: String,
    model: Option<ModelRef>,
    model_settings: ModelSettings,
    tools: Vec<Arc<dyn Tool>>,
    handoffs: Vec<Handoff>,
    input_guardrails: Vec<Arc<dyn InputGuardrail>>,
    output_guardrails: Vec<Arc<dyn OutputGuardrail>>,
    hooks: Vec<Arc<dyn RuntimeHook>>,
    max_cycles: Option<u32>,
    tool_use_behavior: ToolUseBehavior,
    tool_policy: ToolPolicy,
    metadata: Metadata,
}

impl Agent {
    pub fn builder(name: impl Into<String>) -> AgentBuilder {
        AgentBuilder {
            agent: Self {
                name: name.into(),
                instructions: String::new(),
                model: None,
                model_settings: ModelSettings::default(),
                tools: Vec::new(),
                handoffs: Vec::new(),
                input_guardrails: Vec::new(),
                output_guardrails: Vec::new(),
                hooks: Vec::new(),
                max_cycles: None,
                tool_use_behavior: ToolUseBehavior::default(),
                tool_policy: ToolPolicy::default(),
                metadata: Metadata::new(),
            },
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    pub fn model(&self) -> Option<&ModelRef> {
        self.model.as_ref()
    }

    pub fn model_settings(&self) -> &ModelSettings {
        &self.model_settings
    }

    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }

    pub fn handoffs(&self) -> &[Handoff] {
        &self.handoffs
    }

    pub(crate) fn input_guardrails(&self) -> &[Arc<dyn InputGuardrail>] {
        &self.input_guardrails
    }

    pub(crate) fn output_guardrails(&self) -> &[Arc<dyn OutputGuardrail>] {
        &self.output_guardrails
    }

    pub fn hooks(&self) -> &[Arc<dyn RuntimeHook>] {
        &self.hooks
    }

    pub fn max_cycles(&self) -> Option<u32> {
        self.max_cycles
    }

    pub fn tool_use_behavior(&self) -> &ToolUseBehavior {
        &self.tool_use_behavior
    }

    pub fn tool_policy(&self) -> &ToolPolicy {
        &self.tool_policy
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    pub fn as_tool(&self) -> AgentToolBuilder {
        AgentToolBuilder::new(self.clone())
    }

    pub fn as_background_task(&self) -> BackgroundAgentTaskBuilder {
        BackgroundAgentTaskBuilder::new(self.clone())
    }
}

pub struct AgentBuilder {
    agent: Agent,
}

impl AgentBuilder {
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.agent.instructions = instructions.into();
        self
    }

    pub fn model(mut self, model: ModelRef) -> Self {
        self.agent.model = Some(model);
        self
    }

    pub fn model_settings(mut self, settings: ModelSettings) -> Self {
        self.agent.model_settings = settings;
        self
    }

    pub fn tool(mut self, tool: impl Tool + 'static) -> Self {
        self.agent.tools.push(Arc::new(tool));
        self
    }

    pub fn tool_arc(mut self, tool: Arc<dyn Tool>) -> Self {
        self.agent.tools.push(tool);
        self
    }

    pub fn handoff(mut self, handoff: impl Into<Handoff>) -> Self {
        self.agent.handoffs.push(handoff.into());
        self
    }

    pub fn input_guardrail(mut self, guardrail: Arc<dyn InputGuardrail>) -> Self {
        self.agent.input_guardrails.push(guardrail);
        self
    }

    pub fn output_guardrail(mut self, guardrail: Arc<dyn OutputGuardrail>) -> Self {
        self.agent.output_guardrails.push(guardrail);
        self
    }

    pub fn hook(mut self, hook: Arc<dyn RuntimeHook>) -> Self {
        self.agent.hooks.push(hook);
        self
    }

    pub fn max_cycles(mut self, max_cycles: u32) -> Self {
        self.agent.max_cycles = Some(max_cycles);
        self
    }

    pub fn tool_use_behavior(mut self, behavior: ToolUseBehavior) -> Self {
        self.agent.tool_use_behavior = behavior;
        self
    }

    pub fn tool_policy(mut self, tool_policy: ToolPolicy) -> Self {
        self.agent.tool_policy = tool_policy;
        self
    }

    pub fn metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.agent.metadata.insert(key.into(), value);
        self
    }

    pub fn build(self) -> Result<Agent, String> {
        if self.agent.name.trim().is_empty() {
            return Err("agent name cannot be empty".to_string());
        }
        if self.agent.instructions.trim().is_empty() {
            return Err("agent instructions cannot be empty".to_string());
        }
        Ok(self.agent)
    }
}

#[derive(Clone, Default)]
pub enum ToolUseBehavior {
    #[default]
    RunLlmAgain,
    StopOnFirstTool,
    StopAtTools(Vec<String>),
}
