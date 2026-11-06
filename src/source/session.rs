use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::error::DipperError;
use crate::source::preflight::{parse_dict, DictQuery, Dictionary, Finding, Level};
use crate::source::watchdog::{Breaker, Clock, Watchdog};
use crate::source::{Chunk, ChunkIter, Value};
use crate::sql::build::ReadPlan;
use crate::sql::guard::{GuardedSelect, SqlDialect};

/// Fixed statements a source is allowed to run besides a [`GuardedSelect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatement {
    OraDisableParallelQuery,
    OraNlsSortBinary,
    OraNlsCompBinary,
    OraReadOnlySession,
    OraSetTransactionReadOnly,
    SqliteQueryOnly,
    SqliteBegin,
    Rollback,
}

impl SessionStatement {
    pub const ALL: [SessionStatement; 8] = [
        SessionStatement::OraDisableParallelQuery,
        SessionStatement::OraNlsSortBinary,
        SessionStatement::OraNlsCompBinary,
        SessionStatement::OraReadOnlySession,
        SessionStatement::OraSetTransactionReadOnly,
        SessionStatement::SqliteQueryOnly,
        SessionStatement::SqliteBegin,
        SessionStatement::Rollback,
    ];

    pub const fn sql(self) -> &'static str {
        match self {
            SessionStatement::OraDisableParallelQuery => "ALTER SESSION DISABLE PARALLEL QUERY",
            SessionStatement::OraNlsSortBinary => "ALTER SESSION SET NLS_SORT = BINARY",
            SessionStatement::OraNlsCompBinary => "ALTER SESSION SET NLS_COMP = BINARY",
            SessionStatement::OraReadOnlySession => "ALTER SESSION SET READ_ONLY = TRUE",
            SessionStatement::OraSetTransactionReadOnly => "SET TRANSACTION READ ONLY",
            SessionStatement::SqliteQueryOnly => "PRAGMA query_only = ON",
            SessionStatement::SqliteBegin => "BEGIN",
            SessionStatement::Rollback => "ROLLBACK",
        }
    }

    pub fn allowed(dialect: SqlDialect) -> &'static [SessionStatement] {
        match dialect {
            SqlDialect::Oracle => &ORACLE_ALLOWED,
            SqlDialect::Sqlite => &SQLITE_ALLOWED,
        }
    }
}

const ORACLE_ALLOWED: [SessionStatement; 6] = [
    SessionStatement::OraDisableParallelQuery,
    SessionStatement::OraNlsSortBinary,
    SessionStatement::OraNlsCompBinary,
    SessionStatement::OraReadOnlySession,
    SessionStatement::OraSetTransactionReadOnly,
    SessionStatement::Rollback,
];

const SQLITE_ALLOWED: [SessionStatement; 3] = [
    SessionStatement::SqliteQueryOnly,
    SessionStatement::SqliteBegin,
    SessionStatement::Rollback,
];

pub(crate) trait RowCursor {
    fn columns(&self) -> &[String];
    fn next_row(&mut self) -> Result<Option<Vec<Value>>, DipperError>;
}

pub(crate) trait Executor: Send {
    fn dialect(&self) -> SqlDialect;
    fn server_major(&self) -> Option<i32>;
    fn run_session(&self, stmt: SessionStatement) -> Result<(), DipperError>;
    fn open_cursor<'a>(
        &'a self,
        query: &GuardedSelect,
        binds: &[Value],
        fetch_rows: usize,
    ) -> Result<Box<dyn RowCursor + 'a>, DipperError>;
    fn breaker(&self) -> Arc<dyn Breaker>;
    fn close(&self) -> Result<(), DipperError>;
}

/// Process-wide lock: one source session at a time.
pub(crate) struct SessionPermit;

static SESSION_BUSY: AtomicBool = AtomicBool::new(false);

impl SessionPermit {
    fn acquire() -> Result<Self, DipperError> {
        if SESSION_BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(DipperError::SourceBusy("another source session is open"));
        }
        Ok(Self)
    }
}

impl Drop for SessionPermit {
    fn drop(&mut self) {
        SESSION_BUSY.store(false, Ordering::Release);
    }
}

/// One read-only transaction. `finish` and Drop both roll back, and only once.
pub(crate) struct ReadOnlyTxn<'a> {
    exec: &'a dyn Executor,
    open: bool,
}

impl<'a> ReadOnlyTxn<'a> {
    fn begin(exec: &'a dyn Executor) -> Result<Self, DipperError> {
        let stmt = match exec.dialect() {
            SqlDialect::Oracle => SessionStatement::OraSetTransactionReadOnly,
            SqlDialect::Sqlite => SessionStatement::SqliteBegin,
        };
        exec.run_session(stmt)?;
        Ok(Self { exec, open: true })
    }

    fn finish(mut self) -> Result<(), DipperError> {
        self.rollback()
    }

    fn rollback(&mut self) -> Result<(), DipperError> {
        if self.open {
            self.open = false;
            self.exec.run_session(SessionStatement::Rollback)?;
        }
        Ok(())
    }
}

impl Drop for ReadOnlyTxn<'_> {
    fn drop(&mut self) {
        if self.open {
            self.open = false;
            let _ = self.exec.run_session(SessionStatement::Rollback);
        }
    }
}

pub(crate) struct SessionOptions {
    pub source_name: String,
    pub strict: bool,
    pub deadline: Duration,
    pub clock: Arc<dyn Clock>,
    pub tick: Duration,
}

pub(crate) struct Session<E: Executor> {
    exec: E,
    _permit: SessionPermit,
    watchdog: Option<Watchdog>,
    findings: Vec<Finding>,
    strict: bool,
    source_name: String,
}

impl<E: Executor> Session<E> {
    /// Acquire the process permit, apply dialect setup, then start the watchdog.
    ///
    /// Oracle 23 and newer must accept `READ_ONLY = TRUE` or the session stops
    /// immediately, for both default and strict mode, and the connection is closed
    /// before any data SELECT.
    pub(crate) fn start(exec: E, opts: &SessionOptions) -> Result<Self, DipperError> {
        let permit = match SessionPermit::acquire() {
            Ok(permit) => permit,
            Err(err) => {
                let _ = exec.close();
                return Err(err);
            }
        };
        let findings = match apply_setup(&exec, opts) {
            Ok(findings) => findings,
            Err(err) => {
                let _ = exec.close();
                return Err(err);
            }
        };
        let watchdog = Watchdog::start(
            opts.deadline,
            Arc::clone(&opts.clock),
            exec.breaker(),
            opts.tick,
        );
        Ok(Self {
            exec,
            _permit: permit,
            watchdog: Some(watchdog),
            findings,
            strict: opts.strict,
            source_name: opts.source_name.clone(),
        })
    }

    pub(crate) fn findings(&self) -> &[Finding] {
        &self.findings
    }

    pub(crate) fn source_name(&self) -> &str {
        &self.source_name
    }

    #[cfg_attr(not(feature = "oracle"), allow(dead_code))]
    pub(crate) fn server_major(&self) -> Option<i32> {
        self.exec.server_major()
    }

    #[cfg(test)]
    pub(crate) fn executor(&self) -> &E {
        &self.exec
    }

    pub(crate) fn strict(&self) -> bool {
        self.strict
    }

    pub(crate) fn read_all(
        &self,
        query: &GuardedSelect,
        binds: &[Value],
    ) -> Result<Vec<Vec<Value>>, DipperError> {
        self.ensure_deadline()?;
        let txn = ReadOnlyTxn::begin(&self.exec)?;
        let read = (|| {
            let mut cursor = self.exec.open_cursor(query, binds, 1)?;
            let mut rows = Vec::new();
            while let Some(row) = cursor.next_row()? {
                rows.push(row);
            }
            Ok(rows)
        })();
        let rolled = txn.finish();
        match (read, rolled) {
            (Ok(rows), Ok(())) => Ok(rows),
            (Err(err), _) | (Ok(_), Err(err)) => Err(err),
        }
    }

    pub(crate) fn chunks<'a>(&'a self, plan: ReadPlan) -> ChunkIter<'a> {
        if plan.next.is_some() {
            Box::new(KeysetIter {
                session: self,
                plan,
                last_key: None,
                done: false,
            })
        } else {
            Box::new(SnapshotIter {
                session: self,
                plan,
                txn: None,
                cursor: None,
                columns: Vec::new(),
                done: false,
                produced: 0,
            })
        }
    }

    fn ensure_deadline(&self) -> Result<(), DipperError> {
        if self.watchdog.as_ref().is_some_and(Watchdog::expired) {
            let _ = self.exec.run_session(SessionStatement::Rollback);
            return Err(DipperError::DeadlineExceeded {
                source_name: self.source_name.clone(),
            });
        }
        Ok(())
    }
}

impl<E: Executor> Drop for Session<E> {
    fn drop(&mut self) {
        self.watchdog.take();
        let _ = self.exec.close();
    }
}

impl<E: Executor> Dictionary for Session<E> {
    fn rows(&self, query: DictQuery, binds: &[Value]) -> Result<Vec<Vec<Value>>, DipperError> {
        let guarded = parse_dict(query)?;
        self.read_all(&guarded, binds)
    }
}

fn apply_setup<E: Executor>(exec: &E, _opts: &SessionOptions) -> Result<Vec<Finding>, DipperError> {
    match exec.dialect() {
        SqlDialect::Oracle => {
            exec.run_session(SessionStatement::OraDisableParallelQuery)?;
            exec.run_session(SessionStatement::OraNlsSortBinary)?;
            exec.run_session(SessionStatement::OraNlsCompBinary)?;
            match exec.server_major() {
                Some(major) if major >= 23 => {
                    exec.run_session(SessionStatement::OraReadOnlySession)?;
                    Ok(Vec::new())
                }
                Some(major) => Ok(vec![Finding {
                    level: Level::Note,
                    code: "no-session-read-only",
                    dip: None,
                    message: format!("server {major} has no session READ_ONLY mode"),
                }]),
                None => Ok(Vec::new()),
            }
        }
        SqlDialect::Sqlite => {
            exec.run_session(SessionStatement::SqliteQueryOnly)?;
            Ok(Vec::new())
        }
    }
}

struct KeysetIter<'a, E: Executor> {
    session: &'a Session<E>,
    plan: ReadPlan,
    last_key: Option<Value>,
    done: bool,
}

impl<E: Executor> Iterator for KeysetIter<'_, E> {
    type Item = Result<Chunk, DipperError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if let Err(err) = self.session.ensure_deadline() {
            self.done = true;
            return Some(Err(err));
        }
        let size = chunk_size(self.plan.batch_rows, self.plan.limit);
        if size == 0 {
            self.done = true;
            return None;
        }
        let query = if self.last_key.is_none() {
            self.plan.first.clone()
        } else {
            match self.plan.next.clone() {
                Some(query) => query,
                None => {
                    self.done = true;
                    return Some(Err(DipperError::Source {
                        source_name: self.session.source_name.clone(),
                        message: "keyset plan is missing the next page".to_string(),
                    }));
                }
            }
        };
        let mut binds = self.plan.binds.clone();
        if let Some(key) = &self.last_key {
            binds.push(key.clone());
        }
        binds.push(Value::Int(i64::try_from(size).unwrap_or(i64::MAX)));
        let txn = match ReadOnlyTxn::begin(&self.session.exec) {
            Ok(txn) => txn,
            Err(err) => {
                self.done = true;
                return Some(Err(err));
            }
        };
        let read = read_rows(&self.session.exec, &query, &binds, size);
        let rolled = txn.finish();
        let (columns, rows) = match (read, rolled) {
            (Ok(read), Ok(())) => read,
            (Err(err), _) | (Ok(_), Err(err)) => {
                self.done = true;
                return Some(Err(err));
            }
        };
        if rows.is_empty() {
            self.done = true;
            return None;
        }
        let key_pos = self.plan.key_pos.unwrap_or(0);
        self.last_key = rows.last().and_then(|row| row.get(key_pos)).cloned();
        let (columns, rows) = strip_key(columns, rows, self.plan.strip_key, key_pos);
        if (rows.len() as u64) < size as u64 || self.reached_limit(rows.len()) {
            self.done = true;
        }
        Some(Ok(Chunk { columns, rows }))
    }
}

impl<E: Executor> KeysetIter<'_, E> {
    fn reached_limit(&mut self, rows: usize) -> bool {
        let Some(limit) = self.plan.limit.as_mut() else {
            return false;
        };
        *limit = limit.saturating_sub(rows as u64);
        *limit == 0
    }
}

struct SnapshotIter<'a, E: Executor> {
    session: &'a Session<E>,
    plan: ReadPlan,
    txn: Option<ReadOnlyTxn<'a>>,
    cursor: Option<Box<dyn RowCursor + 'a>>,
    columns: Vec<String>,
    done: bool,
    produced: u64,
}

impl<E: Executor> SnapshotIter<'_, E> {
    fn fail(&mut self, err: DipperError) -> DipperError {
        self.rollback();
        self.done = true;
        err
    }

    fn rollback(&mut self) {
        self.cursor.take();
        if let Some(txn) = self.txn.take() {
            let _ = txn.finish();
        }
    }
}

impl<E: Executor> Iterator for SnapshotIter<'_, E> {
    type Item = Result<Chunk, DipperError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self
            .session
            .watchdog
            .as_ref()
            .is_some_and(Watchdog::expired)
        {
            if self.txn.is_none() {
                let _ = self.session.exec.run_session(SessionStatement::Rollback);
            }
            let err = self.fail(DipperError::DeadlineExceeded {
                source_name: self.session.source_name.clone(),
            });
            return Some(Err(err));
        }
        if self.cursor.is_none() {
            let txn = match ReadOnlyTxn::begin(&self.session.exec) {
                Ok(txn) => txn,
                Err(err) => return Some(Err(self.fail(err))),
            };
            let mut binds = self.plan.binds.clone();
            if let Some(limit) = self.plan.limit {
                binds.push(Value::Int(i64::try_from(limit).unwrap_or(i64::MAX)));
            }
            match self.session.exec.open_cursor(
                &self.plan.first,
                &binds,
                self.plan.batch_rows.max(1),
            ) {
                Ok(cursor) => {
                    self.columns = cursor.columns().to_vec();
                    self.cursor = Some(cursor);
                    self.txn = Some(txn);
                }
                Err(err) => {
                    let _ = txn.finish();
                    self.done = true;
                    return Some(Err(err));
                }
            }
        }
        let size = chunk_size(
            self.plan.batch_rows,
            self.plan
                .limit
                .map(|limit| limit.saturating_sub(self.produced)),
        );
        if size == 0 {
            self.rollback();
            self.done = true;
            return None;
        }
        let mut rows = Vec::new();
        while rows.len() < size {
            let step = self
                .cursor
                .as_mut()
                .expect("snapshot cursor is open")
                .next_row();
            match step {
                Ok(Some(row)) => rows.push(row),
                Ok(None) => break,
                Err(err) => return Some(Err(self.fail(err))),
            }
        }
        if rows.is_empty() {
            self.rollback();
            self.done = true;
            return None;
        }
        self.produced += rows.len() as u64;
        let exhausted =
            rows.len() < size || self.plan.limit.is_some_and(|limit| self.produced >= limit);
        if exhausted {
            self.rollback();
            self.done = true;
        }
        Some(Ok(Chunk {
            columns: self.columns.clone(),
            rows,
        }))
    }
}

impl<E: Executor> Drop for SnapshotIter<'_, E> {
    fn drop(&mut self) {
        self.rollback();
    }
}

fn read_rows(
    exec: &dyn Executor,
    query: &GuardedSelect,
    binds: &[Value],
    limit: usize,
) -> Result<(Vec<String>, Vec<Vec<Value>>), DipperError> {
    let mut cursor = exec.open_cursor(query, binds, limit.max(1))?;
    let columns = cursor.columns().to_vec();
    let mut rows = Vec::new();
    while rows.len() < limit {
        match cursor.next_row()? {
            Some(row) => rows.push(row),
            None => break,
        }
    }
    Ok((columns, rows))
}

fn strip_key(
    mut columns: Vec<String>,
    mut rows: Vec<Vec<Value>>,
    strip: bool,
    key_pos: usize,
) -> (Vec<String>, Vec<Vec<Value>>) {
    if strip && key_pos < columns.len() {
        columns.remove(key_pos);
        for row in &mut rows {
            if key_pos < row.len() {
                row.remove(key_pos);
            }
        }
    }
    (columns, rows)
}

fn chunk_size(batch_rows: usize, remaining: Option<u64>) -> usize {
    match remaining {
        Some(left) => batch_rows.min(usize::try_from(left).unwrap_or(usize::MAX)),
        None => batch_rows,
    }
}

/// Serializes tests that open a real source. The process allows one session.
#[cfg(test)]
pub(crate) static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_SERIAL.lock().unwrap_or_else(|err| err.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::watchdog::SystemClock;
    use crate::sql::build;
    use crate::sql::guard::GuardedSelect;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    struct ManualClock {
        base: Instant,
        offset_ms: std::sync::atomic::AtomicU64,
    }

    impl ManualClock {
        fn new() -> Self {
            Self {
                base: Instant::now(),
                offset_ms: std::sync::atomic::AtomicU64::new(0),
            }
        }

        fn advance(&self, by: Duration) {
            self.offset_ms.fetch_add(
                u64::try_from(by.as_millis()).unwrap_or(u64::MAX),
                Ordering::SeqCst,
            );
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.offset_ms.load(Ordering::SeqCst))
        }
    }

    struct FakeCursor {
        columns: Vec<String>,
        rows: std::vec::IntoIter<Vec<Value>>,
        panic_on_row: bool,
    }

    impl RowCursor for FakeCursor {
        fn columns(&self) -> &[String] {
            &self.columns
        }

        fn next_row(&mut self) -> Result<Option<Vec<Value>>, DipperError> {
            if self.panic_on_row {
                panic!("cursor panic");
            }
            Ok(self.rows.next())
        }
    }

    struct FakeExec {
        dialect: SqlDialect,
        major: Option<i32>,
        log: Arc<Mutex<Vec<String>>>,
        fail_read_only: bool,
        autocommit: AtomicBool,
        selects: Arc<AtomicUsize>,
        fail_select_on: Option<usize>,
        panic_on_row: bool,
        rows_per_cursor: usize,
        closed: Arc<AtomicBool>,
    }

    impl FakeExec {
        fn oracle(major: i32) -> Self {
            Self {
                dialect: SqlDialect::Oracle,
                major: Some(major),
                log: Arc::new(Mutex::new(Vec::new())),
                fail_read_only: false,
                autocommit: AtomicBool::new(true),
                selects: Arc::new(AtomicUsize::new(0)),
                fail_select_on: None,
                panic_on_row: false,
                rows_per_cursor: 1,
                closed: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl Executor for FakeExec {
        fn dialect(&self) -> SqlDialect {
            self.dialect
        }

        fn server_major(&self) -> Option<i32> {
            self.major
        }

        fn run_session(&self, stmt: SessionStatement) -> Result<(), DipperError> {
            if stmt == SessionStatement::OraReadOnlySession && self.fail_read_only {
                return Err(DipperError::Source {
                    source_name: "fake".to_string(),
                    message: "READ_ONLY failed".to_string(),
                });
            }
            self.log
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .push(stmt.sql().to_string());
            match stmt {
                SessionStatement::SqliteBegin | SessionStatement::OraSetTransactionReadOnly => {
                    self.autocommit.store(false, Ordering::SeqCst);
                }
                SessionStatement::Rollback => {
                    self.autocommit.store(true, Ordering::SeqCst);
                }
                _ => {}
            }
            Ok(())
        }

        fn open_cursor<'a>(
            &'a self,
            query: &GuardedSelect,
            _binds: &[Value],
            _fetch_rows: usize,
        ) -> Result<Box<dyn RowCursor + 'a>, DipperError> {
            let nth = self.selects.fetch_add(1, Ordering::SeqCst) + 1;
            self.log
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .push(query.sql().to_string());
            if self.fail_select_on == Some(nth) {
                return Err(DipperError::Source {
                    source_name: "fake".to_string(),
                    message: "select failed".to_string(),
                });
            }
            let columns = vec!["K".to_string(), "A".to_string()];
            let rows = (0..self.rows_per_cursor)
                .map(|index| vec![Value::Int(index as i64 + 1), Value::Text("x".to_string())])
                .collect::<Vec<_>>();
            Ok(Box::new(FakeCursor {
                columns,
                rows: rows.into_iter(),
                panic_on_row: self.panic_on_row,
            }))
        }

        fn breaker(&self) -> Arc<dyn Breaker> {
            Arc::new(NoopBreaker)
        }

        fn close(&self) -> Result<(), DipperError> {
            self.closed.store(true, Ordering::SeqCst);
            let rollback = match self.dialect {
                SqlDialect::Oracle => true,
                SqlDialect::Sqlite => !self.autocommit.load(Ordering::SeqCst),
            };
            if rollback {
                self.log
                    .lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .push(SessionStatement::Rollback.sql().to_string());
                self.autocommit.store(true, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    struct NoopBreaker;

    impl Breaker for NoopBreaker {
        fn interrupt(&self) {}
    }

    fn must_err<T>(result: Result<T, DipperError>) -> DipperError {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(err) => err,
        }
    }

    fn options(clock: Arc<dyn Clock>, strict: bool) -> SessionOptions {
        SessionOptions {
            source_name: "company".to_string(),
            strict,
            deadline: Duration::from_secs(60),
            clock,
            tick: Duration::from_millis(50),
        }
    }

    fn keyset_plan(limit: u64, batch_rows: usize) -> ReadPlan {
        build::keyset(
            SqlDialect::Oracle,
            &crate::source::SelectSpec {
                table: "T".to_string(),
                columns: vec!["K".to_string(), "A".to_string()],
                filters: Vec::new(),
                limit: Some(limit),
                batch_rows,
                key: None,
                paging: crate::source::Paging::Keyset,
            },
            "K",
        )
        .unwrap()
    }

    #[test]
    fn statement_strings_match_the_allow_list() {
        let mut all: Vec<_> = SessionStatement::ALL
            .iter()
            .map(|stmt| stmt.sql())
            .collect();
        all.sort_unstable();
        let mut expected = vec![
            "ALTER SESSION DISABLE PARALLEL QUERY",
            "ALTER SESSION SET NLS_COMP = BINARY",
            "ALTER SESSION SET NLS_SORT = BINARY",
            "ALTER SESSION SET READ_ONLY = TRUE",
            "BEGIN",
            "PRAGMA query_only = ON",
            "ROLLBACK",
            "SET TRANSACTION READ ONLY",
        ];
        expected.sort_unstable();
        assert_eq!(all, expected);
        assert_eq!(SessionStatement::allowed(SqlDialect::Oracle).len(), 6);
        assert_eq!(SessionStatement::allowed(SqlDialect::Sqlite).len(), 3);
        let oracle: Vec<_> = SessionStatement::allowed(SqlDialect::Oracle)
            .iter()
            .map(|stmt| stmt.sql())
            .collect();
        assert!(oracle.contains(&"ALTER SESSION SET READ_ONLY = TRUE"));
        assert!(oracle.contains(&"ROLLBACK"));
        assert!(!oracle.contains(&"BEGIN"));
        let sqlite: Vec<_> = SessionStatement::allowed(SqlDialect::Sqlite)
            .iter()
            .map(|stmt| stmt.sql())
            .collect();
        assert_eq!(sqlite, ["PRAGMA query_only = ON", "BEGIN", "ROLLBACK"]);
    }

    #[test]
    fn oracle_19_notes_missing_read_only_and_oracle_23_sets_it() {
        let _lock = test_lock();
        let clock = Arc::new(SystemClock);
        let older = FakeExec::oracle(19);
        let older_log = Arc::clone(&older.log);
        let session = Session::start(older, &options(clock, false)).unwrap();
        assert_eq!(session.findings().len(), 1);
        assert_eq!(session.findings()[0].level, Level::Note);
        assert!(session
            .findings()
            .iter()
            .all(|finding| finding.level != Level::Warn));
        assert_eq!(
            session.findings()[0].message,
            "server 19 has no session READ_ONLY mode"
        );
        assert!(!session.strict());
        let setup = older_log.lock().unwrap().clone();
        assert_eq!(
            setup,
            [
                "ALTER SESSION DISABLE PARALLEL QUERY",
                "ALTER SESSION SET NLS_SORT = BINARY",
                "ALTER SESSION SET NLS_COMP = BINARY",
            ]
        );
        drop(session);

        let newer = FakeExec::oracle(23);
        let newer_log = Arc::clone(&newer.log);
        let session = Session::start(newer, &options(Arc::new(SystemClock), false)).unwrap();
        assert!(session.findings().is_empty());
        assert_eq!(
            newer_log.lock().unwrap().as_slice(),
            [
                "ALTER SESSION DISABLE PARALLEL QUERY",
                "ALTER SESSION SET NLS_SORT = BINARY",
                "ALTER SESSION SET NLS_COMP = BINARY",
                "ALTER SESSION SET READ_ONLY = TRUE",
            ]
        );
        drop(session);
    }

    #[test]
    fn oracle_23_read_only_failure_aborts_without_a_warning() {
        let _lock = test_lock();
        for strict in [false, true] {
            let mut exec = FakeExec::oracle(23);
            exec.fail_read_only = true;
            let closed = Arc::clone(&exec.closed);
            let selects = Arc::clone(&exec.selects);
            let err = must_err(Session::start(
                exec,
                &options(Arc::new(SystemClock), strict),
            ));
            assert!(closed.load(Ordering::SeqCst), "strict={strict}");
            assert_eq!(selects.load(Ordering::SeqCst), 0, "strict={strict}");
            let message = err.to_string();
            assert!(!message.to_ascii_lowercase().contains("warn"), "{message}");
        }
        let again = Session::start(FakeExec::oracle(23), &options(Arc::new(SystemClock), false));
        assert!(again.is_ok());
    }

    #[test]
    fn read_all_and_keyset_chunks_roll_back_each_statement() {
        let _lock = test_lock();
        let exec = FakeExec::oracle(23);
        let log = Arc::clone(&exec.log);
        let session = Session::start(exec, &options(Arc::new(SystemClock), false)).unwrap();
        let probe = GuardedSelect::parse(r#"SELECT "A" FROM "T""#, SqlDialect::Oracle).unwrap();
        session.read_all(&probe, &[]).unwrap();
        let (count, _) = build::count(
            SqlDialect::Oracle,
            &crate::source::SelectSpec {
                table: "T".to_string(),
                columns: vec!["K".to_string()],
                filters: Vec::new(),
                limit: None,
                batch_rows: 1,
                key: None,
                paging: crate::source::Paging::Keyset,
            },
        )
        .unwrap();
        session.read_all(&count, &[]).unwrap();
        let mut iter = session.chunks(keyset_plan(3, 1));
        for _ in 0..3 {
            assert!(iter.next().unwrap().is_ok());
        }
        assert!(iter.next().is_none());
        drop(iter);
        let before_close = log.lock().unwrap().clone();
        let set_txn = before_close
            .iter()
            .filter(|sql| *sql == "SET TRANSACTION READ ONLY")
            .count();
        let selects = before_close
            .iter()
            .filter(|sql| sql.starts_with("SELECT"))
            .count();
        let rollbacks = before_close.iter().filter(|sql| *sql == "ROLLBACK").count();
        assert_eq!(set_txn, 5, "{before_close:?}");
        assert_eq!(selects, 5, "{before_close:?}");
        assert_eq!(rollbacks, set_txn, "{before_close:?}");
        drop(session);
        let after = log.lock().unwrap().clone();
        let rollbacks = after.iter().filter(|sql| *sql == "ROLLBACK").count();
        let set_txn = after
            .iter()
            .filter(|sql| *sql == "SET TRANSACTION READ ONLY")
            .count();
        assert_eq!(rollbacks, set_txn + 1, "{after:?}");
        assert_eq!(after.last().map(String::as_str), Some("ROLLBACK"));
    }

    #[test]
    fn chunk_error_drop_and_panic_still_roll_back() {
        let _lock = test_lock();
        let mut exec = FakeExec::oracle(23);
        exec.fail_select_on = Some(2);
        let log = Arc::clone(&exec.log);
        let session = Session::start(exec, &options(Arc::new(SystemClock), false)).unwrap();
        {
            let mut iter = session.chunks(keyset_plan(3, 1));
            assert!(iter.next().unwrap().is_ok());
            let err = iter.next().unwrap().unwrap_err();
            assert!(err.to_string().contains("select failed"));
            drop(iter);
            let entries = log.lock().unwrap().clone();
            assert_eq!(entries.last().map(String::as_str), Some("ROLLBACK"));
            assert_eq!(
                entries
                    .iter()
                    .filter(|sql| sql.starts_with("SELECT"))
                    .count(),
                2,
                "{entries:?}"
            );
        }
        drop(session);

        let exec = FakeExec::oracle(23);
        let log = Arc::clone(&exec.log);
        let session = Session::start(exec, &options(Arc::new(SystemClock), false)).unwrap();
        {
            let mut iter = session.chunks(keyset_plan(3, 1));
            assert!(iter.next().unwrap().is_ok());
            drop(iter);
            assert_eq!(
                log.lock().unwrap().last().map(String::as_str),
                Some("ROLLBACK")
            );
        }
        drop(session);

        let mut exec = FakeExec::oracle(23);
        exec.rows_per_cursor = 4;
        let log = Arc::clone(&exec.log);
        let session = Session::start(exec, &options(Arc::new(SystemClock), false)).unwrap();
        let plan = build::snapshot(
            SqlDialect::Oracle,
            &crate::source::SelectSpec {
                table: "T".to_string(),
                columns: vec!["K".to_string(), "A".to_string()],
                filters: Vec::new(),
                limit: None,
                batch_rows: 1,
                key: None,
                paging: crate::source::Paging::Keyset,
            },
        )
        .unwrap();
        {
            let mut iter = session.chunks(plan);
            assert!(iter.next().unwrap().is_ok());
            drop(iter);
            assert_eq!(
                log.lock().unwrap().last().map(String::as_str),
                Some("ROLLBACK")
            );
        }
        drop(session);

        let mut exec = FakeExec::oracle(23);
        exec.panic_on_row = true;
        let log = Arc::clone(&exec.log);
        let session = Session::start(exec, &options(Arc::new(SystemClock), false)).unwrap();
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            let mut iter = session.chunks(keyset_plan(1, 1));
            let _ = iter.next();
        }));
        assert!(panicked.is_err());
        assert_eq!(
            log.lock().unwrap().last().map(String::as_str),
            Some("ROLLBACK")
        );
        drop(session);
    }

    #[test]
    fn deadline_rolls_back_before_the_next_chunk() {
        let _lock = test_lock();
        let clock = Arc::new(ManualClock::new());
        let exec = FakeExec::oracle(23);
        let log = Arc::clone(&exec.log);
        let mut opts = options(Arc::clone(&clock) as Arc<dyn Clock>, false);
        opts.deadline = Duration::from_secs(10);
        let session = Session::start(exec, &opts).unwrap();
        let mut iter = session.chunks(keyset_plan(3, 1));
        assert!(iter.next().unwrap().is_ok());
        clock.advance(Duration::from_secs(10));
        let err = iter.next().unwrap().unwrap_err();
        assert!(
            err.to_string().contains("session deadline exceeded"),
            "{err}"
        );
        let entries = log.lock().unwrap().clone();
        assert_eq!(entries.last().map(String::as_str), Some("ROLLBACK"));
        assert!(
            entries
                .iter()
                .filter(|sql| sql.starts_with("SELECT"))
                .count()
                < 3
        );
        drop(iter);
        drop(session);
    }

    #[test]
    fn logging_executor_records_statements_but_not_binds() {
        let _lock = test_lock();
        let sink = Arc::new(Mutex::new(Vec::new()));
        let exec = crate::source::logging::LoggingExecutor::new(
            FakeExec::oracle(23),
            crate::source::logging::SqlLogSink::Memory(Arc::clone(&sink)),
        );
        let session = Session::start(exec, &options(Arc::new(SystemClock), false)).unwrap();
        let probe = GuardedSelect::parse(r#"SELECT "A" FROM "T""#, SqlDialect::Oracle).unwrap();
        session
            .read_all(&probe, &[Value::Text("secret-bind".to_string())])
            .unwrap();
        drop(session);
        let recorded = sink.lock().unwrap().clone();
        assert!(recorded
            .iter()
            .any(|sql| sql == "ALTER SESSION SET READ_ONLY = TRUE"));
        assert!(recorded.iter().any(|sql| sql.contains("SELECT")));
        assert!(recorded.iter().all(|sql| !sql.contains("secret-bind")));
    }

    #[test]
    fn a_second_session_is_busy_until_the_first_closes() {
        let _lock = test_lock();
        let first =
            Session::start(FakeExec::oracle(19), &options(Arc::new(SystemClock), false)).unwrap();
        let err = must_err(Session::start(
            FakeExec::oracle(19),
            &options(Arc::new(SystemClock), false),
        ));
        assert!(err.to_string().contains("another source session is open"));
        assert_eq!(err.exit_code(), 1);
        drop(first);
        let second =
            Session::start(FakeExec::oracle(19), &options(Arc::new(SystemClock), false)).unwrap();
        drop(second);
    }
}
