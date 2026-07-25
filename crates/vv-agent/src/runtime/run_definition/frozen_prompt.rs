use super::*;

pub(super) fn validate_frozen_checkpoint_messages(
    messages: &[Message],
    prompt_bundle: &PromptBundle,
) -> CheckpointResult<()> {
    let Some(first) = messages.first() else {
        return Err(CheckpointError::new(
            "checkpoint_definition_mismatch",
            "checkpoint is missing its frozen system message",
        ));
    };
    if first.role != MessageRole::System || first.content != prompt_bundle.flatten() {
        return Err(CheckpointError::new(
            "checkpoint_definition_mismatch",
            "checkpoint system message does not match the frozen prompt bundle",
        ));
    }
    if messages
        .iter()
        .skip(1)
        .any(|message| message.role == MessageRole::System)
    {
        return Err(CheckpointError::new(
            "checkpoint_definition_mismatch",
            "checkpoint contains a non-canonical system message",
        ));
    }
    Ok(())
}
