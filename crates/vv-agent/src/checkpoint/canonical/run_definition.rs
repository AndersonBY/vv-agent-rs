use std::cmp::Ordering;
use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::{
    canonical_json_bytes, resolve_json_pointer, sha256_canonical, utf16_cmp,
    validate_capability_slot, validate_extension_namespace, validate_i_json, validate_pointer,
    CheckpointError, CheckpointResult, CREDENTIAL_REDACTED, MAX_WIRE_INTEGER,
    RUN_DEFINITION_SCHEMA,
};

mod normalization;

pub use normalization::{normalize_run_definition, redact_run_definition};
use normalization::{normalize_tool_policy, validate_header_names};

pub fn run_definition_digest(definition: &Value) -> CheckpointResult<String> {
    validate_run_definition(definition)?;
    sha256_canonical(definition, "run_definition")
}

pub fn canonical_run_definition_bytes(definition: &Value) -> CheckpointResult<Vec<u8>> {
    validate_run_definition(definition)?;
    canonical_json_bytes(definition, "run_definition")
}

pub fn validate_run_definition(definition: &Value) -> CheckpointResult<()> {
    validate_i_json(definition, "run_definition")?;
    let object = definition.as_object().ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition must be an object",
        )
    })?;
    let required = [
        "schema_version",
        "agent",
        "root_input",
        "prompt_bundle",
        "initial_messages",
        "initial_shared_state",
        "run_metadata",
        "context_ref",
        "model",
        "credential_slots",
        "runtime_controls",
        "tools",
        "tool_policy",
        "checkpoint_policy",
        "budget_limits",
        "output_schema",
        "workspace_ref",
        "session_ref",
        "extensions",
        "capability_refs",
    ];
    let required_set = required.iter().copied().collect::<BTreeSet<_>>();
    if object
        .keys()
        .any(|key| !required_set.contains(key.as_str()))
        || required.iter().any(|key| !object.contains_key(*key))
    {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition has missing or unknown top-level fields",
        ));
    }
    if object.get("schema_version").and_then(Value::as_str) != Some(RUN_DEFINITION_SCHEMA) {
        return Err(CheckpointError::new(
            "checkpoint_definition_schema_unsupported",
            "run_definition schema_version is unsupported",
        ));
    }
    validate_agent(object.get("agent"))?;
    require_string(object.get("root_input"), "run_definition.root_input")?;
    crate::prompt::PromptBundle::from_value(object.get("prompt_bundle").ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition.prompt_bundle is missing",
        )
    })?)
    .map_err(|error| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("run_definition.prompt_bundle is invalid: {error}"),
        )
    })?;
    validate_initial_messages(object.get("initial_messages"))?;
    require_object(
        object.get("initial_shared_state"),
        "run_definition.initial_shared_state",
    )?;
    require_object(object.get("run_metadata"), "run_definition.run_metadata")?;
    validate_model(object.get("model"))?;
    let runtime_controls = require_object(
        object.get("runtime_controls"),
        "run_definition.runtime_controls",
    )?;
    validate_runtime_controls(runtime_controls)?;
    require_array(object.get("tools"), "run_definition.tools")?;
    require_object(object.get("tool_policy"), "run_definition.tool_policy")?;
    require_object(
        object.get("checkpoint_policy"),
        "run_definition.checkpoint_policy",
    )?;
    require_object(
        object.get("capability_refs"),
        "run_definition.capability_refs",
    )?;

    validate_optional_ref(object.get("context_ref"), "run_definition.context_ref")?;
    validate_budget_limits(object.get("budget_limits"))?;
    validate_optional_object_or_null(object.get("output_schema"), "run_definition.output_schema")?;
    validate_optional_ref(object.get("workspace_ref"), "run_definition.workspace_ref")?;
    validate_optional_ref(object.get("session_ref"), "run_definition.session_ref")?;

    let slots = require_array(
        object.get("credential_slots"),
        "run_definition.credential_slots",
    )?;
    validate_credential_slots(definition, slots)?;
    validate_capability_refs(
        object.get("capability_refs"),
        "run_definition.capability_refs",
    )?;
    validate_extensions(object.get("extensions"), "run_definition.extensions")?;
    validate_tool_definitions(object.get("tools"))?;
    validate_tool_policy(object.get("tool_policy"))?;
    validate_checkpoint_policy(object.get("checkpoint_policy"))?;
    validate_header_names(object.get("model"))?;
    Ok(())
}

fn validate_agent(value: Option<&Value>) -> CheckpointResult<()> {
    let object = require_closed_object(
        value,
        "run_definition.agent",
        &["name", "type"],
        &["name", "type"],
    )?;
    require_non_empty_string(object.get("name"), "run_definition.agent.name")?;
    if let Some(agent_type) = object.get("type").filter(|value| !value.is_null()) {
        require_non_empty_string(Some(agent_type), "run_definition.agent.type")?;
    }
    Ok(())
}

fn validate_initial_messages(value: Option<&Value>) -> CheckpointResult<()> {
    let messages = require_array(value, "run_definition.initial_messages")?;
    for (index, message) in messages.iter().enumerate() {
        validate_initial_message(message, index)?;
    }
    Ok(())
}

fn validate_initial_message(message: &Value, index: usize) -> CheckpointResult<()> {
    let label = format!("run_definition.initial_messages[{index}]");
    let object = require_closed_object(
        Some(message),
        &label,
        &[
            "role",
            "content",
            "name",
            "tool_call_id",
            "tool_calls",
            "reasoning_content",
            "image_url",
            "metadata",
            "artifact_ref",
        ],
        &["role", "content"],
    )?;
    if !matches!(
        object.get("role").and_then(Value::as_str),
        Some("system" | "user" | "assistant" | "tool")
    ) {
        return definition_error(format!("{label}.role is invalid"));
    }
    require_string(object.get("content"), &format!("{label}.content"))?;
    for field in ["name", "tool_call_id", "reasoning_content", "image_url"] {
        if object.contains_key(field) {
            require_string(object.get(field), &format!("{label}.{field}"))?;
        }
    }
    if let Some(metadata) = object.get("metadata") {
        require_object(Some(metadata), &format!("{label}.metadata"))?;
    }
    if let Some(artifact) = object.get("artifact_ref") {
        let parsed = serde_json::from_value::<crate::types::ToolArtifactRef>(artifact.clone())
            .map_err(|error| {
                CheckpointError::new(
                    "checkpoint_definition_invalid",
                    format!("{label}.artifact_ref is invalid: {error}"),
                )
            })?;
        parsed.validate().map_err(|error| {
            CheckpointError::new(
                "checkpoint_definition_invalid",
                format!("{label}.artifact_ref is invalid: {error}"),
            )
        })?;
    }
    if let Some(tool_calls) = object.get("tool_calls") {
        for (call_index, call) in require_array(Some(tool_calls), &format!("{label}.tool_calls"))?
            .iter()
            .enumerate()
        {
            validate_initial_tool_call(call, &format!("{label}.tool_calls[{call_index}]"))?;
        }
    }
    Ok(())
}

fn validate_initial_tool_call(value: &Value, label: &str) -> CheckpointResult<()> {
    let object = require_closed_object(
        Some(value),
        label,
        &["id", "type", "function", "extra_content"],
        &["id", "type", "function"],
    )?;
    require_non_empty_string(object.get("id"), &format!("{label}.id"))?;
    if object.get("type").and_then(Value::as_str) != Some("function") {
        return definition_error(format!("{label}.type must be function"));
    }
    let function = require_closed_object(
        object.get("function"),
        &format!("{label}.function"),
        &["name", "arguments"],
        &["name", "arguments"],
    )?;
    require_non_empty_string(function.get("name"), &format!("{label}.function.name"))?;
    let arguments = require_string(
        function.get("arguments"),
        &format!("{label}.function.arguments"),
    )?;
    let decoded = serde_json::from_str::<Value>(arguments).map_err(|_| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("{label}.function.arguments must contain JSON"),
        )
    })?;
    if !decoded.is_object() {
        return definition_error(format!(
            "{label}.function.arguments must contain a JSON object"
        ));
    }
    if let Some(extra_content) = object.get("extra_content") {
        require_object(Some(extra_content), &format!("{label}.extra_content"))?;
    }
    Ok(())
}

fn validate_model(value: Option<&Value>) -> CheckpointResult<()> {
    let object = require_closed_object(
        value,
        "run_definition.model",
        &[
            "backend",
            "model_id",
            "settings",
            "transport_timeout_seconds",
        ],
        &[
            "backend",
            "model_id",
            "settings",
            "transport_timeout_seconds",
        ],
    )?;
    require_non_empty_string(object.get("backend"), "run_definition.model.backend")?;
    require_non_empty_string(object.get("model_id"), "run_definition.model.model_id")?;
    let settings = require_object(object.get("settings"), "run_definition.model.settings")?;
    if settings.contains_key("timeout_seconds") {
        return definition_error(
            "run_definition.model.settings must not contain transport timeout_seconds",
        );
    }
    let parsed = serde_json::from_value::<crate::model_settings::ModelSettings>(Value::Object(
        settings.clone(),
    ))
    .map_err(|error| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("run_definition.model.settings is invalid: {error}"),
        )
    })?;
    parsed.validate().map_err(|error| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("run_definition.model.settings is invalid: {error}"),
        )
    })?;
    let parsed_wire = parsed.to_value();
    let source_wire = Value::Object(settings.clone());
    if canonical_json_bytes(&parsed_wire, "run_definition.model.settings")?
        != canonical_json_bytes(&source_wire, "run_definition.model.settings")?
    {
        return definition_error(
            "run_definition.model.settings must use the complete current wire shape",
        );
    }
    validate_optional_positive_number(
        object.get("transport_timeout_seconds"),
        "run_definition.model.transport_timeout_seconds",
    )
}

fn validate_runtime_controls(controls: &Map<String, Value>) -> CheckpointResult<()> {
    let required = [
        "max_cycles",
        "max_handoffs",
        "no_tool_policy",
        "session_memory_enabled",
        "memory_compact_threshold",
        "memory_threshold_percentage",
        "microcompaction_policy",
        "allow_interruption",
        "native_multimodal",
        "tool_use_behavior",
        "stop_at_tool_names",
    ];
    if controls.len() != required.len()
        || required.iter().any(|field| !controls.contains_key(*field))
    {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition.runtime_controls has missing or unknown fields",
        ));
    }

    let max_cycles = controls
        .get("max_cycles")
        .and_then(Value::as_u64)
        .filter(|value| (1..=u64::from(u32::MAX)).contains(value));
    if max_cycles.is_none() {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition.runtime_controls.max_cycles is invalid",
        ));
    }
    if controls
        .get("max_handoffs")
        .and_then(Value::as_u64)
        .is_none_or(|value| value > u64::from(u32::MAX))
    {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition.runtime_controls.max_handoffs is invalid",
        ));
    }
    if !matches!(
        controls.get("no_tool_policy").and_then(Value::as_str),
        Some("continue" | "wait_user" | "finish")
    ) {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition.runtime_controls.no_tool_policy is invalid",
        ));
    }
    for field in [
        "session_memory_enabled",
        "allow_interruption",
        "native_multimodal",
    ] {
        if controls.get(field).and_then(Value::as_bool).is_none() {
            return Err(CheckpointError::new(
                "checkpoint_definition_invalid",
                format!("run_definition.runtime_controls.{field} must be boolean"),
            ));
        }
    }
    if controls
        .get("memory_compact_threshold")
        .and_then(Value::as_u64)
        .is_none()
    {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition.runtime_controls.memory_compact_threshold is invalid",
        ));
    }
    if controls
        .get("memory_threshold_percentage")
        .and_then(Value::as_u64)
        .is_none_or(|value| value > u64::from(u8::MAX))
    {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition.runtime_controls.memory_threshold_percentage is invalid",
        ));
    }

    let policy_value = controls.get("microcompaction_policy").ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition.runtime_controls.microcompaction_policy is missing",
        )
    })?;
    let policy =
        serde_json::from_value::<crate::memory::MicrocompactionPolicy>(policy_value.clone())
            .map_err(|error| {
                CheckpointError::new(
                    "checkpoint_definition_invalid",
                    format!(
                        "run_definition.runtime_controls.microcompaction_policy is invalid: {error}"
                    ),
                )
            })?;
    if serde_json::to_value(policy).expect("MicrocompactionPolicy is serializable") != *policy_value
    {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition.runtime_controls.microcompaction_policy must use the complete current wire shape",
        ));
    }

    if !matches!(
        controls.get("tool_use_behavior").and_then(Value::as_str),
        Some("run_llm_again" | "stop_on_first_tool" | "stop_at_tool_names")
    ) {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition.runtime_controls.tool_use_behavior is invalid",
        ));
    }
    let stop_names = require_array(
        controls.get("stop_at_tool_names"),
        "run_definition.runtime_controls.stop_at_tool_names",
    )?;
    let mut unique_stop_names = BTreeSet::new();
    if stop_names.iter().any(|name| {
        name.as_str()
            .filter(|name| !name.trim().is_empty() && unique_stop_names.insert(*name))
            .is_none()
    }) {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition.runtime_controls.stop_at_tool_names must contain unique strings",
        ));
    }
    Ok(())
}

fn validate_capability_refs(value: Option<&Value>, field_name: &str) -> CheckpointResult<()> {
    let object = require_object(value, field_name)?;
    for (slot, reference) in object {
        validate_capability_slot(slot)?;
        let reference_object = require_closed_object(
            Some(reference),
            &format!("{field_name}.{slot}"),
            &["id", "version"],
            &["id", "version"],
        )?;
        require_non_empty_string(reference_object.get("id"), "capability reference id")?;
        require_non_empty_string(
            reference_object.get("version"),
            "capability reference version",
        )?;
    }
    Ok(())
}

fn validate_extensions(value: Option<&Value>, field_name: &str) -> CheckpointResult<()> {
    let extensions = require_array(value, field_name)?;
    let mut previous: Option<&str> = None;
    for extension in extensions {
        let object = require_object(Some(extension), "run_definition extension")?;
        require_exact_fields(
            object,
            "run_definition extension",
            &["namespace", "version", "required"],
        )?;
        let namespace = require_string(object.get("namespace"), "extension namespace")?;
        validate_extension_namespace(namespace)?;
        require_non_empty_string(object.get("version"), "extension version")?;
        if !object.get("required").is_some_and(Value::is_boolean) {
            return Err(CheckpointError::new(
                "checkpoint_definition_invalid",
                "extension required must be boolean",
            ));
        }
        if let Some(previous) = previous {
            if previous >= namespace {
                return Err(CheckpointError::new(
                    "checkpoint_definition_invalid",
                    "extensions must be sorted and unique by namespace",
                ));
            }
        }
        previous = Some(namespace);
    }
    Ok(())
}

fn validate_tool_definitions(value: Option<&Value>) -> CheckpointResult<()> {
    let tools = require_array(value, "run_definition.tools")?;
    for tool in tools {
        let object = require_object(Some(tool), "run_definition tool")?;
        let required = ["schema", "tool_metadata", "timeout_seconds", "approval"];
        if object.len() != required.len()
            || required.iter().any(|field| !object.contains_key(*field))
        {
            return Err(CheckpointError::new(
                "checkpoint_definition_invalid",
                "run_definition tool has missing or unknown fields",
            ));
        }
        let schema = require_closed_object(
            object.get("schema"),
            "run_definition tool schema",
            &["type", "function"],
            &["type", "function"],
        )?;
        if schema.get("type").and_then(Value::as_str) != Some("function") {
            return definition_error("run_definition tool schema type must be function");
        }
        let function = require_closed_object(
            schema.get("function"),
            "run_definition tool function schema",
            &["name", "description", "parameters", "strict"],
            &["name", "description", "parameters"],
        )?;
        require_non_empty_string(
            function.get("name"),
            "run_definition tool function schema name",
        )?;
        require_string(
            function.get("description"),
            "run_definition tool function schema description",
        )?;
        require_object(
            function.get("parameters"),
            "run_definition tool function schema parameters",
        )?;
        if function
            .get("strict")
            .is_some_and(|value| !value.is_boolean())
        {
            return definition_error("run_definition tool function schema strict must be boolean");
        }
        let metadata = object.get("tool_metadata").ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_definition_invalid",
                "run_definition tool metadata is required",
            )
        })?;
        let normalized = if metadata.is_null() {
            None
        } else {
            Some(
                serde_json::from_value::<crate::tools::ToolMetadata>(metadata.clone()).map_err(
                    |error| {
                        CheckpointError::new(
                            "checkpoint_definition_invalid",
                            format!("run_definition tool metadata is invalid: {error}"),
                        )
                    },
                )?,
            )
        };
        if normalized
            .as_ref()
            .map(|value| serde_json::to_value(value).expect("tool metadata serializes"))
            .unwrap_or(Value::Null)
            != *metadata
        {
            return Err(CheckpointError::new(
                "checkpoint_definition_invalid",
                "run_definition tool metadata is not normalized",
            ));
        }
        validate_optional_positive_number(
            object.get("timeout_seconds"),
            "run_definition tool timeout_seconds",
        )?;
        let approval = require_object(object.get("approval"), "run_definition tool approval")?;
        match approval.get("mode").and_then(Value::as_str) {
            Some("static") => {
                require_exact_fields(
                    approval,
                    "run_definition tool approval",
                    &["mode", "required"],
                )?;
                if !approval.get("required").is_some_and(Value::is_boolean) {
                    return definition_error(
                        "run_definition static tool approval required must be boolean",
                    );
                }
            }
            Some("referenced") => {
                require_exact_fields(approval, "run_definition tool approval", &["mode", "ref"])?;
                validate_ref_value(
                    approval.get("ref").ok_or_else(|| {
                        CheckpointError::new(
                            "checkpoint_definition_invalid",
                            "run_definition referenced tool approval ref is missing",
                        )
                    })?,
                    "run_definition tool approval ref",
                )?;
            }
            _ => return definition_error("run_definition tool approval mode is invalid"),
        }
    }
    Ok(())
}

fn validate_tool_policy(value: Option<&Value>) -> CheckpointResult<()> {
    let object = require_object(value, "run_definition.tool_policy")?;
    require_exact_fields(
        object,
        "run_definition.tool_policy",
        &[
            "allowed_tools",
            "disallowed_tools",
            "approval",
            "predicate_ref",
            "approval_timeout_seconds",
            "denied_side_effects",
            "denied_capability_tags",
            "deny_terminal_tools",
            "denied_cost_dimensions",
        ],
    )?;
    if let Some(allowed) = object.get("allowed_tools") {
        if !allowed.is_null() {
            validate_sorted_unique_strings(Some(allowed), "tool_policy.allowed_tools")?;
        }
    }
    validate_sorted_unique_strings(
        object.get("disallowed_tools"),
        "tool_policy.disallowed_tools",
    )?;
    if let Some(predicate) = object.get("predicate_ref") {
        if !predicate.is_null() {
            validate_ref_value(predicate, "tool_policy.predicate_ref")?;
        }
    }
    if !matches!(
        object.get("approval").and_then(Value::as_str),
        Some("default" | "always" | "never" | "on_request")
    ) {
        return definition_error("run_definition.tool_policy.approval is invalid");
    }
    validate_optional_positive_number(
        object.get("approval_timeout_seconds"),
        "run_definition.tool_policy.approval_timeout_seconds",
    )?;
    let denied_side_effects = validate_sorted_unique_strings(
        object.get("denied_side_effects"),
        "tool_policy.denied_side_effects",
    )?;
    if denied_side_effects.iter().any(|value| {
        !matches!(
            *value,
            "unknown" | "none" | "read" | "write" | "execute" | "network" | "external"
        )
    }) {
        return definition_error(
            "run_definition.tool_policy.denied_side_effects contains an invalid value",
        );
    }
    validate_sorted_unique_strings(
        object.get("denied_capability_tags"),
        "tool_policy.denied_capability_tags",
    )?;
    if !object
        .get("deny_terminal_tools")
        .is_some_and(Value::is_boolean)
    {
        return definition_error("run_definition.tool_policy.deny_terminal_tools must be boolean");
    }
    validate_sorted_unique_strings(
        object.get("denied_cost_dimensions"),
        "tool_policy.denied_cost_dimensions",
    )?;
    let mut normalized = Value::Object(object.clone());
    normalize_tool_policy(Some(&mut normalized))?;
    if normalized != Value::Object(object.clone()) {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition tool metadata policy is not normalized",
        ));
    }
    Ok(())
}

fn validate_checkpoint_policy(value: Option<&Value>) -> CheckpointResult<()> {
    let object = require_object(value, "run_definition.checkpoint_policy")?;
    require_exact_fields(
        object,
        "run_definition.checkpoint_policy",
        &[
            "ambiguous_model_policy",
            "ambiguous_tool_policy",
            "max_extension_state_bytes",
        ],
    )?;
    if !matches!(
        object.get("ambiguous_model_policy").and_then(Value::as_str),
        Some("require_reconciliation" | "retry_with_duplicate_risk")
    ) {
        return definition_error(
            "run_definition.checkpoint_policy.ambiguous_model_policy is invalid",
        );
    }
    if !matches!(
        object.get("ambiguous_tool_policy").and_then(Value::as_str),
        Some("require_reconciliation" | "retry_idempotent_only")
    ) {
        return definition_error(
            "run_definition.checkpoint_policy.ambiguous_tool_policy is invalid",
        );
    }
    let max = object.get("max_extension_state_bytes").ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            "checkpoint_policy.max_extension_state_bytes is required",
        )
    })?;
    let Some(max) = max.as_u64() else {
        return Err(CheckpointError::new(
            "checkpoint_definition_not_i_json",
            "max_extension_state_bytes must be a safe integer",
        ));
    };
    if max > MAX_WIRE_INTEGER {
        return Err(CheckpointError::new(
            "checkpoint_definition_not_i_json",
            "max_extension_state_bytes exceeds the safe integer range",
        ));
    }
    Ok(())
}

fn validate_budget_limits(value: Option<&Value>) -> CheckpointResult<()> {
    let Some(value) = value else {
        return definition_error("run_definition.budget_limits is missing");
    };
    if value.is_null() {
        return Ok(());
    }
    let object = require_closed_object(
        Some(value),
        "run_definition.budget_limits",
        &[
            "max_total_tokens",
            "max_uncached_input_tokens",
            "max_tool_calls",
            "max_tool_calls_by_name",
            "max_wall_time_ms",
            "max_host_cost",
            "unavailable_metric_policy",
        ],
        &[
            "max_total_tokens",
            "max_uncached_input_tokens",
            "max_tool_calls",
            "max_tool_calls_by_name",
            "max_wall_time_ms",
            "max_host_cost",
            "unavailable_metric_policy",
        ],
    )?;
    if let Some(host_cost) = object.get("max_host_cost").filter(|value| !value.is_null()) {
        require_closed_object(
            Some(host_cost),
            "run_definition.budget_limits.max_host_cost",
            &["unit", "amount_microunits", "currency"],
            &["unit", "amount_microunits", "currency"],
        )?;
    }
    let parsed = serde_json::from_value::<crate::budget::RunBudgetLimits>(value.clone()).map_err(
        |error| {
            CheckpointError::new(
                "checkpoint_definition_invalid",
                format!("run_definition.budget_limits is invalid: {error}"),
            )
        },
    )?;
    if serde_json::to_value(parsed).expect("RunBudgetLimits is serializable") != *value {
        return definition_error(
            "run_definition.budget_limits must use the complete current wire shape",
        );
    }
    Ok(())
}

fn validate_credential_slots(definition: &Value, slots: &[Value]) -> CheckpointResult<()> {
    let mut validated = Vec::with_capacity(slots.len());
    for slot in slots {
        let slot = slot.as_str().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_credential_slots_invalid",
                "credential slots must be strings",
            )
        })?;
        validate_pointer(slot)?;
        let resolved = resolve_json_pointer(definition, slot)?;
        if resolved.as_str() != Some(CREDENTIAL_REDACTED) {
            return Err(CheckpointError::new(
                "checkpoint_credential_value_not_redacted",
                format!("credential slot {slot} is not redacted"),
            ));
        }
        validated.push(slot);
    }
    for pair in validated.windows(2) {
        if utf16_cmp(pair[0], pair[1]) != Ordering::Less {
            return Err(CheckpointError::new(
                "checkpoint_credential_slots_invalid",
                "credential slots must be sorted and unique",
            ));
        }
    }
    Ok(())
}

fn validate_sorted_unique_strings<'a>(
    value: Option<&'a Value>,
    field_name: &str,
) -> CheckpointResult<Vec<&'a str>> {
    let values = require_array(value, field_name)?;
    let mut previous: Option<&str> = None;
    let mut validated = Vec::with_capacity(values.len());
    for value in values {
        let string = require_non_empty_string(Some(value), field_name)?;
        if let Some(previous) = previous {
            if utf16_cmp(previous, string) != Ordering::Less {
                return Err(CheckpointError::new(
                    "checkpoint_definition_invalid",
                    format!("{field_name} must be sorted and unique"),
                ));
            }
        }
        previous = Some(string);
        validated.push(string);
    }
    Ok(validated)
}

fn validate_ref_value(value: &Value, field_name: &str) -> CheckpointResult<()> {
    let object = require_object(Some(value), field_name)?;
    if object.len() != 2 || !object.contains_key("id") || !object.contains_key("version") {
        return Err(CheckpointError::new(
            "checkpoint_capability_ref_invalid",
            format!("{field_name} must contain exactly id and version"),
        ));
    }
    require_non_empty_string(object.get("id"), &format!("{field_name}.id"))?;
    require_non_empty_string(object.get("version"), &format!("{field_name}.version"))?;
    Ok(())
}

fn require_closed_object<'a>(
    value: Option<&'a Value>,
    field_name: &str,
    allowed: &[&str],
    required: &[&str],
) -> CheckpointResult<&'a Map<String, Value>> {
    let object = require_object(value, field_name)?;
    let allowed = allowed.iter().copied().collect::<BTreeSet<_>>();
    let missing = required
        .iter()
        .filter(|field| !object.contains_key(**field))
        .copied()
        .collect::<Vec<_>>();
    let unknown = object
        .keys()
        .filter(|field| !allowed.contains(field.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() || !unknown.is_empty() {
        return definition_error(format!(
            "{field_name} has invalid fields: missing={missing:?}, unknown={unknown:?}"
        ));
    }
    Ok(object)
}

fn require_exact_fields(
    object: &Map<String, Value>,
    field_name: &str,
    fields: &[&str],
) -> CheckpointResult<()> {
    if object.len() != fields.len() || fields.iter().any(|field| !object.contains_key(*field)) {
        return definition_error(format!("{field_name} has missing or unknown fields"));
    }
    Ok(())
}

fn require_string<'a>(value: Option<&'a Value>, field_name: &str) -> CheckpointResult<&'a str> {
    value.and_then(Value::as_str).ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("{field_name} must be a string"),
        )
    })
}

fn require_non_empty_string<'a>(
    value: Option<&'a Value>,
    field_name: &str,
) -> CheckpointResult<&'a str> {
    let value = require_string(value, field_name)?;
    if value.trim().is_empty() {
        return definition_error(format!("{field_name} must be non-empty"));
    }
    Ok(value)
}

fn require_object<'a>(
    value: Option<&'a Value>,
    field_name: &str,
) -> CheckpointResult<&'a Map<String, Value>> {
    value.and_then(Value::as_object).ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("{field_name} must be an object"),
        )
    })
}

fn require_array<'a>(
    value: Option<&'a Value>,
    field_name: &str,
) -> CheckpointResult<&'a Vec<Value>> {
    value.and_then(Value::as_array).ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("{field_name} must be an array"),
        )
    })
}

fn validate_optional_object_or_null(
    value: Option<&Value>,
    field_name: &str,
) -> CheckpointResult<()> {
    if value.is_some_and(|value| !value.is_object() && !value.is_null()) {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("{field_name} must be an object or null"),
        ));
    }
    Ok(())
}

fn validate_optional_ref(value: Option<&Value>, field_name: &str) -> CheckpointResult<()> {
    let Some(value) = value else {
        return definition_error(format!("{field_name} is missing"));
    };
    if value.is_null() {
        return Ok(());
    }
    validate_ref_value(value, field_name)
}

fn validate_optional_positive_number(
    value: Option<&Value>,
    field_name: &str,
) -> CheckpointResult<()> {
    let Some(value) = value else {
        return definition_error(format!("{field_name} is missing"));
    };
    if value.is_null() {
        return Ok(());
    }
    if value
        .as_f64()
        .is_none_or(|number| !number.is_finite() || number <= 0.0)
    {
        return definition_error(format!(
            "{field_name} must be a finite positive number or null"
        ));
    }
    Ok(())
}

fn definition_error<T>(message: impl Into<String>) -> CheckpointResult<T> {
    Err(CheckpointError::new(
        "checkpoint_definition_invalid",
        message.into(),
    ))
}
