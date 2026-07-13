use std::collections::BTreeMap;

use serde_json::Value;

mod defaults;

use super::base::{ToolContext, ToolHandler, ToolNotFoundError, ToolSpec};
use super::{ToolExecutor, ToolSpecExecutor};
use crate::types::{AgentTask, ToolCall, ToolExecutionResult};

pub use defaults::build_default_registry;

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, ToolSpec>,
    schemas: BTreeMap<String, Value>,
    tool_order: Vec<String>,
    planner_extra_tool_names: Vec<String>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, mut spec: ToolSpec) -> Result<(), String> {
        if self.tools.contains_key(&spec.name) {
            return Err(format!("tool already registered: {}", spec.name));
        }
        if let Some(schema) = super::schemas::schema_for(&spec.name) {
            spec.schema = schema;
        }
        self.schemas
            .entry(spec.name.clone())
            .or_insert_with(|| spec.schema.clone());
        self.tool_order.push(spec.name.clone());
        self.tools.insert(spec.name.clone(), spec);
        Ok(())
    }

    pub fn register_many(&mut self, specs: Vec<ToolSpec>) -> Result<(), String> {
        for spec in specs {
            self.register(spec)?;
        }
        Ok(())
    }

    pub fn register_schema(&mut self, tool_name: impl Into<String>, schema: Value) {
        self.schemas.insert(tool_name.into(), schema);
    }

    pub fn register_schemas(&mut self, schemas: BTreeMap<String, Value>) {
        for (tool_name, schema) in schemas {
            self.register_schema(tool_name, schema);
        }
    }

    pub fn get(&self, name: &str) -> Result<&ToolSpec, ToolNotFoundError> {
        self.tools
            .get(name)
            .ok_or_else(|| ToolNotFoundError(name.to_string()))
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn has_schema(&self, name: &str) -> bool {
        self.schemas.contains_key(name)
    }

    pub fn list_planner_extra_tool_names(&self) -> Vec<String> {
        self.planner_extra_tool_names.clone()
    }

    pub fn get_schema(&self, name: &str) -> Option<Value> {
        self.schemas.get(name).cloned()
    }

    pub fn list_openai_schemas(&self, tool_names: Option<&[String]>) -> Result<Vec<Value>, String> {
        match tool_names {
            Some(names) => names
                .iter()
                .filter(|name| self.is_model_visible(name))
                .map(|name| {
                    self.get_schema(name)
                        .ok_or_else(|| format!("Schema not registered: {name}"))
                })
                .collect(),
            None => self
                .tool_order
                .iter()
                .filter(|name| self.is_model_visible(name))
                .map(|name| {
                    self.get_schema(name)
                        .ok_or_else(|| format!("Schema not registered: {name}"))
                })
                .collect(),
        }
    }

    pub fn planned_tool_names(&self, task: &AgentTask) -> Vec<String> {
        crate::runtime::tool_planner::plan_tool_names(task, None)
    }

    fn is_model_visible(&self, name: &str) -> bool {
        self.tools
            .get(name)
            .is_none_or(|spec| spec.exposure != crate::tools::ToolExposure::Hidden)
    }

    pub fn planned_openai_schemas(&self, task: &AgentTask) -> Vec<Value> {
        crate::runtime::tool_planner::plan_tool_schemas(self, task, None)
    }

    pub fn register_tool(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        handler: ToolHandler,
    ) -> Result<(), String> {
        self.register_tool_with_parameters(
            name,
            description,
            serde_json::json!({"type": "object", "properties": {}, "required": []}),
            handler,
        )
    }

    pub fn register_tool_with_parameters(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        handler: ToolHandler,
    ) -> Result<(), String> {
        let name = name.into();
        let description = description.into();
        let mut spec = ToolSpec::new(name.clone(), description.clone(), handler);
        spec.schema = serde_json::json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": parameters,
                }
        });
        self.register(spec)?;
        self.planner_extra_tool_names.push(name);
        Ok(())
    }

    pub fn execute(
        &self,
        call: &ToolCall,
        context: &mut ToolContext,
    ) -> Result<ToolExecutionResult, ToolNotFoundError> {
        let tool = self.get(&call.name)?;
        context.begin_tool_call(call);
        Ok((tool.handler)(context, &call.arguments))
    }

    pub fn executors(&self) -> Vec<std::sync::Arc<dyn ToolExecutor>> {
        self.tool_order
            .iter()
            .filter_map(|name| self.tools.get(name).cloned())
            .map(|spec| ToolSpecExecutor::new(spec).into_arc())
            .collect()
    }
}
