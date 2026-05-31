#![allow(deprecated)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    build_default_registry, run_with_options_and_agent_request, AfterLlmEvent, AgentDefinition,
    AgentResourceLoader, AgentRuntime, AgentSDKClient, AgentSDKOptions, AgentSessionRunRequest,
    AgentStatus, BeforeLlmEvent, LLMResponse, LlmBuilder, LlmClient, LlmError, LlmRequest, Message,
    MessageRole, NoToolPolicy, ResolvedModelConfig, RuntimeExecutionBackend, RuntimeHook,
    ScriptedLlmClient, ThreadBackend, ToolCall, ToolDirective, ToolExecutionResult,
    ToolRegistryFactory, ToolResultStatus,
};

#[path = "sdk_resources/client_discovery.rs"]
mod client_discovery;
#[path = "sdk_resources/loader.rs"]
mod loader;
#[path = "sdk_resources/module_helpers.rs"]
mod module_helpers;
#[path = "sdk_resources/prepare_task.rs"]
mod prepare_task;
#[path = "sdk_resources/runtime_options.rs"]
mod runtime_options;
