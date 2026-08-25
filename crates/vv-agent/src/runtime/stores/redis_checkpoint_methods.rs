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
            let receipt_keys: Vec<String> =
                connection.smembers(&receipt_set_key).map_err(redis_error)?;
            let controller_keys: Vec<String> = connection
                .smembers(&controller_set_key)
                .map_err(redis_error)?;
            let host_keys: Vec<String> = connection.smembers(&host_set_key).map_err(redis_error)?;
            let notification_keys: Vec<String> = connection
                .smembers(&notification_set_key)
                .map_err(redis_error)?;
            for key in receipt_keys
                .iter()
                .chain(controller_keys.iter())
                .chain(host_keys.iter())
                .chain(notification_keys.iter())
            {
                redis::cmd("WATCH")
                    .arg(key)
                    .query::<()>(&mut *connection)
                    .map_err(redis_error)?;
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
                Ok(None) => continue,
                Err(error) => return Err(redis_error(error)),
            }
        }
        Err(CheckpointError::new(
            "checkpoint_store_transaction_retry_exhausted",
            "Redis checkpoint cleanup retry limit exceeded",
        ))
    }

fn redis_list_checkpoints(store: &RedisCheckpointStore) -> CheckpointResult<Vec<String>> {
        let mut connection = store.lock()?;
        let keys = connection
            .scan_match::<_, String>(format!("{KEY_PREFIX}*"))
            .map_err(redis_error)?
            .filter(|key| !key.ends_with(LEASE_SUFFIX))
            .collect::<Vec<_>>();
        let mut checkpoint_keys = Vec::new();
        for key in keys {
            let Some(raw) = connection
                .get::<_, Option<String>>(&key)
                .map_err(redis_error)?
            else {
                continue;
            };
            let checkpoint = decode_storage(
                &raw,
                connection
                    .get::<_, Option<u64>>(format!("{key}{LEASE_SUFFIX}"))
                    .map_err(redis_error)?,
            )?;
            checkpoint_keys.push(checkpoint.checkpoint_key);
        }
        checkpoint_keys.sort();
        Ok(checkpoint_keys)
    }
