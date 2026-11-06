use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
use rusqlite::limits::Limit;
use rusqlite::types::{ToSql, ToSqlOutput, ValueRef};
use rusqlite::{config::DbConfig, Connection, OpenFlags, Row};

use crate::error::DipperError;
use crate::source::logging::LoggingExecutor;
use crate::source::preflight::{blocks, run_sqlite};
use crate::source::session::{Executor, RowCursor, Session, SessionOptions, SessionStatement};
use crate::source::watchdog::{Breaker, SystemClock};
use crate::source::{
    session_deadline, ChunkIter, OpenOptions, Opened, Paging, PreflightReport, SelectSpec, Source,
    Value,
};
use crate::sql::build;
use crate::sql::guard::{GuardedSelect, SqlDialect};

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
        _fetch_rows: usize,
    ) -> Result<Box<dyn RowCursor + 'a>, DipperError> {
        let mut statement = self
            .conn
            .prepare(query.sql())
            .map_err(|err| self.fail(err))?;
        let columns: Vec<String> = statement.column_names().map(str::to_string).collect();
        let params: Vec<Bound> = binds.iter().cloned().map(Bound).collect();
        let mut rows = statement
            .query(rusqlite::params_from_iter(params.iter()))
            .map_err(|err| self.fail(err))?;
        let mut stored = Vec::new();
        while let Some(row) = rows.next().map_err(|err| self.fail(err))? {
            stored.push(read_row(row, columns.len()).map_err(|err| self.fail(err))?);
        }
        Ok(Box::new(VecCursor {
            columns,
            rows: stored.into_iter(),
        }))
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

fn authorize(ctx: AuthContext<'_>) -> Authorization {
    match ctx.action {
        AuthAction::Select => Authorization::Allow,
        AuthAction::Read { .. } => Authorization::Allow,
        // DML and DDL still fail because the file is opened read-only and
        // `query_only` is on. Allowing them here lets SQLite report
        // SQLITE_READONLY instead of hiding that result behind the authorizer.
        AuthAction::Insert { .. }
        | AuthAction::Update { .. }
        | AuthAction::Delete { .. }
        | AuthAction::CreateTable { .. }
        | AuthAction::CreateIndex { .. }
        | AuthAction::DropTable { .. }
        | AuthAction::DropIndex { .. } => Authorization::Allow,
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
        _ => Authorization::Deny,
    }
}

struct VecCursor {
    columns: Vec<String>,
    rows: std::vec::IntoIter<Vec<Value>>,
}

impl RowCursor for VecCursor {
    fn columns(&self) -> &[String] {
        &self.columns
    }

    fn next_row(&mut self) -> Result<Option<Vec<Value>>, DipperError> {
        Ok(self.rows.next())
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
        assert_eq!(create_err.sqlite_error_code(), Some(ErrorCode::ReadOnly));
        let insert_err = src
            .connection()
            .execute("INSERT INTO ORDERS (ORDER_ID, STATUS) VALUES (2, 'NO')", [])
            .unwrap_err();
        assert_eq!(insert_err.sqlite_error_code(), Some(ErrorCode::ReadOnly));
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
