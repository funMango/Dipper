//! Half-open period ledger. Rows are written only inside a DuckDB transaction.

use chrono::{NaiveDateTime, TimeDelta};

use crate::error::DipperError;
use crate::store::{quote, store_err, Store};

pub const SLICES_TABLE: &str = "_dipper_slices";

const CREATE_SLICES: &str = "CREATE TABLE IF NOT EXISTS \"_dipper_slices\" (\"dip\" VARCHAR NOT NULL, \"time_from\" TIMESTAMP NOT NULL, \"time_to\" TIMESTAMP NOT NULL, \"rows\" BIGINT, \"loaded_at\" TIMESTAMP NOT NULL)";

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
    /// `None` when this fragment is only part of an older slice. A refresh
    /// does not re-count the rows that remain outside the reloaded window,
    /// so a leftover fragment cannot keep the parent slice's row count.
    pub rows: Option<u64>,
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
///
/// A remnant that is not the whole original slice gets `rows = None`. The
/// ledger does not record which source rows fell inside the cut, so the
/// leftover count cannot be computed. Merging a known count with `None`
/// stays `None`.
pub fn plan_refresh(held: &[SliceRow], request: HalfOpen, new_rows: u64) -> Vec<SliceRow> {
    let mut pieces = Vec::new();
    for slice in held {
        let original = HalfOpen {
            start: slice.start,
            end: slice.end,
        };
        let remnants = subtract_ranges(original, &[request]);
        for remnant in remnants {
            let untouched = remnant.start == slice.start && remnant.end == slice.end;
            pieces.push(SliceRow {
                start: remnant.start,
                end: remnant.end,
                rows: if untouched { slice.rows } else { None },
            });
        }
    }
    pieces.push(SliceRow {
        start: request.start,
        end: request.end,
        rows: Some(new_rows),
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
                last.rows = match (last.rows, row.rows) {
                    (Some(left), Some(right)) => Some(left.saturating_add(right)),
                    _ => None,
                };
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
                    row.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(store_err)?;
        let mut slices = Vec::new();
        for row in rows {
            let (start, end, count) = row.map_err(store_err)?;
            slices.push(SliceRow {
                start: read_timestamp(start)?,
                end: read_timestamp(end)?,
                rows: count.and_then(|count| u64::try_from(count).ok()),
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
    #[cfg(test)]
    if let Some(probe) = COMMIT_PROBE
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .take()
    {
        probe(&tx).map_err(store_err)?;
    }
    tx.commit().map_err(store_err)?;
    Ok(())
}

#[cfg(test)]
type CommitProbe = Box<dyn for<'a> Fn(&duckdb::Transaction<'a>) -> Result<(), String> + Send>;

#[cfg(test)]
static COMMIT_PROBE: std::sync::Mutex<Option<CommitProbe>> = std::sync::Mutex::new(None);

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
                match slice.rows {
                    Some(rows) => duckdb::types::Value::BigInt(
                        i64::try_from(rows).unwrap_or(i64::MAX),
                    ),
                    None => duckdb::types::Value::Null,
                },
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

    #[test]
    fn plan_refresh_leaves_unknown_row_counts_on_partial_fragments() {
        let held = vec![SliceRow {
            start: day(8, 1),
            end: day(9, 1),
            rows: Some(10),
        }];
        let planned = plan_refresh(&held, range(day(8, 10), day(8, 20)), 4);
        assert_eq!(planned.len(), 1, "{planned:?}");
        assert_eq!(planned[0].start, day(8, 1));
        assert_eq!(planned[0].end, day(9, 1));
        assert_eq!(
            planned[0].rows, None,
            "a leftover fragment cannot keep the parent row count"
        );
    }

    #[test]
    fn commit_failure_rolls_back_table_and_ledger_together() {
        let dir = tempfile::tempdir().unwrap();
        let perf = crate::config::Performance {
            batch_rows: 10,
            memory_limit: "64MiB".to_string(),
            temp_directory: dir.path().join("tmp"),
            progress_every_rows: 1000,
        };
        let store = Store::open(&dir.path().join("store.duckdb"), &perf).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TABLE events (ID VARCHAR, TS VARCHAR);
                 INSERT INTO events VALUES ('1', '2026-08-21 00:00:00');
                 CREATE TABLE events__staging (ID VARCHAR, TS VARCHAR);
                 INSERT INTO events__staging VALUES ('2', '2026-08-22 00:00:00');",
            )
            .unwrap();
        insert_one_slice(&store, "events", day(8, 21), day(8, 22), Some(1));
        *COMMIT_PROBE.lock().unwrap() = Some(Box::new(|tx| {
            let table: i64 = tx
                .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
                .unwrap();
            let slices: i64 = tx
                .query_row(
                    "SELECT count(*) FROM \"_dipper_slices\" WHERE dip = 'events'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if table != 2 {
                return Err(format!(
                    "table change is not inside the commit transaction ({table})"
                ));
            }
            if slices != 2 {
                return Err(format!(
                    "ledger write is not inside the commit transaction ({slices})"
                ));
            }
            Err("injected commit failure".to_string())
        }));
        let err = commit_period_load(
            &store,
            "events",
            "events__staging",
            1,
            &["ID".to_string(), "TS".to_string()],
            PeriodWrite::Fill,
            &[SliceRow {
                start: day(8, 22),
                end: day(8, 23),
                rows: Some(1),
            }],
        )
        .unwrap_err();
        assert!(err.to_string().contains("injected commit failure"), "{err}");
        let count: i64 = store
            .conn
            .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let slices = store.list_slices("events").unwrap();
        assert_eq!(slices.len(), 1);
        assert_eq!(slices[0].rows, Some(1));
    }

    fn insert_one_slice(
        store: &Store,
        dip: &str,
        start: NaiveDateTime,
        end: NaiveDateTime,
        rows: Option<u64>,
    ) {
        let tx = store.conn.unchecked_transaction().unwrap();
        insert_slices(&tx, dip, &[SliceRow { start, end, rows }]).unwrap();
        tx.commit().unwrap();
    }
}
