mod common;

use std::sync::{Arc, Mutex};

use dipper::config::SourceConfig;
use dipper::source::{open, OpenOptions, Paging, SelectSpec, SessionStatement, SqlLogSink, Value};
use dipper::sql::guard::{GuardedOverlapSelect, GuardedSelect, SqlDialect};

static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

fn spec(table: &str, columns: &[&str], key: Option<&str>, limit: Option<u64>) -> SelectSpec {
    SelectSpec {
        table: table.to_string(),
        columns: columns.iter().map(|column| (*column).to_string()).collect(),
        filters: Vec::new(),
        limit,
        batch_rows: 1000,
        key: key.map(str::to_string),
        paging: Paging::Keyset,
    }
}

fn read_all(
    path: &std::path::Path,
    table: &str,
    columns: &[&str],
) -> (Vec<Vec<Value>>, Vec<usize>) {
    let cfg = SourceConfig::Sqlite {
        path: path.to_path_buf(),
        max_session_minutes: 480,
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let opened = open(
        "sample",
        &cfg,
        &OpenOptions {
            strict: false,
            log: SqlLogSink::Memory(Arc::clone(&log)),
            action: "check".to_string(),
        },
    )
    .unwrap();
    let mut rows = Vec::new();
    let mut sizes = Vec::new();
    let mut chunks = opened
        .source
        .select(&spec(table, columns, None, None))
        .unwrap();
    for chunk in chunks.by_ref() {
        let chunk = chunk.unwrap();
        assert!(chunk.rows.len() <= 1000, "{}", chunk.rows.len());
        sizes.push(chunk.rows.len());
        rows.extend(chunk.rows);
    }
    drop(chunks);
    drop(opened);
    let recorded = log.lock().unwrap().clone();
    assert_statements(&recorded);
    (rows, sizes)
}

fn assert_statements(recorded: &[String]) {
    let allowed: Vec<&str> = SessionStatement::allowed(SqlDialect::Sqlite)
        .iter()
        .map(|stmt| stmt.sql())
        .collect();
    for sql in recorded {
        let guarded = GuardedSelect::parse(sql, SqlDialect::Sqlite).is_ok();
        assert!(
            allowed.iter().any(|item| *item == sql) || guarded,
            "unexpected statement: {sql}"
        );
    }
    let begins = recorded
        .iter()
        .filter(|sql| sql.as_str() == "BEGIN")
        .count();
    let rollbacks = recorded
        .iter()
        .filter(|sql| sql.as_str() == "ROLLBACK")
        .count();
    assert_eq!(begins, rollbacks, "{recorded:?}");
}

fn direct(path: &std::path::Path, sql: &str, width: usize) -> Vec<Vec<Value>> {
    let conn = rusqlite::Connection::open(path).unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let mut values = Vec::with_capacity(width);
        for index in 0..width {
            values.push(match row.get_ref(index).unwrap() {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(value) => Value::Int(value),
                rusqlite::types::ValueRef::Real(value) => Value::Float(value),
                rusqlite::types::ValueRef::Text(value) => {
                    Value::Text(String::from_utf8_lossy(value).into_owned())
                }
                rusqlite::types::ValueRef::Blob(value) => Value::Text(format!("{value:02x?}")),
            });
        }
        out.push(values);
    }
    out
}

#[test]
fn chunked_reads_match_the_source_and_stay_read_only() {
    let _lock = lock();
    let dir = tempfile::tempdir().unwrap();
    let path = common::make_bulk_sqlite(dir.path(), "bulk.db");
    let before = common::sha256_file(&path);
    let cases = [
        (
            "ORDERS",
            vec!["ORDER_ID", "STATUS", "AMOUNT"],
            "SELECT ORDER_ID, STATUS, AMOUNT FROM ORDERS ORDER BY rowid",
        ),
        (
            "CUSTOMERS",
            vec!["CUSTOMER_ID", "REGION"],
            "SELECT CUSTOMER_ID, REGION FROM CUSTOMERS ORDER BY rowid",
        ),
        (
            "ITEMS",
            vec!["ITEM_ID", "ORDER_ID", "QTY"],
            "SELECT ITEM_ID, ORDER_ID, QTY FROM ITEMS ORDER BY rowid",
        ),
    ];
    for (table, columns, sql) in cases {
        let expected = direct(&path, sql, columns.len());
        let (rows, sizes) = read_all(&path, table, &columns);
        assert_eq!(rows.len(), expected.len(), "{table}");
        assert_eq!(rows, expected, "{table}");
        assert!(sizes.iter().all(|size| *size <= 1000), "{table} {sizes:?}");
    }
    assert_eq!(common::sha256_file(&path), before);
    for name in common::list_dir(dir.path()) {
        assert!(!name.contains("-journal"), "{name}");
        assert!(!name.ends_with("-wal"), "{name}");
        assert!(!name.ends_with("-shm"), "{name}");
    }
}

#[test]
fn filter_value_is_bound_and_limit_is_honored() {
    let _lock = lock();
    let dir = tempfile::tempdir().unwrap();
    let path = common::make_bulk_sqlite(dir.path(), "bulk.db");
    let cfg = SourceConfig::Sqlite {
        path: path.clone(),
        max_session_minutes: 480,
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let opened = open(
        "sample",
        &cfg,
        &OpenOptions {
            strict: false,
            log: SqlLogSink::Memory(Arc::clone(&log)),
            action: "check".to_string(),
        },
    )
    .unwrap();
    let mut filtered = spec("ORDERS", &["ORDER_ID", "STATUS", "AMOUNT"], None, None);
    filtered.filters.push(dipper::config::Filter {
        column: "STATUS".to_string(),
        op: dipper::config::FilterOp::Eq,
        value: Some(toml::Value::String("' OR 1=1 --".to_string())),
    });
    let rows: Vec<_> = opened
        .source
        .select(&filtered)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .flat_map(|chunk| chunk.rows)
        .collect();
    assert!(rows.is_empty());
    let recorded = log.lock().unwrap().clone();
    assert!(recorded.iter().all(|sql| !sql.contains("' OR 1=1 --")));

    log.lock().unwrap().clear();
    let limited = spec("ORDERS", &["ORDER_ID"], None, Some(2500));
    let chunks: Vec<_> = opened
        .source
        .select(&limited)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let lens: Vec<_> = chunks.iter().map(|chunk| chunk.rows.len()).collect();
    assert_eq!(lens, vec![1000, 1000, 500]);

    log.lock().unwrap().clear();
    let mut counted = spec("ORDERS", &["ORDER_ID"], None, None);
    counted.filters.push(dipper::config::Filter {
        column: "STATUS".to_string(),
        op: dipper::config::FilterOp::Eq,
        value: Some(toml::Value::String("PAID".to_string())),
    });
    let count = opened.source.count(&counted).unwrap();
    let paid = direct(
        &path,
        "SELECT COUNT(*) FROM ORDERS WHERE STATUS = 'PAID'",
        1,
    );
    assert_eq!(paid, vec![vec![Value::Int(count as i64)]]);
    let recorded = log.lock().unwrap().clone();
    let index = recorded
        .iter()
        .position(|sql| sql.contains("COUNT(*)"))
        .unwrap();
    assert_eq!(recorded[index - 1], "BEGIN");
    assert_eq!(recorded[index + 1], "ROLLBACK");
}

#[test]
fn explicit_key_matches_and_warns() {
    let _lock = lock();
    let dir = tempfile::tempdir().unwrap();
    let path = common::make_bulk_sqlite(dir.path(), "bulk.db");
    let cfg = SourceConfig::Sqlite {
        path: path.clone(),
        max_session_minutes: 480,
    };
    let opened = open(
        "sample",
        &cfg,
        &OpenOptions {
            strict: false,
            log: SqlLogSink::Off,
            action: "check".to_string(),
        },
    )
    .unwrap();
    let columns = ["ORDER_ID", "STATUS", "AMOUNT"];
    let expected = direct(
        &path,
        "SELECT ORDER_ID, STATUS, AMOUNT FROM ORDERS ORDER BY ORDER_ID",
        3,
    );
    let rows: Vec<_> = opened
        .source
        .select(&spec("ORDERS", &columns, Some("ORDER_ID"), None))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .flat_map(|chunk| chunk.rows)
        .collect();
    assert_eq!(rows, expected);
    let report = opened
        .source
        .preflight("orders", &spec("ORDERS", &columns, Some("ORDER_ID"), None))
        .unwrap();
    assert!(report
        .findings
        .iter()
        .any(|finding| finding.code == "key-unverified"));
}

#[test]
fn a_second_source_is_busy_until_the_first_closes() {
    let _lock = lock();
    let dir = tempfile::tempdir().unwrap();
    let path = common::make_sqlite(dir.path(), "sample.db", &["ORDERS"]);
    let cfg = SourceConfig::Sqlite {
        path,
        max_session_minutes: 480,
    };
    let opts = OpenOptions {
        strict: false,
        log: SqlLogSink::Off,
        action: "check".to_string(),
    };
    let first = open("sample", &cfg, &opts).unwrap();
    let err = match open("other", &cfg, &opts) {
        Ok(_) => panic!("second source opened"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("another source session is open"));
    drop(first);
    let second = open("other", &cfg, &opts).unwrap();
    drop(second);
}

#[test]
fn check_relation_counts_run_inside_rollback_and_through_the_guard() {
    let _lock = lock();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE ORDERS (CROSS_ID TEXT);
         CREATE TABLE INTERSECTIONS (CROSS_ID TEXT);
         INSERT INTO ORDERS (CROSS_ID) VALUES ('A');
         INSERT INTO INTERSECTIONS (CROSS_ID) VALUES ('A');",
    )
    .unwrap();
    drop(conn);
    let cfg = SourceConfig::Sqlite {
        path,
        max_session_minutes: 480,
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let opened = open(
        "sample",
        &cfg,
        &OpenOptions {
            strict: false,
            log: SqlLogSink::Memory(Arc::clone(&log)),
            action: "check".to_string(),
        },
    )
    .unwrap();
    let counts = opened
        .source
        .check_relation(
            "main",
            "ORDERS",
            &["CROSS_ID".to_string()],
            "main",
            "INTERSECTIONS",
            &["CROSS_ID".to_string()],
        )
        .unwrap();
    assert_eq!(counts.source_non_null_rows, 1);
    assert_eq!(counts.target_non_null_rows, 1);
    assert_eq!(counts.matched_rows, 1);
    drop(opened);
    let recorded = log.lock().unwrap().clone();
    let counts_sql: Vec<&String> = recorded
        .iter()
        .filter(|sql| sql.starts_with("SELECT COUNT(*)"))
        .collect();
    assert_eq!(counts_sql.len(), 3, "{recorded:?}");
    let mut overlap = 0;
    for sql in &counts_sql {
        let select_ok = GuardedSelect::parse(sql, SqlDialect::Sqlite).is_ok();
        let overlap_ok = GuardedOverlapSelect::parse(sql, SqlDialect::Sqlite).is_ok();
        assert!(
            select_ok || overlap_ok,
            "count SQL did not pass a guard: {sql}"
        );
        if sql.contains("EXISTS") {
            assert!(!select_ok, "EXISTS reached GuardedSelect: {sql}");
            assert!(overlap_ok, "{sql}");
            overlap += 1;
        }
    }
    assert_eq!(overlap, 1, "{recorded:?}");
    let mut wrapped = 0;
    let mut index = 0;
    while index < recorded.len() {
        if recorded[index] == "BEGIN"
            && index + 2 < recorded.len()
            && recorded[index + 1].starts_with("SELECT COUNT(*)")
            && recorded[index + 2] == "ROLLBACK"
        {
            wrapped += 1;
            index += 3;
            continue;
        }
        index += 1;
    }
    assert_eq!(wrapped, 3, "{recorded:?}");
}
