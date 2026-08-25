impl RedisCheckpointStore {
    fn replace_claimed(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
        kind: ReplaceKind,
    ) -> CheckpointResult<bool> {
        let data_key = Self::data_key(&checkpoint.checkpoint_key);
        let lease_key = Self::lease_key(&checkpoint.checkpoint_key);
        let result = self.transaction(&data_key, &lease_key, |connection, pipeline| {
            let Some(raw) = connection
                .get::<_, Option<String>>(&data_key)
                .map_err(redis_error)?
            else {
                return Ok(None);
            };
            let current = decode_storage(
                &raw,
                connection
                    .get::<_, Option<u64>>(&lease_key)
                    .map_err(redis_error)?,
            )?;
            let updated = match kind {
                ReplaceKind::Progress => {
                    prepare_progress(&current, checkpoint.clone(), claim_token, expected_revision)?
                }
                ReplaceKind::Suspend => {
                    prepare_suspend(&current, checkpoint.clone(), claim_token, expected_revision)?
                }
                ReplaceKind::Commit => {
                    prepare_commit(&current, checkpoint.clone(), claim_token, expected_revision)?
                }
                ReplaceKind::FinalizeClaimed => prepare_finalize_claimed(
                    &current,
                    checkpoint.clone(),
                    claim_token,
                    expected_revision,
                )?,
            };
            let Some(updated) = updated else {
                return Ok(None);
            };
            let payload = checkpoint_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?;
            pipeline.set(&data_key, payload).ignore();
            if updated.claim_token.is_none() {
                pipeline.del(&lease_key).ignore();
            }
            Ok(Some(true))
        });
        match result {
            Ok(value) => Ok(value),
            Err(error) if error.code() == "checkpoint_store_conflict" => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[derive(Clone, Copy)]
enum ReplaceKind {
    Progress,
    Suspend,
    Commit,
    FinalizeClaimed,
}

fn decode_storage(raw: &str, lease: Option<u64>) -> CheckpointResult<Checkpoint> {
    let mut value: Value = serde_json::from_str(raw)
        .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string()))?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "lease_expires_at_ms".to_string(),
            lease.map_or(Value::Null, Value::from),
        );
    }
    let payload = serde_json::to_string(&value)
        .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string()))?;
    checkpoint_from_json(&payload, MAX_EXTENSION_STATE_BYTES)
}

fn encode_receipt(receipt: &crate::checkpoint::DeferredReceipt) -> CheckpointResult<String> {
    receipt.validate()?;
    serde_json::to_string(receipt)
        .map_err(|error| CheckpointError::new("deferred_receipt_invalid", error.to_string()))
}

fn decode_receipt(raw: &str) -> CheckpointResult<crate::checkpoint::DeferredReceipt> {
    let receipt: crate::checkpoint::DeferredReceipt = serde_json::from_str(raw)
        .map_err(|error| CheckpointError::new("deferred_receipt_invalid", error.to_string()))?;
    receipt.validate()?;
    Ok(receipt)
}

fn redis_error(error: redis::RedisError) -> CheckpointError {
    CheckpointError::new("checkpoint_store_redis", error.to_string())
}
