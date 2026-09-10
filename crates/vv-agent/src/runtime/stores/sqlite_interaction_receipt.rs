type SqliteControllerWakeRow = (
    ControllerCommandReceipt,
    String,
    String,
    u64,
    Option<String>,
    Option<u64>,
    Option<u64>,
    Option<String>,
);

fn load_controller_wake_row(
    transaction: &Transaction<'_>,
    command_id: &str,
    command_digest: &str,
) -> CheckpointResult<Option<SqliteControllerWakeRow>> {
    let Some((bound_receipt, bound_command)) = load_controller_receipt(transaction, command_id)?
    else {
        return Ok(None);
    };
    if bound_receipt.command_digest != command_digest
        || bound_command.command_digest != command_digest
    {
        return Err(CheckpointError::new(
            "controller_command_conflict",
            "controller wake command digest is not bound to its receipt",
        ));
    }
    let raw = transaction
        .query_row(
            "SELECT receipt, command_digest, outbox_state, attempt, claim_token, lease_expires_at_ms, delivered_at_ms, last_error FROM controller_command_receipts WHERE command_id = ?1 AND command_digest = ?2",
            params![command_id, command_digest],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    raw.map(
        |(
            receipt,
            stored_digest,
            state,
            attempt,
            claim_token,
            lease,
            delivered_at,
            last_error,
        )| {
            if stored_digest != command_digest {
                return Err(CheckpointError::new(
                    "controller_command_conflict",
                    "controller wake digest does not match receipt",
                ));
            }
            let mut receipt = ControllerCommandReceipt::from_value(&serde_json::from_str(&receipt)?)?;
            if receipt.command_id != command_id || receipt.command_digest != stored_digest {
                return Err(CheckpointError::new(
                    "controller_command_conflict",
                    "controller wake receipt identity conflicts with its indexed row",
                ));
            }
            receipt.outbox_state = state;
            receipt.outbox_attempt = to_u64(attempt)?;
            receipt.validate()?;
            let lease = lease.map(to_u64).transpose()?;
            let delivered_at = delivered_at.map(to_u64).transpose()?;
            if (claim_token.is_some()) != lease.is_some()
                || (receipt.outbox_state == "claimed") != claim_token.is_some()
            {
                return Err(CheckpointError::new(
                    "controller_command_conflict",
                    "controller wake claim and lease are inconsistent",
                ));
            }
            if claim_token
                .as_ref()
                .is_some_and(|value| value.trim().is_empty() || value.len() > 512)
                || lease.is_some_and(|value| value > crate::checkpoint::MAX_WIRE_INTEGER)
                || delivered_at.is_some_and(|value| value > crate::checkpoint::MAX_WIRE_INTEGER)
                || last_error
                    .as_ref()
                    .is_some_and(|value| value.len() > crate::checkpoint::HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES)
            {
                return Err(CheckpointError::new(
                    "controller_command_conflict",
                    "controller wake lifecycle metadata is invalid",
                ));
            }
            Ok((
                receipt,
                stored_digest,
                command_id.to_string(),
                to_u64(attempt)?,
                claim_token,
                lease,
                delivered_at,
                last_error,
            ))
        },
    )
    .transpose()
}

fn load_controller_receipt(
    transaction: &Transaction<'_>,
    command_id: &str,
) -> CheckpointResult<Option<(ControllerCommandReceipt, ControllerCommand)>> {
    let raw = transaction
        .query_row(
            "SELECT receipt, command, checkpoint_key, handle, command_digest, resume_attempt, expected_revision, resulting_status, resulting_revision, outbox_id, outbox_action, outbox_destination FROM controller_command_receipts WHERE command_id = ?1",
            params![command_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, Option<String>>(11)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    raw.map(
        |(
            receipt,
            command,
            checkpoint_key,
            handle,
            stored_digest,
            resume_attempt,
            expected_revision,
            resulting_status,
            resulting_revision,
            outbox_id,
            outbox_action,
            outbox_destination,
        )| {
            let receipt = ControllerCommandReceipt::from_value(&serde_json::from_str(&receipt)?)?;
            let command = ControllerCommand::from_value(&serde_json::from_str(&command)?)?;
            let row_handle = crate::checkpoint::ControllerHandle::from_value(
                &serde_json::from_str(&handle)?,
            )?;
            let resume_attempt = to_u64(resume_attempt)?;
            let expected_revision = to_u64(expected_revision)?;
            let resulting_revision = to_u64(resulting_revision)?;
            if receipt.command_id != command_id
                || receipt.command_id != command.command_id
                || receipt.command_digest != command.command_digest
                || stored_digest != command.command_digest
                || checkpoint_key != command.handle.checkpoint_key
                || row_handle != command.handle
                || receipt.handle != command.handle
                || receipt.resume_attempt != command.resume_attempt
                || receipt.resume_attempt != resume_attempt
                || receipt.expected_revision != command.expected_revision
                || receipt.expected_revision != expected_revision
                || receipt.resulting_status != resulting_status
                || receipt.resulting_revision != resulting_revision
                || outbox_id
                    != crate::checkpoint::controller_receipt_outbox_id(
                        command_id,
                        &command.command_digest,
                    )?
                || receipt.outbox_action != outbox_action
                || receipt.outbox_destination != outbox_destination
            {
                return Err(CheckpointError::new(
                    "controller_command_conflict",
                    "controller receipt and command payload identity conflicts",
                ));
            }
            Ok((receipt, command))
        },
    )
    .transpose()
}
