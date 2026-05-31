use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::context::{RunContext, ToolCallContext};
use crate::tools::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::types::ToolArguments;

pub struct FunctionTool<Args = Value> {
    name: String,
    description: String,
    parameters_schema: Value,
    handler: Arc<dyn Fn(ToolCallContext, Value) -> Result<ToolOutput, String> + Send + Sync>,
    _args: PhantomData<fn() -> Args>,
}

impl FunctionTool<Value> {
    pub fn builder(name: impl Into<String>) -> FunctionToolBuilder<Value> {
        FunctionToolBuilder {
            name: name.into(),
            description: String::new(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            handler: None,
            _args: PhantomData,
        }
    }
}

impl<Args> Clone for FunctionTool<Args> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters_schema: self.parameters_schema.clone(),
            handler: self.handler.clone(),
            _args: PhantomData,
        }
    }
}

impl<Args> Tool for FunctionTool<Args>
where
    Args: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> &Value {
        &self.parameters_schema
    }

    fn as_tool_spec(&self) -> ToolSpec {
        let name = self.name.clone();
        let description = self.description.clone();
        let parameters_schema = self.parameters_schema.clone();
        let handler = self.handler.clone();
        let mut spec = ToolSpec::new(
            name.clone(),
            description.clone(),
            Arc::new(
                move |context: &mut ToolContext, arguments: &ToolArguments| {
                    let raw_arguments = Value::Object(arguments.clone().into_iter().collect());
                    let call_context = ToolCallContext {
                        run: RunContext {
                            run_id: context.task_id.clone(),
                            agent_name: context
                                .metadata
                                .get("agent_name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            model: None,
                            workspace: Some(context.workspace.clone()),
                            metadata: context.metadata.clone(),
                            ..RunContext::default()
                        },
                        tool_call_id: String::new(),
                        tool_name: name.clone(),
                        raw_arguments: raw_arguments.clone(),
                        metadata: context.metadata.clone(),
                        app_state: None,
                    };
                    match handler(call_context, raw_arguments) {
                        Ok(output) => output.to_result(""),
                        Err(error) => ToolOutput::error(error).to_result(""),
                    }
                },
            ),
        );
        spec.schema = serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": parameters_schema,
            }
        });
        spec
    }
}

pub struct FunctionToolBuilder<Args = Value> {
    name: String,
    description: String,
    parameters_schema: Value,
    handler:
        Option<Arc<dyn Fn(ToolCallContext, Value) -> Result<ToolOutput, String> + Send + Sync>>,
    _args: PhantomData<fn() -> Args>,
}

impl<Args> FunctionToolBuilder<Args> {
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn json_schema(mut self, schema: Value) -> Self {
        self.parameters_schema = schema;
        self
    }

    pub fn handler<NextArgs, F, Fut>(self, handler: F) -> FunctionToolBuilder<NextArgs>
    where
        NextArgs: DeserializeOwned + Send + Sync + 'static,
        F: Fn(ToolCallContext, NextArgs) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ToolOutput, String>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        FunctionToolBuilder {
            name: self.name,
            description: self.description,
            parameters_schema: self.parameters_schema,
            handler: Some(Arc::new(move |context, raw_arguments| {
                let args = serde_json::from_value::<NextArgs>(raw_arguments)
                    .map_err(|error| format!("invalid tool arguments: {error}"))?;
                block_on_tool_future(handler(context, args))
            })),
            _args: PhantomData,
        }
    }

    pub fn build(self) -> Result<FunctionTool<Args>, String> {
        if self.name.trim().is_empty() {
            return Err("tool name cannot be empty".to_string());
        }
        let Some(handler) = self.handler else {
            return Err("tool handler is required".to_string());
        };
        Ok(FunctionTool {
            name: self.name,
            description: self.description,
            parameters_schema: self.parameters_schema,
            handler,
            _args: PhantomData,
        })
    }
}

fn block_on_tool_future<F>(future: F) -> Result<ToolOutput, String>
where
    F: Future<Output = Result<ToolOutput, String>> + Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            tokio::task::block_in_place(|| handle.block_on(future))
        } else {
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?
                    .block_on(future)
            })
            .join()
            .map_err(|_| "tool handler thread panicked".to_string())?
        }
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?
            .block_on(future)
    }
}
