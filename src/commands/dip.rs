//! `dipper dip` loads selected dips and prints the summary table.

use std::io::Write;
use std::path::Path;

use crate::config::{self};
use crate::error::DipperError;
use crate::pipeline::{self, progress, DefaultOpener, RunOptions};
use crate::source::SqlLogSink;
use crate::store::Store;

pub fn run(
    config_path: &Path,
    names: &[String],
    fail_fast: bool,
    strict: bool,
    log_sql: bool,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    let config = config::load(config_path)?;
    let dips = pipeline::select_dips(&config, names)?;
    let store = Store::open(&config.store, &config.performance)?;
    let opts = RunOptions {
        fail_fast,
        strict: strict || config.strict_readonly,
        log: if log_sql {
            SqlLogSink::Stderr
        } else {
            SqlLogSink::Off
        },
    };
    let mut progress = progress::for_stderr(config.performance.progress_every_rows);
    let mut stderr = std::io::stderr();
    let report = pipeline::run(
        &config,
        &dips,
        &store,
        &opts,
        &DefaultOpener,
        progress.as_mut(),
        &mut stderr,
    );
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
