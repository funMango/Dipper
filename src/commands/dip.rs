//! `dipper dip` loads selected dips and prints the summary table.

use std::io::{IsTerminal, Write};
use std::path::Path;

use chrono::NaiveDateTime;

use crate::config::{
    self, apply_period, describe_window, ledger_interval, parse_instant, range_exceeds_limit,
    resolve_period, rows_exceed_preview, Filter, FilterOp, PeriodWindow, MAX_PERIOD_DAYS,
    MAX_PREVIEW_ROWS,
};
use crate::error::DipperError;
use crate::pipeline::{self, progress, spec_for, DefaultOpener, PeriodJob, PeriodKind, RunOptions};
use crate::source::{Level, OpenOptions, SqlLogSink};
use crate::store::{HalfOpen, SliceRow, Store};

/// Optional period bounds for one `dipper dip` invocation.
pub struct PeriodCli {
    pub from: Option<String>,
    pub to: Option<String>,
    pub time_column: Option<String>,
    pub yes: bool,
    pub replace_table: bool,
    pub refresh: bool,
}

pub fn run(
    config_path: &Path,
    names: &[String],
    fail_fast: bool,
    strict: bool,
    log_sql: bool,
    period: PeriodCli,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    let request = period_request(&period)?;
    let config = config::load(config_path)?;
    let selected = pipeline::select_dips(&config, names)?;
    let strict = strict || config.strict_readonly;
    let log = if log_sql {
        SqlLogSink::Stderr
    } else {
        SqlLogSink::Off
    };

    let Some(request) = request else {
        let store = Store::open(&config.store, &config.performance)?;
        let opts = RunOptions {
            fail_fast,
            strict,
            log,
        };
        let mut progress = progress::for_stderr(config.performance.progress_every_rows);
        let mut stderr = std::io::stderr();
        let report = pipeline::run(
            &config,
            &selected,
            &store,
            &opts,
            &DefaultOpener,
            progress.as_mut(),
            &mut stderr,
        );
        return write_report(out, &report);
    };

    let window = resolve_period(
        parse_instant(&request.from).map_err(dip_err)?,
        parse_instant(&request.to).map_err(dip_err)?,
    )
    .map_err(dip_err)?;
    let (request_start, request_end) = ledger_interval(&window);
    let exact = matches!(window, PeriodWindow::Exact { .. });
    let store = Store::open(&config.store, &config.performance)?;
    if !period.replace_table && !period.refresh {
        let mut missing_ledger = Vec::new();
        for dip in &selected {
            if store.table_exists(&dip.name)? && !store.has_slices(&dip.name)? {
                missing_ledger.push(dip.name.clone());
            }
        }
        if !missing_ledger.is_empty() {
            return Err(dip_err(format!(
                "{}: has no slice ledger. A period load only fills missing ranges when a ledger exists. Pass --refresh to reload the requested window or --replace-table to replace the whole table. --yes does not bypass this check.",
                missing_ledger.join(", ")
            )));
        }
    }
    let mut jobs = Vec::new();
    for dip in &selected {
        let held = store.list_slices(&dip.name)?;
        let held_ranges: Vec<HalfOpen> = held
            .iter()
            .map(|slice| HalfOpen {
                start: slice.start,
                end: slice.end,
            })
            .collect();
        let holes = if period.refresh || period.replace_table {
            vec![(request_start, request_end)]
        } else {
            crate::store::subtract_ranges(
                HalfOpen {
                    start: request_start,
                    end: request_end,
                },
                &held_ranges,
            )
            .into_iter()
            .map(|hole| (hole.start, hole.end))
            .collect::<Vec<_>>()
        };
        for (start, end) in &holes {
            if range_exceeds_limit(*start, *end) {
                return Err(dip_err(format!(
                    "{}: missing range is longer than {MAX_PERIOD_DAYS} days; narrow the request. Nothing was loaded. --yes does not bypass this check.",
                    dip.name
                )));
            }
        }
        let applied = apply_period(dip, &request.time_column, &window).map_err(dip_err)?;
        if holes.is_empty() {
            note_overridden_filters(&dip.name, &request.time_column, &applied.ignored)?;
            write_interval("requested", &dip.name, request_start, request_end)?;
            for slice in merge_held(&held) {
                write_interval("have", &dip.name, slice.start, slice.end)?;
            }
            writeln!(std::io::stderr(), "nothing to fetch: {}", dip.name)
                .map_err(|err| dip_err(err.to_string()))?;
            continue;
        }
        jobs.push(PeriodWork {
            dip: (*dip).clone(),
            ignored: applied.ignored,
            held,
            holes,
        });
    }
    if jobs.is_empty() {
        return Ok(());
    }
    for work in &jobs {
        note_overridden_filters(&work.dip.name, &request.time_column, &work.ignored)?;
        write_interval("requested", &work.dip.name, request_start, request_end)?;
        for slice in merge_held(&work.held) {
            write_interval("have", &work.dip.name, slice.start, slice.end)?;
        }
        if exact {
            writeln!(
                std::io::stderr(),
                "preview: {}: {}",
                work.dip.name,
                describe_window(&request.time_column, &window)
            )
            .map_err(|err| dip_err(err.to_string()))?;
        }
        for (start, end) in &work.holes {
            let hole_window = if exact {
                window.clone()
            } else {
                PeriodWindow::Range {
                    start: *start,
                    end: *end,
                }
            };
            let prepared =
                apply_period(&work.dip, &request.time_column, &hole_window).map_err(dip_err)?;
            let count = preview_count(&config, &prepared.dip, strict, log.clone())?;
            write_fetch(&work.dip.name, *start, *end, count)?;
            if period.replace_table && count == 0 && store.table_exists(&work.dip.name)? {
                return Err(dip_err(format!(
                    "{}: --replace-table would load 0 rows and replace the table with an empty one; choose a different period. Nothing was loaded.",
                    work.dip.name
                )));
            }
            let limit = preview_row_limit();
            if rows_exceed_preview(count, limit) {
                return Err(dip_err(format!(
                    "{}: preview count {count} exceeds {limit}; nothing was loaded",
                    work.dip.name
                )));
            }
        }
    }
    confirm_period(period.yes)?;
    drop(store);
    let kind = if period.refresh {
        PeriodKind::Refresh
    } else if period.replace_table {
        PeriodKind::Replace
    } else {
        PeriodKind::Fill
    };
    let jobs: Vec<PeriodJob> = jobs
        .into_iter()
        .map(|work| PeriodJob {
            dip: work.dip,
            time_column: request.time_column.clone(),
            exact,
            request_start,
            request_end,
            holes: work.holes,
            kind: kind_clone(&kind),
        })
        .collect();
    let store = Store::open(&config.store, &config.performance)?;
    let opts = RunOptions {
        fail_fast,
        strict,
        log,
    };
    let mut progress = progress::for_stderr(config.performance.progress_every_rows);
    let mut stderr = std::io::stderr();
    let report = pipeline::run_period(
        &config,
        &jobs,
        &store,
        &opts,
        &DefaultOpener,
        progress.as_mut(),
        &mut stderr,
    );
    write_report(out, &report)
}

struct PeriodWork {
    dip: crate::config::Dip,
    ignored: Vec<Filter>,
    held: Vec<SliceRow>,
    holes: Vec<(NaiveDateTime, NaiveDateTime)>,
}

fn kind_clone(kind: &PeriodKind) -> PeriodKind {
    match kind {
        PeriodKind::Fill => PeriodKind::Fill,
        PeriodKind::Refresh => PeriodKind::Refresh,
        PeriodKind::Replace => PeriodKind::Replace,
    }
}

fn merge_held(held: &[SliceRow]) -> Vec<HalfOpen> {
    crate::store::merge_ranges(
        &held
            .iter()
            .map(|slice| HalfOpen {
                start: slice.start,
                end: slice.end,
            })
            .collect::<Vec<_>>(),
    )
}

fn write_interval(
    label: &str,
    dip: &str,
    start: NaiveDateTime,
    end: NaiveDateTime,
) -> Result<(), DipperError> {
    writeln!(
        std::io::stderr(),
        "preview: {dip}: {label} [{}, {})",
        stamp(start),
        stamp(end)
    )
    .map_err(|err| dip_err(err.to_string()))
}

fn write_fetch(
    dip: &str,
    start: NaiveDateTime,
    end: NaiveDateTime,
    count: u64,
) -> Result<(), DipperError> {
    writeln!(
        std::io::stderr(),
        "preview: {dip}: fetch [{}, {}), count {count}",
        stamp(start),
        stamp(end)
    )
    .map_err(|err| dip_err(err.to_string()))
}

fn preview_row_limit() -> u64 {
    // Test-only override. The shipped limit stays MAX_PREVIEW_ROWS.
    if let Ok(raw) = std::env::var("DIPPER_TEST_PREVIEW_LIMIT") {
        if let Ok(limit) = raw.parse::<u64>() {
            return limit;
        }
    }
    MAX_PREVIEW_ROWS
}

fn stamp(at: NaiveDateTime) -> String {
    at.format("%Y-%m-%dT%H:%M:%S").to_string()
}

struct PeriodRequest {
    from: String,
    to: String,
    time_column: String,
}

fn period_request(period: &PeriodCli) -> Result<Option<PeriodRequest>, DipperError> {
    if period.refresh && period.replace_table {
        return Err(dip_err("--refresh cannot be combined with --replace-table"));
    }
    match (
        period.from.as_deref(),
        period.to.as_deref(),
        period.time_column.as_deref(),
        period.replace_table,
        period.refresh,
    ) {
        (None, None, None, false, false) => Ok(None),
        (Some(from), Some(to), Some(column), _, _) => Ok(Some(PeriodRequest {
            from: from.to_string(),
            to: to.to_string(),
            time_column: column.to_string(),
        })),
        (None, None, _, true, _) => Err(dip_err("--replace-table requires --from and --to")),
        (None, None, _, _, true) => Err(dip_err("--refresh requires --from and --to")),
        (None, None, Some(_), false, false) => {
            Err(dip_err("--time-column requires --from and --to"))
        }
        _ => Err(dip_err(
            "--from, --to, and --time-column must be set together",
        )),
    }
}

fn preview_count(
    config: &crate::config::Config,
    dip: &crate::config::Dip,
    strict: bool,
    log: SqlLogSink,
) -> Result<u64, DipperError> {
    let source_cfg = config
        .sources
        .get(&dip.source)
        .ok_or_else(|| dip_err(format!("unknown source: {}", dip.source)))?;
    let opened = crate::source::open(
        &dip.source,
        source_cfg,
        &OpenOptions {
            strict,
            log,
            action: format!("dip:{}", dip.name),
        },
    )?;
    let spec = spec_for(config, dip);
    let mut findings = opened.findings.clone();
    let report = opened.source.preflight(&dip.name, &spec)?;
    findings.extend(report.findings);
    if let Some(reason) = block_reason(&findings, strict) {
        drop(opened);
        return Err(dip_err(format!("{}: {reason}", dip.name)));
    }
    let count = opened.source.count(&spec)?;
    drop(opened);
    Ok(count)
}

fn block_reason(findings: &[crate::source::Finding], strict: bool) -> Option<String> {
    if let Some(finding) = findings
        .iter()
        .find(|finding| finding.level == Level::Refuse)
    {
        return Some(format!("refused: {}", finding.message));
    }
    if strict {
        let warnings = findings
            .iter()
            .filter(|finding| finding.level == Level::Warn)
            .count();
        if warnings > 0 {
            return Some(format!("strict: {warnings} warning(s)"));
        }
    }
    None
}

fn note_overridden_filters(
    dip_name: &str,
    time_column: &str,
    ignored: &[Filter],
) -> Result<(), DipperError> {
    if ignored.is_empty() {
        return Ok(());
    }
    writeln!(
        std::io::stderr(),
        "note: {dip_name}: --from/--to overrides config filters on time column {time_column} for this run; the config file was not changed"
    )
    .map_err(|err| dip_err(err.to_string()))?;
    for filter in ignored {
        let value = match &filter.value {
            Some(value) => value.to_string(),
            None => "-".to_string(),
        };
        writeln!(
            std::io::stderr(),
            "ignored filter: {} {} {value}",
            filter.column,
            filter_op_name(filter.op)
        )
        .map_err(|err| dip_err(err.to_string()))?;
    }
    Ok(())
}

fn filter_op_name(op: FilterOp) -> &'static str {
    match op {
        FilterOp::Eq => "eq",
        FilterOp::Ne => "ne",
        FilterOp::In => "in",
        FilterOp::Gt => "gt",
        FilterOp::Gte => "gte",
        FilterOp::Lt => "lt",
        FilterOp::Lte => "lte",
        FilterOp::Between => "between",
        FilterOp::Like => "like",
        FilterOp::IsNull => "is_null",
        FilterOp::NotNull => "not_null",
    }
}

fn confirm_period(yes: bool) -> Result<(), DipperError> {
    if yes {
        return Ok(());
    }
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(dip_err(
            "period load needs confirmation; rerun with --yes (stdin is not a terminal)",
        ));
    }
    eprint!("Proceed? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|err| dip_err(err.to_string()))?;
    if accepts_confirmation(&line) {
        Ok(())
    } else {
        Err(dip_err("period load cancelled"))
    }
}

pub(crate) fn accepts_confirmation(line: &str) -> bool {
    let answer = line.trim();
    answer == "y" || answer == "Y"
}

fn write_report(out: &mut dyn Write, report: &pipeline::RunReport) -> Result<(), DipperError> {
    if report.outcomes.is_empty() && report.not_run.is_empty() {
        write_line(out, "0 of 0 dips OK")?;
        return Ok(());
    }
    let mut table = comfy_table::Table::new();
    table.load_style(comfy_table::presets::ASCII_FULL);
    table.set_header(vec!["dip", "source", "rows", "seconds", "status", "error"]);
    for outcome in &report.outcomes {
        let status = if outcome.result.is_ok() { "OK" } else { "FAIL" };
        let error = outcome
            .result
            .as_ref()
            .err()
            .map(String::as_str)
            .unwrap_or("");
        table.add_row(vec![
            outcome.dip.clone(),
            outcome.source.clone(),
            outcome.rows.to_string(),
            format!("{:.1}", outcome.elapsed.as_secs_f64()),
            status.to_string(),
            error.to_string(),
        ]);
    }
    write_line(out, table.to_string().trim_end())?;
    if !report.not_run.is_empty() {
        write_line(
            out,
            &format!(
                "stopped at first failure (--fail-fast): {} dip(s) not run: {}",
                report.not_run.len(),
                report.not_run.join(", ")
            ),
        )?;
    }
    let ok = report
        .outcomes
        .iter()
        .filter(|outcome| outcome.result.is_ok())
        .count();
    write_line(out, &format!("{ok} of {} dips OK", report.total()))?;
    let failed = report.failed();
    if failed == 0 {
        Ok(())
    } else {
        Err(DipperError::DipFailed {
            failed,
            total: report.total(),
        })
    }
}

fn write_line(out: &mut dyn Write, line: &str) -> Result<(), DipperError> {
    writeln!(out, "{line}").map_err(|err| DipperError::Source {
        source_name: "stdout".to_string(),
        message: err.to_string(),
    })
}

fn dip_err(message: impl Into<String>) -> DipperError {
    DipperError::Source {
        source_name: "dip".to_string(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::accepts_confirmation;

    #[test]
    fn only_y_confirms_a_period_load() {
        assert!(accepts_confirmation("y"));
        assert!(accepts_confirmation("Y\n"));
        assert!(!accepts_confirmation("n"));
        assert!(!accepts_confirmation(""));
        assert!(!accepts_confirmation("yes"));
    }
}
