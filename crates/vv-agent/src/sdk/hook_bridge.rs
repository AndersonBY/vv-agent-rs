use std::collections::BTreeMap;
use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

use crate::runtime::normalize_token_usage;
use crate::runtime::{
    AfterLlmEvent, AfterToolCallEvent, BeforeLlmEvent, BeforeLlmPatch, BeforeMemoryCompactEvent,
    BeforeToolCallEvent, BeforeToolCallPatch, RuntimeHook,
};
use crate::tools::ToolContext;
use crate::types::{LLMResponse, Message, TokenUsage, ToolCall, ToolExecutionResult};

#[derive(Debug)]
pub struct RuntimeHookBridge {
    hook_file: PathBuf,
}

impl RuntimeHookBridge {
    pub fn new(hook_file: impl Into<PathBuf>) -> Self {
        Self {
            hook_file: hook_file.into(),
        }
    }

    fn invoke(&self, method: &str, event: Value) -> Option<Value> {
        invoke_agent_hook(&self.hook_file, method, event)
            .unwrap_or_else(|error| panic!("runtime hook failed: {error}"))
    }
}

impl RuntimeHook for RuntimeHookBridge {
    fn before_memory_compact(&self, event: BeforeMemoryCompactEvent<'_>) -> Option<Vec<Message>> {
        let output = self.invoke("before_memory_compact", before_memory_compact_event(event))?;
        parse_messages(output)
    }

    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        let output = self.invoke("before_llm", before_llm_event(event))?;
        parse_before_llm_patch(output)
    }

    fn after_llm(&self, event: AfterLlmEvent<'_>) -> Option<LLMResponse> {
        let output = self.invoke("after_llm", after_llm_event(event))?;
        parse_llm_response(output)
    }

    fn before_tool_call(&self, event: BeforeToolCallEvent<'_>) -> Option<BeforeToolCallPatch> {
        let output = self.invoke("before_tool_call", before_tool_call_event(event))?;
        parse_before_tool_call_patch(output)
    }

    fn after_tool_call(&self, event: AfterToolCallEvent<'_>) -> Option<ToolExecutionResult> {
        let output = self.invoke("after_tool_call", after_tool_call_event(event))?;
        ToolExecutionResult::from_dict(&output).ok()
    }
}

fn invoke_agent_hook(
    hook_file: &Path,
    method: &str,
    event: Value,
) -> Result<Option<Value>, String> {
    let runner = resolve_hook_runner()?;
    let mut child = runner
        .command()
        .arg("-c")
        .arg(HOOK_BRIDGE_SHIM)
        .arg(hook_file)
        .arg(method)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!(
                "failed to start runtime hook bridge with {}: {error}",
                runner.label
            )
        })?;

    let stdin = child
        .stdin
        .as_mut()
        .ok_or_else(|| "failed to open runtime hook stdin".to_string())?;
    stdin
        .write_all(event.to_string().as_bytes())
        .map_err(|error| format!("failed to write runtime hook event: {error}"))?;
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to wait for runtime hook: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "runtime hook failed for {}::{method}: {stderr}",
            hook_file.display()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Ok(None);
    }
    let parsed = serde_json::from_str::<Value>(&stdout)
        .map_err(|error| format!("invalid runtime hook bridge JSON: {error}: {stdout}"))?;
    if parsed.get("kind").and_then(Value::as_str) == Some("none") {
        return Ok(None);
    }
    Ok(parsed.get("value").cloned())
}

#[derive(Debug, Clone)]
struct HookRunner {
    program: String,
    args: Vec<String>,
    current_dir: Option<PathBuf>,
    label: String,
}

impl HookRunner {
    fn new(program: impl Into<String>) -> Self {
        let program = program.into();
        Self {
            label: program.clone(),
            program,
            args: Vec::new(),
            current_dir: None,
        }
    }

    fn uv(project_dir: impl Into<PathBuf>) -> Self {
        let project_dir = project_dir.into();
        Self {
            program: "uv".to_string(),
            args: vec!["run".to_string(), "python".to_string()],
            current_dir: Some(project_dir.clone()),
            label: format!("uv run hook bridge in {}", project_dir.display()),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        if let Some(current_dir) = &self.current_dir {
            command.current_dir(current_dir);
        }
        command
    }
}

fn resolve_hook_runner() -> Result<HookRunner, String> {
    if let Ok(program) = env::var("VV_AGENT_HOOK_RUNNER") {
        let program = program.trim();
        if !program.is_empty() {
            let runner = HookRunner::new(program);
            if runner_supports_hooks(&runner) {
                return Ok(runner);
            }
            return Err(format!(
                "VV_AGENT_HOOK_RUNNER points to {program}, but it is not an interpreter >= 3.12"
            ));
        }
    }

    if let Some(hook_project_dir) = find_hook_runtime_project_dir() {
        let runner = HookRunner::uv(hook_project_dir);
        if runner_supports_hooks(&runner) {
            return Ok(runner);
        }
    }

    let mut errors = Vec::new();
    for program in ["python3.12", "python3.13", "python", "python3"] {
        let runner = HookRunner::new(program);
        if runner_supports_hooks(&runner) {
            return Ok(runner);
        }
        errors.push(program);
    }
    Err(format!(
        "could not find an interpreter for runtime hooks; tried {}. Set VV_AGENT_HOOK_RUNNER to an explicit interpreter path.",
        errors.join(", ")
    ))
}

fn runner_supports_hooks(runner: &HookRunner) -> bool {
    runner
        .command()
        .arg("-c")
        .arg("import sys; import openai; raise SystemExit(0 if sys.version_info >= (3, 12) else 1)")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn find_hook_runtime_project_dir() -> Option<PathBuf> {
    let current = env::current_dir().ok()?;
    for parent in std::iter::once(current.as_path()).chain(current.ancestors()) {
        for candidate in [parent.join("v-agent"), parent.join("../v-agent")] {
            if candidate.join("pyproject.toml").is_file()
                && candidate.join("src").join("vv_agent").is_dir()
            {
                return Some(candidate);
            }
        }
    }
    None
}

fn before_memory_compact_event(event: BeforeMemoryCompactEvent<'_>) -> Value {
    json!({
        "task": event.task.to_dict(),
        "cycle_index": event.cycle_index,
        "messages": messages_to_value(event.messages),
        "shared_state": map_to_value(event.shared_state),
    })
}

fn before_llm_event(event: BeforeLlmEvent<'_>) -> Value {
    json!({
        "task": event.task.to_dict(),
        "cycle_index": event.cycle_index,
        "messages": messages_to_value(event.messages),
        "tool_schemas": event.tool_schemas,
        "shared_state": map_to_value(event.shared_state),
    })
}

fn after_llm_event(event: AfterLlmEvent<'_>) -> Value {
    json!({
        "task": event.task.to_dict(),
        "cycle_index": event.cycle_index,
        "messages": messages_to_value(event.messages),
        "tool_schemas": event.tool_schemas,
        "response": llm_response_to_value(event.response),
        "shared_state": map_to_value(event.shared_state),
    })
}

fn before_tool_call_event(event: BeforeToolCallEvent<'_>) -> Value {
    json!({
        "task": event.task.to_dict(),
        "cycle_index": event.cycle_index,
        "call": event.call.to_dict(),
        "context": tool_context_to_value(event.context),
    })
}

fn after_tool_call_event(event: AfterToolCallEvent<'_>) -> Value {
    json!({
        "task": event.task.to_dict(),
        "cycle_index": event.cycle_index,
        "call": event.call.to_dict(),
        "context": tool_context_to_value(event.context),
        "result": event.result.to_dict(),
    })
}

fn messages_to_value(messages: &[Message]) -> Value {
    Value::Array(messages.iter().map(Message::to_dict).collect())
}

fn llm_response_to_value(response: &LLMResponse) -> Value {
    json!({
        "content": response.content,
        "tool_calls": response.tool_calls.iter().map(ToolCall::to_dict).collect::<Vec<_>>(),
        "raw": map_to_value(&response.raw),
        "token_usage": token_usage_to_value(&response.token_usage),
    })
}

fn token_usage_to_value(usage: &TokenUsage) -> Value {
    json!({
        "prompt_tokens": usage.prompt_tokens,
        "completion_tokens": usage.completion_tokens,
        "total_tokens": usage.total_tokens,
        "cached_tokens": usage.cached_tokens,
        "reasoning_tokens": usage.reasoning_tokens,
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_creation_tokens": usage.cache_creation_tokens,
        "raw": usage.raw,
    })
}

fn tool_context_to_value(context: &ToolContext) -> Value {
    json!({
        "workspace": context.workspace.display().to_string(),
        "shared_state": map_to_value(&context.shared_state),
        "metadata": map_to_value(&context.metadata),
        "cycle_index": context.cycle_index,
    })
}

fn map_to_value(map: &BTreeMap<String, Value>) -> Value {
    Value::Object(map.clone().into_iter().collect())
}

fn parse_messages(value: Value) -> Option<Vec<Message>> {
    value
        .as_array()?
        .iter()
        .map(Message::from_dict)
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

fn parse_before_llm_patch(value: Value) -> Option<BeforeLlmPatch> {
    let object = value.as_object()?;
    let messages = object.get("messages").cloned().and_then(parse_messages);
    let tool_schemas = object
        .get("tool_schemas")
        .and_then(Value::as_array)
        .map(|items| items.to_vec());
    if messages.is_none() && tool_schemas.is_none() {
        return None;
    }
    Some(BeforeLlmPatch {
        messages,
        tool_schemas,
    })
}

fn parse_llm_response(value: Value) -> Option<LLMResponse> {
    let object = value.as_object()?;
    let content = object
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let tool_calls = object
        .get("tool_calls")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .map(ToolCall::from_dict)
                .collect::<Result<Vec<_>, _>>()
                .ok()
        })
        .unwrap_or_default();
    let raw = object
        .get("raw")
        .and_then(Value::as_object)
        .map(|raw| raw.clone().into_iter().collect::<BTreeMap<_, _>>())
        .unwrap_or_default();
    let token_usage = object
        .get("token_usage")
        .and_then(|value| serde_json::from_value::<TokenUsage>(value.clone()).ok())
        .filter(TokenUsage::has_usage)
        .unwrap_or_else(|| normalize_token_usage(raw.get("usage").unwrap_or(&Value::Null)));
    Some(LLMResponse {
        content,
        tool_calls,
        raw,
        token_usage,
    })
}

fn parse_before_tool_call_patch(value: Value) -> Option<BeforeToolCallPatch> {
    let kind = value.get("kind").and_then(Value::as_str);
    match kind {
        Some("tool_call") => value
            .get("value")
            .and_then(|value| ToolCall::from_dict(value).ok())
            .map(BeforeToolCallPatch::call),
        Some("tool_result") => value
            .get("value")
            .and_then(|value| ToolExecutionResult::from_dict(value).ok())
            .map(BeforeToolCallPatch::result),
        Some("patch") => {
            let patch = value.get("value")?;
            let call = patch
                .get("call")
                .and_then(|value| ToolCall::from_dict(value).ok());
            let result = patch
                .get("result")
                .and_then(|value| ToolExecutionResult::from_dict(value).ok());
            if call.is_none() && result.is_none() {
                return None;
            }
            Some(BeforeToolCallPatch { call, result })
        }
        _ => {
            let object = value.as_object()?;
            let call = object
                .get("call")
                .and_then(|value| ToolCall::from_dict(value).ok());
            let result = object
                .get("result")
                .and_then(|value| ToolExecutionResult::from_dict(value).ok());
            if call.is_none() && result.is_none() {
                return None;
            }
            Some(BeforeToolCallPatch { call, result })
        }
    }
}

const HOOK_BRIDGE_SHIM: &str = r#"
from __future__ import annotations

import contextlib
import importlib.util
import json
import pathlib
import sys
import types

_hook_path = pathlib.Path(sys.argv[1]).resolve()
for _base in (_hook_path, pathlib.Path.cwd().resolve()):
    for _parent in [_base, *_base.parents]:
        _candidate = _parent / "v-agent" / "src"
        if (_candidate / "vv_agent").is_dir():
            sys.path.insert(0, str(_candidate))
            break
        _candidate = _parent.parent / "v-agent" / "src"
        if (_candidate / "vv_agent").is_dir():
            sys.path.insert(0, str(_candidate))
            break
    else:
        continue
    break


class _AttrObject:
    def __init__(self, **kwargs):
        self.__dict__.update(kwargs)


def _message_from_dict(data):
    from vv_agent.types import Message

    return Message.from_dict(data)


def _tool_call_from_dict(data):
    from vv_agent.types import ToolCall

    return ToolCall.from_dict(data)


def _tool_result_from_dict(data):
    from vv_agent.types import ToolExecutionResult

    return ToolExecutionResult.from_dict(data)


def _llm_response_from_dict(data):
    from vv_agent.types import LLMResponse, ToolCall

    response = LLMResponse(
        content=str(data.get("content") or ""),
        tool_calls=[ToolCall.from_dict(item) for item in data.get("tool_calls") or []],
    )
    raw = data.get("raw")
    if isinstance(raw, dict):
        response.raw.update(raw)
    return response


def _task_from_dict(data):
    from vv_agent.types import AgentTask

    return AgentTask.from_dict(data)


def _context_from_dict(data):
    return _AttrObject(
        workspace=data.get("workspace"),
        shared_state=data.get("shared_state") or {},
        metadata=data.get("metadata") or {},
        cycle_index=data.get("cycle_index") or 0,
    )


def _load_hooks(path):
    module_name = f"vv_agent_user_hook_bridge_{abs(hash(path))}"
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        return []
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    candidates = []
    create_hook = getattr(module, "create_hook", None)
    if callable(create_hook):
        created = create_hook()
        if isinstance(created, list):
            candidates.extend(created)
        elif created is not None:
            candidates.append(created)
    for attr in ("HOOK", "HOOKS"):
        if not hasattr(module, attr):
            continue
        value = getattr(module, attr)
        if isinstance(value, list):
            candidates.extend(value)
        else:
            candidates.append(value)
    return [hook for hook in candidates if callable(getattr(hook, method, None))]


def _value_to_json(value):
    from vv_agent.runtime import BeforeLLMPatch, BeforeToolCallPatch
    from vv_agent.types import LLMResponse, Message, ToolCall, ToolExecutionResult

    if value is None:
        return {"kind": "none"}
    if isinstance(value, LLMResponse):
        token_usage = getattr(value, "token_usage", None)
        return {"kind": "llm_response", "value": {
            "content": value.content,
            "tool_calls": [call.to_dict() for call in value.tool_calls],
            "raw": dict(value.raw),
            "token_usage": token_usage.to_dict() if token_usage is not None else {},
        }}
    if isinstance(value, BeforeLLMPatch):
        return {"kind": "before_llm_patch", "value": {
            "messages": [message.to_dict() for message in value.messages] if value.messages is not None else None,
            "tool_schemas": value.tool_schemas,
        }}
    if isinstance(value, BeforeToolCallPatch):
        return {"kind": "patch", "value": {
            "call": value.call.to_dict() if value.call is not None else None,
            "result": value.result.to_dict() if value.result is not None else None,
        }}
    if isinstance(value, ToolCall):
        return {"kind": "tool_call", "value": value.to_dict()}
    if isinstance(value, ToolExecutionResult):
        return {"kind": "tool_result", "value": value.to_dict()}
    if isinstance(value, list) and all(isinstance(item, Message) for item in value):
        return {"kind": "messages", "value": [message.to_dict() for message in value]}
    return {"kind": "json", "value": value}


def _apply_hooks(method, payload):
    hooks = _load_hooks(hook_path)
    if not hooks:
        return None

    task = _task_from_dict(payload["task"])
    cycle_index = int(payload.get("cycle_index") or 0)
    shared_state = payload.get("shared_state") or {}

    if method == "before_memory_compact":
        from vv_agent.runtime import BeforeMemoryCompactEvent

        current_messages = [_message_from_dict(item) for item in payload.get("messages") or []]
        changed = False
        for hook in hooks:
            handler = getattr(hook, method, None)
            if not callable(handler):
                continue
            value = handler(BeforeMemoryCompactEvent(
                task=task,
                cycle_index=cycle_index,
                messages=list(current_messages),
                shared_state=shared_state,
            ))
            if value is not None:
                current_messages = list(value)
                changed = True
        return current_messages if changed else None

    if method == "before_llm":
        from vv_agent.runtime import BeforeLLMEvent, BeforeLLMPatch

        current_messages = [_message_from_dict(item) for item in payload.get("messages") or []]
        current_tool_schemas = list(payload.get("tool_schemas") or [])
        messages_changed = False
        schemas_changed = False
        for hook in hooks:
            handler = getattr(hook, method, None)
            if not callable(handler):
                continue
            value = handler(BeforeLLMEvent(
                task=task,
                cycle_index=cycle_index,
                messages=list(current_messages),
                tool_schemas=list(current_tool_schemas),
                shared_state=shared_state,
            ))
            if value is None:
                continue
            if value.messages is not None:
                current_messages = list(value.messages)
                messages_changed = True
            if value.tool_schemas is not None:
                current_tool_schemas = list(value.tool_schemas)
                schemas_changed = True
        if not messages_changed and not schemas_changed:
            return None
        return BeforeLLMPatch(
            messages=current_messages if messages_changed else None,
            tool_schemas=current_tool_schemas if schemas_changed else None,
        )

    if method == "after_llm":
        from vv_agent.runtime import AfterLLMEvent

        current_response = _llm_response_from_dict(payload.get("response") or {})
        changed = False
        messages = [_message_from_dict(item) for item in payload.get("messages") or []]
        tool_schemas = list(payload.get("tool_schemas") or [])
        for hook in hooks:
            handler = getattr(hook, method, None)
            if not callable(handler):
                continue
            value = handler(AfterLLMEvent(
                task=task,
                cycle_index=cycle_index,
                messages=list(messages),
                tool_schemas=list(tool_schemas),
                response=current_response,
                shared_state=shared_state,
            ))
            if value is not None:
                current_response = value
                changed = True
        return current_response if changed else None

    if method == "before_tool_call":
        from vv_agent.runtime import BeforeToolCallEvent, BeforeToolCallPatch
        from vv_agent.types import ToolCall, ToolExecutionResult

        current_call = _tool_call_from_dict(payload.get("call") or {})
        context = _context_from_dict(payload.get("context") or {})
        call_changed = False
        short_circuit = None
        for hook in hooks:
            handler = getattr(hook, method, None)
            if not callable(handler):
                continue
            value = handler(BeforeToolCallEvent(
                task=task,
                cycle_index=cycle_index,
                call=current_call,
                context=context,
            ))
            if value is None:
                continue
            if isinstance(value, ToolExecutionResult):
                short_circuit = value
                break
            if isinstance(value, ToolCall):
                current_call = value
                call_changed = True
                continue
            if isinstance(value, BeforeToolCallPatch):
                if value.call is not None:
                    current_call = value.call
                    call_changed = True
                if value.result is not None:
                    short_circuit = value.result
                    break
        if short_circuit is not None:
            if call_changed:
                return BeforeToolCallPatch(call=current_call, result=short_circuit)
            return short_circuit
        if call_changed:
            return current_call
        return None

    if method == "after_tool_call":
        from vv_agent.runtime import AfterToolCallEvent

        call = _tool_call_from_dict(payload.get("call") or {})
        context = _context_from_dict(payload.get("context") or {})
        current_result = _tool_result_from_dict(payload.get("result") or {})
        changed = False
        for hook in hooks:
            handler = getattr(hook, method, None)
            if not callable(handler):
                continue
            value = handler(AfterToolCallEvent(
                task=task,
                cycle_index=cycle_index,
                call=call,
                context=context,
                result=current_result,
            ))
            if value is not None:
                current_result = value
                changed = True
        return current_result if changed else None

    raise ValueError(f"unknown hook method: {method}")


hook_path = sys.argv[1]
method = sys.argv[2]
payload = json.load(sys.stdin)
result = _value_to_json(_apply_hooks(method, payload))
json.dump(result, sys.stdout, ensure_ascii=False, separators=(",", ":"))
"#;
