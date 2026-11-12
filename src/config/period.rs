//! Period bounds for `dipper dip --from/--to`. Values stay bound parameters.

use chrono::{NaiveDate, NaiveDateTime, TimeDelta};

use crate::config::{Dip, Filter, FilterOp};
use crate::sql::validate_column_ident;

pub const MAX_PERIOD_DAYS: i64 = 31;
pub const MAX_PREVIEW_ROWS: u64 = 1_000_000;

/// A parsed `--from` or `--to` value. Date-only inputs have no clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeriodInstant {
    pub at: NaiveDateTime,
    pub has_time: bool,
}

/// How a period narrows one time column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeriodWindow {
    /// `column >= start AND column < end`.
    Range {
        start: NaiveDateTime,
        end: NaiveDateTime,
    },
    /// Both bounds are the same timestamp: one bucket, `column = at`.
    Exact { at: NaiveDateTime },
}

pub fn rows_exceed_preview(rows: u64, limit: u64) -> bool {
    rows > limit
}

pub fn parse_instant(text: &str) -> Result<PeriodInstant, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty period value".to_string());
    }
    if text.len() == 6 && text.bytes().all(|byte| byte.is_ascii_digit()) {
        let year = 2000 + parse_num(&text[0..2])?;
        let month = parse_num(&text[2..4])?;
        let day = parse_num(&text[4..6])?;
        let date = date_opt(year, month, day)?;
        return Ok(PeriodInstant {
            at: midnight(date)?,
            has_time: false,
        });
    }
    if text.len() == 10
        && text.as_bytes().get(4) == Some(&b'-')
        && text.as_bytes().get(7) == Some(&b'-')
    {
        let year = parse_num(&text[0..4])?;
        let month = parse_num(&text[5..7])?;
        let day = parse_num(&text[8..10])?;
        let date = date_opt(year, month, day)?;
        return Ok(PeriodInstant {
            at: midnight(date)?,
            has_time: false,
        });
    }
    if text.len() == 19 && text.as_bytes().get(10) == Some(&b'T') {
        if text.contains('Z') || text.contains('+') {
            return Err("period timestamps cannot include a timezone".to_string());
        }
        let date = parse_instant(&text[..10])?;
        let clock = &text[11..];
        if clock.len() != 8
            || clock.as_bytes().get(2) != Some(&b':')
            || clock.as_bytes().get(5) != Some(&b':')
        {
            return Err("period time must be HH:MM:SS".to_string());
        }
        let hour = parse_num(&clock[0..2])?;
        let minute = parse_num(&clock[3..5])?;
        let second = parse_num(&clock[6..8])?;
        let at = date
            .at
            .date()
            .and_hms_opt(hour, minute, second)
            .ok_or_else(|| "invalid period time".to_string())?;
        return Ok(PeriodInstant { at, has_time: true });
    }
    Err("period must be YYMMDD, YYYY-MM-DD, or YYYY-MM-DDTHH:MM:SS".to_string())
}

pub fn resolve_period(from: PeriodInstant, to: PeriodInstant) -> Result<PeriodWindow, String> {
    if from.has_time && to.has_time && from.at == to.at {
        return Ok(PeriodWindow::Exact { at: from.at });
    }
    let start = from.at;
    let end = if to.has_time {
        to.at
    } else {
        next_midnight(to.at.date())?
    };
    if start >= end {
        return Err("period start is after period end".to_string());
    }
    Ok(PeriodWindow::Range { start, end })
}

/// Ledger interval for a window. An exact instant is the marker `[T, T+1s)`.
pub fn ledger_interval(window: &PeriodWindow) -> (NaiveDateTime, NaiveDateTime) {
    match window {
        PeriodWindow::Range { start, end } => (*start, *end),
        PeriodWindow::Exact { at } => (*at, *at + TimeDelta::seconds(1)),
    }
}

/// True when a missing half-open range is longer than [`MAX_PERIOD_DAYS`].
pub fn range_exceeds_limit(start: NaiveDateTime, end: NaiveDateTime) -> bool {
    end - start > TimeDelta::days(MAX_PERIOD_DAYS)
}

/// One dip after CLI period filters replace same-column config filters.
pub struct AppliedPeriod {
    pub dip: Dip,
    /// Config filters dropped for this run, in config order. The file is unchanged.
    pub ignored: Vec<Filter>,
}

pub fn apply_period(
    dip: &Dip,
    time_column: &str,
    window: &PeriodWindow,
) -> Result<AppliedPeriod, String> {
    validate_column_ident(time_column)?;
    let mut filters = Vec::with_capacity(dip.filters.len() + 2);
    let mut ignored = Vec::new();
    for filter in &dip.filters {
        if filter.column.eq_ignore_ascii_case(time_column) {
            ignored.push(filter.clone());
        } else {
            filters.push(filter.clone());
        }
    }
    // A matching config filter keeps that file's spelling. Otherwise use the CLI spelling.
    let column = ignored
        .first()
        .map(|filter| filter.column.as_str())
        .unwrap_or(time_column);
    match window {
        PeriodWindow::Range { start, end } => {
            filters.push(bound_filter(column, FilterOp::Gte, *start));
            filters.push(bound_filter(column, FilterOp::Lt, *end));
        }
        PeriodWindow::Exact { at } => {
            filters.push(bound_filter(column, FilterOp::Eq, *at));
        }
    }
    Ok(AppliedPeriod {
        dip: Dip {
            name: dip.name.clone(),
            source: dip.source.clone(),
            table: dip.table.clone(),
            columns: dip.columns.clone(),
            filters,
            limit: dip.limit,
            count_first: dip.count_first,
            key: dip.key.clone(),
            paging: dip.paging,
        },
        ignored,
    })
}

pub fn describe_window(column: &str, window: &PeriodWindow) -> String {
    match window {
        PeriodWindow::Range { start, end } => format!(
            "{column} gte {}, lt {}",
            format_stamp(*start),
            format_stamp(*end)
        ),
        PeriodWindow::Exact { at } => format!("{column} eq {}", format_stamp(*at)),
    }
}

fn bound_filter(column: &str, op: FilterOp, at: NaiveDateTime) -> Filter {
    let text = at.format("%Y-%m-%dT%H:%M:%S").to_string();
    Filter {
        column: column.to_string(),
        op,
        value: Some(toml::Value::Datetime(
            text.parse()
                .expect("formatted timestamp is a TOML datetime"),
        )),
    }
}

fn format_stamp(at: NaiveDateTime) -> String {
    at.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn parse_num(text: &str) -> Result<u32, String> {
    text.parse::<u32>()
        .map_err(|_| "invalid period number".to_string())
}

fn date_opt(year: u32, month: u32, day: u32) -> Result<NaiveDate, String> {
    let year = i32::try_from(year).map_err(|_| "invalid period year".to_string())?;
    NaiveDate::from_ymd_opt(year, month, day).ok_or_else(|| "invalid period date".to_string())
}

fn midnight(date: NaiveDate) -> Result<NaiveDateTime, String> {
    date.and_hms_opt(0, 0, 0)
        .ok_or_else(|| "invalid period date".to_string())
}

fn next_midnight(date: NaiveDate) -> Result<NaiveDateTime, String> {
    let next = date
        .succ_opt()
        .ok_or_else(|| "period end is out of range".to_string())?;
    midnight(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Dip, FilterOp, Paging};

    fn stamp(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").unwrap()
    }

    fn sample(filters: Vec<crate::config::Filter>) -> Dip {
        Dip {
            name: "events".into(),
            source: "sample".into(),
            table: "EVENTS".into(),
            columns: vec!["TS".into()],
            filters,
            limit: None,
            count_first: false,
            key: None,
            paging: Paging::Keyset,
        }
    }

    #[test]
    fn parses_yymmdd_iso_date_and_timestamp() {
        let ymd = parse_instant("260821").unwrap();
        assert!(!ymd.has_time);
        assert_eq!(ymd.at, stamp("2026-08-21T00:00:00"));
        let iso = parse_instant("2026-08-21").unwrap();
        assert_eq!(iso, ymd);
        let timed = parse_instant("2026-08-21T09:00:00").unwrap();
        assert!(timed.has_time);
        assert_eq!(timed.at, stamp("2026-08-21T09:00:00"));
    }

    #[test]
    fn date_only_end_is_inclusive_and_same_day_is_one_day() {
        let from = parse_instant("260821").unwrap();
        let to = parse_instant("260920").unwrap();
        let window = resolve_period(from, to).unwrap();
        match window {
            PeriodWindow::Range { start, end } => {
                assert_eq!(start, stamp("2026-08-21T00:00:00"));
                assert_eq!(end, stamp("2026-09-21T00:00:00"));
            }
            PeriodWindow::Exact { .. } => panic!("expected a range"),
        }
        let one = resolve_period(from, parse_instant("260821").unwrap()).unwrap();
        match one {
            PeriodWindow::Range { start, end } => {
                assert_eq!(start, stamp("2026-08-21T00:00:00"));
                assert_eq!(end, stamp("2026-08-22T00:00:00"));
            }
            PeriodWindow::Exact { .. } => panic!("a date-only day is a range"),
        }
    }

    #[test]
    fn thirty_one_days_allowed_and_thirty_two_rejected() {
        let short = resolve_period(
            parse_instant("260821").unwrap(),
            parse_instant("260920").unwrap(),
        )
        .unwrap();
        let (start, end) = ledger_interval(&short);
        assert!(!range_exceeds_limit(start, end));
        let long = resolve_period(
            parse_instant("260821").unwrap(),
            parse_instant("260921").unwrap(),
        )
        .unwrap();
        let (start, end) = ledger_interval(&long);
        assert!(range_exceeds_limit(start, end));
    }

    #[test]
    fn exact_timestamp_is_stored_as_one_second_half_open() {
        let window = resolve_period(
            parse_instant("2026-08-21T08:00:00").unwrap(),
            parse_instant("2026-08-21T08:00:00").unwrap(),
        )
        .unwrap();
        let (start, end) = ledger_interval(&window);
        assert_eq!(start, stamp("2026-08-21T08:00:00"));
        assert_eq!(end, stamp("2026-08-21T08:00:01"));
    }

    #[test]
    fn rejects_bad_dates_order_and_timezones() {
        assert!(parse_instant("").is_err());
        assert!(parse_instant("260231").is_err());
        assert!(parse_instant("2026-08-21T09:00").is_err());
        assert!(parse_instant("2026-08-21T09:00:00Z").is_err());
        assert!(parse_instant("2026-08-21T09:00:00+09:00").is_err());
        let err = resolve_period(
            parse_instant("260822").unwrap(),
            parse_instant("260821").unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("after"), "{err}");
    }

    #[test]
    fn timed_end_is_exclusive_and_the_same_instant_is_one_bucket() {
        let from = parse_instant("2026-08-21T08:00:00").unwrap();
        let to = parse_instant("2026-08-21T09:00:00").unwrap();
        match resolve_period(from, to).unwrap() {
            PeriodWindow::Range { start, end } => {
                assert_eq!(start, stamp("2026-08-21T08:00:00"));
                assert_eq!(end, stamp("2026-08-21T09:00:00"));
            }
            PeriodWindow::Exact { .. } => panic!("expected a half-open range"),
        }
        match resolve_period(from, from).unwrap() {
            PeriodWindow::Exact { at } => assert_eq!(at, stamp("2026-08-21T08:00:00")),
            PeriodWindow::Range { .. } => panic!("the same timestamp is one bucket"),
        }
    }

    #[test]
    fn apply_appends_bounds_and_refuses_a_duplicate_column() {
        let window = resolve_period(
            parse_instant("260821").unwrap(),
            parse_instant("260920").unwrap(),
        )
        .unwrap();
        let applied = apply_period(&sample(Vec::new()), "TOT_DT", &window).unwrap();
        assert!(applied.ignored.is_empty());
        assert_eq!(applied.dip.filters.len(), 2);
        assert_eq!(applied.dip.filters[0].column, "TOT_DT");
        assert_eq!(applied.dip.filters[0].op, FilterOp::Gte);
        assert_eq!(applied.dip.filters[1].op, FilterOp::Lt);
        assert!(describe_window("TOT_DT", &window).contains("gte"));
        assert!(describe_window("TOT_DT", &window).contains("lt"));

        let exact = resolve_period(
            parse_instant("2026-08-21T08:00:00").unwrap(),
            parse_instant("2026-08-21T08:00:00").unwrap(),
        )
        .unwrap();
        let applied = apply_period(&sample(Vec::new()), "TS", &exact).unwrap();
        assert_eq!(applied.dip.filters.len(), 1);
        assert_eq!(applied.dip.filters[0].column, "TS");
        assert_eq!(applied.dip.filters[0].op, FilterOp::Eq);
        assert!(describe_window("TS", &exact).contains("eq"));

        let existing = sample(vec![
            Filter {
                column: "ID".into(),
                op: FilterOp::Gt,
                value: Some(toml::Value::Integer(0)),
            },
            Filter {
                column: "TS".into(),
                op: FilterOp::Eq,
                value: None,
            },
            Filter {
                column: "ts".into(),
                op: FilterOp::Lt,
                value: None,
            },
        ]);
        let applied = apply_period(&existing, "ts", &window).unwrap();
        assert_eq!(
            applied
                .ignored
                .iter()
                .map(|filter| filter.column.as_str())
                .collect::<Vec<_>>(),
            ["TS", "ts"]
        );
        assert_eq!(applied.dip.filters.len(), 3);
        assert_eq!(applied.dip.filters[0].column, "ID");
        assert_eq!(applied.dip.filters[0].op, FilterOp::Gt);
        assert!(applied
            .dip
            .filters
            .iter()
            .skip(1)
            .all(|filter| filter.column == "TS"));
        assert_eq!(applied.dip.filters[1].op, FilterOp::Gte);
        assert_eq!(applied.dip.filters[2].op, FilterOp::Lt);
        assert!(apply_period(&sample(Vec::new()), "1bad", &exact).is_err());
    }

    #[test]
    fn period_filter_uses_the_first_config_column_spelling() {
        let window = resolve_period(
            parse_instant("260821").unwrap(),
            parse_instant("260821").unwrap(),
        )
        .unwrap();
        let configured = sample(vec![Filter {
            column: "TS".into(),
            op: FilterOp::Eq,
            value: Some(toml::Value::Datetime(
                "2026-08-21T08:00:00".parse().unwrap(),
            )),
        }]);
        let applied = apply_period(&configured, "ts", &window).unwrap();
        assert_eq!(applied.ignored.len(), 1);
        assert_eq!(applied.ignored[0].column, "TS");
        assert!(applied
            .dip
            .filters
            .iter()
            .all(|filter| filter.column == "TS"));
        assert!(applied
            .dip
            .filters
            .iter()
            .any(|filter| filter.op == FilterOp::Gte));
        assert!(applied
            .dip
            .filters
            .iter()
            .any(|filter| filter.op == FilterOp::Lt));
        assert!(!applied
            .dip
            .filters
            .iter()
            .any(|filter| filter.op == FilterOp::Eq));

        let untouched = apply_period(&sample(Vec::new()), "ts", &window).unwrap();
        assert!(untouched.ignored.is_empty());
        assert!(untouched
            .dip
            .filters
            .iter()
            .all(|filter| filter.column == "ts"));
    }

    #[test]
    fn preview_limit_is_one_million() {
        assert_eq!(MAX_PREVIEW_ROWS, 1_000_000);
        assert!(!rows_exceed_preview(1_000_000, MAX_PREVIEW_ROWS));
        assert!(rows_exceed_preview(1_000_001, MAX_PREVIEW_ROWS));
        assert!(rows_exceed_preview(5, 4));
        assert!(!rows_exceed_preview(4, 4));
    }
}
