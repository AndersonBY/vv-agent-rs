use std::path::Path;

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use vv_agent::{MemorySession, Session, SessionItem, SqliteSessionStore};

const CODEC_FIXTURE: &str = include_str!("fixtures/parity/session_codec_v1.json");
const LEGACY_SQL_FIXTURE: &str = include_str!("fixtures/parity/session_sqlite_rust_legacy_v0.sql");
const CANONICAL_SQL_FIXTURE: &str = include_str!("fixtures/parity/session_sqlite_canonical_v1.sql");
const PYTHON_UNVERSIONED_SQL_FIXTURE: &str =
    include_str!("fixtures/parity/session_sqlite_python_unversioned_v0.sql");
const INVALID_LEGACY_SQL_FIXTURE: &str =
    include_str!("fixtures/parity/session_sqlite_invalid_legacy_v0.sql");

const FIXTURE_HASHES: [(&str, &str, &str); 5] = [
    (
        "session_codec_v1.json",
        CODEC_FIXTURE,
        "ddb771fd89827145557297d8bfc6d734684fadf9ce019ed87a9b38b884782eb8",
    ),
    (
        "session_sqlite_rust_legacy_v0.sql",
        LEGACY_SQL_FIXTURE,
        "1cfa0fa6550cb7ddf6b6029cfb63fdb007287cb4289bd481e86d28942fcbbed5",
    ),
    (
        "session_sqlite_canonical_v1.sql",
        CANONICAL_SQL_FIXTURE,
        "03e1dbf36e2299cf8f7d0b9d4e85ec685a7e10c46bcdc2cef4ee8b87d0d5d18d",
    ),
    (
        "session_sqlite_python_unversioned_v0.sql",
        PYTHON_UNVERSIONED_SQL_FIXTURE,
        "215ae69f2fb44b18a0a7e2473d11ab0d86cb93552791b00350de0a056d7dc956",
    ),
    (
        "session_sqlite_invalid_legacy_v0.sql",
        INVALID_LEGACY_SQL_FIXTURE,
        "37e8b0662fc346d3c424a83d213357ee54545c8b66a9d2df65ca7ec100845aa6",
    ),
];

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
    let query = if columns == ["session_id", "item_index", "payload"] {
        "SELECT item_index, session_id, payload FROM session_items ORDER BY item_index"
    } else {
        "SELECT id, session_id, item_json FROM session_items ORDER BY id"
    };
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
fn shared_session_parity_fixtures_have_stable_hashes() {
    for (name, fixture, expected) in FIXTURE_HASHES {
        let actual = format!("{:x}", Sha256::digest(fixture.as_bytes()));
        assert_eq!(actual, expected, "fixture hash changed for {name}");
    }
}

#[test]
fn session_codec_matches_canonical_and_legacy_contract() {
    let fixture = serde_json::from_str::<Value>(CODEC_FIXTURE).expect("codec fixture JSON");
    for section in ["canonical_cases", "legacy_cases"] {
        for case in fixture[section].as_array().expect("codec case array") {
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
async fn sqlite_migrates_rust_legacy_schema_and_payloads_transactionally() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("legacy.sqlite3");
    seed_database(&path, LEGACY_SQL_FIXTURE);

    let store = SqliteSessionStore::open(&path).expect("migrate legacy session store");
    let shared = store.session("shared");
    let actual = shared
        .get_items(None)
        .await
        .expect("shared items")
        .iter()
        .map(|item| serde_json::to_value(item).expect("serialize shared item"))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            json!({"role": "user", "content": "legacy user"}),
            json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_legacy",
                    "type": "function",
                    "function": {
                        "name": "lookup",
                        "arguments": "{\"a\":{\"x\":1,\"y\":2},\"z\":1}"
                    }
                }]
            }),
        ]
    );
    assert_eq!(
        store
            .session("other")
            .get_items(None)
            .await
            .expect("other items"),
        vec![SessionItem::System {
            content: "other session".to_string()
        }]
    );
    shared
        .add_items(vec![SessionItem::Tool {
            content: "done".to_string(),
            tool_call_id: "call_legacy".to_string(),
        }])
        .await
        .expect("append canonical item");
    drop(shared);
    drop(store);

    let (version, columns, rows) = schema_state(&path);
    assert_eq!(version, 1);
    assert_eq!(columns, ["session_id", "item_index", "payload"]);
    assert_eq!(
        rows.iter()
            .map(|(index, session_id, _)| (*index, session_id.as_str()))
            .collect::<Vec<_>>(),
        vec![(2, "shared"), (5, "other"), (8, "shared"), (9, "shared")]
    );
    for (_, _, payload) in rows {
        let object = serde_json::from_str::<Value>(&payload)
            .expect("canonical payload")
            .as_object()
            .expect("canonical payload object")
            .clone();
        assert!(object.contains_key("role"));
        assert!(!object.contains_key("type"));
        assert!(!object.contains_key("message"));
    }
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

#[tokio::test]
async fn sqlite_upgrades_unversioned_python_schema_in_place() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("python-unversioned.sqlite3");
    seed_database(&path, PYTHON_UNVERSIONED_SQL_FIXTURE);

    let store = SqliteSessionStore::open(&path).expect("upgrade Python session store");
    assert_eq!(
        store
            .session("shared")
            .get_items(None)
            .await
            .expect("unversioned Python items"),
        vec![SessionItem::User {
            content: "python unversioned".to_string()
        }]
    );
    drop(store);

    let (version, columns, rows) = schema_state(&path);
    assert_eq!(version, 1);
    assert_eq!(columns, ["session_id", "item_index", "payload"]);
    assert_eq!(
        rows,
        vec![(
            4,
            "shared".to_string(),
            r#"{"role":"user","content":"python unversioned"}"#.to_string()
        )]
    );

    let connection = Connection::open(&path).expect("open upgraded Python database");
    let indexes = {
        let mut statement = connection
            .prepare("PRAGMA index_list(session_items)")
            .expect("prepare session indexes");
        statement
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query session indexes")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect session indexes")
    };
    assert!(indexes
        .iter()
        .any(|name| name == "idx_session_items_session_id_item_index"));
}

#[test]
fn sqlite_failed_legacy_migration_rolls_back_schema_and_rows() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("invalid-legacy.sqlite3");
    seed_database(&path, INVALID_LEGACY_SQL_FIXTURE);

    let error = match SqliteSessionStore::open(&path) {
        Ok(_) => panic!("invalid legacy migration unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(error.contains("unknown variant"), "{error}");

    let (version, columns, rows) = schema_state(&path);
    assert_eq!(version, 0);
    assert_eq!(columns, ["id", "session_id", "item_json"]);
    assert_eq!(
        rows.iter().map(|(index, _, _)| *index).collect::<Vec<_>>(),
        vec![1, 2]
    );
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
    assert!(error.contains("newer than supported"), "{error}");

    let (version, columns, rows) = schema_state(&path);
    assert_eq!(version, 2);
    assert_eq!(columns, ["session_id", "item_index", "payload"]);
    assert_eq!(rows.len(), 3);
}
