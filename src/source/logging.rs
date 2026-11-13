use std::sync::{Arc, Mutex};

use crate::error::DipperError;
use crate::source::session::{Executor, RowCursor, SessionStatement};
use crate::source::watchdog::Breaker;
use crate::source::Value;
use crate::sql::guard::{GuardedSelect, SqlDialect};

/// Recorded statements. Bind values are never stored.
pub type SqlLog = Arc<Mutex<Vec<String>>>;

/// Where each statement sent to a source is copied.
#[derive(Clone, Debug)]
pub enum SqlLogSink {
    Off,
    Stderr,
    Memory(SqlLog),
}

/// Writes each session statement and guarded SELECT, then forwards the call.
pub(crate) struct LoggingExecutor<E: Executor> {
    inner: E,
    sink: SqlLogSink,
}

impl<E: Executor> LoggingExecutor<E> {
    pub(crate) fn new(inner: E, sink: SqlLogSink) -> Self {
        Self { inner, sink }
    }

    #[cfg(test)]
    pub(crate) fn inner(&self) -> &E {
        &self.inner
    }

    fn record(&self, sql: &str) {
        match &self.sink {
            SqlLogSink::Off => {}
            SqlLogSink::Stderr => eprintln!("sql: {sql}"),
            SqlLogSink::Memory(log) => {
                log.lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .push(sql.to_string());
            }
        }
    }
}

impl<E: Executor> Executor for LoggingExecutor<E> {
    fn dialect(&self) -> SqlDialect {
        self.inner.dialect()
    }

    fn server_major(&self) -> Option<i32> {
        self.inner.server_major()
    }

    fn run_session(&self, stmt: SessionStatement) -> Result<(), DipperError> {
        self.record(stmt.sql());
        self.inner.run_session(stmt)
    }

    fn open_sql<'a>(
        &'a self,
        sql: &str,
        binds: &[crate::source::Value],
        fetch_rows: usize,
    ) -> Result<Box<dyn crate::source::session::RowCursor + 'a>, DipperError> {
        self.record(sql);
        self.inner.open_sql(sql, binds, fetch_rows)
    }

    fn open_cursor<'a>(
        &'a self,
        query: &GuardedSelect,
        binds: &[Value],
        fetch_rows: usize,
    ) -> Result<Box<dyn RowCursor + 'a>, DipperError> {
        self.record(query.sql());
        self.inner.open_cursor(query, binds, fetch_rows)
    }

    fn read_fixed(&self, sql: &str) -> Result<Vec<Vec<Value>>, DipperError> {
        self.record(sql);
        self.inner.read_fixed(sql)
    }

    fn breaker(&self) -> Arc<dyn Breaker> {
        self.inner.breaker()
    }

    fn close(&self) -> Result<(), DipperError> {
        if self.dialect() == SqlDialect::Oracle {
            self.record(SessionStatement::Rollback.sql());
        }
        self.inner.close()
    }
}
