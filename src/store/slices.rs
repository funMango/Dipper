//! Half-open period ledger. Rows are written only inside a DuckDB transaction.

use chrono::{NaiveDateTime, TimeDelta};

use crate::error::DipperError;
use crate::store::{quote, store_err, Store};

pub const SLICES_TABLE: &str = "_dipper_slices";

const CREATE_SLICES: &str = "CREATE TABLE IF NOT EXISTS \"_dipper_slices\" (\"dip\" VARCHAR NOT NULL, \"time_from\" TIMESTAMP NOT NULL, \"time_to\" TIMESTAMP NOT NULL, \"rows\" BIGINT NOT NULL, \"loaded_at\" TIMESTAMP NOT NULL)";

/// `[start, end)` with `start < end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HalfOpen {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
}

/// One ledger row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceRow {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub rows: u64,
}

pub(crate) fn ensure_slices_table(conn: &duckdb::Connection) -> Result<(), DipperError> {
    conn.execute_batch(CREATE_SLICES).map_err(store_err)
}

/// Merge overlapping and adjacent half-open ranges. Adjacent means `a.end == b.start`.
pub fn merge_ranges(ranges: &[HalfOpen]) -> Vec<HalfOpen> {
    let mut ranges: Vec<HalfOpen> = ranges
        .iter()
        .copied()
        .filter(|range| range.start < range.end)
        .collect();
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<HalfOpen> = Vec::new();
    for range in ranges {
        if let Some(last) = merged.last_mut() {
            if range.start <= last.end {
                if range.end > last.end {
                    last.end = range.end;
                }
                continue;
            }
        }
        merged.push(range);
    }
    merged
}

/// Subtract merged `held` ranges from `request`.
pub fn subtract_ranges(request: HalfOpen, held: &[HalfOpen]) -> Vec<HalfOpen> {
    if request.start >= request.end {
        return Vec::new();
    }
    let held = merge_ranges(held);
    let mut holes = Vec::new();
    let mut cursor = request.start;
    for piece in held {
        if piece.end <= cursor {
            continue;
        }
        if piece.start >= request.end {
            break;
        }
        let start = piece.start.max(cursor);
        if start > cursor {
            holes.push(HalfOpen {
                start: cursor,
                end: start.min(request.end),
            });
        }
        if piece.end > cursor {
            cursor = piece.end;
        }
        if cursor >= request.end {
            break;
        }
    }
    if cursor < request.end {
        holes.push(HalfOpen {
            start: cursor,
            end: request.end,
        });
    }
    holes
}

/// Cut `request` out of `held`, then store `request` as one slice and merge neighbors.
pub fn plan_refresh(held: &[SliceRow], request: HalfOpen, new_rows: u64) -> Vec<SliceRow> {
    let mut pieces = Vec::new();
    for slice in held {
        let remnants = subtract_ranges(
            HalfOpen {
                start: slice.start,
                end: slice.end,
            },
            &[request],
        );
        for (index, remnant) in remnants.into_iter().enumerate() {
            pieces.push(SliceRow {
                start: remnant.start,
                end: remnant.end,
                rows: if index == 0 { slice.rows } else { 0 },
            });
        }
    }
    pieces.push(SliceRow {
        start: request.start,
        end: request.end,
        rows: new_rows,
    });
    merge_rows(pieces)
}

fn merge_rows(rows: Vec<SliceRow>) -> Vec<SliceRow> {
    let mut rows: Vec<SliceRow> = rows.into_iter().filter(|row| row.start < row.end).collect();
    rows.sort_by_key(|row| row.start);
    let mut merged: Vec<SliceRow> = Vec::new();
    for row in rows {
        if let Some(last) = merged.last_mut() {
            if row.start <= last.end {
                if row.end > last.end {
                    last.end = row.end;
                }
                last.rows = last.rows.saturating_add(row.rows);
                continue;
            }
        }
        merged.push(row);
    }
    merged
}

impl Store {
    pub fn list_slices(&self, dip: &str) -> Result<Vec<SliceRow>, DipperError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT \"time_from\", \"time_to\", \"rows\" FROM \"_dipper_slices\" WHERE \"dip\" = ? ORDER BY \"time_from\"",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([dip], |row| {
                Ok((
                    row.get::<_, duckdb::types::Value>(0)?,
                    row.get::<_, duckdb::types::Value>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(store_err)?;
        let mut slices = Vec::new();
        for row in rows {
            let (start, end, count) = row.map_err(store_err)?;
            slices.push(SliceRow {
                start: read_timestamp(start)?,
                end: read_timestamp(end)?,
                rows: u64::try_from(count).unwrap_or(0),
            });
        }
        Ok(slices)
    }

    pub fn has_slices(&self, dip: &str) -> Result<bool, DipperError> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT count(*) FROM \"_dipper_slices\" WHERE \"dip\" = ?",
                [dip],
                |row| row.get(0),
            )
            .map_err(store_err)?;
        Ok(count > 0)
    }
}

pub fn commit_period_load(
    store: &Store,
    table: &str,
    staging: &str,
    staged_rows: u64,
    fallback_columns: &[String],
    mode: PeriodWrite,
    slices: &[SliceRow],
) -> Result<(), DipperError> {
    let tx = store.conn.unchecked_transaction().map_err(store_err)?;
    let live = quote(table);
    let stage = quote(staging);
    let exists = table_exists_tx(&tx, table)?;
    match &mode {
        PeriodWrite::Replace => {
            publish_staging(
                &tx,
                &live,
                &stage,
                staged_rows,
                table,
                fallback_columns,
                true,
            )?;
            delete_slices(&tx, table)?;
            insert_slices(&tx, table, slices)?;
        }
        PeriodWrite::Refresh {
            column,
            start,
            end,
            exact,
        } => {
            if exists {
                delete_window(&tx, table, column, *start, *end, *exact)?;
                if staged_rows > 0 {
                    tx.execute_batch(&format!(
                        "INSERT INTO {live} SELECT * FROM {stage}; DROP TABLE {stage}"
                    ))
                    .map_err(store_err)?;
                } else {
                    tx.execute_batch(&format!("DROP TABLE IF EXISTS {stage}"))
                        .map_err(store_err)?;
                }
            } else {
                publish_staging(
                    &tx,
                    &live,
                    &stage,
                    staged_rows,
                    table,
                    fallback_columns,
                    true,
                )?;
            }
            delete_slices(&tx, table)?;
            insert_slices(&tx, table, slices)?;
        }
        PeriodWrite::Fill => {
            if exists {
                if staged_rows > 0 {
                    tx.execute_batch(&format!(
                        "INSERT INTO {live} SELECT * FROM {stage}; DROP TABLE {stage}"
                    ))
                    .map_err(store_err)?;
                } else {
                    tx.execute_batch(&format!("DROP TABLE IF EXISTS {stage}"))
                        .map_err(store_err)?;
                }
            } else {
                publish_staging(
                    &tx,
                    &live,
                    &stage,
                    staged_rows,
                    table,
                    fallback_columns,
                    true,
                )?;
            }
            insert_slices(&tx, table, slices)?;
        }
    }
    tx.commit().map_err(store_err)?;
    Ok(())
}

pub enum PeriodWrite {
    Fill,
    Refresh {
        column: String,
        start: NaiveDateTime,
        end: NaiveDateTime,
        exact: bool,
    },
    Replace,
}

fn publish_staging(
    tx: &duckdb::Transaction<'_>,
    live: &str,
    stage: &str,
    staged_rows: u64,
    table: &str,
    fallback_columns: &[String],
    drop_live: bool,
) -> Result<(), DipperError> {
    if staged_rows > 0 {
        let drop = if drop_live {
            format!("DROP TABLE IF EXISTS {live}; ")
        } else {
            String::new()
        };
        tx.execute_batch(&format!("{drop}ALTER TABLE {stage} RENAME TO {live}"))
            .map_err(store_err)?;
        return Ok(());
    }
    tx.execute_batch(&format!("DROP TABLE IF EXISTS {stage}"))
        .map_err(store_err)?;
    if !table_exists_tx(tx, table)? {
        create_empty(tx, table, fallback_columns)?;
    }
    Ok(())
}

fn create_empty(
    tx: &duckdb::Transaction<'_>,
    table: &str,
    columns: &[String],
) -> Result<(), DipperError> {
    let defs = columns
        .iter()
        .map(|name| format!("{} VARCHAR", quote(name)))
        .collect::<Vec<_>>()
        .join(", ");
    tx.execute_batch(&format!("CREATE TABLE {} ({defs})", quote(table)))
        .map_err(store_err)?;
    Ok(())
}

fn delete_window(
    tx: &duckdb::Transaction<'_>,
    table: &str,
    column: &str,
    start: NaiveDateTime,
    end: NaiveDateTime,
    exact: bool,
) -> Result<(), DipperError> {
    let column = format!("CAST({} AS TIMESTAMP)", quote(column));
    let sql = if exact {
        format!("DELETE FROM {} WHERE {column} = ?", quote(table))
    } else {
        format!(
            "DELETE FROM {} WHERE {column} >= ? AND {column} < ?",
            quote(table)
        )
    };
    if exact {
        tx.execute(&sql, [stamp(start)]).map_err(store_err)?;
    } else {
        tx.execute(&sql, [stamp(start), stamp(end)])
            .map_err(store_err)?;
    }
    Ok(())
}

fn delete_slices(tx: &duckdb::Transaction<'_>, dip: &str) -> Result<(), DipperError> {
    tx.execute("DELETE FROM \"_dipper_slices\" WHERE \"dip\" = ?", [dip])
        .map_err(store_err)?;
    Ok(())
}

fn insert_slices(
    tx: &duckdb::Transaction<'_>,
    dip: &str,
    slices: &[SliceRow],
) -> Result<(), DipperError> {
    let loaded_at = stamp_now()?;
    for slice in slices {
        tx.execute(
            "INSERT INTO \"_dipper_slices\" (\"dip\", \"time_from\", \"time_to\", \"rows\", \"loaded_at\") VALUES (?, ?, ?, ?, ?)",
            duckdb::params![
                dip,
                stamp(slice.start),
                stamp(slice.end),
                i64::try_from(slice.rows).unwrap_or(i64::MAX),
                loaded_at.clone(),
            ],
        )
        .map_err(store_err)?;
    }
    Ok(())
}

fn table_exists_tx(tx: &duckdb::Transaction<'_>, name: &str) -> Result<bool, DipperError> {
    let folded = name.to_ascii_lowercase();
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM duckdb_tables() WHERE schema_name = 'main' AND lower(table_name) = ?",
            [folded],
            |row| row.get(0),
        )
        .map_err(store_err)?;
    Ok(count > 0)
}

fn stamp(at: NaiveDateTime) -> duckdb::types::Value {
    duckdb::types::Value::Timestamp(
        duckdb::types::TimeUnit::Microsecond,
        at.and_utc().timestamp_micros(),
    )
}

fn stamp_now() -> Result<duckdb::types::Value, DipperError> {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| store_err("system clock is before the unix epoch"))?
        .as_micros();
    let micros =
        i64::try_from(micros).map_err(|_| store_err("clock does not fit in microseconds"))?;
    Ok(duckdb::types::Value::Timestamp(
        duckdb::types::TimeUnit::Microsecond,
        micros,
    ))
}

fn read_timestamp(value: duckdb::types::Value) -> Result<NaiveDateTime, DipperError> {
    match value {
        duckdb::types::Value::Timestamp(unit, raw) => {
            let micros = match unit {
                duckdb::types::TimeUnit::Microsecond => raw,
                duckdb::types::TimeUnit::Millisecond => raw.saturating_mul(1000),
                duckdb::types::TimeUnit::Second => raw.saturating_mul(1_000_000),
                duckdb::types::TimeUnit::Nanosecond => raw / 1000,
            };
            chrono::DateTime::from_timestamp_micros(micros)
                .map(|time| time.naive_utc())
                .ok_or_else(|| store_err("slice timestamp is out of range"))
        }
        other => Err(store_err(format!("slice timestamp has type {other:?}"))),
    }
}

/// Same-instant loads are stored as a one-second half-open marker.
pub fn exact_marker(at: NaiveDateTime) -> HalfOpen {
    HalfOpen {
        start: at,
        end: at + TimeDelta::seconds(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").unwrap()
    }

    fn day(month: u32, day: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, month, day)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
    }

    fn range(start: NaiveDateTime, end: NaiveDateTime) -> HalfOpen {
        HalfOpen { start, end }
    }

    #[test]
    fn merge_overlaps_and_adjacent_half_open_ranges() {
        let merged = merge_ranges(&[
            range(day(8, 1), day(8, 10)),
            range(day(8, 10), day(8, 15)),
            range(day(8, 14), day(8, 20)),
            range(day(9, 1), day(9, 2)),
        ]);
        assert_eq!(
            merged,
            vec![range(day(8, 1), day(8, 20)), range(day(9, 1), day(9, 2)),]
        );
    }

    #[test]
    fn subtract_held_august_from_mid_aug_to_sep_request() {
        let holes = subtract_ranges(
            range(day(8, 15), day(10, 1)),
            &[range(day(8, 1), day(9, 1))],
        );
        assert_eq!(holes, vec![range(day(9, 1), day(10, 1))]);
    }

    #[test]
    fn subtract_leaves_multiple_holes() {
        let holes = subtract_ranges(
            range(day(8, 1), day(8, 10)),
            &[range(day(8, 3), day(8, 4)), range(day(8, 6), day(8, 7))],
        );
        assert_eq!(
            holes,
            vec![
                range(day(8, 1), day(8, 3)),
                range(day(8, 4), day(8, 6)),
                range(day(8, 7), day(8, 10)),
            ]
        );
    }

    #[test]
    fn covered_request_has_no_holes() {
        let holes = subtract_ranges(
            range(day(8, 10), day(8, 20)),
            &[range(day(8, 1), day(9, 1))],
        );
        assert!(holes.is_empty());
    }

    #[test]
    fn exact_timestamp_marker_is_one_second() {
        let marker = exact_marker(at("2026-08-21T08:00:00"));
        assert_eq!(marker.start, at("2026-08-21T08:00:00"));
        assert_eq!(marker.end, at("2026-08-21T08:00:01"));
    }
}
