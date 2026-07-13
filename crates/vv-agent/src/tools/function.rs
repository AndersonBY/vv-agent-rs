use std::future::Future;
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::context::{RunContext, ToolCallContext};
use crate::tools::{
    Tool, ToolApprovalRule, ToolContext, ToolEnablementContext, ToolEnablementRule, ToolExposure,
    ToolOutput, ToolSpec, ToolSpecExecutor,
};
use crate::types::{Metadata, ToolArguments};

type DynamicFunctionHandler =
    Arc<dyn Fn(ToolCallContext, Value) -> Result<ToolOutput, String> + Send + Sync>;
pub type ToolErrorMapper = Arc<dyn Fn(&str) -> String + Send + Sync + 'static>;

pub struct FunctionTool<Args = Value> {
    name: String,
    description: String,
    parameters_schema: Value,
    handler: DynamicFunctionHandler,
    strict_schema: bool,
    exposure: ToolExposure,
    timeout: Option<Duration>,
    approval: ToolApprovalRule,
    enablement: ToolEnablementRule,
    error_mapper: Option<ToolErrorMapper>,
    metadata: Metadata,
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
            strict_schema: true,
            exposure: ToolExposure::Direct,
            timeout: None,
            approval: ToolApprovalRule::default(),
            enablement: ToolEnablementRule::default(),
            error_mapper: None,
            metadata: Metadata::new(),
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
            strict_schema: self.strict_schema,
            exposure: self.exposure,
            timeout: self.timeout,
            approval: self.approval.clone(),
            enablement: self.enablement.clone(),
            error_mapper: self.error_mapper.clone(),
            metadata: self.metadata.clone(),
            _args: PhantomData,
        }
    }
}

impl<Args> FunctionTool<Args> {
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

impl<Args> FunctionTool<Args>
where
    Args: Send + Sync + 'static,
{
    pub fn to_executor(&self) -> std::sync::Arc<dyn crate::tools::ToolExecutor> {
        ToolSpecExecutor::new(self.as_tool_spec()).into_arc()
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

    fn strict_schema(&self) -> bool {
        self.strict_schema
    }

    fn exposure(&self) -> ToolExposure {
        self.exposure
    }

    fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    fn approval_rule(&self) -> ToolApprovalRule {
        self.approval.clone()
    }

    fn is_enabled(&self, context: &ToolEnablementContext) -> bool {
        self.enablement.is_enabled(context)
    }

    fn as_tool_spec(&self) -> ToolSpec {
        let name = self.name.clone();
        let description = self.description.clone();
        let parameters_schema = self.parameters_schema.clone();
        let handler = self.handler.clone();
        let error_mapper = self.error_mapper.clone();
        let mut spec = ToolSpec::new(
            name.clone(),
            description.clone(),
            Arc::new(
                move |context: &mut ToolContext, arguments: &ToolArguments| {
                    let raw_arguments = Value::Object(arguments.clone().into_iter().collect());
                    let shared_state = Arc::new(Mutex::new(context.shared_state.clone()));
                    let run_context = context.run_context.clone().unwrap_or_else(|| RunContext {
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
                        app_state: context.app_state.clone(),
                    });
                    let call_context = ToolCallContext {
                        run: run_context,
                        tool_call_id: context.tool_call_id.clone(),
                        tool_name: context.tool_name.clone(),
                        raw_arguments: raw_arguments.clone(),
                        metadata: context.metadata.clone(),
                        app_state: context.app_state.clone(),
                        shared_state: shared_state.clone(),
                    };
                    let outcome =
                        catch_unwind(AssertUnwindSafe(|| handler(call_context, raw_arguments)));
                    context.shared_state = shared_state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    match outcome {
                        Ok(Ok(output)) => output.to_result(&context.tool_call_id),
                        Ok(Err(error)) => function_error_result(
                            &name,
                            &context.tool_call_id,
                            &error,
                            error_mapper.as_ref(),
                        ),
                        Err(payload) => function_error_result(
                            &name,
                            &context.tool_call_id,
                            &panic_message(payload),
                            error_mapper.as_ref(),
                        ),
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
                "strict": self.strict_schema,
            }
        });
        spec.strict_schema = self.strict_schema;
        spec.exposure = self.exposure;
        spec.timeout = self.timeout;
        spec.approval = self.approval.clone();
        spec.metadata = self.metadata.clone();
        spec
    }
}

pub struct FunctionToolBuilder<Args = Value> {
    name: String,
    description: String,
    parameters_schema: Value,
    handler: Option<DynamicFunctionHandler>,
    strict_schema: bool,
    exposure: ToolExposure,
    timeout: Option<Duration>,
    approval: ToolApprovalRule,
    enablement: ToolEnablementRule,
    error_mapper: Option<ToolErrorMapper>,
    metadata: Metadata,
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

    pub fn strict_schema(mut self, strict_schema: bool) -> Self {
        self.strict_schema = strict_schema;
        self
    }

    pub fn exposure(mut self, exposure: ToolExposure) -> Self {
        self.exposure = exposure;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn needs_approval(mut self, approval: impl Into<ToolApprovalRule>) -> Self {
        self.approval = approval.into();
        self
    }

    pub fn needs_approval_if<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&ToolContext, &ToolArguments) -> bool + Send + Sync + 'static,
    {
        self.approval = ToolApprovalRule::predicate(predicate);
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enablement = ToolEnablementRule::Static(enabled);
        self
    }

    pub fn enabled_if<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&ToolEnablementContext) -> bool + Send + Sync + 'static,
    {
        self.enablement = ToolEnablementRule::predicate(predicate);
        self
    }

    pub fn failure_error_function<F>(mut self, mapper: F) -> Self
    where
        F: Fn(&str) -> String + Send + Sync + 'static,
    {
        self.error_mapper = Some(Arc::new(mapper));
        self
    }

    pub fn metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
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
            strict_schema: self.strict_schema,
            exposure: self.exposure,
            timeout: self.timeout,
            approval: self.approval,
            enablement: self.enablement,
            error_mapper: self.error_mapper,
            metadata: self.metadata,
            _args: PhantomData,
        }
    }

    pub fn build(self) -> Result<FunctionTool<Args>, String> {
        if self.name.trim().is_empty() {
            return Err("tool name cannot be empty".to_string());
        }
        if self.timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err("tool timeout must be greater than zero".to_string());
        }
        let Some(handler) = self.handler else {
            return Err("tool handler is required".to_string());
        };
        Ok(FunctionTool {
            name: self.name,
            description: self.description,
            parameters_schema: self.parameters_schema,
            handler,
            strict_schema: self.strict_schema,
            exposure: self.exposure,
            timeout: self.timeout,
            approval: self.approval,
            enablement: self.enablement,
            error_mapper: self.error_mapper,
            metadata: self.metadata,
            _args: PhantomData,
        })
    }
}

fn function_error_result(
    tool_name: &str,
    tool_call_id: &str,
    error: &str,
    mapper: Option<&ToolErrorMapper>,
) -> crate::types::ToolExecutionResult {
    let message = mapper
        .map(|mapper| mapper(error))
        .unwrap_or_else(|| format!("Tool execution failed ({tool_name}): {error}"));
    ToolOutput::error(message)
        .with_code("tool_execution_failed")
        .to_result(tool_call_id)
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "tool handler panicked".to_string()
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
