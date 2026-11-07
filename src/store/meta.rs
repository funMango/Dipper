//! `_dipper_runs` records one row for every dip that actually started.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::DipperError;
use crate::store::{store_err, Store};

pub const RUNS_TABLE: &str = "_dipper_runs";

const CREATE_RUNS: &str = "CREATE TABLE IF NOT EXISTS \"_dipper_runs\" (\"dip\" VARCHAR NOT NULL, \"source\" VARCHAR NOT NULL, \"started_at\" TIMESTAMP NOT NULL, \"finished_at\" TIMESTAMP NOT NULL, \"rows\" BIGINT NOT NULL, \"ok\" BOOLEAN NOT NULL, \"error\" VARCHAR, \"paging\" VARCHAR NOT NULL, \"warnings\" BIGINT NOT NULL)";

/// One finished or failed dip. Times are UTC instants.
pub struct RunRecord {
    pub dip: String,
    pub source: String,
    pub started_at: SystemTime,
    pub finished_at: SystemTime,
    pub rows: u64,
    pub ok: bool,
    pub error: Option<String>,
    pub paging: &'static str,
    pub warnings: u64,
}

pub(crate) fn ensure_runs_table(conn: &duckdb::Connection) -> Result<(), DipperError> {
    conn.execute_batch(CREATE_RUNS).map_err(store_err)
}

impl Store {
    /// Append exactly one history row.
    pub fn record_run(&self, record: &RunRecord) -> Result<(), DipperError> {
        let mut appender = self.conn.appender(RUNS_TABLE).map_err(store_err)?;
        let row = [
            duckdb::types::Value::Text(record.dip.clone()),
            duckdb::types::Value::Text(record.source.clone()),
            timestamp_value(record.started_at)?,
            timestamp_value(record.finished_at)?,
            duckdb::types::Value::BigInt(fit_i64(record.rows, "rows")?),
            duckdb::types::Value::Boolean(record.ok),
            match &record.error {
                Some(error) => duckdb::types::Value::Text(error.clone()),
                None => duckdb::types::Value::Null,
            },
            duckdb::types::Value::Text(record.paging.to_string()),
            duckdb::types::Value::BigInt(fit_i64(record.warnings, "warnings")?),
        ];
        let params: Vec<&dyn duckdb::ToSql> = row.iter().map(|value| value as _).collect();
        appender.append_row(params.as_slice()).map_err(store_err)?;
        appender.flush().map_err(store_err)?;
        Ok(())
    }
}

fn timestamp_value(time: SystemTime) -> Result<duckdb::types::Value, DipperError> {
    let micros = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_micros())
            .map_err(|_| store_err("timestamp does not fit in microseconds"))?,
        Err(err) => {
            let micros = i64::try_from(err.duration().as_micros())
                .map_err(|_| store_err("timestamp does not fit in microseconds"))?;
            micros
                .checked_neg()
                .ok_or_else(|| store_err("timestamp overflow"))?
        }
    };
    Ok(duckdb::types::Value::Timestamp(
        duckdb::types::TimeUnit::Microsecond,
        micros,
    ))
}

fn fit_i64(value: u64, label: &str) -> Result<i64, DipperError> {
    i64::try_from(value).map_err(|_| store_err(format!("{label} does not fit in BIGINT")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Performance;
    use std::time::Duration;

    #[test]
    fn record_run_writes_one_row_with_nine_columns() {
        let dir = tempfile::tempdir().unwrap();
        let perf = Performance {
            batch_rows: 10,
            memory_limit: "64MiB".to_string(),
            temp_directory: dir.path().join("tmp"),
            progress_every_rows: 1000,
        };
        let store = Store::open(&dir.path().join("store.duckdb"), &perf).unwrap();
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let finished = started + Duration::from_secs(2);
        store
            .record_run(&RunRecord {
                dip: "orders".to_string(),
                source: "sample".to_string(),
                started_at: started,
                finished_at: finished,
                rows: 3,
                ok: true,
                error: None,
                paging: "keyset",
                warnings: 0,
            })
            .unwrap();
        let mut stmt = store
            .conn
            .prepare("SELECT * FROM \"_dipper_runs\"")
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        assert!(rows.next().unwrap().is_some());
        drop(rows);
        assert_eq!(stmt.column_count(), 9);
        let names = stmt.column_names();
        assert_eq!(
            names,
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
        let count: i64 = store
            .conn
            .query_row("SELECT count(*) FROM \"_dipper_runs\"", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
        let runs = store.testing_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert!(runs[0].ok);
        assert!(runs[0].error.is_none());
    }
}
