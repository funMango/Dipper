mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dipper::config::SourceConfig;
use dipper::error::DipperError;
use dipper::pipeline::{self, progress::LogProgress, RunOptions, SourceOpener};
use dipper::source::{
    open, Chunk, ChunkIter, OpenOptions, Opened, SelectSpec, SessionStatement, Source, SqlLogSink,
};
use dipper::sql::guard::{GuardedSelect, SqlDialect};
use dipper::store::Store;

static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

fn prepare(dir: &Path, performance: &str, dips: &str) -> (PathBuf, PathBuf) {
    let db = common::make_bulk_sqlite(dir, "source.db");
    let config = common::write_config(
        dir,
        &format!(
            "store = \".dipper/store.duckdb\"\n\n[performance]\n{performance}\n\n[sources.sample]\nkind = \"sqlite\"\npath = \"source.db\"\n\n{dips}\n"
        ),
    );
    (db, config)
}

fn store_path(dir: &Path) -> PathBuf {
    dir.join(".dipper").join("store.duckdb")
}

fn texts(output: &std::process::Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

const THREE_DIPS: &str = r#"
[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID", "STATUS", "AMOUNT"]
filters = [{ column = "STATUS", op = "in", value = ["PAID"] }]

[[dips]]
name = "customers"
source = "sample"
table = "CUSTOMERS"
columns = ["CUSTOMER_ID", "REGION"]

[[dips]]
name = "items"
source = "sample"
table = "ITEMS"
columns = ["ITEM_ID", "ORDER_ID", "QTY"]
"#;

#[derive(Debug, PartialEq)]
enum Cell {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
}

fn duck_rows(path: &Path, table: &str, columns: &[&str]) -> Vec<Vec<Cell>> {
    let conn = duckdb::Connection::open(path).unwrap();
    let listed = columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {listed} FROM \"{table}\" ORDER BY \"{}\"",
        columns[0]
    );
    let mut stmt = conn.prepare(&sql).unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let mut cells = Vec::new();
        for index in 0..columns.len() {
            cells.push(from_duck(row.get(index).unwrap()));
        }
        out.push(cells);
    }
    out
}

fn sqlite_rows(path: &Path, sql: &str) -> Vec<Vec<Cell>> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    let width = stmt.column_count();
    let mut rows = stmt.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let mut cells = Vec::new();
        for index in 0..width {
            cells.push(match row.get_ref(index).unwrap() {
                rusqlite::types::ValueRef::Null => Cell::Null,
                rusqlite::types::ValueRef::Integer(value) => Cell::Int(value),
                rusqlite::types::ValueRef::Real(value) => Cell::Float(value),
                rusqlite::types::ValueRef::Text(value) => {
                    Cell::Text(String::from_utf8_lossy(value).into_owned())
                }
                rusqlite::types::ValueRef::Blob(_) => panic!("unexpected blob"),
            });
        }
        out.push(cells);
    }
    out
}

fn from_duck(value: duckdb::types::Value) -> Cell {
    match value {
        duckdb::types::Value::Null => Cell::Null,
        duckdb::types::Value::TinyInt(value) => Cell::Int(i64::from(value)),
        duckdb::types::Value::SmallInt(value) => Cell::Int(i64::from(value)),
        duckdb::types::Value::Int(value) => Cell::Int(i64::from(value)),
        duckdb::types::Value::BigInt(value) => Cell::Int(value),
        duckdb::types::Value::Float(value) => Cell::Float(f64::from(value)),
        duckdb::types::Value::Double(value) => Cell::Float(value),
        duckdb::types::Value::Text(value) => Cell::Text(value),
        other => panic!("unexpected duckdb value {other:?}"),
    }
}

fn as_i64(value: duckdb::types::Value) -> i64 {
    match value {
        duckdb::types::Value::BigInt(value) => value,
        duckdb::types::Value::Int(value) => i64::from(value),
        duckdb::types::Value::HugeInt(value) => i64::try_from(value).unwrap(),
        duckdb::types::Value::UBigInt(value) => i64::try_from(value).unwrap(),
        duckdb::types::Value::Double(value) => value as i64,
        other => panic!("unexpected number {other:?}"),
    }
}

fn table_names(path: &Path) -> Vec<String> {
    let conn = duckdb::Connection::open(path).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT table_name FROM duckdb_tables() WHERE schema_name = 'main' ORDER BY table_name",
        )
        .unwrap();
    let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
    rows.map(|row| row.unwrap()).collect()
}

fn count_and_sum(path: &Path, table: &str, column: &str) -> (i64, i64) {
    let conn = duckdb::Connection::open(path).unwrap();
    let sql = format!("SELECT count(*), coalesce(sum(\"{column}\"), 0) FROM \"{table}\"");
    let (count, sum): (i64, duckdb::types::Value) = conn
        .query_row(&sql, [], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap();
    (count, as_i64(sum))
}

#[test]
fn three_tables_match_the_filtered_source() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = prepare(dir.path(), "batch_rows = 1000", THREE_DIPS);
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, _) = texts(&output);
    assert_eq!(stdout.lines().last(), Some("3 of 3 dips OK"));
    let store = store_path(dir.path());
    assert_eq!(
        duck_rows(&store, "orders", &["ORDER_ID", "STATUS", "AMOUNT"]),
        sqlite_rows(
            &db,
            "SELECT ORDER_ID, STATUS, AMOUNT FROM ORDERS WHERE STATUS = 'PAID' ORDER BY ORDER_ID"
        )
    );
    assert_eq!(
        duck_rows(&store, "customers", &["CUSTOMER_ID", "REGION"]),
        sqlite_rows(
            &db,
            "SELECT CUSTOMER_ID, REGION FROM CUSTOMERS ORDER BY CUSTOMER_ID"
        )
    );
    assert_eq!(
        duck_rows(&store, "items", &["ITEM_ID", "ORDER_ID", "QTY"]),
        sqlite_rows(
            &db,
            "SELECT ITEM_ID, ORDER_ID, QTY FROM ITEMS ORDER BY ITEM_ID"
        )
    );
    assert_eq!(count_and_sum(&store, "orders", "ORDER_ID").0, 5_000);
    assert_eq!(count_and_sum(&store, "customers", "CUSTOMER_ID").0, 20_000);
    assert_eq!(count_and_sum(&store, "items", "ITEM_ID").0, 30_000);
}

#[test]
fn named_dip_is_case_insensitive_and_unknown_dip_does_not_create_a_store() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = prepare(dir.path(), "batch_rows = 1000", THREE_DIPS);
    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(["dip", "customers"])
        .assert()
        .code(0);
    let names = table_names(&store_path(dir.path()));
    assert!(names.iter().any(|name| name == "customers"));
    assert!(!names.iter().any(|name| name == "orders" || name == "items"));
    let conn = duckdb::Connection::open(store_path(dir.path())).unwrap();
    let runs: i64 = conn
        .query_row("SELECT count(*) FROM \"_dipper_runs\"", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(runs, 1);

    let upper = tempfile::tempdir().unwrap();
    let (_db, config) = prepare(upper.path(), "batch_rows = 1000", THREE_DIPS);
    let output = common::dipper()
        .current_dir(upper.path())
        .args(["--config"])
        .arg(&config)
        .args(["dip", "CUSTOMERS"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, _) = texts(&output);
    assert!(stdout.contains("customers"), "{stdout}");
    assert!(table_names(&store_path(upper.path()))
        .iter()
        .any(|name| name == "customers"));

    let missing = tempfile::tempdir().unwrap();
    let (_db, config) = prepare(missing.path(), "batch_rows = 1000", THREE_DIPS);
    let output = common::dipper()
        .current_dir(missing.path())
        .args(["--config"])
        .arg(&config)
        .args(["dip", "nope"])
        .assert()
        .code(2)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("unknown dip: nope"), "{stderr}");
    assert!(!missing.path().join(".dipper").exists());
}

#[test]
fn log_sql_is_allowed_and_the_source_file_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = prepare(dir.path(), "batch_rows = 1000", THREE_DIPS);
    let before = common::sha256_file(&db);
    let names_before = common::list_dir(dir.path());
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--log-sql", "--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    let allowed: Vec<&str> = SessionStatement::allowed(SqlDialect::Sqlite)
        .iter()
        .map(|statement| statement.sql())
        .collect();
    let sql_lines: Vec<&str> = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("sql: "))
        .collect();
    assert!(!sql_lines.is_empty(), "{stderr}");
    for sql in sql_lines {
        let allowed_statement = allowed.contains(&sql);
        let guarded = GuardedSelect::parse(sql, SqlDialect::Sqlite).is_ok();
        assert!(allowed_statement || guarded, "unexpected statement: {sql}");
    }
    assert_eq!(common::sha256_file(&db), before);
    for name in common::list_dir(dir.path()) {
        assert!(
            !name.ends_with("-journal") && !name.ends_with("-wal") && !name.ends_with("-shm"),
            "{name} appeared; before {names_before:?}"
        );
    }
}

#[test]
fn a_second_run_replaces_rows_and_clears_staging() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = prepare(dir.path(), "batch_rows = 1000", THREE_DIPS);
    for _ in 0..2 {
        common::dipper()
            .current_dir(dir.path())
            .args(["--config"])
            .arg(&config)
            .arg("dip")
            .assert()
            .code(0);
    }
    let store = store_path(dir.path());
    assert_eq!(count_and_sum(&store, "orders", "ORDER_ID").0, 5_000);
    assert_eq!(count_and_sum(&store, "customers", "CUSTOMER_ID").0, 20_000);
    assert_eq!(count_and_sum(&store, "items", "ITEM_ID").0, 30_000);
    let conn = duckdb::Connection::open(&store).unwrap();
    for dip in ["orders", "customers", "items"] {
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM \"_dipper_runs\" WHERE dip = ?",
                [dip],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "{dip}");
    }
    assert!(!table_names(&store)
        .iter()
        .any(|name| name.ends_with("__staging")));
}

#[test]
fn an_injected_chunk_failure_keeps_the_loaded_table_for_both_paging_modes() {
    injected_chunk_failure("snapshot");
    injected_chunk_failure("keyset");
}

fn injected_chunk_failure(paging: &str) {
    let dir = tempfile::tempdir().unwrap();
    let dips = format!(
        r#"
[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID", "STATUS", "AMOUNT"]
paging = "{paging}"

[[dips]]
name = "customers"
source = "sample"
table = "CUSTOMERS"
columns = ["CUSTOMER_ID", "REGION"]
"#
    );
    let (_db, config) = prepare(dir.path(), "batch_rows = 1000", &dips);
    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(["dip", "orders"])
        .assert()
        .code(0);
    let store = store_path(dir.path());
    let before = count_and_sum(&store, "orders", "ORDER_ID");
    let loaded = dipper::config::load(&config).unwrap();
    let selected = pipeline::select_dips(&loaded, &["orders".to_string()]).unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let opener = FailingOpener {
        log: Arc::clone(&log),
    };
    let report = {
        let _guard = lock();
        let store = Store::open(&loaded.store, &loaded.performance).unwrap();
        let opts = RunOptions {
            fail_fast: false,
            strict: false,
            log: SqlLogSink::Off,
        };
        let mut progress = LogProgress::new(std::io::sink(), 1_000_000);
        let mut events = Vec::new();
        pipeline::run(
            &loaded,
            &selected,
            &store,
            &opts,
            &opener,
            &mut progress,
            &mut events,
        )
    };
    let err = report.outcomes[0].result.as_ref().unwrap_err();
    assert!(err.contains("injected failure"), "{err}");
    assert_eq!(count_and_sum(&store, "orders", "ORDER_ID"), before);
    assert!(table_names(&store)
        .iter()
        .any(|name| name == "orders__staging"));
    let recorded = log.lock().unwrap().clone();
    assert_eq!(
        recorded.last().map(String::as_str),
        Some("ROLLBACK"),
        "{recorded:?}"
    );
    let begins = recorded
        .iter()
        .filter(|sql| sql.as_str() == "BEGIN")
        .count();
    let rollbacks = recorded
        .iter()
        .filter(|sql| sql.as_str() == "ROLLBACK")
        .count();
    assert_eq!(begins, rollbacks, "{recorded:?}");
    let conn = duckdb::Connection::open(&store).unwrap();
    let (ok, error): (bool, Option<String>) = conn
        .query_row(
            "SELECT ok, error FROM \"_dipper_runs\" ORDER BY rowid DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(!ok);
    assert!(error.unwrap_or_default().contains("injected failure"));
    drop(conn);

    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(["dip", "customers"])
        .assert()
        .code(0);
    assert!(!table_names(&store)
        .iter()
        .any(|name| name == "orders__staging"));
}

struct FailingOpener {
    log: Arc<Mutex<Vec<String>>>,
}

impl SourceOpener for FailingOpener {
    fn open(
        &self,
        name: &str,
        cfg: &SourceConfig,
        opts: &OpenOptions,
    ) -> Result<Opened, DipperError> {
        let opened = open(
            name,
            cfg,
            &OpenOptions {
                strict: opts.strict,
                log: SqlLogSink::Memory(Arc::clone(&self.log)),
                action: opts.action.clone(),
            },
        )?;
        Ok(Opened {
            source: Box::new(FailingSource {
                inner: opened.source,
            }),
            findings: opened.findings,
        })
    }
}

struct FailingSource {
    inner: Box<dyn Source>,
}

impl Source for FailingSource {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }
    fn test_connection(&self) -> Result<(), DipperError> {
        self.inner.test_connection()
    }
    fn preflight(
        &self,
        dip: &str,
        spec: &SelectSpec,
    ) -> Result<dipper::source::PreflightReport, DipperError> {
        self.inner.preflight(dip, spec)
    }
    fn count(&self, spec: &SelectSpec) -> Result<u64, DipperError> {
        self.inner.count(spec)
    }
    fn select<'a>(&'a self, spec: &SelectSpec) -> Result<ChunkIter<'a>, DipperError> {
        let inner = self.inner.select(spec)?;
        Ok(Box::new(FailAfter {
            inner: Some(inner),
            yielded: 0,
        }))
    }
}

struct FailAfter<'a> {
    inner: Option<ChunkIter<'a>>,
    yielded: usize,
}

impl Iterator for FailAfter<'_> {
    type Item = Result<Chunk, DipperError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.yielded >= 2 {
            self.inner.take();
            return Some(Err(DipperError::Source {
                source_name: "sample".to_string(),
                message: "injected failure".to_string(),
            }));
        }
        match self.inner.as_mut()?.next()? {
            Ok(chunk) => {
                self.yielded += 1;
                Some(Ok(chunk))
            }
            Err(err) => Some(Err(err)),
        }
    }
}

#[test]
fn one_failure_continues_unless_fail_fast() {
    let dir = tempfile::tempdir().unwrap();
    let dips = r#"
[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID", "STATUS", "AMOUNT"]

[[dips]]
name = "missing"
source = "sample"
table = "NOPE"
columns = ["ORDER_ID"]

[[dips]]
name = "items"
source = "sample"
table = "ITEMS"
columns = ["ITEM_ID", "ORDER_ID", "QTY"]
"#;
    let (_db, config) = prepare(dir.path(), "batch_rows = 1000", dips);
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&output);
    assert!(stdout.contains("missing"), "{stdout}");
    assert!(stdout.contains("FAIL"), "{stdout}");
    assert_eq!(stdout.lines().last(), Some("2 of 3 dips OK"));
    assert!(
        stderr.contains("dip failed: 1 of 3 dips failed"),
        "{stderr}"
    );
    let store = store_path(dir.path());
    let names = table_names(&store);
    assert!(names.iter().any(|name| name == "orders"));
    assert!(names.iter().any(|name| name == "items"));
    let conn = duckdb::Connection::open(&store).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT column_name FROM duckdb_columns() WHERE schema_name = 'main' AND table_name = '_dipper_runs' ORDER BY column_index",
        )
        .unwrap();
    let columns: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|column| column.unwrap())
        .collect();
    assert_eq!(
        columns,
        [
            "dip",
            "source",
            "started_at",
            "finished_at",
            "rows",
            "ok",
            "error",
            "paging",
            "warnings"
        ]
    );
    let mut runs = conn
        .prepare(
            "SELECT dip, rows, ok, error, paging, warnings, finished_at >= started_at FROM \"_dipper_runs\" ORDER BY finished_at",
        )
        .unwrap();
    struct RunRow {
        dip: String,
        rows: i64,
        ok: bool,
        error: Option<String>,
        paging: String,
        ordered: bool,
    }
    let rows: Vec<RunRow> = runs
        .query_map([], |row| {
            Ok(RunRow {
                dip: row.get(0)?,
                rows: row.get(1)?,
                ok: row.get(2)?,
                error: row.get(3)?,
                paging: row.get(4)?,
                ordered: row.get(6)?,
            })
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert!(row.ordered, "{}", row.dip);
        assert_eq!(row.paging, "keyset");
        if row.dip == "missing" {
            assert!(!row.ok);
            assert!(row.error.is_some());
        } else {
            assert!(row.ok);
            assert!(row.error.is_none());
            let expected = if row.dip == "orders" { 10_000 } else { 30_000 };
            assert_eq!(row.rows, expected, "{}", row.dip);
        }
    }

    let fast = tempfile::tempdir().unwrap();
    let (_db, config) = prepare(fast.path(), "batch_rows = 1000", dips);
    let output = common::dipper()
        .current_dir(fast.path())
        .args(["--config"])
        .arg(&config)
        .args(["dip", "--fail-fast"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, _) = texts(&output);
    assert!(
        stdout.contains("stopped at first failure (--fail-fast): 1 dip(s) not run: items"),
        "{stdout}"
    );
    let names = table_names(&store_path(fast.path()));
    assert!(names.iter().any(|name| name == "orders"));
    assert!(!names.iter().any(|name| name == "items"));
    let conn = duckdb::Connection::open(store_path(fast.path())).unwrap();
    let items: i64 = conn
        .query_row(
            "SELECT count(*) FROM \"_dipper_runs\" WHERE dip = 'items'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(items, 0);
}

#[test]
fn preflight_refusal_and_strict_warnings_do_not_select() {
    let dir = tempfile::tempdir().unwrap();
    let dips = r#"
[[dips]]
name = "missing"
source = "sample"
table = "NOPE"
columns = ["ORDER_ID"]
"#;
    let (_db, config) = prepare(dir.path(), "batch_rows = 1000", dips);
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--log-sql", "--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&output);
    assert!(stderr.contains("refused:"), "{stderr}");
    assert!(stdout.contains("FAIL"), "{stdout}");
    assert!(!stderr.contains("FROM \"NOPE\""), "{stderr}");

    let view = tempfile::tempdir().unwrap();
    let (_db, _config) = prepare(view.path(), "batch_rows = 1000", "");
    let conn = rusqlite::Connection::open(view.path().join("source.db")).unwrap();
    conn.execute_batch("CREATE VIEW V_ORDERS AS SELECT ORDER_ID, STATUS FROM ORDERS")
        .unwrap();
    drop(conn);
    let config = common::write_config(
        view.path(),
        r#"
store = ".dipper/store.duckdb"
[sources.sample]
kind = "sqlite"
path = "source.db"
[[dips]]
name = "vorders"
source = "sample"
table = "V_ORDERS"
columns = ["ORDER_ID", "STATUS"]
key = "ORDER_ID"
"#,
    );
    let output = common::dipper()
        .current_dir(view.path())
        .args(["--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&output);
    assert!(stderr.contains("warning:"), "{stderr}");
    assert!(stdout.contains("OK"), "{stdout}");
    let conn = duckdb::Connection::open(store_path(view.path())).unwrap();
    let warnings: i64 = conn
        .query_row(
            "SELECT warnings FROM \"_dipper_runs\" WHERE dip = 'vorders'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(warnings >= 1, "{warnings}");
    drop(conn);

    let strict = common::dipper()
        .current_dir(view.path())
        .args(["--strict", "--log-sql", "--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&strict);
    assert!(
        stdout.contains("strict:") && stdout.contains("warning(s)"),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(!stderr.contains("FROM \"V_ORDERS\""), "{stderr}");

    let safety = tempfile::tempdir().unwrap();
    common::make_bulk_sqlite(safety.path(), "source.db");
    let conn = rusqlite::Connection::open(safety.path().join("source.db")).unwrap();
    conn.execute_batch("CREATE VIEW V_ORDERS AS SELECT ORDER_ID, STATUS FROM ORDERS")
        .unwrap();
    drop(conn);
    let config = common::write_config(
        safety.path(),
        r#"
store = ".dipper/store.duckdb"
[safety]
strict_readonly = true
[sources.sample]
kind = "sqlite"
path = "source.db"
[[dips]]
name = "vorders"
source = "sample"
table = "V_ORDERS"
columns = ["ORDER_ID", "STATUS"]
key = "ORDER_ID"
"#,
    );
    let output = common::dipper()
        .current_dir(safety.path())
        .args(["--log-sql", "--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&output);
    assert!(
        stdout.contains("strict:") && stdout.contains("warning(s)"),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(!stderr.contains("FROM \"V_ORDERS\""), "{stderr}");
}

#[test]
fn progress_lines_follow_progress_every_rows() {
    let dir = tempfile::tempdir().unwrap();
    let dips = r#"
[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID", "STATUS", "AMOUNT"]
"#;
    let (_db, config) = prepare(
        dir.path(),
        "batch_rows = 1000\nprogress_every_rows = 1000",
        dips,
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    let lines: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("progress: orders: "))
        .collect();
    assert_eq!(lines.len(), 10, "{stderr}");

    let counted = tempfile::tempdir().unwrap();
    let dips = r#"
[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID", "STATUS", "AMOUNT"]
count_first = true
"#;
    let (_db, config) = prepare(
        counted.path(),
        "batch_rows = 1000\nprogress_every_rows = 1000",
        dips,
    );
    let output = common::dipper()
        .current_dir(counted.path())
        .args(["--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    let lines: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("progress: orders: "))
        .collect();
    assert_eq!(lines.len(), 10, "{stderr}");
    assert!(
        lines.iter().all(|line| line.contains("/10000 rows (")),
        "{stderr}"
    );

    let coarse = tempfile::tempdir().unwrap();
    let dips = r#"
[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID", "STATUS", "AMOUNT"]
"#;
    let (_db, config) = prepare(
        coarse.path(),
        "batch_rows = 1000\nprogress_every_rows = 3000",
        dips,
    );
    let output = common::dipper()
        .current_dir(coarse.path())
        .args(["--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("progress: orders: "))
        .count();
    assert_eq!(lines, 3, "{stderr}");
}

#[test]
fn batch_size_does_not_change_counts_or_sums() {
    fn sums(batch: &str) -> Vec<(i64, i64)> {
        let dir = tempfile::tempdir().unwrap();
        let (_db, config) = prepare(dir.path(), &format!("batch_rows = {batch}"), THREE_DIPS);
        common::dipper()
            .current_dir(dir.path())
            .args(["--config"])
            .arg(&config)
            .arg("dip")
            .assert()
            .code(0);
        let store = store_path(dir.path());
        vec![
            count_and_sum(&store, "orders", "ORDER_ID"),
            count_and_sum(&store, "customers", "CUSTOMER_ID"),
            count_and_sum(&store, "items", "ITEM_ID"),
            count_and_sum(&store, "items", "QTY"),
        ]
    }
    assert_eq!(sums("1000"), sums("10000"));
}

#[test]
fn oracle_password_never_appears_in_dip_output() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::write_config(
        dir.path(),
        r#"
store = ".dipper/store.duckdb"
[sources.company]
kind = "oracle"
connect = "127.0.0.1:1/NOPE"
user = "reader"
password = "${DIPPER_PR3B_PASSWORD}"
[[dips]]
name = "orders"
source = "company"
table = "SALES.ORDERS"
columns = ["ORDER_ID"]
"#,
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .env("DIPPER_PR3B_PASSWORD", "PR2-canary-7f1d9c")
        .args(["--verbose", "--log-sql", "--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&output);
    assert!(!stdout.contains("PR2-canary-7f1d9c"), "{stdout}");
    assert!(!stderr.contains("PR2-canary-7f1d9c"), "{stderr}");
    #[cfg(not(feature = "oracle"))]
    assert!(
        stdout.contains("oracle support is not built in")
            || stderr.contains("oracle support is not built in"),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    #[cfg(feature = "oracle")]
    assert!(
        stdout.contains("DPI-1047") || stderr.contains("DPI-1047"),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let conn = duckdb::Connection::open(store_path(dir.path())).unwrap();
    let error: Option<String> = conn
        .query_row("SELECT error FROM \"_dipper_runs\"", [], |row| row.get(0))
        .unwrap();
    let error = error.unwrap_or_default();
    assert!(!error.contains("PR2-canary-7f1d9c"), "{error}");
}

#[test]
fn missing_config_and_unopenable_store_exit_one() {
    let dir = tempfile::tempdir().unwrap();
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(dir.path().join("missing.toml"))
        .arg("dip")
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("config file not found"), "{stderr}");

    let blocked = tempfile::tempdir().unwrap();
    common::make_sqlite(blocked.path(), "source.db", &["ORDERS"]);
    std::fs::create_dir(blocked.path().join("not-a-file")).unwrap();
    let config = common::write_config(
        blocked.path(),
        r#"
store = "not-a-file"
[sources.sample]
kind = "sqlite"
path = "source.db"
[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID"]
"#,
    );
    let output = common::dipper()
        .current_dir(blocked.path())
        .args(["--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("store:"), "{stderr}");
}
