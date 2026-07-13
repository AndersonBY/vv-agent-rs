use std::io::{Error, Result as IoResult};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vv_agent::runtime::backends::{
    run_checkpointed_cycle, CycleDispatchResult, CycleDispatcher, DistributedBackend,
    InlineBackend, RuntimeExecutionBackend, RuntimeRecipe, ThreadBackend,
};
use vv_agent::runtime::state::{Checkpoint, InMemoryStateStore, StateStore, StateStoreSpec};
use vv_agent::{
    AgentResult, AgentRuntime, AgentStatus, AgentTask, CancellationToken, CycleRecord, LLMResponse,
    Message, ScriptedLlmClient, TaskTokenUsage,
};

include!("runtime_backends/core.rs");
include!("runtime_backends/distributed_execution.rs");
include!("runtime_backends/scheduler_durability.rs");
