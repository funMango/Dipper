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
    let staging: i64 = conn
        .query_row(
            "SELECT count(*) FROM duckdb_tables() WHERE schema_name = 'main' AND table_name LIKE '%\\_\\_staging' ESCAPE '\\'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(staging, 0);
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
    fn catalog(
        &self,
        owner: &str,
        table: &str,
    ) -> Result<dipper::source::CatalogTable, DipperError> {
        self.inner.catalog(owner, table)
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
            "warnings",
            "time_from",
            "time_to",
            "load_mode"
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
fn reserved_names_and_case_duplicates_do_not_touch_the_store() {
    let dir = tempfile::tempdir().unwrap();
    common::make_sqlite(dir.path(), "source.db", &["ORDERS"]);
    let config = common::write_config(
        dir.path(),
        r#"
store = ".dipper/store.duckdb"
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
    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(0);
    let store = store_path(dir.path());
    let before = common::sha256_file(&store);
    let runs_before = count_and_sum(&store, "_dipper_runs", "rows");

    for (name, message) in [
        ("_DIPPER_RUNS", "must not start with _dipper"),
        ("_Dipper_x", "must not start with _dipper"),
        ("ORDERS__STAGING", "must not end with __staging"),
        ("orders__Staging", "must not end with __staging"),
    ] {
        common::write_config(
            dir.path(),
            &format!(
                r#"
store = ".dipper/store.duckdb"
[sources.sample]
kind = "sqlite"
path = "source.db"
[[dips]]
name = "{name}"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID"]
"#
            ),
        );
        for command in ["check", "dip"] {
            let output = common::dipper()
                .current_dir(dir.path())
                .args(["--config"])
                .arg(&config)
                .arg(command)
                .assert()
                .code(1)
                .get_output()
                .clone();
            let (_, stderr) = texts(&output);
            assert!(
                stderr.contains("dips[0].name") && stderr.contains(message),
                "{command} {name}: {stderr}"
            );
            assert_eq!(common::sha256_file(&store), before, "{command} {name}");
        }
    }

    common::write_config(
        dir.path(),
        r#"
store = ".dipper/store.duckdb"
[sources.sample]
kind = "sqlite"
path = "source.db"
[[dips]]
name = "orders"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID"]
[[dips]]
name = "ORDERS"
source = "sample"
table = "ORDERS"
columns = ["ORDER_ID"]
"#,
    );
    for command in ["check", "dip"] {
        let output = common::dipper()
            .current_dir(dir.path())
            .args(["--config"])
            .arg(&config)
            .arg(command)
            .assert()
            .code(1)
            .get_output()
            .clone();
        let (_, stderr) = texts(&output);
        assert!(
            stderr.contains("dips[1].name") && stderr.contains("duplicate dip name"),
            "{command}: {stderr}"
        );
        assert_eq!(common::sha256_file(&store), before, "{command}");
    }
    assert_eq!(count_and_sum(&store, "_dipper_runs", "rows"), runs_before);
    assert!(table_names(&store).iter().any(|name| name == "orders"));
    assert!(!table_names(&store)
        .iter()
        .any(|name| name == "_DIPPER_RUNS"));
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

fn events_config(dir: &std::path::Path, filters: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let db = common::make_events_sqlite(dir, "source.db");
    let config = common::write_config(
        dir,
        &format!(
            r#"
store = ".dipper/store.duckdb"
[sources.sample]
kind = "sqlite"
path = "source.db"
[[dips]]
name = "events"
source = "sample"
table = "EVENTS"
columns = ["ID", "TS"]
{filters}
"#
        ),
    );
    (db, config)
}

fn sqlite_count(db: &std::path::Path, sql: &str) -> i64 {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn sql_lines(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix("sql: "))
        .collect()
}

#[test]
fn same_timestamp_loads_only_that_bucket_without_datetime_literals() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(dir.path(), "");
    let expected = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS = '2026-08-21 08:00:00'",
    );
    assert!(expected > 0, "fixture should contain the 08:00 bucket");
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--log-sql", "--config"])
        .arg(&config)
        .args([
            "dip",
            "--from",
            "2026-08-21T08:00:00",
            "--to",
            "2026-08-21T08:00:00",
            "--time-column",
            "TS",
            "--yes",
        ])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&output);
    assert!(stderr.contains("eq"), "stderr should show eq:\n{stderr}");
    assert_eq!(stdout.lines().last(), Some("1 of 1 dips OK"));
    let store = store_path(dir.path());
    assert_eq!(count_and_sum(&store, "events", "ID").0, expected);
    let logged = sql_lines(&stderr);
    assert!(!logged.is_empty(), "{stderr}");
    for sql in &logged {
        assert!(
            !sql.contains("2026-08-21") && !sql.contains("08:00:00"),
            "datetime literal in sql: {sql}"
        );
        let allowed =
            dipper::source::SessionStatement::allowed(dipper::sql::guard::SqlDialect::Sqlite)
                .iter()
                .any(|statement| statement.sql() == *sql);
        let guarded =
            dipper::sql::guard::GuardedSelect::parse(sql, dipper::sql::guard::SqlDialect::Sqlite)
                .is_ok();
        assert!(allowed || guarded, "{sql}");
    }
    assert!(
        logged.iter().any(|sql| sql.contains('?')),
        "expected a placeholder bind, got {logged:?}"
    );
}

#[test]
fn period_load_matches_source_count_and_guards_existing_tables() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(dir.path(), "");
    let expected = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS >= '2026-08-21 00:00:00' AND TS < '2026-08-22 00:00:00'",
    );
    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args([
            "dip",
            "--from",
            "260821",
            "--to",
            "260821",
            "--time-column",
            "TS",
            "--yes",
        ])
        .assert()
        .code(0);
    let store = store_path(dir.path());
    assert_eq!(count_and_sum(&store, "events", "ID").0, expected);

    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args([
            "dip",
            "--from",
            "260821",
            "--to",
            "260821",
            "--time-column",
            "TS",
            "--yes",
        ])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("nothing to fetch"), "{stderr}");
    assert_eq!(count_and_sum(&store, "events", "ID").0, expected);

    let replaced = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS >= '2026-08-22 00:00:00' AND TS < '2026-08-23 00:00:00'",
    );
    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args([
            "dip",
            "--from",
            "260822",
            "--to",
            "260822",
            "--time-column",
            "TS",
            "--yes",
            "--replace-table",
        ])
        .assert()
        .code(0);
    assert_eq!(count_and_sum(&store, "events", "ID").0, replaced);
}

#[test]
fn bad_periods_do_not_create_a_store() {
    let cases = [
        vec![
            "dip",
            "--from",
            "260231",
            "--to",
            "260821",
            "--time-column",
            "TS",
            "--yes",
        ],
        vec!["dip", "--from", "260821", "--time-column", "TS", "--yes"],
        vec!["dip", "--from", "260821", "--to", "260821", "--yes"],
        vec!["dip", "--time-column", "TS"],
        vec!["dip", "--replace-table"],
    ];
    for args in cases {
        let dir = tempfile::tempdir().unwrap();
        let (_db, config) = events_config(dir.path(), "");
        let output = common::dipper()
            .current_dir(dir.path())
            .args(["--config"])
            .arg(&config)
            .args(&args)
            .assert()
            .code(1)
            .get_output()
            .clone();
        let (_, stderr) = texts(&output);
        assert!(
            !dir.path().join(".dipper").exists(),
            "{args:?} created a store:\n{stderr}"
        );
    }
}

fn assert_no_datetime_literals(logged: &[&str]) {
    for sql in logged {
        assert!(
            !sql.contains("2026-08-21") && !sql.contains("08:00:00"),
            "datetime literal in sql: {sql}"
        );
    }
}

#[test]
fn cli_period_overrides_config_time_filters_for_this_run() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(
        dir.path(),
        r#"filters = [{ column = "TS", op = "eq", value = 2026-08-21T08:00:00 }]"#,
    );
    let before = std::fs::read(&config).unwrap();
    let expected = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS >= '2026-08-21 00:00:00' AND TS < '2026-08-22 00:00:00'",
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--log-sql", "--config"])
        .arg(&config)
        .args([
            "dip",
            "--from",
            "260821",
            "--to",
            "260821",
            "--time-column",
            "TS",
            "--yes",
        ])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains(
        "note: events: --from/--to overrides config filters on time column TS for this run; the config file was not changed"
    ), "{stderr}");
    assert!(
        stderr.contains("ignored filter: TS eq 2026-08-21T08:00:00"),
        "{stderr}"
    );
    assert!(
        stderr.contains("overrides config filters on time column"),
        "{stderr}"
    );
    assert!(
        stderr.contains("the config file was not changed"),
        "{stderr}"
    );
    let note_at = stderr.find("overrides config filters").unwrap();
    let preview_at = stderr.find("preview:").expect("preview line");
    assert!(note_at < preview_at, "{stderr}");
    let logged = sql_lines(&stderr);
    assert_no_datetime_literals(&logged);
    assert!(logged.iter().any(|sql| sql.contains('?')), "{logged:?}");
    assert!(
        logged
            .iter()
            .any(|sql| sql.contains(">=") && sql.contains('<')),
        "{logged:?}"
    );
    assert!(
        logged.iter().all(|sql| !sql.contains("\"TS\" =")),
        "{logged:?}"
    );
    assert_eq!(
        count_and_sum(&store_path(dir.path()), "events", "ID").0,
        expected
    );
    assert_eq!(std::fs::read(&config).unwrap(), before);
}

#[test]
fn cli_period_keeps_other_column_filters() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(
        dir.path(),
        r#"filters = [
  { column = "ID", op = "gt", value = 2 },
  { column = "TS", op = "eq", value = 2026-08-21T08:00:00 }
]"#,
    );
    let expected = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE ID > 2 AND TS >= '2026-08-21 00:00:00' AND TS < '2026-08-22 00:00:00'",
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--log-sql", "--config"])
        .arg(&config)
        .args([
            "dip",
            "--from",
            "260821",
            "--to",
            "260821",
            "--time-column",
            "TS",
            "--yes",
        ])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(
        stderr.contains("overrides config filters on time column"),
        "{stderr}"
    );
    let logged = sql_lines(&stderr);
    assert!(
        logged.iter().any(|sql| sql.contains("\"ID\" >")),
        "{logged:?}"
    );
    assert!(
        logged.iter().all(|sql| !sql.contains("\"TS\" =")),
        "{logged:?}"
    );
    assert_eq!(
        count_and_sum(&store_path(dir.path()), "events", "ID").0,
        expected
    );
}

#[test]
fn config_time_filter_applies_when_no_period_is_given() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(
        dir.path(),
        r#"filters = [{ column = "TS", op = "eq", value = 2026-08-21T08:00:00 }]"#,
    );
    let expected = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS = '2026-08-21 08:00:00'",
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(["dip", "--yes"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(!stderr.contains("overrides config filters"), "{stderr}");
    assert!(!stderr.contains("ignored filter:"), "{stderr}");
    assert_eq!(
        count_and_sum(&store_path(dir.path()), "events", "ID").0,
        expected
    );
}

#[test]
fn period_sql_uses_the_config_column_spelling() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(
        dir.path(),
        r#"filters = [{ column = "TS", op = "eq", value = 2026-08-21T08:00:00 }]"#,
    );
    let before = std::fs::read(&config).unwrap();
    let expected = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS >= '2026-08-21 00:00:00' AND TS < '2026-08-22 00:00:00'",
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--log-sql", "--config"])
        .arg(&config)
        .args([
            "dip",
            "--from",
            "260821",
            "--to",
            "260821",
            "--time-column",
            "ts",
            "--yes",
        ])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("time column ts for this run"), "{stderr}");
    assert!(
        stderr.contains("ignored filter: TS eq 2026-08-21T08:00:00"),
        "{stderr}"
    );
    let logged = sql_lines(&stderr);
    assert_no_datetime_literals(&logged);
    assert!(
        logged
            .iter()
            .any(|sql| sql.contains("\"TS\" >=") && sql.contains("\"TS\" <")),
        "{logged:?}"
    );
    assert!(
        logged.iter().all(|sql| !sql.contains("\"ts\"")),
        "{logged:?}"
    );
    assert_eq!(
        count_and_sum(&store_path(dir.path()), "events", "ID").0,
        expected
    );
    assert_eq!(std::fs::read(&config).unwrap(), before);
}

#[test]
fn period_without_yes_on_a_pipe_does_not_load() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args([
            "dip",
            "--from",
            "260821",
            "--to",
            "260821",
            "--time-column",
            "TS",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("--yes"), "{stderr}");
    let store = store_path(dir.path());
    if store.exists() {
        assert!(!table_names(&store).iter().any(|name| name == "events"));
        let conn = duckdb::Connection::open(&store).unwrap();
        let runs: i64 = conn
            .query_row("SELECT count(*) FROM \"_dipper_runs\"", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(runs, 0);
    }
}

#[test]
fn one_existing_table_refuses_the_whole_period_command() {
    let dir = tempfile::tempdir().unwrap();
    common::make_events_sqlite(dir.path(), "source.db");
    let config = common::write_config(
        dir.path(),
        r#"
store = ".dipper/store.duckdb"
[sources.sample]
kind = "sqlite"
path = "source.db"
[[dips]]
name = "events"
source = "sample"
table = "EVENTS"
columns = ["ID", "TS"]
[[dips]]
name = "later"
source = "sample"
table = "EVENTS"
columns = ["ID", "TS"]
"#,
    );
    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(["dip", "events"])
        .assert()
        .code(0);
    let store = store_path(dir.path());
    let before = count_and_sum(&store, "events", "ID").0;
    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args([
            "dip",
            "events",
            "later",
            "--from",
            "260821",
            "--to",
            "260821",
            "--time-column",
            "TS",
            "--yes",
        ])
        .assert()
        .code(1);
    assert_eq!(count_and_sum(&store, "events", "ID").0, before);
    assert!(!table_names(&store).iter().any(|name| name == "later"));
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args([
            "dip",
            "events",
            "later",
            "--from",
            "260821",
            "--to",
            "260821",
            "--time-column",
            "TS",
            "--yes",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("has no slice ledger"), "{stderr}");
}

fn period_args<'a>(from: &'a str, to: &'a str, extra: &'a [&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "dip",
        "--from",
        from,
        "--to",
        to,
        "--time-column",
        "TS",
        "--yes",
    ];
    args.extend_from_slice(extra);
    args
}

fn run_period(
    dir: &std::path::Path,
    config: &std::path::Path,
    from: &str,
    to: &str,
    extra: &[&str],
) {
    common::dipper()
        .current_dir(dir)
        .args(["--config"])
        .arg(config)
        .args(period_args(from, to, extra))
        .assert()
        .code(0);
}

fn slice_rows(store: &std::path::Path, dip: &str) -> Vec<(String, String, i64)> {
    let conn = duckdb::Connection::open(store).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT CAST(time_from AS VARCHAR), CAST(time_to AS VARCHAR), rows FROM \"_dipper_slices\" WHERE dip = ? ORDER BY time_from",
        )
        .unwrap();
    stmt.query_map([dip], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn run_count(store: &std::path::Path) -> i64 {
    let conn = duckdb::Connection::open(store).unwrap();
    conn.query_row("SELECT count(*) FROM \"_dipper_runs\"", [], |row| {
        row.get(0)
    })
    .unwrap()
}

#[test]
fn period_fill_fetches_only_missing_september_after_august() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(dir.path(), "");
    let august = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS >= '2026-08-01 00:00:00' AND TS < '2026-09-01 00:00:00'",
    );
    let september = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS >= '2026-09-01 00:00:00' AND TS < '2026-10-01 00:00:00'",
    );
    assert!(august > 0 && september > 0);
    run_period(dir.path(), &config, "260801", "260831", &[]);
    let store = store_path(dir.path());
    assert_eq!(count_and_sum(&store, "events", "ID").0, august);
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--log-sql", "--config"])
        .arg(&config)
        .args(period_args("260815", "260930", &[]))
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("requested"), "{stderr}");
    assert!(stderr.contains("have"), "{stderr}");
    assert!(stderr.contains("fetch"), "{stderr}");
    assert!(stderr.contains("count"), "{stderr}");
    let logged = sql_lines(&stderr);
    assert!(logged.iter().any(|sql| sql.contains('?')), "{logged:?}");
    assert!(
        logged
            .iter()
            .any(|sql| sql.contains(">=") && sql.contains('<')),
        "{logged:?}"
    );
    for sql in &logged {
        assert!(!sql.contains("2026-"), "datetime literal in sql: {sql}");
    }
    assert_eq!(count_and_sum(&store, "events", "ID").0, august + september);
    let august_now: i64 = duckdb::Connection::open(&store)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM events WHERE TS < '2026-09-01 00:00:00'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(august_now, august);
}

#[test]
fn period_fill_fetches_each_hole_when_several_exist() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(dir.path(), "");
    run_period(dir.path(), &config, "260821", "260821", &[]);
    let store = store_path(dir.path());
    let held = count_and_sum(&store, "events", "ID").0;
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--log-sql", "--config"])
        .arg(&config)
        .args(period_args("260820", "260822", &[]))
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    let selects = sql_lines(&stderr)
        .into_iter()
        .filter(|sql| sql.contains("FROM \"EVENTS\"") && sql.contains(">="))
        .count();
    assert!(selects >= 2, "{stderr}");
    let expected = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS >= '2026-08-20 00:00:00' AND TS < '2026-08-23 00:00:00'",
    );
    assert_eq!(count_and_sum(&store, "events", "ID").0, expected);
    let still_held: i64 = duckdb::Connection::open(&store)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM events WHERE TS >= '2026-08-21 00:00:00' AND TS < '2026-08-22 00:00:00'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(still_held, held);
}

#[test]
fn period_nothing_to_fetch_exits_zero_without_opening_source() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
    run_period(dir.path(), &config, "260821", "260821", &[]);
    let store = store_path(dir.path());
    let before = count_and_sum(&store, "events", "ID").0;
    let slices = slice_rows(&store, "events");
    let runs = run_count(&store);
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--log-sql", "--config"])
        .arg(&config)
        .args(period_args("260821", "260821", &[]))
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("nothing to fetch"), "{stderr}");
    assert!(!stderr.contains("sql:"), "{stderr}");
    assert_eq!(count_and_sum(&store, "events", "ID").0, before);
    assert_eq!(slice_rows(&store, "events"), slices);
    assert_eq!(run_count(&store), runs);
}

#[test]
fn period_zero_row_window_is_recorded_in_slices() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
    run_period(dir.path(), &config, "260101", "260101", &[]);
    let store = store_path(dir.path());
    let loaded: i64 = duckdb::Connection::open(&store)
        .unwrap()
        .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(loaded, 0);
    let slices = slice_rows(&store, "events");
    assert_eq!(slices.len(), 1);
    assert_eq!(slices[0].2, 0);
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--log-sql", "--config"])
        .arg(&config)
        .args(period_args("260101", "260101", &[]))
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("nothing to fetch"), "{stderr}");
    assert!(!stderr.contains("sql:"), "{stderr}");
}

#[test]
fn period_refresh_replaces_only_the_requested_window() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(dir.path(), "");
    run_period(dir.path(), &config, "260821", "260822", &[]);
    let store = store_path(dir.path());
    let before_next: i64 = duckdb::Connection::open(&store)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM events WHERE TS >= '2026-08-22 00:00:00'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("DELETE FROM EVENTS WHERE ID = 3", []).unwrap();
    drop(conn);
    run_period(dir.path(), &config, "260821", "260821", &["--refresh"]);
    let august_day = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS >= '2026-08-21 00:00:00' AND TS < '2026-08-22 00:00:00'",
    );
    let loaded_day: i64 = duckdb::Connection::open(&store)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM events WHERE TS >= '2026-08-21 00:00:00' AND TS < '2026-08-22 00:00:00'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(loaded_day, august_day);
    let next: i64 = duckdb::Connection::open(&store)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM events WHERE TS >= '2026-08-22 00:00:00'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(next, before_next);
}

#[test]
fn period_merge_failure_rolls_back_table_and_slices() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
    run_period(dir.path(), &config, "260821", "260821", &[]);
    let store = store_path(dir.path());
    let before = count_and_sum(&store, "events", "ID").0;
    let slices = slice_rows(&store, "events");
    common::write_config(
        dir.path(),
        r#"
store = ".dipper/store.duckdb"
[sources.sample]
kind = "sqlite"
path = "source.db"
[[dips]]
name = "events"
source = "sample"
table = "EVENTS"
columns = ["ID", "TS", "NOPE"]
"#,
    );
    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(period_args("260822", "260822", &[]))
        .assert()
        .code(1);
    assert_eq!(count_and_sum(&store, "events", "ID").0, before);
    assert_eq!(slice_rows(&store, "events"), slices);
}

#[test]
fn period_existing_table_without_slices_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(0);
    let store = store_path(dir.path());
    let before = count_and_sum(&store, "events", "ID").0;
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(period_args("260821", "260821", &[]))
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("has no slice ledger"), "{stderr}");
    assert!(stderr.contains("--refresh"), "{stderr}");
    assert!(stderr.contains("--replace-table"), "{stderr}");
    assert_eq!(count_and_sum(&store, "events", "ID").0, before);
}

#[test]
fn period_without_from_to_still_uses_swap_in() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(dir.path(), "");
    run_period(dir.path(), &config, "260821", "260821", &[]);
    let store = store_path(dir.path());
    assert!(!slice_rows(&store, "events").is_empty());
    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(0);
    let all = sqlite_count(&db, "SELECT COUNT(*) FROM EVENTS");
    assert_eq!(count_and_sum(&store, "events", "ID").0, all);
    assert!(slice_rows(&store, "events").is_empty());
}

#[test]
fn preview_over_the_row_limit_loads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
    let output = common::dipper()
        .current_dir(dir.path())
        .env("DIPPER_TEST_PREVIEW_LIMIT", "0")
        .args(["--config"])
        .arg(&config)
        .args(period_args("260821", "260821", &[]))
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("exceeds 0"), "{stderr}");
    assert!(stderr.contains("nothing was loaded"), "{stderr}");
    assert_eq!(dipper::config::MAX_PREVIEW_ROWS, 1_000_000);
    let store = store_path(dir.path());
    if store.exists() {
        let conn = duckdb::Connection::open(&store).unwrap();
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM duckdb_tables() WHERE schema_name = 'main' AND lower(table_name) = 'events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }
}

#[test]
fn refresh_with_replace_table_exits_one_and_leaves_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
    run_period(dir.path(), &config, "260821", "260821", &[]);
    let store = store_path(dir.path());
    let before = count_and_sum(&store, "events", "ID").0;
    let slices = slice_rows(&store, "events");
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(period_args(
            "260821",
            "260821",
            &["--refresh", "--replace-table"],
        ))
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(
        stderr.contains("--refresh cannot be combined with --replace-table"),
        "{stderr}"
    );
    assert_eq!(count_and_sum(&store, "events", "ID").0, before);
    assert_eq!(slice_rows(&store, "events"), slices);
}

#[test]
fn missing_range_of_32_days_fails_and_31_days_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
    run_period(dir.path(), &config, "260101", "260101", &[]);
    let store = store_path(dir.path());
    let slices = slice_rows(&store, "events");
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(period_args("260101", "260202", &[]))
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("longer than 31 days"), "{stderr}");
    assert!(stderr.contains("--yes does not bypass"), "{stderr}");
    assert_eq!(slice_rows(&store, "events"), slices);
    run_period(dir.path(), &config, "260101", "260201", &[]);
    assert!(slice_rows(&store, "events").len() > slices.len());
}

#[test]
fn replace_table_with_zero_rows_leaves_the_existing_table() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
    run_period(dir.path(), &config, "260821", "260821", &[]);
    let store = store_path(dir.path());
    let before = count_and_sum(&store, "events", "ID").0;
    let slices = slice_rows(&store, "events");
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(period_args("260101", "260101", &["--replace-table"]))
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("choose a different period"), "{stderr}");
    assert!(stderr.contains("0 rows"), "{stderr}");
    assert_eq!(count_and_sum(&store, "events", "ID").0, before);
    assert_eq!(slice_rows(&store, "events"), slices);
}

#[test]
fn same_instant_bucket_does_not_include_subsecond_rows() {
    // `column = T` binds the whole second. A source row at T plus a fraction
    // of a second is a different value and is not part of that bucket.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("source.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE EVENTS (ID INTEGER PRIMARY KEY, TS TEXT);
         INSERT INTO EVENTS (ID, TS) VALUES
           (1, '2026-08-21 08:00:00'),
           (2, '2026-08-21 08:00:00.500');",
    )
    .unwrap();
    drop(conn);
    let config = common::write_config(
        dir.path(),
        r#"
store = ".dipper/store.duckdb"
[sources.sample]
kind = "sqlite"
path = "source.db"
[[dips]]
name = "events"
source = "sample"
table = "EVENTS"
columns = ["ID", "TS"]
"#,
    );
    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args([
            "dip",
            "--from",
            "2026-08-21T08:00:00",
            "--to",
            "2026-08-21T08:00:00",
            "--time-column",
            "TS",
            "--yes",
        ])
        .assert()
        .code(0);
    let store = store_path(dir.path());
    assert_eq!(count_and_sum(&store, "events", "ID").0, 1);
}

#[test]
fn period_request_longer_than_31_days_succeeds_when_missing_part_fits() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(dir.path(), "");
    run_period(dir.path(), &config, "260801", "260831", &[]);
    let september = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS >= '2026-09-01 00:00:00' AND TS < '2026-10-01 00:00:00'",
    );
    run_period(dir.path(), &config, "260815", "260930", &[]);
    let store = store_path(dir.path());
    let loaded_september: i64 = duckdb::Connection::open(&store)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM events WHERE TS >= '2026-09-01 00:00:00' AND TS < '2026-10-01 00:00:00'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(loaded_september, september);
}

fn json_doc(stdout: &str) -> serde_json::Value {
    serde_json::from_slice(stdout.as_bytes()).unwrap_or_else(|err| panic!("{err}\n{stdout}"))
}

fn assert_no_secrets(text: &str, label: &str) {
    assert!(!text.contains("PR2-canary-7f1d9c"), "{label}: {text}");
    assert!(!text.contains("reader"), "{label}: {text}");
    assert!(!text.contains("127.0.0.1:1/NOPE"), "{label}: {text}");
}

#[test]
fn dip_json_without_period_reports_rows_and_ok() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(dir.path(), "");
    let expected = sqlite_count(&db, "SELECT COUNT(*) FROM EVENTS");
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(["dip", "--format", "json"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, _) = texts(&output);
    let doc = json_doc(&stdout);
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["command"], "dip");
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["exit_code"], 0);
    assert!(doc["period"].is_null());
    assert!(doc["error"].is_null());
    assert_eq!(doc["dips"][0]["rows_loaded"], expected);
    assert_eq!(doc["dips"][0]["status"], "ok");
    assert_eq!(doc["summary"]["ok"], 1);
}

#[test]
fn dip_json_period_fill_reports_held_and_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(dir.path(), "");
    let september = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS >= '2026-09-01 00:00:00' AND TS < '2026-10-01 00:00:00'",
    );
    run_period(dir.path(), &config, "260801", "260831", &[]);
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(period_args("260815", "260930", &["--format", "json"]))
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, _) = texts(&output);
    let doc = json_doc(&stdout);
    assert_eq!(doc["period"]["mode"], "fill");
    assert_eq!(doc["period"]["exact"], false);
    assert_eq!(doc["period"]["start"], "2026-08-15T00:00:00");
    assert_eq!(doc["period"]["end"], "2026-10-01T00:00:00");
    assert_eq!(doc["dips"][0]["held"].as_array().unwrap().len(), 1);
    assert_eq!(doc["dips"][0]["held"][0]["start"], "2026-08-01T00:00:00");
    assert_eq!(doc["dips"][0]["held"][0]["end"], "2026-09-01T00:00:00");
    assert_eq!(doc["dips"][0]["fetch"].as_array().unwrap().len(), 1);
    assert_eq!(doc["dips"][0]["fetch"][0]["start"], "2026-09-01T00:00:00");
    assert_eq!(doc["dips"][0]["fetch"][0]["end"], "2026-10-01T00:00:00");
    assert_eq!(doc["dips"][0]["fetch"][0]["count"], september);
    assert_eq!(doc["dips"][0]["rows_loaded"], september);
}

#[test]
fn dip_json_nothing_to_fetch_is_ok_without_source_sql() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
    run_period(dir.path(), &config, "260821", "260821", &[]);
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--log-sql", "--config"])
        .arg(&config)
        .args(period_args("260821", "260821", &["--format", "json"]))
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&output);
    let doc = json_doc(&stdout);
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["exit_code"], 0);
    assert_eq!(doc["dips"][0]["status"], "nothing_to_fetch");
    assert!(doc["dips"][0]["fetch"].as_array().unwrap().is_empty());
    assert_eq!(doc["dips"][0]["rows_loaded"], 0);
    assert!(stderr.contains("nothing to fetch"), "{stderr}");
    assert!(!stderr.contains("sql:"), "{stderr}");
    assert!(!stderr.to_ascii_uppercase().contains("SELECT"), "{stderr}");
}

#[test]
fn dip_json_refresh_marks_refresh_true() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(dir.path(), "");
    let day = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS >= '2026-08-21 00:00:00' AND TS < '2026-08-22 00:00:00'",
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(period_args(
            "260821",
            "260821",
            &["--refresh", "--format", "json"],
        ))
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, _) = texts(&output);
    let doc = json_doc(&stdout);
    assert_eq!(doc["period"]["mode"], "refresh");
    assert_eq!(doc["dips"][0]["refresh"], true);
    assert_eq!(doc["dips"][0]["fetch"].as_array().unwrap().len(), 1);
    assert_eq!(doc["dips"][0]["fetch"][0]["start"], "2026-08-21T00:00:00");
    assert_eq!(doc["dips"][0]["fetch"][0]["end"], "2026-08-22T00:00:00");
    assert_eq!(doc["dips"][0]["fetch"][0]["count"], day);
    assert_eq!(doc["dips"][0]["rows_loaded"], day);
}

#[test]
fn dip_json_lists_ignored_config_time_filters() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(
        dir.path(),
        r#"filters = [{ column = "TS", op = "eq", value = 2026-08-21T08:00:00 }]"#,
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(period_args("260821", "260821", &["--format", "json"]))
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&output);
    let doc = json_doc(&stdout);
    let ignored = &doc["dips"][0]["ignored_filters"][0];
    assert_eq!(ignored["column"], "TS");
    assert_eq!(ignored["op"], "eq");
    assert_eq!(ignored["value"], "2026-08-21T08:00:00");
    assert!(stderr.contains(
        "note: events: --from/--to overrides config filters on time column TS for this run; the config file was not changed"
    ), "{stderr}");
}

#[test]
fn dip_json_exact_timestamp_period_is_one_second() {
    let dir = tempfile::tempdir().unwrap();
    let (db, config) = events_config(dir.path(), "");
    let expected = sqlite_count(
        &db,
        "SELECT COUNT(*) FROM EVENTS WHERE TS = '2026-08-21 08:00:00'",
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args([
            "dip",
            "--from",
            "2026-08-21T08:00:00",
            "--to",
            "2026-08-21T08:00:00",
            "--time-column",
            "TS",
            "--yes",
            "--format",
            "json",
        ])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, _) = texts(&output);
    let doc = json_doc(&stdout);
    assert_eq!(doc["period"]["exact"], true);
    assert_eq!(doc["period"]["start"], "2026-08-21T08:00:00");
    assert_eq!(doc["period"]["end"], "2026-08-21T08:00:01");
    assert_eq!(doc["dips"][0]["rows_loaded"], expected);
}

#[test]
fn dip_json_pre_run_failure_writes_error_and_exits_one() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
    common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(0);
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(period_args("260821", "260821", &["--format", "json"]))
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&output);
    let doc = json_doc(&stdout);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["exit_code"], 1);
    assert!(doc["dips"].as_array().unwrap().is_empty());
    assert!(doc["error"]["message"]
        .as_str()
        .unwrap()
        .contains("has no slice ledger"));
    assert!(stderr.contains("dipper:"), "{stderr}");
}

#[test]
fn dip_json_failed_and_not_run_dips_are_listed() {
    let dir = tempfile::tempdir().unwrap();
    let dips = r#"
[[dips]]
name = "missing"
source = "sample"
table = "NOPE"
columns = ["ORDER_ID"]

[[dips]]
name = "items"
source = "sample"
table = "ITEMS"
columns = ["ITEM_ID"]
"#;
    let (_db, config) = prepare(dir.path(), "batch_rows = 1000", dips);
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(["dip", "--fail-fast", "--format", "json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&output);
    let doc = json_doc(&stdout);
    assert_eq!(doc["dips"][0]["name"], "missing");
    assert_eq!(doc["dips"][0]["status"], "failed");
    assert!(doc["dips"][0]["error"].is_string());
    assert_eq!(doc["dips"][1]["name"], "items");
    assert_eq!(doc["dips"][1]["status"], "not_run");
    assert_eq!(doc["dips"][1]["rows_loaded"], 0);
    assert_eq!(doc["summary"]["failed"], 1);
    assert_eq!(doc["summary"]["not_run"], 1);
    assert!(doc["error"].is_null());
    assert!(stderr.contains("dipper:"), "{stderr}");
}

#[test]
fn dip_json_period_without_yes_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args([
            "dip",
            "--from",
            "260821",
            "--to",
            "260821",
            "--time-column",
            "TS",
            "--format",
            "json",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&output);
    let doc = json_doc(&stdout);
    assert_eq!(doc["exit_code"], 1);
    assert!(doc["error"]["message"]
        .as_str()
        .unwrap()
        .contains("needs --yes"));
    assert!(!store_path(dir.path()).exists(), "{stderr}");
}

#[test]
fn dip_json_has_no_row_values() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("source.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE NOTES (ID INTEGER PRIMARY KEY, BODY TEXT);
         INSERT INTO NOTES (ID, BODY) VALUES (1, 'ROWCANARY-7c1e');",
    )
    .unwrap();
    drop(conn);
    let config = common::write_config(
        dir.path(),
        r#"
store = ".dipper/store.duckdb"
[sources.sample]
kind = "sqlite"
path = "source.db"
[[dips]]
name = "notes"
source = "sample"
table = "NOTES"
columns = ["ID", "BODY"]
"#,
    );
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(["dip", "--format", "json"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let (stdout, _) = texts(&output);
    let _ = json_doc(&stdout);
    assert!(!stdout.contains("ROWCANARY-7c1e"), "{stdout}");
}

#[test]
fn dip_json_hides_oracle_password_user_and_connect() {
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
        .args(["--config"])
        .arg(&config)
        .args(["dip", "--format", "json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&output);
    let _ = json_doc(&stdout);
    assert_no_secrets(&stdout, "stdout");
    assert_no_secrets(&stderr, "stderr");
    assert!(stderr.contains("dipper:"), "{stderr}");
    let conn = duckdb::Connection::open(store_path(dir.path())).unwrap();
    let error: Option<String> = conn
        .query_row("SELECT error FROM \"_dipper_runs\"", [], |row| row.get(0))
        .unwrap();
    assert_no_secrets(&error.unwrap_or_default(), "runs");
}

#[test]
fn oracle_secrets_are_redacted_without_format() {
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
        .args(["--config"])
        .arg(&config)
        .arg("dip")
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (stdout, stderr) = texts(&output);
    assert_no_secrets(&stdout, "stdout");
    assert_no_secrets(&stderr, "stderr");
    assert!(stderr.contains("dipper:"), "{stderr}");
    let conn = duckdb::Connection::open(store_path(dir.path())).unwrap();
    let error: Option<String> = conn
        .query_row("SELECT error FROM \"_dipper_runs\"", [], |row| row.get(0))
        .unwrap();
    assert_no_secrets(&error.unwrap_or_default(), "runs");
}

#[test]
fn dip_without_format_keeps_table_output() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
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
    assert!(stdout.contains("dip"), "{stdout}");
    assert!(stdout.contains("rows"), "{stdout}");
    assert!(stdout.contains("status"), "{stdout}");
    assert!(stdout.contains("1 of 1 dips OK"), "{stdout}");
    assert!(serde_json::from_slice::<serde_json::Value>(stdout.as_bytes()).is_err());
}

#[test]
fn replace_table_with_zero_rows_and_no_table_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (_db, config) = events_config(dir.path(), "");
    let output = common::dipper()
        .current_dir(dir.path())
        .args(["--config"])
        .arg(&config)
        .args(period_args("260101", "260101", &["--replace-table"]))
        .assert()
        .code(1)
        .get_output()
        .clone();
    let (_, stderr) = texts(&output);
    assert!(stderr.contains("0 rows"), "{stderr}");
    assert!(stderr.contains("choose a different period"), "{stderr}");
    let store = store_path(dir.path());
    if store.exists() {
        let conn = duckdb::Connection::open(&store).unwrap();
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM duckdb_tables() WHERE schema_name = 'main' AND lower(table_name) = 'events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
        assert!(slice_rows(&store, "events").is_empty());
        assert_eq!(run_count(&store), 0);
    }
}
