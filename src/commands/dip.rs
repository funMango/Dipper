//! `dipper dip` loads selected dips and prints the summary table.

use std::io::{IsTerminal, Write};
use std::path::Path;

use chrono::NaiveDateTime;

use crate::cli::OutputFormat;
use crate::commands::dip_output::{
    self, DipBody, DipDocument, FetchBody, IgnoredFilter, Interval, PeriodBody,
};
use crate::config::{
    self, apply_period, describe_window, ledger_interval, parse_instant, range_exceeds_limit,
    resolve_period, rows_exceed_preview, Filter, FilterOp, PeriodWindow, MAX_PERIOD_DAYS,
    MAX_PREVIEW_ROWS,
};
use crate::error::DipperError;
use crate::pipeline::{
    self, progress, spec_for, DefaultOpener, DipOutcome, PeriodJob, PeriodKind, RunOptions,
    RunReport, SourceOpener,
};
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

#[allow(clippy::too_many_arguments)]
pub fn run(
    config_path: &Path,
    names: &[String],
    fail_fast: bool,
    strict: bool,
    log_sql: bool,
    period: PeriodCli,
    format: OutputFormat,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    run_inner(
        config_path,
        names,
        fail_fast,
        strict,
        log_sql,
        period,
        format,
        &DefaultOpener,
        out,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_with_opener(
    config_path: &Path,
    names: &[String],
    fail_fast: bool,
    strict: bool,
    log_sql: bool,
    period: PeriodCli,
    format: OutputFormat,
    opener: &dyn SourceOpener,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    run_inner(
        config_path,
        names,
        fail_fast,
        strict,
        log_sql,
        period,
        format,
        opener,
        out,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_inner(
    config_path: &Path,
    names: &[String],
    fail_fast: bool,
    strict: bool,
    log_sql: bool,
    period: PeriodCli,
    format: OutputFormat,
    opener: &dyn SourceOpener,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    let request = match period_request(&period) {
        Ok(request) => request,
        Err(err) => return finish_error(format, out, err, &[], None),
    };
    // JSON cannot prompt. Refuse a period before reading the config file.
    if format == OutputFormat::Json && request.is_some() && !period.yes {
        let documented = request.as_ref().and_then(|request| {
            resolved_period(request, load_mode(&period))
                .ok()
                .map(|(body, _, _, _)| body)
        });
        return finish_error(
            format,
            out,
            dip_err("--format json needs --yes for a period load"),
            &[],
            documented,
        );
    }
    let config = match config::load(config_path) {
        Ok(config) => config,
        Err(err) => return finish_error(format, out, err, &[], None),
    };
    let secrets = crate::output::oracle_secret_values(&config);
    let selected = match pipeline::select_dips(&config, names) {
        Ok(selected) => selected,
        Err(err) => return finish_error(format, out, err, &secrets, None),
    };
    let strict = strict || config.strict_readonly;
    let log = if log_sql {
        SqlLogSink::Stderr
    } else {
        SqlLogSink::Off
    };

    let Some(request) = request else {
        let store = match Store::open(&config.store, &config.performance) {
            Ok(store) => store,
            Err(err) => return finish_error(format, out, err, &secrets, None),
        };
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
            opener,
            progress.as_mut(),
            &mut stderr,
        );
        let entries = entries_from_report(&report, &selected, &secrets);
        return emit_finished(format, out, &report, None, entries, &secrets);
    };

    let (period_doc, window, request_start, request_end) =
        match resolved_period(&request, load_mode(&period)) {
            Ok(resolved) => resolved,
            Err(err) => return finish_error(format, out, err, &secrets, None),
        };
    let exact = matches!(window, PeriodWindow::Exact { .. });
    let store = match Store::open(&config.store, &config.performance) {
        Ok(store) => store,
        Err(err) => return finish_error(format, out, err, &secrets, Some(period_doc)),
    };
    if !period.replace_table && !period.refresh {
        let mut missing_ledger = Vec::new();
        for dip in &selected {
            let exists = match store.table_exists(&dip.name) {
                Ok(exists) => exists,
                Err(err) => return finish_error(format, out, err, &secrets, Some(period_doc)),
            };
            let slices = match store.has_slices(&dip.name) {
                Ok(slices) => slices,
                Err(err) => return finish_error(format, out, err, &secrets, Some(period_doc)),
            };
            if exists && !slices {
                missing_ledger.push(dip.name.clone());
            }
        }
        if !missing_ledger.is_empty() {
            return finish_error(
                format,
                out,
                dip_err(format!(
                    "{}: has no slice ledger. A period load only fills missing ranges when a ledger exists. Pass --refresh to reload the requested window or --replace-table to replace the whole table. --yes does not bypass this check.",
                    missing_ledger.join(", ")
                )),
                &secrets,
                Some(period_doc),
            );
        }
    }
    let mut plans = Vec::new();
    for dip in &selected {
        let held = match store.list_slices(&dip.name) {
            Ok(held) => held,
            Err(err) => return finish_error(format, out, err, &secrets, Some(period_doc)),
        };
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
                return finish_error(
                    format,
                    out,
                    dip_err(format!(
                        "{}: missing range is longer than {MAX_PERIOD_DAYS} days; narrow the request. Nothing was loaded. --yes does not bypass this check.",
                        dip.name
                    )),
                    &secrets,
                    Some(period_doc),
                );
            }
        }
        let applied = match apply_period(dip, &request.time_column, &window) {
            Ok(applied) => applied,
            Err(err) => return finish_error(format, out, dip_err(err), &secrets, Some(period_doc)),
        };
        let merged = merge_held(&held);
        if holes.is_empty() {
            if let Err(err) =
                note_overridden_filters(&dip.name, &request.time_column, &applied.ignored)
            {
                return finish_error(format, out, err, &secrets, Some(period_doc));
            }
            if let Err(err) = write_interval("requested", &dip.name, request_start, request_end) {
                return finish_error(format, out, err, &secrets, Some(period_doc));
            }
            for slice in &merged {
                if let Err(err) = write_interval("have", &dip.name, slice.start, slice.end) {
                    return finish_error(format, out, err, &secrets, Some(period_doc));
                }
            }
            if let Err(err) = writeln!(std::io::stderr(), "nothing to fetch: {}", dip.name) {
                return finish_error(
                    format,
                    out,
                    dip_err(err.to_string()),
                    &secrets,
                    Some(period_doc),
                );
            }
        }
        plans.push(Plan {
            dip: (*dip).clone(),
            ignored: applied.ignored,
            held: merged,
            holes,
            counts: Vec::new(),
        });
    }
    let pending: Vec<usize> = plans
        .iter()
        .enumerate()
        .filter(|(_, plan)| !plan.holes.is_empty())
        .map(|(index, _)| index)
        .collect();
    if pending.is_empty() {
        if format == OutputFormat::Json {
            let entries = plans
                .iter()
                .map(|plan| plan_entry(plan, "nothing_to_fetch", period.refresh, None, &secrets))
                .collect();
            return write_success_json(out, Some(period_doc), entries);
        }
        return Ok(());
    }
    for index in pending {
        if let Err(err) = note_overridden_filters(
            &plans[index].dip.name,
            &request.time_column,
            &plans[index].ignored,
        ) {
            return finish_error(format, out, err, &secrets, Some(period_doc));
        }
        if let Err(err) = write_interval(
            "requested",
            &plans[index].dip.name,
            request_start,
            request_end,
        ) {
            return finish_error(format, out, err, &secrets, Some(period_doc));
        }
        let held = plans[index].held.clone();
        for slice in held {
            if let Err(err) = write_interval("have", &plans[index].dip.name, slice.start, slice.end)
            {
                return finish_error(format, out, err, &secrets, Some(period_doc));
            }
        }
        if exact {
            if let Err(err) = writeln!(
                std::io::stderr(),
                "preview: {}: {}",
                plans[index].dip.name,
                describe_window(&request.time_column, &window)
            ) {
                return finish_error(
                    format,
                    out,
                    dip_err(err.to_string()),
                    &secrets,
                    Some(period_doc),
                );
            }
        }
        let holes = plans[index].holes.clone();
        for (start, end) in holes {
            let hole_window = if exact {
                window.clone()
            } else {
                PeriodWindow::Range { start, end }
            };
            let prepared = match apply_period(&plans[index].dip, &request.time_column, &hole_window)
            {
                Ok(prepared) => prepared,
                Err(err) => {
                    return finish_error(format, out, dip_err(err), &secrets, Some(period_doc))
                }
            };
            let count = match preview_count(&config, &prepared.dip, strict, log.clone(), opener) {
                Ok(count) => count,
                Err(err) => return finish_error(format, out, err, &secrets, Some(period_doc)),
            };
            if let Err(err) = write_fetch(&plans[index].dip.name, start, end, count) {
                return finish_error(format, out, err, &secrets, Some(period_doc));
            }
            plans[index].counts.push(count);
            if period.replace_table && count == 0 {
                return finish_error(
                    format,
                    out,
                    dip_err(format!(
                        "{}: --replace-table would load 0 rows and replace the table with an empty one; choose a different period. Nothing was loaded.",
                        plans[index].dip.name
                    )),
                    &secrets,
                    Some(period_doc),
                );
            }
            let limit = preview_row_limit();
            if rows_exceed_preview(count, limit) {
                return finish_error(
                    format,
                    out,
                    dip_err(format!(
                        "{}: preview count {count} exceeds {limit}; nothing was loaded",
                        plans[index].dip.name
                    )),
                    &secrets,
                    Some(period_doc),
                );
            }
        }
    }
    if let Err(err) = confirm_period(period.yes) {
        return finish_error(format, out, err, &secrets, Some(period_doc));
    }
    drop(store);
    let kind = if period.refresh {
        PeriodKind::Refresh
    } else if period.replace_table {
        PeriodKind::Replace
    } else {
        PeriodKind::Fill
    };
    let jobs: Vec<PeriodJob> = plans
        .iter()
        .filter(|plan| !plan.holes.is_empty())
        .map(|plan| PeriodJob {
            dip: plan.dip.clone(),
            time_column: request.time_column.clone(),
            exact,
            request_start,
            request_end,
            holes: plan.holes.clone(),
            kind: kind_clone(&kind),
        })
        .collect();
    let store = match Store::open(&config.store, &config.performance) {
        Ok(store) => store,
        Err(err) => return finish_error(format, out, err, &secrets, Some(period_doc)),
    };
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
        opener,
        progress.as_mut(),
        &mut stderr,
    );
    let entries = entries_from_plans(&plans, &report, period.refresh, &secrets);
    emit_finished(format, out, &report, Some(period_doc), entries, &secrets)
}

struct Plan {
    dip: crate::config::Dip,
    ignored: Vec<Filter>,
    held: Vec<HalfOpen>,
    holes: Vec<(NaiveDateTime, NaiveDateTime)>,
    counts: Vec<u64>,
}

fn load_mode(period: &PeriodCli) -> &'static str {
    if period.refresh {
        "refresh"
    } else if period.replace_table {
        "replace"
    } else {
        "fill"
    }
}

fn resolved_period(
    request: &PeriodRequest,
    mode: &str,
) -> Result<(PeriodBody, PeriodWindow, NaiveDateTime, NaiveDateTime), DipperError> {
    let window = resolve_period(
        parse_instant(&request.from).map_err(dip_err)?,
        parse_instant(&request.to).map_err(dip_err)?,
    )
    .map_err(dip_err)?;
    let (start, end) = ledger_interval(&window);
    let body = PeriodBody {
        time_column: request.time_column.clone(),
        from: request.from.clone(),
        to: request.to.clone(),
        exact: matches!(window, PeriodWindow::Exact { .. }),
        start: stamp(start),
        end: stamp(end),
        mode: mode.to_string(),
    };
    Ok((body, window, start, end))
}

fn finish_error(
    format: OutputFormat,
    out: &mut dyn Write,
    error: DipperError,
    secrets: &[crate::output::KnownSecret],
    period: Option<PeriodBody>,
) -> Result<(), DipperError> {
    let error = redact_error(error, secrets);
    if format == OutputFormat::Json {
        dip_output::write_json(out, &dip_output::json_error_document(&error, period))?;
    }
    Err(error)
}

fn redact_error(error: DipperError, secrets: &[crate::output::KnownSecret]) -> DipperError {
    crate::output::redact_error(error, secrets)
}

fn emit_finished(
    format: OutputFormat,
    out: &mut dyn Write,
    report: &RunReport,
    period: Option<PeriodBody>,
    dips: Vec<DipBody>,
    secrets: &[crate::output::KnownSecret],
) -> Result<(), DipperError> {
    if format == OutputFormat::Table {
        return write_report(out, report, secrets);
    }
    let exit_code = if report.failed() == 0 { 0 } else { 1 };
    let doc = DipDocument {
        schema_version: 1,
        command: "dip",
        ok: exit_code == 0,
        exit_code,
        period,
        summary: dip_output::summary_of(&dips),
        dips,
        error: None,
    };
    dip_output::write_json(out, &doc)?;
    if exit_code == 0 {
        Ok(())
    } else {
        Err(DipperError::DipFailed {
            failed: report.failed(),
            total: report.total(),
        })
    }
}

fn write_success_json(
    out: &mut dyn Write,
    period: Option<PeriodBody>,
    dips: Vec<DipBody>,
) -> Result<(), DipperError> {
    let doc = DipDocument {
        schema_version: 1,
        command: "dip",
        ok: true,
        exit_code: 0,
        period,
        summary: dip_output::summary_of(&dips),
        dips,
        error: None,
    };
    dip_output::write_json(out, &doc)
}

fn entries_from_report(
    report: &RunReport,
    selected: &[&crate::config::Dip],
    secrets: &[crate::output::KnownSecret],
) -> Vec<DipBody> {
    let mut dips = Vec::new();
    for outcome in &report.outcomes {
        dips.push(outcome_entry(outcome, false, &[], &[], &[], secrets));
    }
    for name in &report.not_run {
        let source = selected
            .iter()
            .find(|dip| dip.name == *name)
            .map(|dip| dip.source.as_str())
            .unwrap_or("");
        dips.push(empty_entry(name, source, "not_run", false, &[], &[], &[]));
    }
    dips
}

fn entries_from_plans(
    plans: &[Plan],
    report: &RunReport,
    refresh: bool,
    secrets: &[crate::output::KnownSecret],
) -> Vec<DipBody> {
    plans
        .iter()
        .map(|plan| {
            if plan.holes.is_empty() {
                return plan_entry(plan, "nothing_to_fetch", refresh, None, secrets);
            }
            if report.not_run.iter().any(|name| name == &plan.dip.name) {
                return plan_entry(plan, "not_run", refresh, None, secrets);
            }
            let outcome = report
                .outcomes
                .iter()
                .find(|outcome| outcome.dip == plan.dip.name);
            let status = match outcome.and_then(|outcome| outcome.result.as_ref().err()) {
                Some(_) => "failed",
                None if outcome.is_some() => "ok",
                None => "not_run",
            };
            plan_entry(plan, status, refresh, outcome, secrets)
        })
        .collect()
}

fn plan_entry(
    plan: &Plan,
    status: &str,
    refresh: bool,
    outcome: Option<&DipOutcome>,
    secrets: &[crate::output::KnownSecret],
) -> DipBody {
    if let Some(outcome) = outcome {
        outcome_entry(
            outcome,
            refresh,
            &plan.ignored,
            &plan.held,
            &fetch_bodies(&plan.holes, &plan.counts),
            secrets,
        )
    } else {
        empty_entry(
            &plan.dip.name,
            &plan.dip.source,
            status,
            refresh,
            &plan.ignored,
            &plan.held,
            &fetch_bodies(&plan.holes, &plan.counts),
        )
    }
}

fn outcome_entry(
    outcome: &DipOutcome,
    refresh: bool,
    ignored: &[Filter],
    held: &[HalfOpen],
    fetch: &[FetchBody],
    secrets: &[crate::output::KnownSecret],
) -> DipBody {
    let status = if outcome.result.is_ok() {
        "ok"
    } else {
        "failed"
    };
    DipBody {
        name: outcome.dip.clone(),
        source: outcome.source.clone(),
        status: status.to_string(),
        refresh,
        ignored_filters: ignored_filters(ignored),
        held: intervals(held),
        fetch: fetch.to_vec(),
        rows_loaded: outcome.rows,
        seconds: Some(outcome.elapsed.as_secs_f64()),
        warnings: outcome.warnings,
        error: outcome
            .result
            .as_ref()
            .err()
            .map(|message| hide_text(message, secrets)),
    }
}

fn empty_entry(
    name: &str,
    source: &str,
    status: &str,
    refresh: bool,
    ignored: &[Filter],
    held: &[HalfOpen],
    fetch: &[FetchBody],
) -> DipBody {
    DipBody {
        name: name.to_string(),
        source: source.to_string(),
        status: status.to_string(),
        refresh,
        ignored_filters: ignored_filters(ignored),
        held: intervals(held),
        fetch: fetch.to_vec(),
        rows_loaded: 0,
        seconds: None,
        warnings: 0,
        error: None,
    }
}

fn ignored_filters(filters: &[Filter]) -> Vec<IgnoredFilter> {
    filters
        .iter()
        .map(|filter| IgnoredFilter {
            column: filter.column.clone(),
            op: filter_op_name(filter.op).to_string(),
            value: match &filter.value {
                Some(value) => toml_to_json(value),
                None => serde_json::Value::Null,
            },
        })
        .collect()
}

fn intervals(held: &[HalfOpen]) -> Vec<Interval> {
    held.iter()
        .map(|slice| Interval {
            start: stamp(slice.start),
            end: stamp(slice.end),
        })
        .collect()
}

fn fetch_bodies(holes: &[(NaiveDateTime, NaiveDateTime)], counts: &[u64]) -> Vec<FetchBody> {
    holes
        .iter()
        .zip(counts.iter())
        .map(|((start, end), count)| FetchBody {
            start: stamp(*start),
            end: stamp(*end),
            count: *count,
        })
        .collect()
}

fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(text) => serde_json::Value::String(text.clone()),
        toml::Value::Integer(number) => serde_json::Value::from(*number),
        toml::Value::Float(number) => serde_json::Number::from_f64(*number)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(flag) => serde_json::Value::Bool(*flag),
        toml::Value::Datetime(datetime) => serde_json::Value::String(datetime.to_string()),
        toml::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(toml_to_json).collect())
        }
        toml::Value::Table(table) => {
            let mut map = serde_json::Map::new();
            for (key, item) in table {
                map.insert(key.clone(), toml_to_json(item));
            }
            serde_json::Value::Object(map)
        }
    }
}

fn hide_text(text: &str, secrets: &[crate::output::KnownSecret]) -> String {
    crate::output::redact_known_values(text, secrets)
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
    // Release builds ignore the variable. Debug builds may only lower the cap.
    #[cfg(debug_assertions)]
    {
        let requested = std::env::var("DIPPER_TEST_PREVIEW_LIMIT")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(MAX_PREVIEW_ROWS);
        requested.min(MAX_PREVIEW_ROWS)
    }
    #[cfg(not(debug_assertions))]
    {
        MAX_PREVIEW_ROWS
    }
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
    opener: &dyn SourceOpener,
) -> Result<u64, DipperError> {
    let source_cfg = config
        .sources
        .get(&dip.source)
        .ok_or_else(|| dip_err(format!("unknown source: {}", dip.source)))?;
    let opened = opener.open(
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

fn write_report(
    out: &mut dyn Write,
    report: &pipeline::RunReport,
    secrets: &[crate::output::KnownSecret],
) -> Result<(), DipperError> {
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
            .map(|message| hide_text(message, secrets))
            .unwrap_or_default();
        table.add_row(vec![
            outcome.dip.clone(),
            outcome.source.clone(),
            outcome.rows.to_string(),
            format!("{:.1}", outcome.elapsed.as_secs_f64()),
            status.to_string(),
            error,
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
    fn preview_limit_cannot_be_raised_above_one_million() {
        assert_eq!(crate::config::MAX_PREVIEW_ROWS, 1_000_000);
        for raw in ["1000001", &u64::MAX.to_string(), "999999999999"] {
            std::env::set_var("DIPPER_TEST_PREVIEW_LIMIT", raw);
            assert_eq!(
                super::preview_row_limit(),
                1_000_000,
                "{raw} must not raise the preview limit"
            );
        }
        std::env::set_var("DIPPER_TEST_PREVIEW_LIMIT", "0");
        assert_eq!(super::preview_row_limit(), 0);
        std::env::remove_var("DIPPER_TEST_PREVIEW_LIMIT");
        assert_eq!(super::preview_row_limit(), 1_000_000);
    }

    #[test]
    fn only_y_confirms_a_period_load() {
        assert!(accepts_confirmation("y"));
        assert!(accepts_confirmation("Y\n"));
        assert!(!accepts_confirmation("n"));
        assert!(!accepts_confirmation(""));
        assert!(!accepts_confirmation("yes"));
    }
}
