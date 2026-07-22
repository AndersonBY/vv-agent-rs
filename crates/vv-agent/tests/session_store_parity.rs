use std::path::Path;

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use vv_agent::{MemorySession, Session, SessionItem, SqliteSessionStore};

const CODEC_FIXTURE: &str = include_str!("fixtures/parity/session_codec.json");
const CANONICAL_SQL_FIXTURE: &str = include_str!("fixtures/parity/session_sqlite_canonical.sql");

fn seed_database(path: &Path, sql: &str) {
    let connection = Connection::open(path).expect("open seed database");
    connection.execute_batch(sql).expect("apply SQL seed");
}

fn schema_state(path: &Path) -> (i64, Vec<String>, Vec<(i64, String, String)>) {
    let connection = Connection::open(path).expect("open schema database");
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .expect("schema version");
    let columns = {
        let mut statement = connection
            .prepare("PRAGMA table_info(session_items)")
            .expect("prepare table info");
        statement
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query table info")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect table info")
    };
    let query = "SELECT item_index, session_id, payload FROM session_items ORDER BY item_index";
    let rows = {
        let mut statement = connection.prepare(query).expect("prepare session rows");
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .expect("query session rows")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect session rows")
    };
    (version, columns, rows)
}

#[test]
fn session_codec_matches_canonical_contract() {
    let fixture = serde_json::from_str::<Value>(CODEC_FIXTURE).expect("codec fixture JSON");
    for case in fixture["canonical_cases"]
        .as_array()
        .expect("codec case array")
    {
        let item = serde_json::from_value::<SessionItem>(case["input"].clone())
            .unwrap_or_else(|error| panic!("{}: {error}", case["name"]));
        let actual = serde_json::to_value(&item).expect("canonical session item");
        assert_eq!(actual, case["canonical"], "{}", case["name"]);

        let reparsed = serde_json::from_value::<SessionItem>(actual.clone())
            .expect("reparse canonical session item");
        assert_eq!(
            serde_json::to_value(reparsed).expect("reserialize canonical session item"),
            actual
        );
    }

    for case in fixture["invalid_cases"]
        .as_array()
        .expect("invalid codec case array")
    {
        assert!(
            serde_json::from_value::<SessionItem>(case["input"].clone()).is_err(),
            "{}",
            case["name"]
        );
    }
}

#[tokio::test]
async fn memory_session_returns_isolated_snapshots() {
    let session = MemorySession::new("memory-parity");
    session
        .add_items(vec![SessionItem::User {
            content: "snapshot".to_string(),
        }])
        .await
        .expect("add memory item");

    let mut snapshot = session.get_items(None).await.expect("memory snapshot");
    let SessionItem::User { content } = &mut snapshot[0] else {
        panic!("expected user item");
    };
    *content = "mutated".to_string();

    assert_eq!(
        session.get_items(None).await.expect("stored items"),
        vec![SessionItem::User {
            content: "snapshot".to_string()
        }]
    );
}

#[tokio::test]
async fn sqlite_opens_canonical_schema_written_by_either_runtime() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("canonical.sqlite3");
    seed_database(&path, CANONICAL_SQL_FIXTURE);

    let store = SqliteSessionStore::open(&path).expect("open canonical session store");
    let shared = store.session("shared");
    let items = shared.get_items(None).await.expect("canonical items");
    assert_eq!(items[0].to_message().content, "canonical user");
    assert_eq!(
        items[1].to_message().tool_calls[0].arguments,
        [
            ("a".to_string(), Value::from(1)),
            ("z".to_string(), Value::from(2))
        ]
        .into_iter()
        .collect()
    );
    shared
        .add_items(vec![SessionItem::Tool {
            content: "canonical result".to_string(),
            tool_call_id: "call_canonical".to_string(),
        }])
        .await
        .expect("append canonical result");
    drop(shared);
    drop(store);

    let (version, columns, rows) = schema_state(&path);
    assert_eq!(version, 1);
    assert_eq!(columns, ["session_id", "item_index", "payload"]);
    assert_eq!(rows.last().expect("last canonical row").0, 10);
    assert_eq!(
        serde_json::from_str::<Value>(&rows.last().expect("last canonical row").2)
            .expect("last canonical payload"),
        json!({
            "role": "tool",
            "content": "canonical result",
            "tool_call_id": "call_canonical"
        })
    );
}

#[test]
fn sqlite_rejects_existing_schema_without_current_version() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("missing-version.sqlite3");
    seed_database(&path, CANONICAL_SQL_FIXTURE);
    let connection = Connection::open(&path).expect("open current database");
    connection
        .execute_batch("PRAGMA user_version = 0;")
        .expect("remove schema version");
    drop(connection);

    let error = match SqliteSessionStore::open(&path) {
        Ok(_) => panic!("missing schema version unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(error.contains("does not match required version"), "{error}");

    let connection = Connection::open(&path).expect("reopen rejected database");
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .expect("schema version");
    assert_eq!(version, 0);
}

#[tokio::test]
async fn sqlite_corrupt_pop_does_not_delete_the_row() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("corrupt-pop.sqlite3");
    seed_database(&path, CANONICAL_SQL_FIXTURE);
    let connection = Connection::open(&path).expect("open corrupt database");
    connection
        .execute(
            "INSERT INTO session_items (session_id, item_index, payload) VALUES (?1, ?2, ?3)",
            params![
                "shared",
                20_i64,
                r#"{"role":"developer","content":"corrupt"}"#
            ],
        )
        .expect("insert corrupt row");
    drop(connection);

    let store = SqliteSessionStore::open(&path).expect("open session store");
    let error = store
        .session("shared")
        .pop_item()
        .await
        .expect_err("corrupt pop must fail");
    assert!(error.contains("unknown message role"), "{error}");
    drop(store);

    let (_, _, rows) = schema_state(&path);
    assert!(rows.iter().any(|(index, _, _)| *index == 20));
}

#[test]
fn sqlite_rejects_newer_schema_without_mutating_it() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("newer.sqlite3");
    seed_database(&path, CANONICAL_SQL_FIXTURE);
    let connection = Connection::open(&path).expect("open newer database");
    connection
        .execute_batch("PRAGMA user_version = 2;")
        .expect("set newer version");
    drop(connection);

    let error = match SqliteSessionStore::open(&path) {
        Ok(_) => panic!("newer schema unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(error.contains("does not match required version"), "{error}");

    let (version, columns, rows) = schema_state(&path);
    assert_eq!(version, 2);
    assert_eq!(columns, ["session_id", "item_index", "payload"]);
    assert_eq!(rows.len(), 3);
}
