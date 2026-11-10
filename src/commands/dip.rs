//! `dipper dip` loads selected dips and prints the summary table.

use std::io::{IsTerminal, Write};
use std::path::Path;

use crate::config::{
    self, apply_period, describe_window, parse_instant, resolve_period, rows_exceed_preview,
    AppliedPeriod, Filter, FilterOp, MAX_PREVIEW_ROWS,
};
use crate::error::DipperError;
use crate::pipeline::{self, progress, spec_for, DefaultOpener, RunOptions};
use crate::source::{Level, OpenOptions, SqlLogSink};
use crate::store::Store;

/// Optional period bounds for one `dipper dip` invocation.
pub struct PeriodCli {
    pub from: Option<String>,
    pub to: Option<String>,
    pub time_column: Option<String>,
    pub yes: bool,
    pub replace_table: bool,
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

    let dips: Vec<crate::config::Dip> = if let Some(request) = request {
        let window = resolve_period(
            parse_instant(&request.from).map_err(dip_err)?,
            parse_instant(&request.to).map_err(dip_err)?,
        )
        .map_err(dip_err)?;
        let mut prepared: Vec<AppliedPeriod> = Vec::with_capacity(selected.len());
        for dip in &selected {
            prepared.push(apply_period(dip, &request.time_column, &window).map_err(dip_err)?);
        }
        let store = Store::open(&config.store, &config.performance)?;
        if !period.replace_table {
            let mut existing = Vec::new();
            for applied in &prepared {
                if store.table_exists(&applied.dip.name)? {
                    existing.push(applied.dip.name.clone());
                }
            }
            if !existing.is_empty() {
                return Err(dip_err(format!(
                    "local table(s) already exist ({}). A period load replaces the whole table and drops existing rows; it does not accumulate periods (that is PR6). Pass --replace-table to replace them. --yes does not bypass this check.",
                    existing.join(", ")
                )));
            }
        }
        for applied in &prepared {
            note_overridden_filters(&applied.dip.name, &request.time_column, &applied.ignored)?;
        }
        for applied in &prepared {
            let dip = &applied.dip;
            let count = preview_count(&config, dip, strict, log.clone())?;
            writeln!(
                std::io::stderr(),
                "preview: {}: {}, count {count}",
                dip.name,
                describe_window(&request.time_column, &window)
            )
            .map_err(|err| dip_err(err.to_string()))?;
            if rows_exceed_preview(count, MAX_PREVIEW_ROWS) {
                return Err(dip_err(format!(
                    "{}: preview count {count} exceeds {MAX_PREVIEW_ROWS}; nothing was loaded",
                    dip.name
                )));
            }
        }
        confirm_period(period.yes)?;
        drop(store);
        prepared.into_iter().map(|applied| applied.dip).collect()
    } else {
        Vec::new()
    };

    let store = Store::open(&config.store, &config.performance)?;
    let opts = RunOptions {
        fail_fast,
        strict,
        log,
    };
    let mut progress = progress::for_stderr(config.performance.progress_every_rows);
    let mut stderr = std::io::stderr();
    let report = if dips.is_empty() && request_was_empty(&period) {
        pipeline::run(
            &config,
            &selected,
            &store,
            &opts,
            &DefaultOpener,
            progress.as_mut(),
            &mut stderr,
        )
    } else {
        let refs: Vec<&crate::config::Dip> = dips.iter().collect();
        pipeline::run(
            &config,
            &refs,
            &store,
            &opts,
            &DefaultOpener,
            progress.as_mut(),
            &mut stderr,
        )
    };
    write_report(out, &report)
}

fn request_was_empty(period: &PeriodCli) -> bool {
    period.from.is_none() && period.to.is_none()
}

struct PeriodRequest {
    from: String,
    to: String,
    time_column: String,
}

fn period_request(period: &PeriodCli) -> Result<Option<PeriodRequest>, DipperError> {
    match (
        period.from.as_deref(),
        period.to.as_deref(),
        period.time_column.as_deref(),
        period.replace_table,
    ) {
        (None, None, None, false) => Ok(None),
        (Some(from), Some(to), Some(column), _) => Ok(Some(PeriodRequest {
            from: from.to_string(),
            to: to.to_string(),
            time_column: column.to_string(),
        })),
        (None, None, _, true) => Err(dip_err("--replace-table requires --from and --to")),
        (None, None, Some(_), false) => Err(dip_err("--time-column requires --from and --to")),
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
