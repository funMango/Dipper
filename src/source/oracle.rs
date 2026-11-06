use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDateTime;

use crate::config::Secret;
use crate::error::DipperError;
use crate::source::logging::LoggingExecutor;
use crate::source::preflight::blocks;
use crate::source::session::{Executor, RowCursor, Session, SessionOptions, SessionStatement};
use crate::source::watchdog::{Breaker, SystemClock};
use crate::source::{
    session_deadline, ChunkIter, OpenOptions, Opened, Paging, PreflightReport, SelectSpec, Source,
    Value,
};
use crate::sql::build;
use crate::sql::guard::{GuardedSelect, SqlDialect};

pub(crate) struct OracleSource {
    session: Session<LoggingExecutor<OracleExec>>,
}

impl OracleSource {
    pub(crate) fn connect(
        name: &str,
        connect: &str,
        user: &str,
        password: &Secret,
        call_timeout_secs: u64,
        max_session_minutes: u64,
        opts: &OpenOptions,
    ) -> Result<Opened, DipperError> {
        let conn = match oracle::Connection::connect(user, password.expose(), connect) {
            Ok(conn) => conn,
            Err(err) => return Err(oracle_error(name, &err, Some(password))),
        };
        if conn.autocommit() {
            drop(conn);
            return Err(DipperError::Source {
                source_name: name.to_owned(),
                message: "autocommit is enabled; refusing the connection".to_owned(),
            });
        }
        let (conn, major) = prepare_session(name, conn, call_timeout_secs, opts)?;
        let conn = Arc::new(conn);
        let breaker = Arc::new(OracleBreaker(Arc::clone(&conn)));
        let exec = LoggingExecutor::new(
            OracleExec {
                name: name.to_owned(),
                conn,
                breaker,
                major,
            },
            opts.log.clone(),
        );
        let session = Session::start(
            exec,
            &SessionOptions {
                source_name: name.to_owned(),
                strict: opts.strict,
                deadline: session_deadline(max_session_minutes),
                clock: Arc::new(SystemClock),
                tick: Duration::from_secs(1),
            },
        )?;
        let findings = session.findings().to_vec();
        Ok(Opened {
            source: Box::new(Self { session }),
            findings,
        })
    }
}

fn prepare_session(
    name: &str,
    conn: oracle::Connection,
    call_timeout_secs: u64,
    opts: &OpenOptions,
) -> Result<(oracle::Connection, i32), DipperError> {
    conn.set_module("Dipper")
        .map_err(|err| oracle_error(name, &err, None))?;
    conn.set_action(&opts.action)
        .map_err(|err| oracle_error(name, &err, None))?;
    conn.set_client_identifier("Dipper")
        .map_err(|err| oracle_error(name, &err, None))?;
    conn.set_call_timeout(Some(Duration::from_secs(call_timeout_secs)))
        .map_err(|err| oracle_error(name, &err, None))?;
    let major = conn
        .server_version()
        .map_err(|err| oracle_error(name, &err, None))?
        .0
        .major();
    Ok((conn, major))
}

struct OracleBreaker(Arc<oracle::Connection>);

impl Breaker for OracleBreaker {
    fn interrupt(&self) {
        let _ = self.0.break_execution();
    }
}

struct OracleExec {
    name: String,
    conn: Arc<oracle::Connection>,
    breaker: Arc<OracleBreaker>,
    major: i32,
}

impl OracleExec {
    fn fail(&self, err: oracle::Error) -> DipperError {
        oracle_error(&self.name, &err, None)
    }
}

impl Executor for OracleExec {
    fn dialect(&self) -> SqlDialect {
        SqlDialect::Oracle
    }

    fn server_major(&self) -> Option<i32> {
        Some(self.major)
    }

    fn run_session(&self, stmt: SessionStatement) -> Result<(), DipperError> {
        match stmt {
            SessionStatement::Rollback => self.conn.rollback().map_err(|err| self.fail(err)),
            SessionStatement::OraDisableParallelQuery
            | SessionStatement::OraNlsSortBinary
            | SessionStatement::OraNlsCompBinary
            | SessionStatement::OraReadOnlySession
            | SessionStatement::OraSetTransactionReadOnly => self
                .conn
                .execute(stmt.sql(), &[] as &[&dyn oracle::sql_type::ToSql])
                .map(|_| ())
                .map_err(|err| self.fail(err)),
            SessionStatement::SqliteQueryOnly | SessionStatement::SqliteBegin => {
                Err(DipperError::Source {
                    source_name: self.name.clone(),
                    message: "session statement is not valid for oracle".to_owned(),
                })
            }
        }
    }

    fn open_cursor<'a>(
        &'a self,
        query: &GuardedSelect,
        binds: &[Value],
        fetch_rows: usize,
    ) -> Result<Box<dyn RowCursor + 'a>, DipperError> {
        let size = u32::try_from(fetch_rows.max(1)).unwrap_or(u32::MAX);
        let statement = self
            .conn
            .statement(query.sql())
            .fetch_array_size(size)
            .prefetch_rows(size)
            .build()
            .map_err(|err| self.fail(err))?;
        if statement.statement_type() != oracle::StatementType::Select {
            return Err(DipperError::Source {
                source_name: self.name.clone(),
                message: "prepared statement is not a SELECT".to_owned(),
            });
        }
        let owned = bind_values(binds);
        let params: Vec<&dyn oracle::sql_type::ToSql> =
            owned.iter().map(|value| value.as_ref()).collect();
        let statement = Box::new(statement);
        let statement_ptr =
            statement.as_ref() as *const oracle::Statement as *mut oracle::Statement;
        // SAFETY: `statement` stays on the heap for as long as `rows` borrows it.
        let rows = unsafe { (*statement_ptr).query(&params) }.map_err(|err| self.fail(err))?;
        let columns: Vec<String> = rows
            .column_info()
            .iter()
            .map(|info| info.name().to_owned())
            .collect();
        let types: Vec<oracle::sql_type::OracleType> = rows
            .column_info()
            .iter()
            .map(|info| info.oracle_type().clone())
            .collect();
        Ok(Box::new(OracleStream {
            _binds: owned,
            statement,
            rows: Box::into_raw(Box::new(rows)).cast(),
            columns,
            types,
            name: self.name.clone(),
        }))
    }

    fn breaker(&self) -> Arc<dyn Breaker> {
        Arc::clone(&self.breaker) as Arc<dyn Breaker>
    }

    fn close(&self) -> Result<(), DipperError> {
        self.conn.rollback().map_err(|err| self.fail(err))
    }
}

fn bind_values(binds: &[Value]) -> Vec<Box<dyn oracle::sql_type::ToSql>> {
    binds
        .iter()
        .map(|value| -> Box<dyn oracle::sql_type::ToSql> {
            match value {
                Value::Null => Box::new(None::<i64>),
                Value::Int(value) => Box::new(*value),
                Value::Float(value) => Box::new(*value),
                Value::Text(value) => Box::new(value.clone()),
                Value::Date(value) => Box::new(format!("{}", value.format("%Y-%m-%d"))),
                Value::Timestamp(value) => {
                    Box::new(format!("{}", value.format("%Y-%m-%d %H:%M:%S%.f")))
                }
            }
        })
        .collect()
}

fn read_cell(
    row: &oracle::Row,
    index: usize,
    oracle_type: &oracle::sql_type::OracleType,
) -> Result<Value, oracle::Error> {
    let cell = &row.sql_values()[index];
    if cell.is_null()? {
        return Ok(Value::Null);
    }
    use oracle::sql_type::OracleType;
    match oracle_type {
        OracleType::Number(precision, scale)
            if *precision > 0 && *precision <= 18 && *scale == 0 =>
        {
            Ok(Value::Int(cell.get::<i64>()?))
        }
        OracleType::Number(precision, scale) if *precision > 0 && *scale > 0 => {
            Ok(Value::Float(cell.get::<f64>()?))
        }
        OracleType::Number(0, _) => Ok(Value::Text(cell.get::<String>()?)),
        OracleType::Float(_) | OracleType::BinaryFloat | OracleType::BinaryDouble => {
            Ok(Value::Float(cell.get::<f64>()?))
        }
        OracleType::Date | OracleType::Timestamp(_) => {
            Ok(parse_oracle_timestamp(&cell.get::<String>()?))
        }
        OracleType::Varchar2(_)
        | OracleType::NVarchar2(_)
        | OracleType::Char(_)
        | OracleType::NChar(_) => Ok(Value::Text(cell.get::<String>()?)),
        _ => Ok(Value::Text(cell.get::<String>()?)),
    }
}

fn parse_oracle_timestamp(text: &str) -> Value {
    for format in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d"] {
        if let Ok(timestamp) = NaiveDateTime::parse_from_str(text, format) {
            return Value::Timestamp(timestamp);
        }
    }
    Value::Text(text.to_owned())
}

struct OracleStream {
    _binds: Vec<Box<dyn oracle::sql_type::ToSql>>,
    /// Keeps the statement alive for `rows`.
    #[allow(dead_code)]
    statement: Box<oracle::Statement>,
    rows: *mut oracle::ResultSet<'static, oracle::Row>,
    columns: Vec<String>,
    types: Vec<oracle::sql_type::OracleType>,
    name: String,
}

impl Drop for OracleStream {
    fn drop(&mut self) {
        if !self.rows.is_null() {
            // SAFETY: created with `Box::into_raw`; the statement it borrows is still alive.
            unsafe { drop(Box::from_raw(self.rows)) };
            self.rows = std::ptr::null_mut();
        }
    }
}

impl RowCursor for OracleStream {
    fn columns(&self) -> &[String] {
        &self.columns
    }

    fn next_row(&mut self) -> Result<Option<Vec<Value>>, DipperError> {
        let rows = unsafe { &mut *self.rows };
        match rows.next() {
            Some(Ok(row)) => {
                let mut values = Vec::with_capacity(self.columns.len());
                for (index, oracle_type) in self.types.iter().enumerate() {
                    values.push(
                        read_cell(&row, index, oracle_type)
                            .map_err(|err| oracle_error(&self.name, &err, None))?,
                    );
                }
                Ok(Some(values))
            }
            Some(Err(err)) => Err(oracle_error(&self.name, &err, None)),
            None => Ok(None),
        }
    }
}

impl Source for OracleSource {
    fn name(&self) -> &str {
        self.session.source_name()
    }

    fn kind(&self) -> &'static str {
        "oracle"
    }

    fn test_connection(&self) -> Result<(), DipperError> {
        let query = GuardedSelect::parse("SELECT 1 FROM DUAL", SqlDialect::Oracle)?;
        self.session.read_all(&query, &[])?;
        Ok(())
    }

    fn preflight(&self, dip: &str, spec: &SelectSpec) -> Result<PreflightReport, DipperError> {
        Ok(crate::source::preflight::run_oracle(
            &self.session,
            self.session.server_major().unwrap_or(0),
            spec,
            dip,
        ))
    }

    fn count(&self, spec: &SelectSpec) -> Result<u64, DipperError> {
        let (query, binds) = build::count(SqlDialect::Oracle, spec)?;
        let rows = self.session.read_all(&query, &binds)?;
        match rows.first().and_then(|row| row.first()) {
            Some(Value::Int(value)) if *value >= 0 => Ok(*value as u64),
            Some(Value::Null) | None => Ok(0),
            Some(_) => Err(DipperError::Source {
                source_name: self.name().to_owned(),
                message: "COUNT(*) returned an unexpected value".to_owned(),
            }),
        }
    }

    fn select<'a>(&'a self, spec: &SelectSpec) -> Result<ChunkIter<'a>, DipperError> {
        let report = self.preflight(&spec.table, spec)?;
        if blocks(&report, self.session.strict()) {
            return Err(DipperError::Source {
                source_name: self.name().to_owned(),
                message: "preflight blocked the read".to_owned(),
            });
        }
        let plan = match spec.paging {
            Paging::Snapshot => build::snapshot(SqlDialect::Oracle, spec)?,
            Paging::Keyset => {
                let key = spec
                    .key
                    .clone()
                    .or_else(|| report.key.map(|key| key.column))
                    .ok_or_else(|| DipperError::Source {
                        source_name: self.name().to_owned(),
                        message: "no key; set key or paging = \"snapshot\"".to_owned(),
                    })?;
                build::keyset(SqlDialect::Oracle, spec, &key)?
            }
        };
        Ok(self.session.chunks(plan))
    }
}

fn oracle_error(name: &str, err: &oracle::Error, secret: Option<&Secret>) -> DipperError {
    let text = crate::source::redact_driver_message(&err.to_string(), secret.map(Secret::expose));
    let message = if text.contains("DPI-1047") {
        format!("{text}; Oracle Instant Client not found; see docs/ORACLE_SETUP.md")
    } else {
        text
    };
    DipperError::Source {
        source_name: name.to_owned(),
        message,
    }
}
