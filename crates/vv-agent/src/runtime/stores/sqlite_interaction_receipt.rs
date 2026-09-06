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
