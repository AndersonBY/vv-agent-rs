use rusqlite::Connection;
use tempfile::tempdir;
use vv_agent::SqliteCheckpointStore;

const CANONICAL_SQL_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_sqlite_canonical.sql");

#[derive(Debug, PartialEq, Eq)]
struct DatabaseState {
    objects: Vec<(String, String, String, Option<String>)>,
    schema_version: i64,
    user_version: i64,
    journal_mode: String,
    wal_sidecar_exists: bool,
    shm_sidecar_exists: bool,
}

fn seed_database(path: &std::path::Path, sql: &str) {
    let connection = Connection::open(path).expect("open seed database");
    connection.execute_batch(sql).expect("seed database");
}

fn database_state(path: &std::path::Path) -> DatabaseState {
    let (objects, schema_version, user_version, journal_mode) = {
        let connection = Connection::open(path).expect("open database for inspection");
        let mut statement = connection
            .prepare(
                "SELECT type, name, tbl_name, sql
                 FROM sqlite_master
                 WHERE name NOT LIKE 'sqlite_%'
                 ORDER BY type, name",
            )
            .expect("prepare schema inspection");
        let objects = statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .expect("query schema objects")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect schema objects");
        let schema_version = connection
            .query_row("PRAGMA schema_version", [], |row| row.get(0))
            .expect("schema version");
        let user_version = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user version");
        let journal_mode = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode");
        (objects, schema_version, user_version, journal_mode)
    };
    let (wal_sidecar_exists, shm_sidecar_exists) = sqlite_sidecar_state(path);
    DatabaseState {
        objects,
        schema_version,
        user_version,
        journal_mode,
        wal_sidecar_exists,
        shm_sidecar_exists,
    }
}

fn sqlite_sidecar_state(path: &std::path::Path) -> (bool, bool) {
    let mut wal_path = path.as_os_str().to_os_string();
    wal_path.push("-wal");
    let mut shm_path = path.as_os_str().to_os_string();
    shm_path.push("-shm");
    (
        std::path::PathBuf::from(wal_path).exists(),
        std::path::PathBuf::from(shm_path).exists(),
    )
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.replace("IF NOT EXISTS", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalized_schema_objects(
    state: &DatabaseState,
) -> Vec<(String, String, String, Option<String>)> {
    state
        .objects
        .iter()
        .map(|(object_type, name, table_name, sql)| {
            (
                object_type.clone(),
                name.clone(),
                table_name.clone(),
                sql.as_ref().map(|value| normalize_schema_sql(value)),
            )
        })
        .collect()
}

fn assert_schema_rejected_without_changes(path: &std::path::Path, context: &str) {
    let before = database_state(path);
    let error =
        SqliteCheckpointStore::new(path).expect_err("invalid related schema must be rejected");
    assert_eq!(
        error.code(),
        "checkpoint_store_schema_mismatch",
        "{context}"
    );
    assert_eq!(database_state(path), before, "{context}");
}

#[test]
fn sqlite_opens_the_canonical_checkpoint_schema_fixture() {
    let directory = tempdir().expect("temp directory");
    let path = directory.path().join("canonical-checkpoint.sqlite3");
    seed_database(&path, CANONICAL_SQL_FIXTURE);

    SqliteCheckpointStore::new(&path).expect("canonical checkpoint schema must open");
}

#[test]
fn sqlite_fresh_schema_matches_the_canonical_fixture() {
    let directory = tempdir().expect("temp directory");
    let canonical_path = directory.path().join("canonical-checkpoint.sqlite3");
    let generated_path = directory.path().join("generated-checkpoint.sqlite3");
    seed_database(&canonical_path, CANONICAL_SQL_FIXTURE);
    drop(SqliteCheckpointStore::new(&generated_path).expect("create fresh checkpoint schema"));

    let canonical = database_state(&canonical_path);
    let generated = database_state(&generated_path);
    assert_eq!(
        normalized_schema_objects(&generated),
        normalized_schema_objects(&canonical)
    );
    assert_eq!(generated.schema_version, canonical.schema_version);
    assert_eq!(generated.user_version, canonical.user_version);
    assert_eq!(generated.journal_mode, canonical.journal_mode);
}

#[test]
fn sqlite_rejects_each_missing_related_object_without_ddl_or_writes() {
    for (name, drop_statement) in [
        ("checkpoints", "DROP TABLE checkpoints;"),
        (
            "checkpoints_status_idx",
            "DROP INDEX checkpoints_status_idx;",
        ),
        (
            "deferred_resolution_receipts",
            "DROP TABLE deferred_resolution_receipts;",
        ),
        (
            "deferred_receipts_checkpoint_idx",
            "DROP INDEX deferred_receipts_checkpoint_idx;",
        ),
    ] {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join(format!("missing-{name}.sqlite3"));
        seed_database(&path, CANONICAL_SQL_FIXTURE);
        Connection::open(&path)
            .expect("open seeded database")
            .execute_batch(drop_statement)
            .expect("remove related object");
        let before = database_state(&path);

        let error =
            SqliteCheckpointStore::new(&path).expect_err("missing related object must be rejected");
        assert_eq!(error.code(), "checkpoint_store_schema_mismatch", "{name}");
        assert_eq!(database_state(&path), before, "{name}");
    }
}

#[test]
fn sqlite_rejects_malformed_related_objects_without_ddl_or_writes() {
    let directory = tempdir().expect("temp directory");
    let checkpoint_path = directory.path().join("malformed-checkpoints.sqlite3");
    seed_database(
        &checkpoint_path,
        "CREATE TABLE checkpoints (checkpoint_key TEXT PRIMARY KEY);",
    );
    assert_schema_rejected_without_changes(&checkpoint_path, "malformed checkpoints");

    let receipt_path = directory.path().join("malformed-deferred-receipts.sqlite3");
    seed_database(&receipt_path, CANONICAL_SQL_FIXTURE);
    Connection::open(&receipt_path)
        .expect("open canonical receipt database")
        .execute_batch(
            "DROP TABLE deferred_resolution_receipts;
             CREATE TABLE deferred_resolution_receipts (
                 handle_key TEXT PRIMARY KEY,
                 checkpoint_key TEXT NOT NULL
             );
             CREATE INDEX deferred_receipts_checkpoint_idx
                 ON deferred_resolution_receipts(checkpoint_key);",
        )
        .expect("replace receipt table with malformed schema");
    assert_schema_rejected_without_changes(&receipt_path, "malformed deferred receipts");
}

#[test]
fn sqlite_rejects_wrong_type_and_case_variant_related_objects_without_changes() {
    for (name, sql) in [
        (
            "case-variant-checkpoints",
            "CREATE TABLE CheckPoints (status TEXT);",
        ),
        (
            "wrong-type-checkpoints-view",
            "CREATE VIEW checkpoints AS SELECT 1;",
        ),
    ] {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join(format!("{name}.sqlite3"));
        seed_database(&path, sql);
        assert_schema_rejected_without_changes(&path, name);
    }

    let directory = tempdir().expect("temp directory");
    let wrong_type_index_path = directory.path().join("wrong-type-index.sqlite3");
    seed_database(&wrong_type_index_path, CANONICAL_SQL_FIXTURE);
    Connection::open(&wrong_type_index_path)
        .expect("open canonical index database")
        .execute_batch(
            "DROP INDEX checkpoints_status_idx;
             CREATE TABLE checkpoints_status_idx (id INTEGER PRIMARY KEY);",
        )
        .expect("replace index with wrong object type");
    assert_schema_rejected_without_changes(&wrong_type_index_path, "wrong type index");

    let case_variant_index_path = directory.path().join("case-variant-index.sqlite3");
    seed_database(&case_variant_index_path, CANONICAL_SQL_FIXTURE);
    Connection::open(&case_variant_index_path)
        .expect("open canonical case variant database")
        .execute_batch(
            "DROP INDEX checkpoints_status_idx;
             CREATE INDEX CheckPoints_Status_Idx ON checkpoints(status);",
        )
        .expect("replace index with case variant");
    assert_schema_rejected_without_changes(&case_variant_index_path, "case variant index");
}

#[test]
fn sqlite_rejects_canonical_table_with_case_variant_trigger_without_changes() {
    let directory = tempdir().expect("temp directory");
    let path = directory.path().join("same-name-trigger.sqlite3");
    seed_database(&path, CANONICAL_SQL_FIXTURE);
    Connection::open(&path)
        .expect("open canonical trigger database")
        .execute_batch(
            "CREATE TRIGGER CheckPoints AFTER INSERT ON checkpoints
             BEGIN
                 SELECT 1;
             END;",
        )
        .expect("create case-variant trigger");

    assert_schema_rejected_without_changes(&path, "canonical table and case-variant trigger");
}

#[test]
fn sqlite_rejects_a_canonical_auxiliary_only_database_without_writes() {
    let directory = tempdir().expect("temp directory");
    let path = directory.path().join("auxiliary-only.sqlite3");
    seed_database(
        &path,
        r#"
CREATE TABLE deferred_resolution_receipts (
    handle_key TEXT PRIMARY KEY,
    checkpoint_key TEXT NOT NULL,
    handle TEXT NOT NULL,
    result TEXT NOT NULL,
    result_digest TEXT NOT NULL,
    event_id TEXT NOT NULL,
    event_payload_digest TEXT NOT NULL,
    receipt_status TEXT NOT NULL CHECK (receipt_status IN ('succeeded', 'failed')),
    FOREIGN KEY (checkpoint_key) REFERENCES checkpoints(checkpoint_key) ON DELETE CASCADE
);
CREATE INDEX deferred_receipts_checkpoint_idx
    ON deferred_resolution_receipts(checkpoint_key);
"#,
    );
    assert_schema_rejected_without_changes(&path, "auxiliary-only schema");
    let state = database_state(&path);
    assert!(!state
        .objects
        .iter()
        .any(|(_, name, _, _)| name == "checkpoints"));
}
