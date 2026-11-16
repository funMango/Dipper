//! `dipper ontology` reads a dictionary and writes local JSON and Markdown.

use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::cli::OutputFormat;
use crate::error::DipperError;
use crate::ontology::{self, render, split_table, validate};
use crate::pipeline::{DefaultOpener, SourceOpener};

pub fn pull(
    config_path: &Path,
    source: &str,
    table: Option<&str>,
    log_sql: bool,
    strict: bool,
    format: OutputFormat,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    pull_inner(
        config_path,
        source,
        table,
        log_sql,
        strict,
        format,
        &DefaultOpener,
        out,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn pull_with_opener(
    config_path: &Path,
    source: &str,
    table: Option<&str>,
    log_sql: bool,
    strict: bool,
    format: OutputFormat,
    opener: &dyn SourceOpener,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    pull_inner(
        config_path,
        source,
        table,
        log_sql,
        strict,
        format,
        opener,
        out,
    )
}

#[allow(clippy::too_many_arguments)]
fn pull_inner(
    config_path: &Path,
    source: &str,
    table: Option<&str>,
    log_sql: bool,
    strict: bool,
    format: OutputFormat,
    opener: &dyn SourceOpener,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    match ontology::pull(config_path, source, table, log_sql, strict, opener) {
        Ok(paths) => {
            let files: Vec<String> = paths
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            if format == OutputFormat::Json {
                write_pull_json(out, source, &files, true, 0, None)?;
            } else {
                for file in &files {
                    writeln!(out, "{file}").map_err(write_err)?;
                }
            }
            Ok(())
        }
        Err(err) => {
            let secrets = crate::config::load(config_path)
                .map(|config| crate::output::oracle_secret_values(&config))
                .unwrap_or_default();
            let err = redact_ontology_error(err, &secrets);
            if format == OutputFormat::Json {
                write_pull_json(
                    out,
                    source,
                    &[],
                    false,
                    err.exit_code(),
                    Some(err.to_string()),
                )?;
            }
            Err(err)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn check_relation(
    config_path: &Path,
    source: &str,
    from_table: &str,
    from_columns: &[String],
    to_table: &str,
    to_columns: &[String],
    log_sql: bool,
    strict: bool,
    format: OutputFormat,
    out: &mut dyn Write,
) -> Result<(), DipperError> {
    let outcome = run_check(
        config_path,
        source,
        from_table,
        from_columns,
        to_table,
        to_columns,
        log_sql,
        strict,
    );
    match outcome {
        Ok(body) => {
            if format == OutputFormat::Json {
                let text = serde_json::to_string(&body).map_err(write_json_err)?;
                writeln!(out, "{text}").map_err(write_err)?;
            } else {
                writeln!(out, "source_non_null_rows: {}", body.source_non_null_rows)
                    .map_err(write_err)?;
                writeln!(out, "target_non_null_rows: {}", body.target_non_null_rows)
                    .map_err(write_err)?;
                writeln!(out, "matched_rows: {}", body.matched_rows).map_err(write_err)?;
                writeln!(out, "match_ratio: {}", body.match_ratio).map_err(write_err)?;
            }
            Ok(())
        }
        Err(err) => {
            let secrets = crate::config::load(config_path)
                .map(|config| crate::output::oracle_secret_values(&config))
                .unwrap_or_default();
            let err = redact_ontology_error(err, &secrets);
            if format == OutputFormat::Json {
                let body = CheckDocument {
                    schema_version: 1,
                    command: "ontology.check_relation",
                    ok: false,
                    exit_code: err.exit_code(),
                    source: source.to_string(),
                    from: CheckSide {
                        owner: String::new(),
                        table: from_table.to_string(),
                        columns: from_columns.to_vec(),
                    },
                    to: CheckSide {
                        owner: String::new(),
                        table: to_table.to_string(),
                        columns: to_columns.to_vec(),
                    },
                    source_non_null_rows: 0,
                    target_non_null_rows: 0,
                    matched_rows: 0,
                    match_ratio: 0.0,
                    error: Some(PullError {
                        message: err.to_string(),
                    }),
                };
                let text = serde_json::to_string(&body).map_err(write_json_err)?;
                writeln!(out, "{text}").map_err(write_err)?;
            }
            Err(err)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_check(
    config_path: &Path,
    source_name: &str,
    from_table: &str,
    from_columns: &[String],
    to_table: &str,
    to_columns: &[String],
    log_sql: bool,
    strict: bool,
) -> Result<CheckDocument, DipperError> {
    if from_columns.len() != to_columns.len() || from_columns.is_empty() {
        return Err(DipperError::Source {
            source_name: source_name.to_string(),
            message: "from-column and to-column counts must match".to_string(),
        });
    }
    let config = crate::config::load(config_path)?;
    let source_cfg = config
        .sources
        .get(source_name)
        .ok_or_else(|| DipperError::Source {
            source_name: source_name.to_string(),
            message: format!("unknown source: {source_name}"),
        })?;
    let (from_owner, from_name) = split_table(from_table, source_cfg)?;
    let (to_owner, to_name) = split_table(to_table, source_cfg)?;
    let strict = strict || config.strict_readonly;
    let opened = crate::source::open(
        source_name,
        source_cfg,
        &crate::source::OpenOptions {
            strict,
            log: if log_sql {
                crate::source::SqlLogSink::Stderr
            } else {
                crate::source::SqlLogSink::Off
            },
            action: format!("ontology:{source_name}"),
        },
    )?;
    let counts = opened.source.check_relation(
        &from_owner,
        &from_name,
        from_columns,
        &to_owner,
        &to_name,
        to_columns,
    )?;
    let ratio = if counts.source_non_null_rows == 0 {
        0.0
    } else {
        counts.matched_rows as f64 / counts.source_non_null_rows as f64
    };
    Ok(CheckDocument {
        schema_version: 1,
        command: "ontology.check_relation",
        ok: true,
        exit_code: 0,
        source: source_name.to_string(),
        from: CheckSide {
            owner: from_owner,
            table: from_name,
            columns: from_columns.to_vec(),
        },
        to: CheckSide {
            owner: to_owner,
            table: to_name,
            columns: to_columns.to_vec(),
        },
        source_non_null_rows: counts.source_non_null_rows,
        target_non_null_rows: counts.target_non_null_rows,
        matched_rows: counts.matched_rows,
        match_ratio: ratio,
        error: None,
    })
}

pub fn render_docs(root: &Path, out: &mut dyn Write) -> Result<(), DipperError> {
    let paths = render(root)?;
    for path in &paths {
        writeln!(out, "{}", path.display()).map_err(write_err)?;
    }
    writeln!(out, "{} ontology files rendered", paths.len()).map_err(write_err)?;
    Ok(())
}

pub fn validate_docs(root: &Path, out: &mut dyn Write) -> Result<(), DipperError> {
    validate(root)?;
    writeln!(out, "ontology ok").map_err(write_err)?;
    Ok(())
}

#[derive(Serialize)]
struct PullDocument<'a> {
    schema_version: i32,
    command: &'static str,
    ok: bool,
    exit_code: i32,
    source: &'a str,
    files: &'a [String],
    error: Option<PullError>,
}

#[derive(Serialize)]
struct PullError {
    message: String,
}

#[derive(Serialize)]
struct CheckSide {
    owner: String,
    table: String,
    columns: Vec<String>,
}

#[derive(Serialize)]
struct CheckDocument {
    schema_version: i32,
    command: &'static str,
    ok: bool,
    exit_code: i32,
    source: String,
    from: CheckSide,
    to: CheckSide,
    source_non_null_rows: u64,
    target_non_null_rows: u64,
    matched_rows: u64,
    match_ratio: f64,
    error: Option<PullError>,
}

fn write_pull_json(
    out: &mut dyn Write,
    source: &str,
    files: &[String],
    ok: bool,
    exit_code: i32,
    message: Option<String>,
) -> Result<(), DipperError> {
    let doc = PullDocument {
        schema_version: 1,
        command: "ontology.pull",
        ok,
        exit_code,
        source,
        files,
        error: message.map(|message| PullError { message }),
    };
    let text = serde_json::to_string(&doc).map_err(write_json_err)?;
    writeln!(out, "{text}").map_err(write_err)
}

fn redact_ontology_error(
    error: DipperError,
    secrets: &[crate::output::KnownSecret],
) -> DipperError {
    crate::output::redact_error(error, secrets)
}

fn write_err(err: std::io::Error) -> DipperError {
    DipperError::Source {
        source_name: "stdout".to_string(),
        message: err.to_string(),
    }
}

fn write_json_err(err: serde_json::Error) -> DipperError {
    DipperError::Source {
        source_name: "stdout".to_string(),
        message: err.to_string(),
    }
}
