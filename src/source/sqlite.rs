use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
use rusqlite::limits::Limit;
use rusqlite::types::{ToSql, ToSqlOutput, ValueRef};
use rusqlite::{config::DbConfig, Connection, OpenFlags, Row};

use crate::error::DipperError;
use crate::source::logging::LoggingExecutor;
use crate::source::preflight::{blocks, run_sqlite, DictQuery, Dictionary};
use crate::source::session::{Executor, RowCursor, Session, SessionOptions, SessionStatement};
use crate::source::watchdog::{Breaker, SystemClock};
use crate::source::{
    cell_i64, cell_text, session_deadline, CatalogColumn, CatalogForeignKey, CatalogTable,
    ChunkIter, OpenOptions, Opened, Paging, PreflightReport, SelectSpec, Source, Value,
};
use crate::sql::build;
use crate::sql::guard::{GuardedSelect, GuardedSql, SqlDialect};

pub struct SqliteSource {
    session: Session<LoggingExecutor<SqliteExec>>,
}

impl SqliteSource {
    pub(crate) fn open(
        name: &str,
        path: &Path,
        max_session_minutes: u64,
        opts: &OpenOptions,
    ) -> Result<Opened, DipperError> {
        let source = Self::connect(name, path, max_session_minutes, opts)?;
        let findings = source.session.findings().to_vec();
        Ok(Opened {
            source: Box::new(source),
            findings,
        })
    }

    pub(crate) fn connect(
        name: &str,
        path: &Path,
        max_session_minutes: u64,
        opts: &OpenOptions,
    ) -> Result<Self, DipperError> {
        let mut conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|err| {
            source_error(
                name,
                format!("cannot open sqlite file {}: {err}", path.display()),
            )
        })?;
        conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)
            .map_err(|err| source_error(name, err.to_string()))?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
            .map_err(|err| source_error(name, err.to_string()))?;
        conn.authorizer(Some(authorize))
            .map_err(|err| source_error(name, err.to_string()))?;
        let breaker = Arc::new(SqliteBreaker(conn.get_interrupt_handle()));
        let exec = LoggingExecutor::new(
            SqliteExec {
                name: name.to_string(),
                conn,
                breaker,
            },
            opts.log.clone(),
        );
        let session = Session::start(
            exec,
            &SessionOptions {
                source_name: name.to_string(),
                strict: opts.strict,
                deadline: session_deadline(max_session_minutes),
                clock: Arc::new(SystemClock),
                tick: Duration::from_secs(1),
            },
        )?;
        Ok(Self { session })
    }

    #[cfg(test)]
    fn connection(&self) -> &Connection {
        self.session.executor().inner().connection()
    }
}

struct SqliteBreaker(rusqlite::InterruptHandle);

impl Breaker for SqliteBreaker {
    fn interrupt(&self) {
        self.0.interrupt();
    }
}

struct SqliteExec {
    name: String,
    conn: Connection,
    breaker: Arc<SqliteBreaker>,
}

impl SqliteExec {
    #[cfg(test)]
    fn connection(&self) -> &Connection {
        &self.conn
    }

    fn fail(&self, err: rusqlite::Error) -> DipperError {
        source_error(&self.name, err.to_string())
    }
}

impl Executor for SqliteExec {
    fn dialect(&self) -> SqlDialect {
        SqlDialect::Sqlite
    }

    fn server_major(&self) -> Option<i32> {
        None
    }

    fn run_session(&self, stmt: SessionStatement) -> Result<(), DipperError> {
        match stmt {
            SessionStatement::Rollback => {
                if !self.conn.is_autocommit() {
                    self.conn
                        .execute(stmt.sql(), [])
                        .map_err(|err| self.fail(err))?;
                }
                Ok(())
            }
            SessionStatement::SqliteBegin | SessionStatement::SqliteQueryOnly => self
                .conn
                .execute(stmt.sql(), [])
                .map(|_| ())
                .map_err(|err| self.fail(err)),
            _ => Err(source_error(
                &self.name,
                "session statement is not valid for sqlite".to_string(),
            )),
        }
    }

    fn open_cursor<'a>(
        &'a self,
        query: &GuardedSelect,
        binds: &[Value],
        fetch_rows: usize,
    ) -> Result<Box<dyn RowCursor + 'a>, DipperError> {
        self.open_statement(query.sql(), binds, fetch_rows)
    }

    fn open_sql<'a>(
        &'a self,
        query: &GuardedSql,
        binds: &[Value],
        fetch_rows: usize,
    ) -> Result<Box<dyn RowCursor + 'a>, DipperError> {
        self.open_statement(query.sql(), binds, fetch_rows)
    }

    fn read_fixed(&self, sql: &str) -> Result<Vec<Vec<Value>>, DipperError> {
        let allowed = (sql.starts_with("PRAGMA table_xinfo(\"")
            || sql.starts_with("PRAGMA foreign_key_list(\""))
            && sql.ends_with("\")");
        if !allowed {
            return Err(source_error(
                &self.name,
                "fixed catalog read is not supported".to_string(),
            ));
        }
        let mut statement = self.conn.prepare(sql).map_err(|err| self.fail(err))?;
        let width = statement.column_count();
        let mut rows = statement.query([]).map_err(|err| self.fail(err))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|err| self.fail(err))? {
            out.push(read_row(row, width).map_err(|err| self.fail(err))?);
        }
        Ok(out)
    }

    fn breaker(&self) -> Arc<dyn Breaker> {
        Arc::clone(&self.breaker) as Arc<dyn Breaker>
    }

    fn close(&self) -> Result<(), DipperError> {
        if !self.conn.is_autocommit() {
            self.conn
                .execute(SessionStatement::Rollback.sql(), [])
                .map_err(|err| self.fail(err))?;
        }
        Ok(())
    }
}

impl SqliteExec {
    fn open_statement<'a>(
        &'a self,
        sql: &str,
        binds: &[Value],
        _fetch_rows: usize,
    ) -> Result<Box<dyn RowCursor + 'a>, DipperError> {
        let statement = Box::new(self.conn.prepare(sql).map_err(|err| self.fail(err))?);
        let columns: Vec<String> = statement.column_names().map(str::to_string).collect();
        let params: Vec<Bound> = binds.iter().cloned().map(Bound).collect();
        let statement = Box::into_raw(statement);
        let rows = match unsafe { query_erased(statement, &params) } {
            Ok(rows) => rows,
            Err(err) => {
                // SAFETY: `query` failed before a row stream existed, so nothing borrows
                // this statement. The pointer came from `Box::into_raw`.
                unsafe { drop(Box::from_raw(statement)) };
                return Err(self.fail(err));
            }
        };
        Ok(Box::new(SqliteStream {
            statement,
            rows,
            columns,
            name: self.name.clone(),
        }))
    }
}

fn pragma_arg_is_ident(value: &str) -> bool {
    let value = value.trim().trim_matches('"');
    crate::sql::validate_column_ident(value).is_ok()
}

fn authorize(ctx: AuthContext<'_>) -> Authorization {
    match ctx.action {
        AuthAction::Select => Authorization::Allow,
        AuthAction::Read { .. } => Authorization::Allow,
        AuthAction::Transaction {
            operation: TransactionOperation::Begin | TransactionOperation::Rollback,
        } => Authorization::Allow,
        AuthAction::Function { function_name } if function_name.eq_ignore_ascii_case("count") => {
            Authorization::Allow
        }
        AuthAction::Pragma {
            pragma_name,
            pragma_value,
        } if pragma_name.eq_ignore_ascii_case("query_only")
            && matches!(pragma_value, None | Some("ON")) =>
        {
            Authorization::Allow
        }
        AuthAction::Pragma {
            pragma_name,
            pragma_value: Some(value),
        } if (pragma_name.eq_ignore_ascii_case("table_xinfo")
            || pragma_name.eq_ignore_ascii_case("foreign_key_list"))
            && pragma_arg_is_ident(value) =>
        {
            Authorization::Allow
        }
        _ => Authorization::Deny,
    }
}

/// One open statement. Rows are stepped one at a time, not copied up front.
///
/// Both fields are raw pointers from `Box::into_raw`. `rows` borrows `statement`,
/// so `Drop` frees `rows` first.
struct SqliteStream<'conn> {
    statement: *mut rusqlite::Statement<'conn>,
    rows: *mut rusqlite::Rows<'static>,
    columns: Vec<String>,
    name: String,
}

/// Bind `params` and return a row stream that borrows `statement`.
///
/// # Safety
/// `statement` must be a live `Box::into_raw` pointer. The caller must drop the
/// returned rows before freeing `statement`.
unsafe fn query_erased(
    statement: *mut rusqlite::Statement<'_>,
    params: &[Bound],
) -> rusqlite::Result<*mut rusqlite::Rows<'static>> {
    // SAFETY: caller owns `statement` and keeps it alive until the rows are dropped.
    let stmt = unsafe { &mut *statement };
    let rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
    // SAFETY: `Rows` only borrows `statement`. Erasing the lifetime is sound because
    // the caller drops these rows before `statement`.
    let rows: rusqlite::Rows<'static> = unsafe { std::mem::transmute(rows) };
    Ok(Box::into_raw(Box::new(rows)))
}

impl Drop for SqliteStream<'_> {
    fn drop(&mut self) {
        if !self.rows.is_null() {
            // SAFETY: `rows` came from `Box::into_raw` and still borrows `statement`.
            unsafe { drop(Box::from_raw(self.rows)) };
            self.rows = std::ptr::null_mut();
        }
        if !self.statement.is_null() {
            // SAFETY: `statement` came from `Box::into_raw`. `rows` is already gone,
            // so nothing still borrows it.
            unsafe { drop(Box::from_raw(self.statement)) };
            self.statement = std::ptr::null_mut();
        }
    }
}

impl RowCursor for SqliteStream<'_> {
    fn columns(&self) -> &[String] {
        &self.columns
    }

    fn next_row(&mut self) -> Result<Option<Vec<Value>>, DipperError> {
        // SAFETY: `rows` is non-null until Drop and this is the only live borrow.
        let rows = unsafe { &mut *self.rows };
        let width = self.columns.len();
        let name = self.name.clone();
        match rows.next() {
            Ok(Some(row)) => read_row(row, width)
                .map(Some)
                .map_err(|err| DipperError::Source {
                    source_name: name,
                    message: err.to_string(),
                }),
            Ok(None) => Ok(None),
            Err(err) => Err(DipperError::Source {
                source_name: name,
                message: err.to_string(),
            }),
        }
    }
}

struct Bound(Value);

impl ToSql for Bound {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        match &self.0 {
            Value::Null => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Null)),
            Value::Int(value) => Ok(ToSqlOutput::from(*value)),
            Value::Float(value) => Ok(ToSqlOutput::from(*value)),
            Value::Text(value) => Ok(ToSqlOutput::from(value.as_str())),
            Value::Date(value) => Ok(ToSqlOutput::from(value.format("%Y-%m-%d").to_string())),
            Value::Timestamp(value) => Ok(ToSqlOutput::from(
                value.format("%Y-%m-%d %H:%M:%S%.f").to_string(),
            )),
        }
    }
}

fn read_row(row: &Row<'_>, width: usize) -> rusqlite::Result<Vec<Value>> {
    let mut values = Vec::with_capacity(width);
    for index in 0..width {
        values.push(match row.get_ref(index)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(value) => Value::Int(value),
            ValueRef::Real(value) => Value::Float(value),
            ValueRef::Text(value) => Value::Text(String::from_utf8_lossy(value).into_owned()),
            ValueRef::Blob(value) => Value::Text(hex_encode(value)),
        });
    }
    Ok(values)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn source_error(name: &str, message: String) -> DipperError {
    DipperError::Source {
        source_name: name.to_string(),
        message,
    }
}

impl Source for SqliteSource {
    fn name(&self) -> &str {
        self.session.source_name()
    }

    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn test_connection(&self) -> Result<(), DipperError> {
        let query =
            GuardedSelect::parse("SELECT 1 FROM sqlite_master LIMIT 1", SqlDialect::Sqlite)?;
        self.session.read_all(&query, &[])?;
        Ok(())
    }

    fn preflight(&self, dip: &str, spec: &SelectSpec) -> Result<PreflightReport, DipperError> {
        Ok(run_sqlite(&self.session, spec, dip))
    }

    fn count(&self, spec: &SelectSpec) -> Result<u64, DipperError> {
        let (query, binds) = build::count(SqlDialect::Sqlite, spec)?;
        let rows = self.session.read_all(&query, &binds)?;
        match rows.first().and_then(|row| row.first()) {
            Some(Value::Int(value)) if *value >= 0 => Ok(*value as u64),
            Some(Value::Null) | None => Ok(0),
            Some(other) => Err(source_error(
                self.name(),
                format!("COUNT(*) returned {other:?}"),
            )),
        }
    }

    fn select<'a>(&'a self, spec: &SelectSpec) -> Result<ChunkIter<'a>, DipperError> {
        let report = self.preflight(&spec.table, spec)?;
        if blocks(&report, self.session.strict()) {
            return Err(blocked(self.name(), &report));
        }
        let plan = match spec.paging {
            Paging::Snapshot => build::snapshot(SqlDialect::Sqlite, spec)?,
            Paging::Keyset => {
                let key = spec
                    .key
                    .clone()
                    .or_else(|| report.key.map(|key| key.column))
                    .unwrap_or_else(|| "rowid".to_string());
                build::keyset(SqlDialect::Sqlite, spec, &key)?
            }
        };
        Ok(self.session.chunks(plan))
    }

    fn catalog(&self, owner: &str, table: &str) -> Result<CatalogTable, DipperError> {
        if !owner.eq_ignore_ascii_case("main") {
            return Err(source_error(
                self.name(),
                format!("schema prefix {owner} is not allowed; only main is allowed"),
            ));
        }
        crate::sql::validate_column_ident(table)
            .map_err(|message| source_error(self.name(), message))?;
        let master = self
            .session
            .rows(DictQuery::SqliteMaster, &[Value::Text(table.to_string())])?;
        let Some(object) = master.first() else {
            return Err(source_error(
                self.name(),
                format!("object not found: {table}"),
            ));
        };
        let kind = cell_text(object, 0).unwrap_or_default();
        let object_type = if kind.eq_ignore_ascii_case("view") {
            "VIEW"
        } else if kind.eq_ignore_ascii_case("table") {
            "TABLE"
        } else {
            return Err(source_error(
                self.name(),
                format!("ontology pull supports tables and views, found {kind}"),
            ));
        };
        let pragma = self.session.read_pragma(table)?;
        let mut columns = Vec::with_capacity(pragma.len());
        let mut keyed = Vec::new();
        for row in &pragma {
            let name = cell_text(row, 1).ok_or_else(|| {
                source_error(
                    self.name(),
                    "pragma table_xinfo returned no column name".to_string(),
                )
            })?;
            let data_type = cell_text(row, 2).unwrap_or_else(|| "NONE".to_string());
            let notnull = cell_i64(row, 3).map_err(|message| source_error(self.name(), message))?;
            let pk = cell_i64(row, 5).map_err(|message| source_error(self.name(), message))?;
            let hidden = cell_i64(row, 6).map_err(|message| source_error(self.name(), message))?;
            if let Some(position) = pk.filter(|position| *position > 0) {
                keyed.push((position, name.clone()));
            }
            let (data_length, data_precision, data_scale) = sqlite_type_numbers(&data_type);
            columns.push(CatalogColumn {
                name,
                data_type,
                data_length,
                data_precision,
                data_scale,
                nullable: notnull.unwrap_or(0) == 0,
                virtual_column: matches!(hidden, Some(2) | Some(3)),
                db_comment: None,
            });
        }
        keyed.sort_by_key(|(position, _)| *position);
        let foreign_keys = self.sqlite_foreign_keys(table)?;
        Ok(CatalogTable {
            object_type: object_type.to_string(),
            table_comment: None,
            primary_key: keyed.into_iter().map(|(_, name)| name).collect(),
            columns,
            foreign_keys,
        })
    }

    fn check_relation(
        &self,
        from_owner: &str,
        from_table: &str,
        from_columns: &[String],
        to_owner: &str,
        to_table: &str,
        to_columns: &[String],
    ) -> Result<crate::source::OverlapCounts, DipperError> {
        if !from_owner.eq_ignore_ascii_case("main") || !to_owner.eq_ignore_ascii_case("main") {
            return Err(source_error(
                self.name(),
                "schema prefix is not allowed; only main is allowed".to_string(),
            ));
        }
        run_overlap(
            self.name(),
            SqlDialect::Sqlite,
            &self.session,
            from_table,
            from_columns,
            to_table,
            to_columns,
        )
    }
}

fn run_overlap(
    name: &str,
    dialect: SqlDialect,
    session: &Session<LoggingExecutor<SqliteExec>>,
    from_table: &str,
    from_columns: &[String],
    to_table: &str,
    to_columns: &[String],
) -> Result<crate::source::OverlapCounts, DipperError> {
    for column in from_columns.iter().chain(to_columns) {
        crate::sql::validate_column_ident(column).map_err(|message| source_error(name, message))?;
    }
    crate::sql::validate_column_ident(from_table).map_err(|message| source_error(name, message))?;
    crate::sql::validate_column_ident(to_table).map_err(|message| source_error(name, message))?;
    if from_columns.len() != to_columns.len() {
        return Err(source_error(
            name,
            "from and to column counts must match".to_string(),
        ));
    }
    let source_sql = non_null_count(dialect, from_table, from_columns)?;
    let target_sql = non_null_count(dialect, to_table, to_columns)?;
    let matched =
        crate::sql::build::overlap_counts(dialect, from_table, from_columns, to_table, to_columns)?;
    let source_sql = GuardedSql::from(source_sql);
    let target_sql = GuardedSql::from(target_sql);
    let matched = GuardedSql::from(matched);
    Ok(crate::source::OverlapCounts {
        source_non_null_rows: session.count_sql(&source_sql)?,
        target_non_null_rows: session.count_sql(&target_sql)?,
        matched_rows: session.count_sql(&matched)?,
    })
}

fn non_null_count(
    dialect: SqlDialect,
    table: &str,
    columns: &[String],
) -> Result<GuardedSelect, DipperError> {
    let filters = columns
        .iter()
        .map(|column| crate::config::Filter {
            column: column.clone(),
            op: crate::config::FilterOp::NotNull,
            value: None,
        })
        .collect();
    let spec = SelectSpec {
        table: table.to_string(),
        columns: vec![columns[0].clone()],
        filters,
        limit: None,
        batch_rows: 1,
        key: None,
        paging: Paging::Snapshot,
    };
    Ok(crate::sql::build::count(dialect, &spec)?.0)
}

impl SqliteSource {
    fn sqlite_foreign_keys(&self, table: &str) -> Result<Vec<CatalogForeignKey>, DipperError> {
        let rows = self.session.read_foreign_keys(table)?;
        let mut groups: std::collections::BTreeMap<i64, Vec<(i64, String, String, String)>> =
            std::collections::BTreeMap::new();
        for row in &rows {
            let id = cell_i64(row, 0)
                .map_err(|message| source_error(self.name(), message))?
                .unwrap_or(0);
            let seq = cell_i64(row, 1)
                .map_err(|message| source_error(self.name(), message))?
                .unwrap_or(0);
            let target_table = cell_text(row, 2).unwrap_or_default();
            let from = cell_text(row, 3).unwrap_or_default();
            let to = cell_text(row, 4).unwrap_or_default();
            if target_table.is_empty() || from.is_empty() || to.is_empty() {
                continue;
            }
            groups
                .entry(id)
                .or_default()
                .push((seq, from, target_table, to));
        }
        let mut foreign_keys = Vec::new();
        for (id, mut parts) in groups {
            parts.sort_by_key(|(seq, _, _, _)| *seq);
            let target_table = parts[0].2.clone();
            foreign_keys.push(CatalogForeignKey {
                name: Some(format!("fk_{id}")),
                columns: parts.iter().map(|(_, from, _, _)| from.clone()).collect(),
                target_owner: "main".to_string(),
                target_table,
                target_columns: parts.into_iter().map(|(_, _, _, to)| to).collect(),
            });
        }
        Ok(foreign_keys)
    }
}

fn sqlite_type_numbers(data_type: &str) -> (Option<i64>, Option<i64>, Option<i64>) {
    let upper = data_type.to_ascii_uppercase();
    let Some(open) = upper.find('(') else {
        return (None, None, None);
    };
    let Some(close) = upper.find(')') else {
        return (None, None, None);
    };
    if close < open {
        return (None, None, None);
    }
    let mut numbers = upper[open + 1..close]
        .split(',')
        .filter_map(|part| part.trim().parse::<i64>().ok());
    let first = numbers.next();
    let second = numbers.next();
    if upper.starts_with("CHAR") || upper.starts_with("VARCHAR") || upper.starts_with("NCHAR") {
        (first, None, None)
    } else if upper.starts_with("NUM") || upper.starts_with("DEC") || upper.starts_with("FLOAT") {
        (None, first, second)
    } else {
        (first, second, None)
    }
}

fn blocked(name: &str, report: &PreflightReport) -> DipperError {
    if let Some(finding) = report
        .findings
        .iter()
        .find(|finding| finding.level == crate::source::Level::Refuse)
    {
        return DipperError::Refused {
            source_name: name.to_string(),
            dip: finding.dip.clone().unwrap_or_default(),
            message: finding.message.clone(),
        };
    }
    let warnings = report
        .findings
        .iter()
        .filter(|finding| finding.level == crate::source::Level::Warn)
        .count();
    DipperError::StrictReadonly {
        source_name: name.to_string(),
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::ErrorCode;

    fn connect(name: &str, path: &Path) -> SqliteSource {
        SqliteSource::connect(
            name,
            path,
            480,
            &OpenOptions {
                strict: false,
                log: crate::source::SqlLogSink::Off,
                action: "check".to_string(),
            },
        )
        .unwrap()
    }

    fn sample(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("ro.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute("CREATE TABLE ORDERS (ORDER_ID INTEGER, STATUS TEXT)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO ORDERS (ORDER_ID, STATUS) VALUES (1, 'PAID')",
            [],
        )
        .unwrap();
        drop(conn);
        path
    }

    #[test]
    fn pragma_catalog_rejects_bad_identifiers_inside_a_read_only_transaction() {
        let _lock = crate::source::session::test_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = sample(dir.path());
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let src = SqliteSource::connect(
            "t",
            &path,
            480,
            &OpenOptions {
                strict: false,
                log: crate::source::SqlLogSink::Memory(Arc::clone(&log)),
                action: "ontology".to_string(),
            },
        )
        .unwrap();
        let err = src.catalog("main", "bad name").unwrap_err();
        assert!(err.to_string().contains("invalid identifier"), "{err}");
        let recorded = log.lock().unwrap().clone();
        assert!(
            recorded.iter().all(|sql| !sql.contains("table_xinfo")),
            "{recorded:?}"
        );
        let before = recorded.len();
        src.catalog("main", "ORDERS").unwrap();
        let recorded = log.lock().unwrap().clone();
        let slice = &recorded[before..];
        let pragma_at = slice
            .iter()
            .position(|sql| sql == "PRAGMA table_xinfo(\"ORDERS\")")
            .expect("pragma was not logged");
        assert!(pragma_at > 0, "{slice:?}");
        assert_eq!(slice[pragma_at - 1], "BEGIN");
        assert_eq!(slice[pragma_at + 1], "ROLLBACK");
        assert!(src.connection().is_autocommit());
        let err = src
            .session
            .executor()
            .inner()
            .read_fixed("SELECT 1")
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("fixed catalog read is not supported"),
            "{err}"
        );
        let count: i64 = src
            .connection()
            .query_row("SELECT COUNT(*) FROM ORDERS", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn catalog_accepts_main_owner_ignoring_case() {
        let _lock = crate::source::session::test_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = sample(dir.path());
        let src = connect("t", &path);
        let catalog = src.catalog("MAIN", "ORDERS").unwrap();
        assert_eq!(catalog.object_type, "TABLE");
        assert!(catalog
            .columns
            .iter()
            .any(|column| column.name == "ORDER_ID"));
    }

    #[test]
    fn check_relation_rejects_non_main_schema() {
        let _lock = crate::source::session::test_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = sample(dir.path());
        let src = connect("t", &path);
        let columns = vec!["ORDER_ID".to_string()];
        let err = src
            .check_relation("other", "ORDERS", &columns, "main", "ORDERS", &columns)
            .unwrap_err();
        assert!(err.to_string().contains("only main"), "{err}");
        let err = src
            .check_relation("main", "ORDERS", &columns, "TEMP", "ORDERS", &columns)
            .unwrap_err();
        assert!(err.to_string().contains("only main"), "{err}");
    }

    #[test]
    fn writes_fail_with_readonly() {
        let _lock = crate::source::session::test_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = sample(dir.path());
        let src = SqliteSource::connect(
            "t",
            &path,
            480,
            &OpenOptions {
                strict: false,
                log: crate::source::SqlLogSink::Off,
                action: "check".to_string(),
            },
        )
        .unwrap();
        let create_err = src
            .connection()
            .execute("CREATE TABLE u (id INTEGER)", [])
            .unwrap_err();
        assert_write_blocked(create_err);
        let insert_err = src
            .connection()
            .execute("INSERT INTO ORDERS (ORDER_ID, STATUS) VALUES (2, 'NO')", [])
            .unwrap_err();
        assert_write_blocked(insert_err);
    }

    fn assert_write_blocked(err: rusqlite::Error) {
        assert!(
            matches!(
                err.sqlite_error_code(),
                Some(ErrorCode::ReadOnly | ErrorCode::AuthorizationForStatementDenied)
            ),
            "write should fail read-only or auth-denied, got {err:?}"
        );
    }

    #[test]
    fn missing_file_is_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.db");
        let Err(err) = SqliteSource::connect(
            "missing",
            &path,
            480,
            &OpenOptions {
                strict: false,
                log: crate::source::SqlLogSink::Off,
                action: "check".to_string(),
            },
        ) else {
            panic!("missing sqlite file was opened");
        };
        let message = err.to_string();
        assert!(message.contains("cannot open sqlite file"), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(!path.exists());
    }

    #[test]
    fn query_only_is_on_and_extra_sql_is_rejected() {
        let _lock = crate::source::session::test_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = sample(dir.path());
        let src = connect("t", &path);
        let mode: i64 = src
            .connection()
            .query_row("PRAGMA query_only", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, 1);
        assert!(src
            .connection()
            .execute("INSERT INTO ORDERS (ORDER_ID, STATUS) VALUES (3, 'X')", [])
            .is_err());
        assert!(src
            .connection()
            .execute("CREATE TABLE extra (id INTEGER)", [])
            .is_err());
        let attached = dir.path().join("x.db");
        let attach = format!("ATTACH '{}' AS x", attached.display());
        assert!(src.connection().execute(&attach, []).is_err());
        assert!(!attached.exists(), "ATTACH created {}", attached.display());
        assert!(src
            .connection()
            .query_row("SELECT upper(STATUS) FROM ORDERS", [], |row| {
                row.get::<_, String>(0)
            })
            .is_err());
        assert!(src
            .connection()
            .execute("PRAGMA query_only = OFF", [])
            .is_err());
    }
}
