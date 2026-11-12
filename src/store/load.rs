//! Append source chunks into a staging table, then swap it into place.

use crate::error::DipperError;
use crate::source::{Chunk, Value};
use crate::store::{quote, store_err, Store};

/// DuckDB column type chosen from source values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    BigInt,
    Double,
    Varchar,
    Date,
    Timestamp,
}

impl ColumnType {
    pub fn sql(self) -> &'static str {
        match self {
            Self::BigInt => "BIGINT",
            Self::Double => "DOUBLE",
            Self::Varchar => "VARCHAR",
            Self::Date => "DATE",
            Self::Timestamp => "TIMESTAMP",
        }
    }
}

/// `Null` has no type yet. Every other variant maps to one DuckDB type.
pub fn type_of(value: &Value) -> Option<ColumnType> {
    match value {
        Value::Null => None,
        Value::Int(_) => Some(ColumnType::BigInt),
        Value::Float(_) => Some(ColumnType::Double),
        Value::Text(_) => Some(ColumnType::Varchar),
        Value::Date(_) => Some(ColumnType::Date),
        Value::Timestamp(_) => Some(ColumnType::Timestamp),
    }
}

/// Widen two concrete types. Anything except int/float and date/timestamp becomes text.
pub fn widen(left: ColumnType, right: ColumnType) -> ColumnType {
    use ColumnType::{BigInt, Date, Double, Timestamp, Varchar};
    match (left, right) {
        (same, other) if same == other => same,
        (BigInt, Double) | (Double, BigInt) => Double,
        (Date, Timestamp) | (Timestamp, Date) => Timestamp,
        (BigInt | Double | Varchar | Date | Timestamp, _) => Varchar,
    }
}

/// One dip load. Dropping it leaves the staging table in place and the live table unchanged.
pub struct TableLoad<'a> {
    store: &'a Store,
    table: String,
    staging: String,
    columns: Vec<String>,
    types: Vec<Option<ColumnType>>,
    rows: u64,
    appender: Option<duckdb::Appender<'a>>,
}

impl<'a> TableLoad<'a> {
    pub(super) fn new(store: &'a Store, table: String, staging: String) -> Self {
        Self {
            store,
            table,
            staging,
            columns: Vec::new(),
            types: Vec::new(),
            rows: 0,
            appender: None,
        }
    }

    /// Create the staging table from the first non-empty chunk, then append and flush.
    pub fn append(&mut self, chunk: &Chunk) -> Result<(), DipperError> {
        if chunk.rows.is_empty() {
            return Ok(());
        }
        if self.columns.is_empty() {
            self.columns = chunk.columns.clone();
            self.types = vec![None; self.columns.len()];
            for row in &chunk.rows {
                self.learn_row(row)?;
            }
            self.create_table()?;
        } else if chunk.columns != self.columns {
            return Err(store_err("chunk columns do not match the staging table"));
        }
        for row in &chunk.rows {
            self.append_row(row)?;
        }
        self.flush_appender()?;
        Ok(())
    }

    pub fn rows(&self) -> u64 {
        self.rows
    }

    /// Swap staging into the live table. An empty load becomes an all-VARCHAR table.
    pub fn finish(mut self, fallback_columns: &[String]) -> Result<u64, DipperError> {
        if self.columns.is_empty() {
            self.columns = fallback_columns.to_vec();
            self.types = vec![Some(ColumnType::Varchar); self.columns.len()];
            self.create_table()?;
        }
        self.flush_and_close()?;
        let rows = self.rows;
        self.swap_in()?;
        Ok(rows)
    }

    fn learn_row(&mut self, row: &[Value]) -> Result<(), DipperError> {
        self.check_width(row)?;
        for (index, value) in row.iter().enumerate() {
            if let Some(incoming) = type_of(value) {
                self.types[index] = Some(match self.types[index] {
                    None => incoming,
                    Some(current) => widen(current, incoming),
                });
            }
        }
        Ok(())
    }

    fn append_row(&mut self, row: &[Value]) -> Result<(), DipperError> {
        self.check_width(row)?;
        for (index, value) in row.iter().enumerate() {
            let Some(incoming) = type_of(value) else {
                continue;
            };
            let widened = match self.types[index] {
                None => incoming,
                Some(current) => widen(current, incoming),
            };
            if self.types[index] != Some(widened) {
                self.types[index] = Some(widened);
                self.alter_column(index, widened)?;
            }
        }
        let mut duck_row = Vec::with_capacity(row.len());
        for (index, value) in row.iter().enumerate() {
            let target = self.types[index].unwrap_or(ColumnType::Varchar);
            duck_row.push(to_duck(value, target)?);
        }
        let params: Vec<&dyn duckdb::ToSql> = duck_row.iter().map(|value| value as _).collect();
        self.appender_mut()?
            .append_row(params.as_slice())
            .map_err(store_err)?;
        self.rows += 1;
        Ok(())
    }

    fn check_width(&self, row: &[Value]) -> Result<(), DipperError> {
        if row.len() == self.columns.len() {
            Ok(())
        } else {
            Err(store_err(format!(
                "row has {} values, expected {}",
                row.len(),
                self.columns.len()
            )))
        }
    }

    fn create_table(&mut self) -> Result<(), DipperError> {
        let defs = self
            .columns
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let kind = self.types[index].unwrap_or(ColumnType::Varchar).sql();
                format!("{} {kind}", quote(name))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("CREATE TABLE {} ({defs})", quote(&self.staging));
        self.store.conn.execute_batch(&sql).map_err(store_err)?;
        self.open_appender()
    }

    fn alter_column(&mut self, index: usize, ty: ColumnType) -> Result<(), DipperError> {
        self.flush_and_close()?;
        let sql = format!(
            "ALTER TABLE {} ALTER {} TYPE {}",
            quote(&self.staging),
            quote(&self.columns[index]),
            ty.sql()
        );
        self.store.conn.execute_batch(&sql).map_err(store_err)?;
        self.open_appender()
    }

    fn swap_in(&self) -> Result<(), DipperError> {
        let sql = format!(
            "DROP TABLE IF EXISTS {}; ALTER TABLE {} RENAME TO {}",
            quote(&self.table),
            quote(&self.staging),
            quote(&self.table)
        );
        let tx = self.store.conn.unchecked_transaction().map_err(store_err)?;
        tx.execute_batch(&sql).map_err(store_err)?;
        tx.execute(
            "DELETE FROM \"_dipper_slices\" WHERE \"dip\" = ?",
            [&self.table],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(())
    }

    /// Close the appender and hand the staging name back for a period commit.
    pub fn close_staging(mut self) -> Result<(String, u64, Vec<String>), DipperError> {
        self.flush_and_close()?;
        Ok((self.staging, self.rows, self.columns))
    }

    fn open_appender(&mut self) -> Result<(), DipperError> {
        let staging = self.staging.clone();
        let appender = self.store.conn.appender(&staging).map_err(store_err)?;
        self.appender = Some(appender);
        Ok(())
    }

    fn appender_mut(&mut self) -> Result<&mut duckdb::Appender<'a>, DipperError> {
        if self.appender.is_none() {
            self.open_appender()?;
        }
        self.appender
            .as_mut()
            .ok_or_else(|| store_err("staging appender is not open"))
    }

    fn flush_appender(&mut self) -> Result<(), DipperError> {
        if let Some(appender) = self.appender.as_mut() {
            appender.flush().map_err(store_err)?;
        }
        Ok(())
    }

    fn flush_and_close(&mut self) -> Result<(), DipperError> {
        if let Some(mut appender) = self.appender.take() {
            appender.flush().map_err(store_err)?;
        }
        Ok(())
    }
}

fn to_duck(value: &Value, target: ColumnType) -> Result<duckdb::types::Value, DipperError> {
    if matches!(value, Value::Null) {
        return Ok(duckdb::types::Value::Null);
    }
    if target == ColumnType::Varchar {
        return Ok(duckdb::types::Value::Text(as_text(value)));
    }
    match (target, value) {
        (ColumnType::BigInt, Value::Int(number)) => Ok(duckdb::types::Value::BigInt(*number)),
        (ColumnType::Double, Value::Float(number)) => Ok(duckdb::types::Value::Double(*number)),
        (ColumnType::Double, Value::Int(number)) => {
            Ok(duckdb::types::Value::Double(*number as f64))
        }
        (ColumnType::Date, Value::Date(date)) => {
            Ok(duckdb::types::Value::Date32(date_days(*date)?))
        }
        (ColumnType::Timestamp, Value::Timestamp(time)) => Ok(duckdb::types::Value::Timestamp(
            duckdb::types::TimeUnit::Microsecond,
            timestamp_micros(*time)?,
        )),
        (ColumnType::Timestamp, Value::Date(date)) => {
            let midnight = date
                .and_hms_opt(0, 0, 0)
                .ok_or_else(|| store_err("date cannot be converted to a timestamp"))?;
            Ok(duckdb::types::Value::Timestamp(
                duckdb::types::TimeUnit::Microsecond,
                timestamp_micros(midnight)?,
            ))
        }
        (ColumnType::Varchar, _) => Ok(duckdb::types::Value::Text(as_text(value))),
        (ColumnType::BigInt | ColumnType::Double | ColumnType::Date | ColumnType::Timestamp, _) => {
            Err(store_err("value does not fit the staging column type"))
        }
    }
}

fn as_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Int(number) => number.to_string(),
        Value::Float(number) => format!("{number}"),
        Value::Text(text) => text.clone(),
        Value::Date(date) => date.format("%Y-%m-%d").to_string(),
        Value::Timestamp(time) => time.format("%Y-%m-%d %H:%M:%S%.f").to_string(),
    }
}

fn date_days(date: chrono::NaiveDate) -> Result<i32, DipperError> {
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("unix epoch");
    i32::try_from((date - epoch).num_days()).map_err(|_| store_err("date is outside DATE range"))
}

fn timestamp_micros(time: chrono::NaiveDateTime) -> Result<i64, DipperError> {
    Ok(time.and_utc().timestamp_micros())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Performance;
    use crate::source::Chunk;
    use chrono::{NaiveDate, NaiveDateTime};

    fn store_in(dir: &std::path::Path) -> Store {
        let perf = Performance {
            batch_rows: 10,
            memory_limit: "64MiB".to_string(),
            temp_directory: dir.join("tmp"),
            progress_every_rows: 1000,
        };
        Store::open(&dir.join("store.duckdb"), &perf).unwrap()
    }

    fn chunk(columns: &[&str], rows: Vec<Vec<Value>>) -> Chunk {
        Chunk {
            columns: columns.iter().map(|column| (*column).to_string()).collect(),
            rows,
        }
    }

    fn cell(store: &Store, table: &str, column: &str) -> duckdb::types::Value {
        let sql = format!("SELECT {} FROM {}", quote(column), quote(table));
        store.conn.query_row(&sql, [], |row| row.get(0)).unwrap()
    }

    fn column_type(store: &Store, table: &str, column: &str) -> String {
        let sql = format!(
            "SELECT data_type FROM duckdb_columns() WHERE schema_name = 'main' AND table_name = {} AND column_name = {}",
            sql_string(table),
            sql_string(column)
        );
        store.conn.query_row(&sql, [], |row| row.get(0)).unwrap()
    }

    fn sql_string(text: &str) -> String {
        format!("'{}'", text.replace('\'', "''"))
    }

    #[test]
    fn type_of_covers_every_value() {
        assert_eq!(type_of(&Value::Null), None);
        assert_eq!(type_of(&Value::Int(1)), Some(ColumnType::BigInt));
        assert_eq!(type_of(&Value::Float(1.5)), Some(ColumnType::Double));
        assert_eq!(
            type_of(&Value::Text("a".to_string())),
            Some(ColumnType::Varchar)
        );
        assert_eq!(
            type_of(&Value::Date(NaiveDate::from_ymd_opt(2024, 3, 15).unwrap())),
            Some(ColumnType::Date)
        );
        assert_eq!(
            type_of(&Value::Timestamp(
                NaiveDateTime::parse_from_str("2024-03-15 13:45:30", "%Y-%m-%d %H:%M:%S").unwrap()
            )),
            Some(ColumnType::Timestamp)
        );
    }

    #[test]
    fn widen_promotes_numbers_and_dates_and_falls_back_to_text() {
        assert_eq!(
            widen(ColumnType::BigInt, ColumnType::BigInt),
            ColumnType::BigInt
        );
        assert_eq!(
            widen(ColumnType::BigInt, ColumnType::Double),
            ColumnType::Double
        );
        assert_eq!(
            widen(ColumnType::Double, ColumnType::BigInt),
            ColumnType::Double
        );
        assert_eq!(
            widen(ColumnType::Date, ColumnType::Timestamp),
            ColumnType::Timestamp
        );
        assert_eq!(
            widen(ColumnType::Timestamp, ColumnType::Date),
            ColumnType::Timestamp
        );
        assert_eq!(
            widen(ColumnType::BigInt, ColumnType::Varchar),
            ColumnType::Varchar
        );
        assert_eq!(
            widen(ColumnType::BigInt, ColumnType::Date),
            ColumnType::Varchar
        );
        assert_eq!(
            widen(ColumnType::Date, ColumnType::Double),
            ColumnType::Varchar
        );
    }

    #[test]
    fn values_round_trip_with_the_planned_types() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let date = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap();
        let time =
            NaiveDateTime::parse_from_str("2024-03-15 13:45:30", "%Y-%m-%d %H:%M:%S").unwrap();
        let mut load = store.begin_load("sample").unwrap();
        load.append(&chunk(
            &["n", "f", "t", "d", "ts", "empty"],
            vec![vec![
                Value::Int(7),
                Value::Float(1.5),
                Value::Text("12345678901234567890.123456789".to_string()),
                Value::Date(date),
                Value::Timestamp(time),
                Value::Null,
            ]],
        ))
        .unwrap();
        assert_eq!(load.finish(&[]).unwrap(), 1);
        assert!(matches!(
            cell(&store, "sample", "n"),
            duckdb::types::Value::BigInt(7)
        ));
        assert!(
            matches!(cell(&store, "sample", "f"), duckdb::types::Value::Double(number) if number == 1.5)
        );
        assert!(matches!(
            cell(&store, "sample", "t"),
            duckdb::types::Value::Text(text) if text == "12345678901234567890.123456789"
        ));
        let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
        let days = i32::try_from((date - epoch).num_days()).unwrap();
        assert!(
            matches!(cell(&store, "sample", "d"), duckdb::types::Value::Date32(got) if got == days)
        );
        let micros = time.and_utc().timestamp_micros();
        assert!(matches!(
            cell(&store, "sample", "ts"),
            duckdb::types::Value::Timestamp(unit, got)
                if unit == duckdb::types::TimeUnit::Microsecond && got == micros
        ));
        assert!(matches!(
            cell(&store, "sample", "empty"),
            duckdb::types::Value::Null
        ));
        assert_eq!(column_type(&store, "sample", "t"), "VARCHAR");
    }

    #[test]
    fn null_column_becomes_bigint_when_an_int_arrives() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let mut load = store.begin_load("sample").unwrap();
        load.append(&chunk(&["n"], vec![vec![Value::Null]]))
            .unwrap();
        load.append(&chunk(&["n"], vec![vec![Value::Int(4)]]))
            .unwrap();
        load.finish(&["n".to_string()]).unwrap();
        assert_eq!(column_type(&store, "sample", "n"), "BIGINT");
        assert!(matches!(
            cell(&store, "sample", "n"),
            duckdb::types::Value::BigInt(4) | duckdb::types::Value::Null
        ));
    }

    #[test]
    fn int_then_text_becomes_varchar() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let mut load = store.begin_load("sample").unwrap();
        load.append(&chunk(&["n"], vec![vec![Value::Int(9)]]))
            .unwrap();
        load.append(&chunk(&["n"], vec![vec![Value::Text("later".to_string())]]))
            .unwrap();
        load.finish(&["n".to_string()]).unwrap();
        assert_eq!(column_type(&store, "sample", "n"), "VARCHAR");
        let sql = format!(
            "SELECT {} FROM {} ORDER BY {}",
            quote("n"),
            quote("sample"),
            quote("n")
        );
        let mut stmt = store.conn.prepare(&sql).unwrap();
        let values: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|value| value.unwrap())
            .collect();
        assert_eq!(values, ["9", "later"]);
    }

    #[test]
    fn empty_finish_creates_varchar_columns_and_drop_keeps_the_old_table() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let mut load = store.begin_load("sample").unwrap();
        load.append(&chunk(&["id"], vec![vec![Value::Int(1)]]))
            .unwrap();
        load.finish(&["id".to_string()]).unwrap();

        let mut abandoned = store.begin_load("sample").unwrap();
        abandoned
            .append(&chunk(&["id"], vec![vec![Value::Int(99)]]))
            .unwrap();
        drop(abandoned);
        assert_eq!(store.testing_count("sample").unwrap(), 1);
        assert!(store
            .testing_table_names()
            .unwrap()
            .iter()
            .any(|name| name == "sample__staging"));

        let empty = store.begin_load("other").unwrap();
        assert_eq!(
            empty.finish(&["A".to_string(), "B".to_string()]).unwrap(),
            0
        );
        assert_eq!(store.testing_count("other").unwrap(), 0);
        assert_eq!(column_type(&store, "other", "A"), "VARCHAR");
        assert_eq!(column_type(&store, "other", "B"), "VARCHAR");
    }

    #[test]
    fn a_second_load_replaces_the_table() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let mut first = store.begin_load("sample").unwrap();
        first
            .append(&chunk(&["id"], vec![vec![Value::Int(1)]]))
            .unwrap();
        first.finish(&["id".to_string()]).unwrap();
        let mut second = store.begin_load("sample").unwrap();
        second
            .append(&chunk(
                &["id"],
                vec![vec![Value::Int(2)], vec![Value::Int(3)]],
            ))
            .unwrap();
        assert_eq!(second.finish(&["id".to_string()]).unwrap(), 2);
        assert_eq!(store.testing_count("sample").unwrap(), 2);
        assert!(!store
            .testing_table_names()
            .unwrap()
            .iter()
            .any(|name| name.ends_with("__staging")));
    }
}
