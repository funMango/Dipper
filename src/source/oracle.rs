use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, NaiveDateTime, Timelike};

use crate::config::Secret;
use crate::error::DipperError;
use crate::source::logging::LoggingExecutor;
use crate::source::preflight::{blocks, DictQuery, Dictionary};
use crate::source::session::{Executor, RowCursor, Session, SessionOptions, SessionStatement};
use crate::source::watchdog::{Breaker, SystemClock};
use crate::source::{
    cell_i64, cell_text, session_deadline, CatalogColumn, CatalogTable, ChunkIter, OpenOptions,
    Opened, Paging, PreflightReport, SelectSpec, Source, Value,
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
        let owned = bind_values(binds).map_err(|message| DipperError::Source {
            source_name: self.name.clone(),
            message,
        })?;
        let params: Vec<&dyn oracle::sql_type::ToSql> =
            owned.iter().map(|value| value.as_ref()).collect();
        let rows: oracle::ResultSet<'static, oracle::Row> = statement
            .into_result_set(&params)
            .map_err(|err| self.fail(err))?;
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
            rows,
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

/// One bound parameter. Dates and timestamps stay native so Oracle does not
/// parse them with `NLS_DATE_FORMAT`.
enum Bound {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Timestamp(oracle::sql_type::Timestamp),
}

impl oracle::sql_type::ToSql for Bound {
    fn oratype(&self, conn: &oracle::Connection) -> oracle::Result<oracle::sql_type::OracleType> {
        match self {
            Bound::Null => None::<i64>.oratype(conn),
            Bound::Int(value) => value.oratype(conn),
            Bound::Float(value) => value.oratype(conn),
            Bound::Text(value) => value.oratype(conn),
            // Default `Timestamp` oratype is `TIMESTAMP WITH TIME ZONE`. A naive
            // wall-clock value must stay `TIMESTAMP` so a DATE comparison does
            // not shift through the session time zone.
            Bound::Timestamp(_) => Ok(oracle::sql_type::OracleType::Timestamp(9)),
        }
    }

    fn to_sql(&self, val: &mut oracle::SqlValue<'_>) -> oracle::Result<()> {
        match self {
            Bound::Null => None::<i64>.to_sql(val),
            Bound::Int(value) => value.to_sql(val),
            Bound::Float(value) => value.to_sql(val),
            Bound::Text(value) => value.to_sql(val),
            Bound::Timestamp(value) => value.to_sql(val),
        }
    }
}

fn bind_values(binds: &[Value]) -> Result<Vec<Box<dyn oracle::sql_type::ToSql>>, String> {
    binds
        .iter()
        .map(|value| {
            bind_value(value).map(|bound| Box::new(bound) as Box<dyn oracle::sql_type::ToSql>)
        })
        .collect()
}

fn bind_value(value: &Value) -> Result<Bound, String> {
    let bound = match value {
        Value::Null => Bound::Null,
        Value::Int(value) => Bound::Int(*value),
        Value::Float(value) => Bound::Float(*value),
        Value::Text(value) => Bound::Text(value.clone()),
        Value::Date(value) => Bound::Timestamp(
            oracle::sql_type::Timestamp::new(value.year(), value.month(), value.day(), 0, 0, 0, 0)
                .map_err(|err| err.to_string())?,
        ),
        Value::Timestamp(value) => Bound::Timestamp(
            oracle::sql_type::Timestamp::new(
                value.year(),
                value.month(),
                value.day(),
                value.hour(),
                value.minute(),
                value.second(),
                value.nanosecond(),
            )
            .map_err(|err| err.to_string())?,
        ),
    };
    Ok(bound)
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
    rows: oracle::ResultSet<'static, oracle::Row>,
    columns: Vec<String>,
    types: Vec<oracle::sql_type::OracleType>,
    name: String,
}

impl RowCursor for OracleStream {
    fn columns(&self) -> &[String] {
        &self.columns
    }

    fn next_row(&mut self) -> Result<Option<Vec<Value>>, DipperError> {
        match self.rows.next() {
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
        count_from_value(rows.first().and_then(|row| row.first())).map_err(|message| {
            DipperError::Source {
                source_name: self.name().to_owned(),
                message,
            }
        })
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

    fn catalog(&self, owner: &str, table: &str) -> Result<CatalogTable, DipperError> {
        crate::sql::validate_column_ident(owner)
            .map_err(|message| message_error(self.name(), message))?;
        crate::sql::validate_column_ident(table)
            .map_err(|message| message_error(self.name(), message))?;
        let owner_bind = Value::Text(owner.to_string());
        let table_bind = Value::Text(table.to_string());
        let binds = [owner_bind.clone(), table_bind.clone()];
        let objects = self.session.rows(DictQuery::AllObjects, &binds)?;
        let kinds: Vec<String> = objects.iter().filter_map(|row| cell_text(row, 0)).collect();
        let object_type = if kinds.iter().any(|kind| kind == "TABLE") {
            "TABLE"
        } else if kinds.iter().any(|kind| kind == "VIEW") {
            "VIEW"
        } else if kinds.is_empty() {
            return Err(message_error(
                self.name(),
                format!("object not found: {owner}.{table}"),
            ));
        } else {
            return Err(message_error(
                self.name(),
                format!(
                    "ontology pull supports tables and views, found {}",
                    kinds.join(", ")
                ),
            ));
        };
        let column_rows = self.session.rows(DictQuery::AllTabColumns, &binds)?;
        let mut columns: Vec<(i64, CatalogColumn)> = Vec::with_capacity(column_rows.len());
        for row in &column_rows {
            let name = cell_text(row, 0)
                .ok_or_else(|| message_error(self.name(), "dictionary column has no name"))?;
            let data_type = cell_text(row, 1).unwrap_or_else(|| "UNKNOWN".to_string());
            let data_length =
                cell_i64(row, 2).map_err(|message| message_error(self.name(), message))?;
            let data_precision =
                cell_i64(row, 3).map_err(|message| message_error(self.name(), message))?;
            let data_scale =
                cell_i64(row, 4).map_err(|message| message_error(self.name(), message))?;
            let nullable = cell_text(row, 5).as_deref() != Some("N");
            let column_id = cell_i64(row, 6)
                .map_err(|message| message_error(self.name(), message))?
                .unwrap_or(i64::MAX);
            columns.push((
                column_id,
                CatalogColumn {
                    name,
                    data_type,
                    data_length,
                    data_precision,
                    data_scale,
                    nullable,
                    virtual_column: false,
                    db_comment: None,
                },
            ));
        }
        columns.sort_by_key(|(id, _)| *id);
        let mut columns: Vec<CatalogColumn> =
            columns.into_iter().map(|(_, column)| column).collect();
        // VIRTUAL_COLUMN exists on ALL_TAB_COLS in 19c, not on ALL_TAB_COLUMNS.
        let virtual_rows = self.session.rows(DictQuery::AllTabCols, &binds)?;
        for row in &virtual_rows {
            let Some(name) = cell_text(row, 0) else {
                continue;
            };
            let is_virtual = cell_text(row, 1).as_deref() == Some("YES");
            if let Some(column) = columns.iter_mut().find(|column| column.name == name) {
                column.virtual_column = is_virtual;
            }
        }
        let comments = self.session.rows(DictQuery::AllColComments, &binds)?;
        for row in &comments {
            let Some(name) = cell_text(row, 0) else {
                continue;
            };
            let comment = cell_text(row, 1);
            if let Some(column) = columns.iter_mut().find(|column| column.name == name) {
                column.db_comment = comment;
            }
        }
        let table_comment = self
            .session
            .rows(DictQuery::AllTabComments, &binds)?
            .first()
            .and_then(|row| cell_text(row, 0));
        let constraints = self.session.rows(
            DictQuery::AllConstraints,
            &[owner_bind, table_bind, Value::Text("ENABLED".to_string())],
        )?;
        let pk_names: Vec<String> = constraints
            .iter()
            .filter(|row| cell_text(row, 1).as_deref() == Some("P"))
            .filter_map(|row| cell_text(row, 0))
            .collect();
        let positions = self
            .session
            .rows(DictQuery::AllConsColumnPositions, &binds)?;
        let mut keyed = Vec::new();
        for row in &positions {
            let Some(constraint) = cell_text(row, 0) else {
                continue;
            };
            if !pk_names.iter().any(|name| name == &constraint) {
                continue;
            }
            let Some(name) = cell_text(row, 1) else {
                continue;
            };
            let position = cell_i64(row, 2)
                .map_err(|message| message_error(self.name(), message))?
                .unwrap_or(i64::MAX);
            keyed.push((position, name));
        }
        keyed.sort_by_key(|(position, _)| *position);
        Ok(CatalogTable {
            object_type: object_type.to_string(),
            table_comment,
            primary_key: keyed.into_iter().map(|(_, name)| name).collect(),
            columns,
        })
    }
}

/// Turn one `COUNT(*)` cell into a row count.
///
/// Unconstrained Oracle `NUMBER` arrives as [`Value::Text`]. Whole floats are
/// accepted. Anything else keeps the value in the error.
fn count_from_value(value: Option<&Value>) -> Result<u64, String> {
    match value {
        Some(Value::Int(number)) if *number >= 0 => Ok(*number as u64),
        Some(Value::Text(text)) => parse_count_text(text)
            .ok_or_else(|| format!("COUNT(*) returned an unexpected value: {value:?}")),
        Some(Value::Float(number)) => unsigned_whole_float(*number)
            .ok_or_else(|| format!("COUNT(*) returned an unexpected value: {value:?}")),
        Some(Value::Null) | None => Ok(0),
        Some(_) => Err(format!("COUNT(*) returned an unexpected value: {value:?}")),
    }
}

fn parse_count_text(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn unsigned_whole_float(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return None;
    }
    let whole = value as u64;
    if whole as f64 == value {
        Some(whole)
    } else {
        None
    }
}

fn message_error(name: &str, message: impl Into<String>) -> DipperError {
    DipperError::Source {
        source_name: name.to_string(),
        message: message.into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn count_accepts_text_whole_floats_and_ints() {
        assert_eq!(
            count_from_value(Some(&Value::Text("285".into()))).unwrap(),
            285
        );
        assert_eq!(count_from_value(Some(&Value::Text("0".into()))).unwrap(), 0);
        assert_eq!(count_from_value(Some(&Value::Float(285.0))).unwrap(), 285);
        assert_eq!(count_from_value(Some(&Value::Int(285))).unwrap(), 285);
        assert_eq!(count_from_value(Some(&Value::Null)).unwrap(), 0);
        assert_eq!(count_from_value(None).unwrap(), 0);
    }

    #[test]
    fn count_rejects_non_integers_and_shows_the_value() {
        for value in [
            Value::Text("x".into()),
            Value::Int(-1),
            Value::Float(-1.0),
            Value::Float(285.5),
        ] {
            let err = count_from_value(Some(&value)).unwrap_err();
            assert!(
                err.contains(&format!("{value:?}")),
                "error {err} should include {value:?}"
            );
        }
    }

    #[test]
    fn date_and_timestamp_binds_are_native_timestamps() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let timestamp = date.and_hms_opt(9, 0, 0).unwrap();
        for value in [Value::Date(date), Value::Timestamp(timestamp)] {
            let bound = bind_value(&value).unwrap();
            assert!(
                !matches!(bound, Bound::Text(_)),
                "{value:?} was bound as a character string"
            );
            let Bound::Timestamp(stamp) = bound else {
                panic!("{value:?} was not a native timestamp");
            };
            assert!(!stamp.with_tz(), "{value:?} included a time zone");
            assert_eq!(stamp.year(), 2026);
            assert_eq!(stamp.month(), 11);
            assert_eq!(stamp.day(), 1);
            let hour = if matches!(value, Value::Date(_)) {
                0
            } else {
                9
            };
            assert_eq!(stamp.hour(), hour);
        }
    }
}
