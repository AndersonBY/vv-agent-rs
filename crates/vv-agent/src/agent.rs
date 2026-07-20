use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::sync::Arc;

use serde::de::DeserializeOwned;

use crate::guardrails::{InputGuardrail, OutputGuardrail};
use crate::handoffs::Handoff;
use crate::model::ModelRef;
use crate::model_settings::ModelSettings;
use crate::output_validation::{
    HostOutputValidator, OutputRepair, OutputRepairRequest, OutputValidationContext,
    OutputValidationResult,
};
use crate::runtime::RuntimeHook;
use crate::tools::common::trim_portable_whitespace;
use crate::tools::{AgentToolBuilder, BackgroundAgentTaskBuilder};
use crate::tools::{Tool, ToolPolicy};
use crate::types::{Metadata, NoToolPolicy, SubAgentConfig};

pub type InstructionProvider =
    Arc<dyn Fn(&crate::context::RunContext, &Agent) -> String + Send + Sync>;
pub type OutputValidator = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

#[derive(Clone)]
pub struct Agent {
    name: String,
    instructions: String,
    instruction_provider: Option<InstructionProvider>,
    model: Option<ModelRef>,
    model_settings: ModelSettings,
    tools: Vec<Arc<dyn Tool>>,
    handoffs: Vec<Handoff>,
    input_guardrails: Vec<Arc<dyn InputGuardrail>>,
    output_guardrails: Vec<Arc<dyn OutputGuardrail>>,
    output_type_name: Option<&'static str>,
    output_validator: Option<OutputValidator>,
    output_validation_enabled: bool,
    host_output_validator: Option<HostOutputValidator>,
    output_repair: Option<OutputRepair>,
    output_validation_max_repairs: u8,
    output_repair_model: Option<ModelRef>,
    output_repair_model_settings: Option<ModelSettings>,
    hooks: Vec<Arc<dyn RuntimeHook>>,
    max_cycles: Option<u32>,
    no_tool_policy: Option<NoToolPolicy>,
    tool_use_behavior: ToolUseBehavior,
    tool_policy: ToolPolicy,
    sub_agents: BTreeMap<String, SubAgentConfig>,
    metadata: Metadata,
}

impl Agent {
    pub fn builder(name: impl Into<String>) -> AgentBuilder {
        AgentBuilder {
            agent: Self {
                name: name.into(),
                instructions: String::new(),
                instruction_provider: None,
                model: None,
                model_settings: ModelSettings::default(),
                tools: Vec::new(),
                handoffs: Vec::new(),
                input_guardrails: Vec::new(),
                output_guardrails: Vec::new(),
                output_type_name: None,
                output_validator: None,
                output_validation_enabled: false,
                host_output_validator: None,
                output_repair: None,
                output_validation_max_repairs: 1,
                output_repair_model: None,
                output_repair_model_settings: None,
                hooks: Vec::new(),
                max_cycles: None,
                no_tool_policy: None,
                tool_use_behavior: ToolUseBehavior::default(),
                tool_policy: ToolPolicy::default(),
                sub_agents: BTreeMap::new(),
                metadata: Metadata::new(),
            },
            sub_agent_error: None,
            output_validation_error: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    pub(crate) fn has_dynamic_instructions(&self) -> bool {
        self.instruction_provider.is_some()
    }

    pub fn resolve_instructions(&self, context: &crate::context::RunContext) -> String {
        self.instruction_provider
            .as_ref()
            .map(|provider| provider(context, self))
            .unwrap_or_else(|| self.instructions.clone())
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

    pub fn output_type_name(&self) -> Option<&'static str> {
        self.output_type_name
    }

    pub fn validate_output(&self, output: &str) -> Result<(), String> {
        match self.output_validator.as_ref() {
            Some(validator) => validator(output),
            None => Ok(()),
        }
    }

    pub fn output_validation_enabled(&self) -> bool {
        self.output_validation_enabled
    }

    pub fn host_output_validator(&self) -> Option<&HostOutputValidator> {
        self.host_output_validator.as_ref()
    }

    pub fn output_repair(&self) -> Option<&OutputRepair> {
        self.output_repair.as_ref()
    }

    pub fn output_validation_max_repairs(&self) -> u8 {
        self.output_validation_max_repairs
    }

    pub fn output_repair_model(&self) -> Option<&ModelRef> {
        self.output_repair_model.as_ref()
    }

    pub fn output_repair_model_settings(&self) -> Option<&ModelSettings> {
        self.output_repair_model_settings.as_ref()
    }

    pub fn hooks(&self) -> &[Arc<dyn RuntimeHook>] {
        &self.hooks
    }

    pub fn max_cycles(&self) -> Option<u32> {
        self.max_cycles
    }

    pub fn no_tool_policy(&self) -> Option<NoToolPolicy> {
        self.no_tool_policy
    }

    pub fn tool_use_behavior(&self) -> &ToolUseBehavior {
        &self.tool_use_behavior
    }

    pub fn tool_policy(&self) -> &ToolPolicy {
        &self.tool_policy
    }

    pub fn sub_agents(&self) -> &BTreeMap<String, SubAgentConfig> {
        &self.sub_agents
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
    sub_agent_error: Option<String>,
    output_validation_error: Option<String>,
}

impl AgentBuilder {
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.agent.instructions = instructions.into();
        self.agent.instruction_provider = None;
        self
    }

    pub fn dynamic_instructions(
        mut self,
        provider: impl Fn(&crate::context::RunContext, &Agent) -> String + Send + Sync + 'static,
    ) -> Self {
        self.agent.instructions.clear();
        self.agent.instruction_provider = Some(Arc::new(provider));
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

    pub fn output_type<T>(mut self) -> Self
    where
        T: DeserializeOwned + 'static,
    {
        self.agent.output_type_name = Some(std::any::type_name::<T>());
        self.agent.output_validator = Some(Arc::new(|output| {
            serde_json::from_str::<T>(output)
                .map(|_| ())
                .map_err(|error| error.to_string())
        }));
        self
    }

    pub fn output_validator(
        mut self,
        name: &'static str,
        validator: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.agent.output_type_name = Some(name);
        self.agent.output_validator = Some(Arc::new(validator));
        self
    }

    pub fn output_validation_enabled(mut self, enabled: bool) -> Self {
        self.agent.output_validation_enabled = enabled;
        self
    }

    pub fn host_output_validator(
        mut self,
        validator: impl Fn(&str, &OutputValidationContext) -> OutputValidationResult
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.agent.host_output_validator = Some(Arc::new(validator));
        self
    }

    pub fn output_repair(
        mut self,
        repair: impl Fn(&OutputRepairRequest) -> Result<String, String> + Send + Sync + 'static,
    ) -> Self {
        self.agent.output_repair = Some(Arc::new(repair));
        self
    }

    pub fn output_validation_max_repairs(mut self, max_repairs: u8) -> Self {
        if max_repairs > 1 {
            self.output_validation_error =
                Some("output_validation_max_repairs must be 0 or 1".to_string());
        } else {
            self.agent.output_validation_max_repairs = max_repairs;
        }
        self
    }

    pub fn output_repair_model(mut self, model: ModelRef) -> Self {
        self.agent.output_repair_model = Some(model);
        self
    }

    pub fn output_repair_model_settings(mut self, settings: ModelSettings) -> Self {
        self.agent.output_repair_model_settings = Some(settings);
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

    pub fn no_tool_policy(mut self, policy: NoToolPolicy) -> Self {
        self.agent.no_tool_policy = Some(policy);
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

    pub fn sub_agent(mut self, id: impl AsRef<str>, config: impl Borrow<SubAgentConfig>) -> Self {
        self.insert_sub_agent(id.as_ref(), config.borrow());
        self
    }

    pub fn sub_agents<I, K, C>(mut self, sub_agents: I) -> Self
    where
        I: IntoIterator<Item = (K, C)>,
        K: AsRef<str>,
        C: Borrow<SubAgentConfig>,
    {
        for (id, config) in sub_agents {
            self.insert_sub_agent(id.as_ref(), config.borrow());
        }
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
        if self.agent.instructions.trim().is_empty() && self.agent.instruction_provider.is_none() {
            return Err("agent instructions cannot be empty".to_string());
        }
        if let Some(error) = self.sub_agent_error {
            return Err(error);
        }
        if let Some(error) = self.output_validation_error {
            return Err(error);
        }
        if self.agent.output_validation_enabled && self.agent.host_output_validator.is_none() {
            return Err("enabled output validation requires a host_output_validator".to_string());
        }
        if self.agent.output_repair.is_some() && self.agent.host_output_validator.is_none() {
            return Err("output_repair requires a host_output_validator".to_string());
        }
        Ok(self.agent)
    }

    fn insert_sub_agent(&mut self, id: &str, config: &SubAgentConfig) {
        let normalized_id = trim_portable_whitespace(id);
        if normalized_id.is_empty() {
            self.sub_agent_error
                .get_or_insert_with(|| "sub-agent id cannot be empty".to_string());
            return;
        }
        if self.agent.sub_agents.contains_key(normalized_id) {
            self.sub_agent_error.get_or_insert_with(|| {
                format!("duplicate sub-agent id after normalization: {normalized_id}")
            });
            return;
        }
        self.agent
            .sub_agents
            .insert(normalized_id.to_string(), config.clone());
    }
}

#[derive(Clone, Default)]
pub enum ToolUseBehavior {
    #[default]
    RunLlmAgain,
    StopOnFirstTool,
    StopAtToolNames(Vec<String>),
}
