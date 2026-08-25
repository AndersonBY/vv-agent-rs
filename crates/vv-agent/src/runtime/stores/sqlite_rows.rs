impl SqliteCheckpointStore {
    fn replace_claimed(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
        kind: ReplaceKind,
    ) -> CheckpointResult<bool> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let Some(current) = load_row_transaction(&transaction, &checkpoint.checkpoint_key)? else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(false);
        };
        let updated = match kind {
            ReplaceKind::Progress => {
                prepare_progress(&current, checkpoint, claim_token, expected_revision)?
            }
            ReplaceKind::Suspend => {
                prepare_suspend(&current, checkpoint, claim_token, expected_revision)?
            }
            ReplaceKind::Commit => {
                prepare_commit(&current, checkpoint, claim_token, expected_revision)?
            }
            ReplaceKind::FinalizeClaimed => {
                prepare_finalize_claimed(&current, checkpoint, claim_token, expected_revision)?
            }
        };
        let Some(updated) = updated else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(false);
        };
        let values = SqlValues::from_checkpoint(&updated)?;
        let changed = update_row(
            &transaction,
            &values,
            Some(expected_revision),
            Some(claim_token),
        )?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(changed)
    }
}

#[derive(Clone, Copy)]
enum ReplaceKind {
    Progress,
    Suspend,
    Commit,
    FinalizeClaimed,
}

struct SqlValues {
    checkpoint_key: String,
    schema_version: String,
    run_definition_schema: String,
    run_definition: String,
    task_id: String,
    root_run_id: String,
    trace_id: String,
    run_definition_digest: String,
    resume_attempt: i64,
    cycle_index: i64,
    status: String,
    active_host_interaction: Option<String>,
    suspended_origin: Option<String>,
    messages: String,
    cycles: String,
    model_calls: String,
    shared_state: String,
    budget_usage: Option<String>,
    event_cursor: Option<String>,
    event_outbox: String,
    extension_state: String,
    model_call_journal: String,
    tool_journal: String,
    revision: i64,
    claim_token: Option<String>,
    claimed_cycle: Option<i64>,
    lease_expires_at_ms: Option<i64>,
    terminal_result: Option<String>,
    terminal_acknowledged: i64,
}

impl SqlValues {
    fn from_checkpoint(checkpoint: &Checkpoint) -> CheckpointResult<Self> {
        let value = checkpoint_to_value(checkpoint, MAX_EXTENSION_STATE_BYTES)?;
        let object = value.as_object().expect("codec emits an object");
        Ok(Self {
            checkpoint_key: string_field(object, "checkpoint_key")?,
            schema_version: string_field(object, "schema_version")?,
            run_definition_schema: string_field(object, "run_definition_schema")?,
            run_definition: json_field(object, "run_definition")?,
            task_id: string_field(object, "task_id")?,
            root_run_id: string_field(object, "root_run_id")?,
            trace_id: string_field(object, "trace_id")?,
            run_definition_digest: string_field(object, "run_definition_digest")?,
            resume_attempt: to_i64(checkpoint.resume_attempt, "resume_attempt")?,
            cycle_index: to_i64(checkpoint.cycle_index, "cycle_index")?,
            status: string_field(object, "status")?,
            active_host_interaction: nullable_json_field(object, "active_host_interaction")?,
            suspended_origin: nullable_json_field(object, "suspended_origin")?,
            messages: json_field(object, "messages")?,
            cycles: json_field(object, "cycles")?,
            model_calls: json_field(object, "model_calls")?,
            shared_state: json_field(object, "shared_state")?,
            budget_usage: nullable_json_field(object, "budget_usage")?,
            event_cursor: nullable_json_field(object, "event_cursor")?,
            event_outbox: json_field(object, "event_outbox")?,
            extension_state: json_field(object, "extension_state")?,
            model_call_journal: json_field(object, "model_call_journal")?,
            tool_journal: json_field(object, "tool_journal")?,
            revision: to_i64(checkpoint.revision, "revision")?,
            claim_token: checkpoint.claim_token.clone(),
            claimed_cycle: checkpoint
                .claimed_cycle
                .map(|value| to_i64(value, "claimed_cycle"))
                .transpose()?,
            lease_expires_at_ms: checkpoint
                .lease_expires_at_ms
                .map(|value| to_i64(value, "lease_expires_at_ms"))
                .transpose()?,
            terminal_result: nullable_json_field(object, "terminal_result")?,
            terminal_acknowledged: i64::from(checkpoint.terminal_acknowledged),
        })
    }

    fn params(&self) -> [&(dyn rusqlite::ToSql + Sync); 29] {
        [
            &self.checkpoint_key,
            &self.schema_version,
            &self.run_definition_schema,
            &self.run_definition,
            &self.task_id,
            &self.root_run_id,
            &self.trace_id,
            &self.run_definition_digest,
            &self.resume_attempt,
            &self.cycle_index,
            &self.status,
            &self.active_host_interaction,
            &self.suspended_origin,
            &self.messages,
            &self.cycles,
            &self.model_calls,
            &self.shared_state,
            &self.budget_usage,
            &self.event_cursor,
            &self.event_outbox,
            &self.extension_state,
            &self.model_call_journal,
            &self.tool_journal,
            &self.revision,
            &self.claim_token,
            &self.claimed_cycle,
            &self.lease_expires_at_ms,
            &self.terminal_result,
            &self.terminal_acknowledged,
        ]
    }
}

fn update_row(
    transaction: &Transaction<'_>,
    values: &SqlValues,
    expected_revision: Option<u64>,
    claim_token: Option<&str>,
) -> CheckpointResult<bool> {
    let Some(expected_revision) = expected_revision else {
        return Err(CheckpointError::new(
            "checkpoint_revision_conflict",
            "an expected revision is required for an update",
        ));
    };
    let changed = transaction
        .execute(
            r#"
            UPDATE checkpoints SET
                schema_version = ?1, run_definition_schema = ?2, run_definition = ?3,
                task_id = ?4, root_run_id = ?5, trace_id = ?6, run_definition_digest = ?7,
                resume_attempt = ?8, cycle_index = ?9, status = ?10,
                active_host_interaction = ?11, suspended_origin = ?12,
                messages = ?13, cycles = ?14, model_calls = ?15, shared_state = ?16,
                budget_usage = ?17, event_cursor = ?18, event_outbox = ?19,
                extension_state = ?20, model_call_journal = ?21, tool_journal = ?22,
                revision = ?23, claim_token = ?24, claimed_cycle = ?25,
                lease_expires_at_ms = ?26, terminal_result = ?27,
                terminal_acknowledged = ?28
            WHERE checkpoint_key = ?29 AND revision = ?30
              AND (?31 IS NULL OR claim_token = ?31)
            "#,
            params![
                values.schema_version,
                values.run_definition_schema,
                values.run_definition,
                values.task_id,
                values.root_run_id,
                values.trace_id,
                values.run_definition_digest,
                values.resume_attempt,
                values.cycle_index,
                values.status,
                values.active_host_interaction,
                values.suspended_origin,
                values.messages,
                values.cycles,
                values.model_calls,
                values.shared_state,
                values.budget_usage,
                values.event_cursor,
                values.event_outbox,
                values.extension_state,
                values.model_call_journal,
                values.tool_journal,
                values.revision,
                values.claim_token,
                values.claimed_cycle,
                values.lease_expires_at_ms,
                values.terminal_result,
                values.terminal_acknowledged,
                values.checkpoint_key,
                to_i64(expected_revision, "revision")?,
                claim_token,
            ],
        )
        .map_err(sqlite_error)?;
    Ok(changed == 1)
}

fn load_row(connection: &Connection, checkpoint_key: &str) -> CheckpointResult<Option<Checkpoint>> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT checkpoint_key, schema_version, run_definition_schema, run_definition,
                   task_id, root_run_id, trace_id, run_definition_digest, resume_attempt,
                   cycle_index, status, active_host_interaction, suspended_origin,
                   messages, cycles, model_calls, shared_state,
                   budget_usage, event_cursor, event_outbox, extension_state,
                   model_call_journal, tool_journal, revision, claim_token, claimed_cycle,
                   lease_expires_at_ms, terminal_result, terminal_acknowledged
            FROM checkpoints WHERE checkpoint_key = ?1
            "#,
        )
        .map_err(sqlite_error)?;
    statement
        .query_row(params![checkpoint_key], row_to_checkpoint)
        .optional()
        .map_err(sqlite_error)?
        .transpose()
}

fn load_row_transaction(
    transaction: &Transaction<'_>,
    checkpoint_key: &str,
) -> CheckpointResult<Option<Checkpoint>> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT checkpoint_key, schema_version, run_definition_schema, run_definition,
                   task_id, root_run_id, trace_id, run_definition_digest, resume_attempt,
                   cycle_index, status, active_host_interaction, suspended_origin,
                   messages, cycles, model_calls, shared_state,
                   budget_usage, event_cursor, event_outbox, extension_state,
                   model_call_journal, tool_journal, revision, claim_token, claimed_cycle,
                   lease_expires_at_ms, terminal_result, terminal_acknowledged
            FROM checkpoints WHERE checkpoint_key = ?1
            "#,
        )
        .map_err(sqlite_error)?;
    statement
        .query_row(params![checkpoint_key], row_to_checkpoint)
        .optional()
        .map_err(sqlite_error)?
        .transpose()
}

fn row_to_checkpoint(row: &rusqlite::Row<'_>) -> rusqlite::Result<CheckpointResult<Checkpoint>> {
    let checkpoint_key: String = row.get(0)?;
    let schema_version: String = row.get(1)?;
    let run_definition_schema: String = row.get(2)?;
    let run_definition: String = row.get(3)?;
    let task_id: String = row.get(4)?;
    let root_run_id: String = row.get(5)?;
    let trace_id: String = row.get(6)?;
    let run_definition_digest: String = row.get(7)?;
    let resume_attempt: i64 = row.get(8)?;
    let cycle_index: i64 = row.get(9)?;
    let status: String = row.get(10)?;
    let active_host_interaction: Option<String> = row.get(11)?;
    let suspended_origin: Option<String> = row.get(12)?;
    let messages: String = row.get(13)?;
    let cycles: String = row.get(14)?;
    let model_calls: String = row.get(15)?;
    let shared_state: String = row.get(16)?;
    let budget_usage: Option<String> = row.get(17)?;
    let event_cursor: Option<String> = row.get(18)?;
    let event_outbox: String = row.get(19)?;
    let extension_state: String = row.get(20)?;
    let model_call_journal: String = row.get(21)?;
    let tool_journal: String = row.get(22)?;
    let revision: i64 = row.get(23)?;
    let claim_token: Option<String> = row.get(24)?;
    let claimed_cycle: Option<i64> = row.get(25)?;
    let lease_expires_at_ms: Option<i64> = row.get(26)?;
    let terminal_result: Option<String> = row.get(27)?;
    let terminal_acknowledged: i64 = row.get(28)?;

    let result = (|| {
        let mut object = Map::new();
        object.insert("schema_version".to_string(), Value::String(schema_version));
        object.insert(
            "run_definition_schema".to_string(),
            Value::String(run_definition_schema),
        );
        object.insert("run_definition".to_string(), parse_value(&run_definition)?);
        object.insert("checkpoint_key".to_string(), Value::String(checkpoint_key));
        object.insert("task_id".to_string(), Value::String(task_id));
        object.insert("root_run_id".to_string(), Value::String(root_run_id));
        object.insert("trace_id".to_string(), Value::String(trace_id));
        object.insert(
            "run_definition_digest".to_string(),
            Value::String(run_definition_digest),
        );
        object.insert(
            "resume_attempt".to_string(),
            Value::from(to_u64(resume_attempt)?),
        );
        object.insert("cycle_index".to_string(), Value::from(to_u64(cycle_index)?));
        object.insert("status".to_string(), Value::String(status));
        object.insert(
            "active_host_interaction".to_string(),
            optional_value(active_host_interaction.as_deref())?,
        );
        object.insert(
            "suspended_origin".to_string(),
            optional_value(suspended_origin.as_deref())?,
        );
        object.insert("messages".to_string(), parse_value(&messages)?);
        object.insert("cycles".to_string(), parse_value(&cycles)?);
        object.insert("model_calls".to_string(), parse_value(&model_calls)?);
        object.insert("shared_state".to_string(), parse_value(&shared_state)?);
        object.insert(
            "budget_usage".to_string(),
            optional_value(budget_usage.as_deref())?,
        );
        object.insert(
            "event_cursor".to_string(),
            optional_value(event_cursor.as_deref())?,
        );
        object.insert("event_outbox".to_string(), parse_value(&event_outbox)?);
        object.insert(
            "extension_state".to_string(),
            parse_value(&extension_state)?,
        );
        object.insert(
            "model_call_journal".to_string(),
            parse_value(&model_call_journal)?,
        );
        object.insert("tool_journal".to_string(), parse_value(&tool_journal)?);
        object.insert("revision".to_string(), Value::from(to_u64(revision)?));
        object.insert(
            "claim_token".to_string(),
            claim_token.map_or(Value::Null, Value::String),
        );
        object.insert(
            "claimed_cycle".to_string(),
            claimed_cycle.map_or(Ok(Value::Null), |value| to_u64(value).map(Value::from))?,
        );
        object.insert(
            "lease_expires_at_ms".to_string(),
            lease_expires_at_ms.map_or(Ok(Value::Null), |value| to_u64(value).map(Value::from))?,
        );
        object.insert(
            "terminal_result".to_string(),
            optional_value(terminal_result.as_deref())?,
        );
        object.insert(
            "terminal_acknowledged".to_string(),
            Value::Bool(terminal_acknowledged != 0),
        );
        checkpoint_from_value(&Value::Object(object), MAX_EXTENSION_STATE_BYTES)
    })();
    Ok(result)
}

fn string_field(object: &Map<String, Value>, field: &str) -> CheckpointResult<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            CheckpointError::new("checkpoint_row_invalid", format!("{field} is not a string"))
        })
}

fn json_field(object: &Map<String, Value>, field: &str) -> CheckpointResult<String> {
    serde_json::to_string(object.get(field).ok_or_else(|| {
        CheckpointError::new("checkpoint_row_invalid", format!("{field} is missing"))
    })?)
    .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string()))
}

fn nullable_json_field(
    object: &Map<String, Value>,
    field: &str,
) -> CheckpointResult<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::to_string(value)
            .map(Some)
            .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string())),
    }
}

fn parse_value(raw: &str) -> CheckpointResult<Value> {
    serde_json::from_str(raw)
        .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string()))
}

fn optional_value(raw: Option<&str>) -> CheckpointResult<Value> {
    raw.map_or(Ok(Value::Null), parse_value)
}

fn to_i64(value: u64, field: &str) -> CheckpointResult<i64> {
    i64::try_from(value).map_err(|_| {
        CheckpointError::new(
            "checkpoint_integer_invalid",
            format!("{field} does not fit SQLite INTEGER"),
        )
    })
}

fn to_u64(value: i64) -> CheckpointResult<u64> {
    u64::try_from(value).map_err(|_| {
        CheckpointError::new(
            "checkpoint_row_invalid",
            "negative SQLite integer in checkpoint",
        )
    })
}

fn sqlite_error(error: rusqlite::Error) -> CheckpointError {
    CheckpointError::new("checkpoint_store_sqlite", error.to_string())
}
