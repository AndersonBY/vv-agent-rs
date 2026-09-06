fn redis_delete_checkpoint(
    store: &RedisCheckpointStore,
    checkpoint_key: &str,
) -> CheckpointResult<()> {
        let mut connection = store.lock()?;
        let data_key = RedisCheckpointStore::data_key(checkpoint_key);
        let lease_key = RedisCheckpointStore::lease_key(checkpoint_key);
        let receipt_set_key = RedisCheckpointStore::deferred_receipts_checkpoint_set_key(checkpoint_key);
        let controller_set_key = RedisCheckpointStore::controller_receipts_checkpoint_set_key(checkpoint_key);
        let host_set_key = RedisCheckpointStore::host_interactions_checkpoint_set_key(checkpoint_key);
        let notification_set_key =
            RedisCheckpointStore::host_interaction_notifications_checkpoint_set_key(checkpoint_key);
        for _ in 0..TRANSACTION_MAX_ATTEMPTS {
            // Resolve watches both the checkpoint and the receipt index.  A
            // resolver that races this cleanup must invalidate EXEC before it
            // can add a receipt to the set, so no orphan receipt survives.
            redis::cmd("WATCH")
                .arg(&data_key)
                .arg(&lease_key)
                .arg(&receipt_set_key)
                .arg(&controller_set_key)
                .arg(&host_set_key)
                .arg(&notification_set_key)
                .query::<()>(&mut *connection)
                .map_err(redis_error)?;
            let raw_checkpoint = match connection.get::<_, Option<String>>(&data_key) {
                Ok(raw) => raw,
                Err(error) => {
                    redis::cmd("UNWATCH")
                        .query::<()>(&mut *connection)
                        .map_err(redis_error)?;
                    return Err(redis_error(error));
                }
            };
            if let Some(raw_checkpoint) = raw_checkpoint {
                let lease = match connection.get::<_, Option<u64>>(&lease_key) {
                    Ok(lease) => lease,
                    Err(error) => {
                        redis::cmd("UNWATCH")
                            .query::<()>(&mut *connection)
                            .map_err(redis_error)?;
                        return Err(redis_error(error));
                    }
                };
                if let Err(error) = decode_storage_for_key(&raw_checkpoint, lease, checkpoint_key) {
                    redis::cmd("UNWATCH")
                        .query::<()>(&mut *connection)
                        .map_err(redis_error)?;
                    return Err(error);
                }
            }
            let receipt_keys: Vec<String> = match connection.smembers(&receipt_set_key) {
                Ok(keys) => keys,
                Err(error) => {
                    redis::cmd("UNWATCH")
                        .query::<()>(&mut *connection)
                        .map_err(redis_error)?;
                    return Err(redis_error(error));
                }
            };
            let controller_keys: Vec<String> = match connection.smembers(&controller_set_key) {
                Ok(keys) => keys,
                Err(error) => {
                    redis::cmd("UNWATCH")
                        .query::<()>(&mut *connection)
                        .map_err(redis_error)?;
                    return Err(redis_error(error));
                }
            };
            let host_keys: Vec<String> = match connection.smembers(&host_set_key) {
                Ok(keys) => keys,
                Err(error) => {
                    redis::cmd("UNWATCH")
                        .query::<()>(&mut *connection)
                        .map_err(redis_error)?;
                    return Err(redis_error(error));
                }
            };
            let notification_keys: Vec<String> = match connection.smembers(&notification_set_key) {
                Ok(keys) => keys,
                Err(error) => {
                    redis::cmd("UNWATCH")
                        .query::<()>(&mut *connection)
                        .map_err(redis_error)?;
                    return Err(redis_error(error));
                }
            };
            let validation = (|| -> CheckpointResult<()> {
                for receipt_key in &receipt_keys {
                    let raw = connection
                        .get::<_, Option<String>>(receipt_key)
                        .map_err(redis_error)?
                        .ok_or_else(|| {
                            CheckpointError::new(
                                "checkpoint_store_conflict",
                                "Redis deferred receipt index contains a missing member",
                            )
                        })?;
                    let receipt = decode_receipt(&raw)?;
                    if receipt.handle.checkpoint_key != checkpoint_key
                        || RedisCheckpointStore::deferred_receipt_key(&receipt.handle_key)
                            != *receipt_key
                    {
                        return Err(CheckpointError::new(
                            "deferred_receipt_identity_invalid",
                            "Redis deferred receipt index is bound to a different handle",
                        ));
                    }
                }
                for controller_key in &controller_keys {
                    let raw = connection
                        .get::<_, Option<String>>(controller_key)
                        .map_err(redis_error)?
                        .ok_or_else(|| {
                            CheckpointError::new(
                                "checkpoint_store_conflict",
                                "Redis controller receipt index contains a missing member",
                            )
                        })?;
                    let receipt = redis_decode_controller_receipt(&raw)?;
                    if receipt.handle.checkpoint_key != checkpoint_key
                        || RedisCheckpointStore::controller_command_key(&receipt.command_id)
                            != *controller_key
                    {
                        return Err(CheckpointError::new(
                            "controller_command_conflict",
                            "Redis controller receipt index is bound to a different command",
                        ));
                    }
                    let command = redis_load_controller_command(
                        &mut connection,
                        &receipt.command_id,
                        &receipt.command_digest,
                    )?;
                    redis_validate_controller_receipt_binding(&receipt, &command)?;
                    let outbox_key =
                        RedisCheckpointStore::controller_command_outbox_key(&receipt.command_id);
                    let raw_outbox = connection
                        .get::<_, Option<String>>(&outbox_key)
                        .map_err(redis_error)?
                        .ok_or_else(|| {
                            CheckpointError::new(
                                "controller_command_conflict",
                                "Redis controller receipt has no wake outbox",
                            )
                        })?;
                    let wake = RedisControllerWakeOutbox::from_value(&serde_json::from_str(
                        &raw_outbox,
                    )?)?;
                    redis_wake_outbox_matches_receipt(&wake, &receipt)?;
                }
                for host_key in &host_keys {
                    let raw = connection
                        .get::<_, Option<String>>(host_key)
                        .map_err(redis_error)?
                        .ok_or_else(|| {
                            CheckpointError::new(
                                "checkpoint_store_conflict",
                                "Redis host interaction index contains a missing member",
                            )
                        })?;
                    let record = redis_decode_host_record(&raw)?;
                    if record.checkpoint_key != checkpoint_key
                        || RedisCheckpointStore::host_interaction_key(
                            checkpoint_key,
                            &record.interaction_id,
                        ) != *host_key
                    {
                        return Err(CheckpointError::new(
                            "host_interaction_conflict",
                            "Redis host interaction index is bound to a different checkpoint",
                        ));
                    }
                }
                for notification_key in &notification_keys {
                    let raw = connection
                        .get::<_, Option<String>>(notification_key)
                        .map_err(redis_error)?
                        .ok_or_else(|| {
                            CheckpointError::new(
                                "checkpoint_store_conflict",
                                "Redis notification index contains a missing member",
                            )
                        })?;
                    let notification = redis_decode_notification(&raw)?;
                    if notification.checkpoint_key != checkpoint_key
                        || RedisCheckpointStore::host_interaction_notification_key(
                            &notification.notification_id,
                        ) != *notification_key
                    {
                        return Err(CheckpointError::new(
                            "notification_conflict",
                            "Redis notification index is bound to a different checkpoint",
                        ));
                    }
                }
                Ok(())
            })();
            if let Err(error) = validation {
                redis::cmd("UNWATCH")
                    .query::<()>(&mut *connection)
                    .map_err(redis_error)?;
                return Err(error);
            }
            for key in receipt_keys
                .iter()
                .chain(controller_keys.iter())
                .chain(host_keys.iter())
                .chain(notification_keys.iter())
            {
                if let Err(error) = redis::cmd("WATCH")
                    .arg(key)
                    .query::<()>(&mut *connection)
                {
                    redis::cmd("UNWATCH")
                        .query::<()>(&mut *connection)
                        .map_err(redis_error)?;
                    return Err(redis_error(error));
                }
            }
            let mut keys = vec![
                data_key.clone(),
                lease_key.clone(),
                receipt_set_key.clone(),
                controller_set_key.clone(),
                host_set_key.clone(),
                notification_set_key.clone(),
            ];
            keys.extend(receipt_keys);
            keys.extend(controller_keys.iter().cloned());
            keys.extend(
                controller_keys
                    .iter()
                    .flat_map(|key| {
                        [
                            format!("{key}:command"),
                            format!("{key}:outbox"),
                        ]
                    }),
            );
            keys.extend(host_keys);
            keys.extend(notification_keys);
            let mut pipeline = redis::pipe();
            pipeline
                .atomic()
                .del(keys)
                .srem(CHECKPOINT_KEYS_INDEX, checkpoint_key)
                .cmd("PING")
                .ignore();
            match pipeline.query::<Option<()>>(&mut *connection) {
                Ok(Some(())) => {
                    redis::cmd("UNWATCH")
                        .query::<()>(&mut *connection)
                        .map_err(redis_error)?;
                    return Ok(());
                }
                Ok(None) => {
                    redis::cmd("UNWATCH")
                        .query::<()>(&mut *connection)
                        .map_err(redis_error)?;
                }
                Err(error) => {
                    let transaction_error = redis_error(error);
                    redis::cmd("UNWATCH")
                        .query::<()>(&mut *connection)
                        .map_err(redis_error)?;
                    return Err(transaction_error);
                }
            }
        }
        Err(CheckpointError::new(
            "checkpoint_store_transaction_retry_exhausted",
            "Redis checkpoint cleanup retry limit exceeded",
        ))
    }

fn redis_list_checkpoints(store: &RedisCheckpointStore) -> CheckpointResult<Vec<String>> {
    let mut connection = store.lock()?;
    let logical_keys = connection
        .smembers::<_, Vec<String>>(CHECKPOINT_KEYS_INDEX)
        .map_err(redis_error)?;
    let mut checkpoint_keys = Vec::new();
    for checkpoint_key in logical_keys {
        let data_key = RedisCheckpointStore::data_key(&checkpoint_key);
        let Some(raw) = connection
            .get::<_, Option<String>>(&data_key)
            .map_err(redis_error)?
        else {
            continue;
        };
        let lease = connection
            .get::<_, Option<u64>>(RedisCheckpointStore::lease_key(&checkpoint_key))
            .map_err(redis_error)?;
        let checkpoint = decode_storage_for_key(&raw, lease, &checkpoint_key)?;
        checkpoint_keys.push(checkpoint.checkpoint_key);
    }
    checkpoint_keys.sort();
    Ok(checkpoint_keys)
}
