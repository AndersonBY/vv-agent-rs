use std::collections::BTreeMap;

use crate::memory::artifacts::{
    build_compacted_tool_content, has_recovery_envelope, is_compacted_tool_content,
    ToolResultArtifactConfig,
};
use crate::memory::token_utils::count_messages_tokens;
use crate::tools::ToolResultRetention;
use crate::types::{Message, MessageRole, ToolArtifactRef};
use crate::workspace::{persist_text_artifact, read_validated_text_artifact};

use super::MemoryManager;

#[derive(Debug, Clone)]
pub(crate) struct MicrocompactionPlan {
    candidates: Vec<MicrocompactionCandidate>,
    current_usage: u64,
    target_usage: u64,
    pub(crate) candidate_count: usize,
    pub(crate) estimated_reclaimable_tokens: u64,
}

#[derive(Debug, Clone)]
struct MicrocompactionCandidate {
    message_index: usize,
    tool_name: String,
    existing_artifact: Option<ToolArtifactRef>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MicrocompactionApplication {
    pub(crate) messages: Vec<Message>,
    pub(crate) archived_count: usize,
    pub(crate) reclaimed_tokens: u64,
    pub(crate) artifact_failure_count: usize,
}

impl MicrocompactionPlan {
    pub(crate) fn empty() -> Self {
        Self {
            candidates: Vec::new(),
            current_usage: 0,
            target_usage: 0,
            candidate_count: 0,
            estimated_reclaimable_tokens: 0,
        }
    }

    pub(crate) fn has_candidates(&self) -> bool {
        !self.candidates.is_empty()
    }
}

impl MemoryManager {
    pub(crate) fn validate_model_recovery_surface(
        &mut self,
        messages: &[Message],
        tool_schemas: &[serde_json::Value],
    ) -> Result<(), String> {
        let recovery_tool_available = tool_schemas.iter().any(|schema| {
            schema["function"]["name"].as_str() == Some(crate::constants::READ_FILE_TOOL_NAME)
        });
        self.recovery_tool_available = recovery_tool_available;
        if !recovery_tool_available
            && messages
                .iter()
                .any(|message| is_compacted_tool_content(&message.content))
        {
            return Err(
                "microcompaction_recovery_unavailable: compacted tool results require \
                 a model-visible read_file tool"
                    .to_string(),
            );
        }
        Ok(())
    }

    pub(crate) fn plan_cycle_microcompaction(
        &self,
        messages: &[Message],
        current_cycle: u32,
        current_usage: u64,
    ) -> MicrocompactionPlan {
        let cleaned = self.remove_previous_summary(messages);
        let sanitized = crate::memory::message_sanitizer::filter_empty_assistant_messages(&cleaned);
        self.plan_microcompaction(&sanitized, current_cycle, current_usage)
    }

    pub(crate) fn plan_microcompaction(
        &self,
        messages: &[Message],
        current_cycle: u32,
        current_usage: u64,
    ) -> MicrocompactionPlan {
        if !self.recovery_tool_available
            || messages.is_empty()
            || current_usage <= self.microcompact_trigger_threshold()
        {
            return MicrocompactionPlan::empty();
        }

        let policy = self.config.microcompaction_policy;
        let tool_call_names = build_tool_call_name_map(messages);
        let inferred_cycles = infer_message_cycles(messages);
        let max_inferred_cycle = inferred_cycles.last().copied().unwrap_or_default();
        let effective_current_cycle = current_cycle.max(max_inferred_cycle.saturating_add(1));
        let protected_cycle = effective_current_cycle.saturating_sub(policy.keep_recent_cycles);
        let target = self.microcompact_target_threshold();
        let marker_config = self.marker_config();
        let mut candidates = Vec::new();
        let mut estimated_reclaimable_tokens = 0u64;

        for (message_index, (message, inferred_cycle)) in
            messages.iter().zip(inferred_cycles).enumerate()
        {
            let Some(tool_name) = eligible_tool_name(
                message,
                inferred_cycle,
                protected_cycle,
                policy.min_result_chars as usize,
                &tool_call_names,
                &self.tool_result_retentions,
            ) else {
                continue;
            };
            let artifact_path = message
                .artifact_ref
                .as_ref()
                .map(|artifact| artifact.path.as_str())
                .unwrap_or(".vv-agent/artifacts/task/call-00000000000000000000000000000000.txt");
            let excerpt_source = message
                .metadata
                .get("_vv_agent_microcompact_excerpt")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&message.content);
            let estimated_marker = build_compacted_tool_content(
                excerpt_source,
                artifact_path,
                tool_name,
                &marker_config,
            );
            let before = count_messages_tokens(std::slice::from_ref(message), &self.config.model);
            let after = count_messages_tokens(
                &[Message::tool(
                    estimated_marker,
                    message.tool_call_id.clone().unwrap_or_default(),
                )],
                &self.config.model,
            );
            let reclaimable = before.saturating_sub(after);
            if reclaimable == 0 {
                continue;
            }
            estimated_reclaimable_tokens = estimated_reclaimable_tokens.saturating_add(reclaimable);
            candidates.push(MicrocompactionCandidate {
                message_index,
                tool_name: tool_name.to_string(),
                existing_artifact: message.artifact_ref.clone(),
            });
        }

        MicrocompactionPlan {
            candidate_count: candidates.len(),
            candidates,
            current_usage,
            target_usage: target,
            estimated_reclaimable_tokens,
        }
    }

    pub(crate) fn apply_microcompaction(
        &self,
        messages: &[Message],
        plan: &MicrocompactionPlan,
    ) -> MicrocompactionApplication {
        if !plan.has_candidates() {
            return MicrocompactionApplication {
                messages: messages.to_vec(),
                ..MicrocompactionApplication::default()
            };
        }
        let Some(backend) = self.workspace_backend.as_ref() else {
            return MicrocompactionApplication {
                messages: messages.to_vec(),
                artifact_failure_count: plan.candidate_count,
                ..MicrocompactionApplication::default()
            };
        };

        let marker_config = self.marker_config();
        let mut updated = messages.to_vec();
        let mut archived_count = 0usize;
        let mut reclaimed_tokens = 0u64;
        let mut artifact_failure_count = 0usize;
        let mut projected_usage = plan.current_usage;
        for candidate in &plan.candidates {
            if projected_usage <= plan.target_usage {
                break;
            }
            let message = &messages[candidate.message_index];
            let Some((artifact, complete_content)) =
                archive_tool_message(self, backend, message, candidate)
            else {
                artifact_failure_count += 1;
                continue;
            };
            let marker = build_compacted_tool_content(
                &complete_content,
                &artifact.path,
                &candidate.tool_name,
                &marker_config,
            );
            let before = count_messages_tokens(std::slice::from_ref(message), &self.config.model);
            let mut replacement = message.clone();
            replacement.content = marker;
            replacement.artifact_ref = Some(artifact);
            replacement
                .metadata
                .remove("_vv_agent_microcompact_excerpt");
            let after =
                count_messages_tokens(std::slice::from_ref(&replacement), &self.config.model);
            let actual_reclaimed = before.saturating_sub(after);
            if actual_reclaimed == 0 {
                continue;
            }
            reclaimed_tokens = reclaimed_tokens.saturating_add(actual_reclaimed);
            updated[candidate.message_index] = replacement;
            archived_count += 1;
            projected_usage = projected_usage.saturating_sub(actual_reclaimed);
        }
        MicrocompactionApplication {
            messages: updated,
            archived_count,
            reclaimed_tokens,
            artifact_failure_count,
        }
    }

    pub fn microcompact_trigger_threshold(&self) -> u64 {
        (self.autocompact_threshold() as f64 * self.config.microcompaction_policy.trigger_ratio)
            .floor() as u64
    }

    pub fn microcompact_target_threshold(&self) -> u64 {
        (self.autocompact_threshold() as f64 * self.config.microcompaction_policy.target_ratio)
            .floor() as u64
    }

    pub fn should_preemptive_microcompact(&self, message_length: u64) -> bool {
        let threshold = self.microcompact_trigger_threshold();
        threshold > 0 && message_length > threshold
    }

    pub fn microcompact_messages(
        &self,
        messages: &[Message],
        cycle_index: Option<u32>,
    ) -> (Vec<Message>, usize) {
        let Some(cycle_index) = cycle_index else {
            return (messages.to_vec(), 0);
        };
        let current_usage = count_messages_tokens(messages, &self.config.model);
        let plan = self.plan_microcompaction(messages, cycle_index, current_usage);
        let application = self.apply_microcompaction(messages, &plan);
        (application.messages, application.archived_count)
    }

    fn marker_config(&self) -> ToolResultArtifactConfig {
        ToolResultArtifactConfig {
            artifact_namespace: self.artifact_namespace.clone(),
            compact_threshold: self.config.tool_result_compact_threshold,
            keep_last: self.config.tool_result_keep_last,
            excerpt_head: self.config.tool_result_excerpt_head,
            excerpt_tail: self.config.tool_result_excerpt_tail,
        }
    }
}

fn archive_tool_message(
    manager: &MemoryManager,
    backend: &std::sync::Arc<dyn crate::workspace::WorkspaceBackend>,
    message: &Message,
    candidate: &MicrocompactionCandidate,
) -> Option<(ToolArtifactRef, String)> {
    if let Some(artifact) = candidate.existing_artifact.as_ref() {
        let complete_content = read_validated_text_artifact(backend.as_ref(), artifact).ok()?;
        return Some((artifact.clone(), complete_content));
    }
    if has_recovery_envelope(&message.content) {
        return None;
    }
    let artifact = persist_text_artifact(
        backend.clone(),
        &manager.artifact_namespace,
        message.tool_call_id.as_deref().unwrap_or("tool-result"),
        &message.content,
    )
    .ok()?;
    Some((artifact, message.content.clone()))
}

fn build_tool_call_name_map(messages: &[Message]) -> BTreeMap<String, String> {
    let mut tool_call_names = BTreeMap::new();
    for message in messages {
        if message.role != MessageRole::Assistant {
            continue;
        }
        for tool_call in &message.tool_calls {
            tool_call_names.insert(tool_call.id.clone(), tool_call.name.clone());
        }
    }
    tool_call_names
}

fn infer_message_cycles(messages: &[Message]) -> Vec<u32> {
    let mut current_cycle = 0;
    let mut inferred = Vec::with_capacity(messages.len());
    for message in messages {
        if message.role == MessageRole::Assistant {
            current_cycle += 1;
        }
        inferred.push(current_cycle);
    }
    inferred
}

fn eligible_tool_name<'a>(
    message: &'a Message,
    inferred_cycle: u32,
    protected_cycle: u32,
    min_result_chars: usize,
    tool_call_names: &'a BTreeMap<String, String>,
    retentions: &BTreeMap<String, ToolResultRetention>,
) -> Option<&'a str> {
    if message.role != MessageRole::Tool
        || inferred_cycle >= protected_cycle
        || message.content.chars().count() <= min_result_chars
        || is_compacted_tool_content(&message.content)
        || (message.artifact_ref.is_none() && has_recovery_envelope(&message.content))
    {
        return None;
    }
    let tool_name = tool_call_names.get(message.tool_call_id.as_deref()?)?;
    (retentions.get(tool_name).copied().unwrap_or_default() == ToolResultRetention::Archive)
        .then_some(tool_name.as_str())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::memory::{MemoryManager, MemoryManagerConfig};
    use crate::types::Message;
    use crate::workspace::MemoryWorkspaceBackend;

    use super::{MicrocompactionCandidate, MicrocompactionPlan};

    #[test]
    fn application_skips_zero_gain_replacement_and_continues_to_later_candidate() {
        let manager = MemoryManager::new(MemoryManagerConfig::default())
            .with_workspace_backend(Arc::new(MemoryWorkspaceBackend::default()));
        let messages = vec![
            Message::tool("small result", "first"),
            Message::tool("large result ".repeat(1_000), "second"),
        ];
        let plan = MicrocompactionPlan {
            candidates: vec![
                MicrocompactionCandidate {
                    message_index: 0,
                    tool_name: "first_tool".to_string(),
                    existing_artifact: None,
                },
                MicrocompactionCandidate {
                    message_index: 1,
                    tool_name: "second_tool".to_string(),
                    existing_artifact: None,
                },
            ],
            current_usage: 100,
            target_usage: 99,
            candidate_count: 2,
            estimated_reclaimable_tokens: 200,
        };

        let applied = manager.apply_microcompaction(&messages, &plan);

        assert_eq!(applied.archived_count, 1);
        assert_eq!(applied.artifact_failure_count, 0);
        assert_eq!(applied.messages[0], messages[0]);
        assert!(applied.messages[1]
            .content
            .starts_with(crate::memory::TOOL_RESULT_COMPACT_MARKER));
    }
}
